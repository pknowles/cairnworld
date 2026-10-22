use leptos::prelude::*;
use serde::{Deserialize, Serialize};

use crate::inspection::{DevEvent, ObjectType};

#[cfg(feature = "hydrate")]
use {
    crate::inspection::{
        AgentInfo, BranchGroup, BranchNode, BranchParent, Content, MessageContent, MessageOrigin,
        RawHistoryCursor, RawHistoryEntry, RawHistoryPageResponse, RecordedAction,
        RecordedInference, RecordedOutcome, Request, Response, Role, SequenceInspection,
        SequenceNode, ToolCall, WorldObjects,
    },
    std::{cell::RefCell, rc::Rc},
    wasm_bindgen::{JsCast, JsValue, closure::Closure},
    wasm_bindgen_futures::{JsFuture, spawn_local},
    web_sys::{
        Event, HtmlElement, IntersectionObserver, IntersectionObserverEntry, MessageEvent,
        Response as WebResponse, WebSocket,
    },
};

/// Messages accepted from the browser's player-chat transport.
#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientEvent {
    Message { text: String },
}

/// Player-visible events emitted by the server's player-chat transport.
#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerEvent {
    Entry {
        role: ChatRole,
        text: String,
        /// Who said it, for speech whose authored text does not carry attribution.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        speaker: Option<String>,
        /// The stored message and its Developer Mode sequence, when this
        /// entry comes from a durable row. `None` for text streamed before
        /// its row is known to exist; a later resync fills it in, so a live
        /// entry does not have to block on an extra store round trip.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<i64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        sequence_id: Option<i64>,
    },
    Activity {
        activity: ChatActivity,
    },
    CanAct {
        value: bool,
    },
    Error {
        message: String,
    },
    /// The server is shutting down and is about to close this connection.
    Stopping,
}

/// Work the server is currently performing for a connected player.
#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ChatActivity {
    PreparingOpening,
    Responding,
}

/// The source of a player-visible chat entry.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ChatRole {
    User,
    Assistant,
    Narration,
    /// A character's words, attributed in the text.
    Speech,
    Notice,
}

/// A stored or live entry rendered in the player's chat transcript.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct ChatEntry {
    pub role: ChatRole,
    pub text: String,
    /// Who said it, for speech whose authored text does not carry attribution.
    #[serde(default)]
    pub speaker: Option<String>,
    /// See `ServerEvent::Entry`: `None` until this entry's durable row (and,
    /// for Developer Mode, its sequence) is known.
    #[serde(default)]
    pub id: Option<i64>,
    #[serde(default)]
    pub sequence_id: Option<i64>,
}

/// What the Developer Mode inspect pane is currently showing. Selecting a
/// different view never navigates or reopens the chat socket - the pane just
/// re-fetches and re-renders in place. Matches user_declarations.md's four
/// views: the turn (sequence) view, the inference view, the agent chat
/// history view, and the world object/agent navigator.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum InspectSelection {
    Sequence {
        id: i64,
        entry_id: Option<i64>,
    },
    Inference(i64),
    AgentHistory {
        agent_id: i64,
        branch_id: Option<i64>,
        entry_id: Option<i64>,
    },
    Agents,
    Objects {
        object_type: Option<ObjectType>,
        id: Option<i64>,
    },
}

/// The inspect part of the existing game-page URL. It is deliberately a
/// small conversion beside the selection data, not a second client router:
/// PlayerChat stays mounted while the inspected record changes.
#[cfg(feature = "hydrate")]
impl InspectSelection {
    fn from_location() -> Result<Option<Self>, String> {
        let window = web_sys::window().ok_or("no browser window is available")?;
        let search = window
            .location()
            .search()
            .map_err(|error| format!("could not read the page URL: {error:?}"))?;
        let query = web_sys::UrlSearchParams::new_with_str(&search)
            .map_err(|error| format!("could not read inspect URL parameters: {error:?}"))?;
        let Some(kind) = query.get("inspect") else {
            return Ok(None);
        };
        let id = |name: &str| {
            query
                .get(name)
                .ok_or_else(|| format!("inspect={kind} needs {name}"))?
                .parse::<i64>()
                .map_err(|_| format!("inspect={kind} has an invalid {name}"))
        };
        match kind.as_str() {
            "turn" => Ok(Some(Self::Sequence {
                id: id("id")?,
                entry_id: query
                    .get("entry")
                    .map(|value| value.parse())
                    .transpose()
                    .map_err(|_| "inspect=turn has an invalid entry")?,
            })),
            "inference" => Ok(Some(Self::Inference(id("id")?))),
            "history" => Ok(Some(Self::AgentHistory {
                agent_id: id("agent")?,
                branch_id: query
                    .get("branch")
                    .map(|value| value.parse())
                    .transpose()
                    .map_err(|_| "inspect=history has an invalid branch")?,
                entry_id: query
                    .get("entry")
                    .map(|value| value.parse())
                    .transpose()
                    .map_err(|_| "inspect=history has an invalid entry")?,
            })),
            "agents" => Ok(Some(Self::Agents)),
            "objects" => Ok(Some(Self::Objects {
                object_type: query
                    .get("type")
                    .map(|value| {
                        ObjectType::parse(&value)
                            .ok_or_else(|| format!("inspect=objects has an invalid type {value:?}"))
                    })
                    .transpose()?,
                id: query
                    .get("id")
                    .map(|value| value.parse())
                    .transpose()
                    .map_err(|_| "inspect=objects has an invalid id")?,
            })),
            _ => Err(format!("unknown inspect view {kind:?}")),
        }
    }

    fn write_to_location(selection: Option<Self>) -> Result<(), String> {
        let window = web_sys::window().ok_or("no browser window is available")?;
        let location = window.location();
        let search = location
            .search()
            .map_err(|error| format!("could not read the page URL: {error:?}"))?;
        let query = web_sys::UrlSearchParams::new_with_str(&search)
            .map_err(|error| format!("could not read inspect URL parameters: {error:?}"))?;
        if selection.is_some() && Self::from_location()? == selection {
            return Ok(());
        }
        if selection.is_none() && query.get("inspect").is_none() {
            return Ok(());
        }
        for name in ["inspect", "id", "agent", "branch", "entry", "type"] {
            query.delete(name);
        }
        if let Some(selection) = selection {
            match selection {
                Self::Sequence { id, entry_id } => {
                    query.set("inspect", "turn");
                    query.set("id", &id.to_string());
                    if let Some(entry_id) = entry_id {
                        query.set("entry", &entry_id.to_string());
                    }
                }
                Self::Inference(id) => {
                    query.set("inspect", "inference");
                    query.set("id", &id.to_string());
                }
                Self::AgentHistory {
                    agent_id,
                    branch_id,
                    entry_id,
                } => {
                    query.set("inspect", "history");
                    query.set("agent", &agent_id.to_string());
                    if let Some(branch_id) = branch_id {
                        query.set("branch", &branch_id.to_string());
                    }
                    if let Some(entry_id) = entry_id {
                        query.set("entry", &entry_id.to_string());
                    }
                }
                Self::Agents => query.set("inspect", "agents"),
                Self::Objects { object_type, id } => {
                    query.set("inspect", "objects");
                    if let Some(object_type) = object_type {
                        query.set("type", object_type.as_str());
                    }
                    if let Some(id) = id {
                        query.set("id", &id.to_string());
                    }
                }
            }
        }
        let query = query
            .to_string()
            .as_string()
            .ok_or("could not serialize inspect URL parameters")?;
        let path = location
            .pathname()
            .map_err(|error| format!("could not read the page path: {error:?}"))?;
        let hash = location
            .hash()
            .map_err(|error| format!("could not read the page fragment: {error:?}"))?;
        let url = format!(
            "{path}{}{hash}",
            if query.is_empty() {
                String::new()
            } else {
                format!("?{query}")
            }
        );
        window
            .history()
            .map_err(|error| format!("could not access browser history: {error:?}"))?
            .push_state_with_url(&JsValue::NULL, "", Some(&url))
            .map_err(|error| format!("could not update inspect URL: {error:?}"))
    }

    fn scroll_position() -> Option<i32> {
        web_sys::window()?
            .history()
            .ok()?
            .state()
            .ok()?
            .as_f64()
            .and_then(|value| i32::try_from(value as i64).ok())
    }

    fn remember_scroll(scroll_top: i32) -> Result<(), String> {
        let window = web_sys::window().ok_or("no browser window is available")?;
        let location = window.location();
        let path = location
            .pathname()
            .map_err(|error| format!("could not read the page path: {error:?}"))?;
        let search = location
            .search()
            .map_err(|error| format!("could not read the page URL: {error:?}"))?;
        let hash = location
            .hash()
            .map_err(|error| format!("could not read the page fragment: {error:?}"))?;
        window
            .history()
            .map_err(|error| format!("could not access browser history: {error:?}"))?
            .replace_state_with_url(
                &JsValue::from_f64(f64::from(scroll_top)),
                "",
                Some(&format!("{path}{search}{hash}")),
            )
            .map_err(|error| format!("could not save inspect scroll position: {error:?}"))
    }
}

/// One branch's own paged entries inside the whole-DAG response from
/// `/inspect/agents/:id/branches`, keyed separately from every sibling
/// branch's paging state - a merge's shared ancestor is one `BranchSection`
/// referenced by more than one child, never duplicated.
#[cfg(feature = "hydrate")]
#[derive(Clone, Debug)]
struct BranchSection {
    branch_id: i64,
    parents: Vec<BranchParent>,
    head: Option<BranchGroup>,
    entries: Vec<RawHistoryEntry>,
    cursor: Option<RawHistoryCursor>,
}

/// Whichever view is currently loaded in the pane, keyed by `InspectSelection`.
#[cfg(feature = "hydrate")]
#[derive(Clone, Debug)]
enum InspectData {
    Sequence {
        id: i64,
        trigger: String,
        caused_by: Vec<i64>,
        nodes: Vec<SequenceNode>,
    },
    Inference(RecordedInference),
    /// `branches` is never empty: a plain player/NPC/Storyteller agent has
    /// exactly one section here, with no parents and no head, rendered
    /// exactly as the single linear view always has been. A world_gm agent
    /// whose groups have split/merged has one section per branch in its DAG.
    AgentHistory {
        agent_id: i64,
        branches: Vec<BranchSection>,
    },
    Agents(Vec<AgentInfo>),
    Objects(WorldObjects),
}

/// Mutable state shared by inspection fetches. Keeping it together makes a
/// fetch operate on one coherent pane generation rather than passing several
/// unrelated signals through every recursive refresh path.
#[cfg(feature = "hydrate")]
#[derive(Clone, Copy)]
struct FetchState {
    generation: RwSignal<u64>,
    data: RwSignal<Option<InspectData>>,
    error: RwSignal<Option<String>>,
    invalidated_during_fetch: RwSignal<bool>,
    in_flight: RwSignal<bool>,
}

/// The inspection pane's navigation, visibility and retained scroll position.
#[derive(Clone, Copy)]
struct PaneState {
    selection: RwSignal<Option<InspectSelection>>,
    open: RwSignal<bool>,
    last_selection: RwSignal<Option<InspectSelection>>,
    scroll: NodeRef<leptos::html::Aside>,
    restore_scroll: RwSignal<Option<i32>>,
}

/// The pane is owned by the chat island, so switching views only changes
/// this local state; the mounted chat and its websocket stay intact
/// (user_declarations.md: "Links between views are important for
/// navigation" - none of them navigate away from the game page).
fn inspect_pane(
    world_id: i64,
    pane: PaneState,
    narrow_inspect: RwSignal<bool>,
    dev_events: RwSignal<Option<DevEvent>>,
    developer_mode: RwSignal<bool>,
    dev_error: RwSignal<Option<String>>,
    dev_reconnect_nonce: RwSignal<u32>,
) -> AnyView {
    let scroll = pane.scroll;
    let restore_scroll = pane.restore_scroll;
    let selection = pane.selection;
    let inspect_open = pane.open;
    let last_inspecting = pane.last_selection;
    #[cfg(not(feature = "hydrate"))]
    let _ = (
        world_id,
        inspect_open,
        last_inspecting,
        narrow_inspect,
        dev_events,
        developer_mode,
        dev_error,
        dev_reconnect_nonce,
        scroll,
        restore_scroll,
    );
    #[cfg(feature = "hydrate")]
    let data = RwSignal::new(None::<InspectData>);
    // The existing authorized agent list is the one source of human-facing
    // identities for every inspection view. Inference records retain only the
    // durable foreign key; do not introduce copied names into every record.
    #[cfg(feature = "hydrate")]
    let agents = RwSignal::new(Vec::<AgentInfo>::new());
    let error = RwSignal::new(None::<String>);
    // A persistent notice from a broadcast-lag resync, shown until the next
    // selection loads cleanly. Distinct from `error` (a failed fetch/parse):
    // this reports lost live coverage even though the reload that follows it
    // usually succeeds.
    let resync_notice = RwSignal::new(None::<String>);
    // Text streamed for a still-running inference, keyed by inference id.
    // Patched live from dev_events; cleared once that inference completes
    // and the recorded outcome takes over. Only the turn view refreshes its
    // node list live on InferenceStarted - the other three views describe a
    // point-in-time snapshot (an agent's history, the object list) that a
    // live turn does not change.
    #[cfg(feature = "hydrate")]
    let live_tokens = RwSignal::new(std::collections::HashMap::<i64, String>::new());
    // Bumped on every selection change; an in-flight fetch tags itself with
    // the generation active when it started and is discarded on arrival if
    // the pane has since moved on (design.md: "a response applies only to
    // that generation").
    #[cfg(feature = "hydrate")]
    let generation = RwSignal::new(0u64);
    // Set while the current generation's fetch is in flight and a `Changed`
    // matching its target arrived before the response did; on arrival the
    // just-loaded snapshot is refetched once rather than trusted, since it
    // may already be stale (design.md: "fetch again once ... repeat only in
    // response to actual changes").
    #[cfg(feature = "hydrate")]
    let invalidated_during_fetch = RwSignal::new(false);
    // Whether the current generation's fetch is still awaiting its
    // response - distinct from `invalidated_during_fetch` (whether a
    // matching Changed arrived while it was), so a Changed event can tell
    // "already fetching, remember to refetch after" apart from "idle, fetch
    // now" without inferring it from `data`/`error`.
    #[cfg(feature = "hydrate")]
    let fetch_in_flight = RwSignal::new(false);
    #[cfg(feature = "hydrate")]
    let fetch_state = FetchState {
        generation,
        data,
        error,
        invalidated_during_fetch,
        in_flight: fetch_in_flight,
    };

    /// Fetches the view for `selected`, tagging the request with `generation`
    /// so a response arriving after the pane has moved to a different
    /// selection is silently dropped instead of overwriting newer state.
    #[cfg(feature = "hydrate")]
    fn fetch_selection(
        world_id: i64,
        selected: InspectSelection,
        for_generation: u64,
        state: FetchState,
    ) {
        state.invalidated_during_fetch.set(false);
        state.in_flight.set(true);
        let apply = move |result: InspectData| {
            // Stale: the pane has since moved to a different selection.
            if state.generation.get_untracked() != for_generation {
                return;
            }
            state.in_flight.set(false);
            state.data.set(Some(result));
            // A Changed event for this exact target arrived while this
            // fetch was in flight - the snapshot that just landed may
            // already be behind it, so reload once more.
            if state.invalidated_during_fetch.get_untracked() {
                fetch_selection(world_id, selected, for_generation, state);
            }
        };
        match selected {
            InspectSelection::Sequence {
                id: sequence_id, ..
            } => fetch_json(
                format!("/world/{world_id}/inspect/sequences/{sequence_id}"),
                "turn view",
                move |inspection: SequenceInspection| {
                    apply(InspectData::Sequence {
                        id: sequence_id,
                        trigger: inspection.trigger,
                        caused_by: inspection.caused_by,
                        nodes: inspection.nodes,
                    })
                },
                state.error,
            ),
            InspectSelection::Inference(inference_id) => fetch_json(
                format!("/world/{world_id}/inspect/inferences/{inference_id}"),
                "inference view",
                move |inference| apply(InspectData::Inference(inference)),
                state.error,
            ),
            InspectSelection::AgentHistory {
                agent_id,
                branch_id,
                entry_id,
            } => fetch_json(
                format!("/world/{world_id}/inspect/agents/{agent_id}/branches"),
                "agent branch graph",
                move |nodes: Vec<BranchNode>| {
                    fetch_branch_sections(
                        world_id,
                        agent_id,
                        branch_id,
                        entry_id,
                        nodes,
                        apply,
                        state.error,
                    );
                },
                state.error,
            ),
            InspectSelection::Agents => fetch_json(
                format!("/world/{world_id}/inspect/agents"),
                "agent list",
                move |agents| apply(InspectData::Agents(agents)),
                state.error,
            ),
            InspectSelection::Objects { object_type, id } => fetch_json(
                format!(
                    "/world/{world_id}/inspect/objects{}{}",
                    object_type
                        .map(|value| format!("?type={}", value.as_str()))
                        .unwrap_or_default(),
                    id.map(|value| format!(
                        "{}id={value}",
                        if object_type.is_some() { "&" } else { "?" }
                    ))
                    .unwrap_or_default()
                ),
                "world objects",
                move |objects| apply(InspectData::Objects(objects)),
                state.error,
            ),
        }
    }

    /// Fetches every branch's own first history page and combines them into
    /// one `InspectData::AgentHistory` once all have arrived - a single
    /// linear agent has exactly one node here, so this costs one extra small
    /// unpaged request over the old direct history fetch, not an extra round
    /// trip for the common case (both requests fire in parallel).
    #[cfg(feature = "hydrate")]
    fn fetch_branch_sections(
        world_id: i64,
        agent_id: i64,
        selected_branch: Option<i64>,
        selected_entry: Option<i64>,
        nodes: Vec<BranchNode>,
        apply: impl Fn(InspectData) + Clone + 'static,
        error: RwSignal<Option<String>>,
    ) {
        let remaining = Rc::new(RefCell::new(nodes.len()));
        let sections: Rc<RefCell<Vec<Option<BranchSection>>>> =
            Rc::new(RefCell::new(vec![None; nodes.len()]));
        for (index, node) in nodes.into_iter().enumerate() {
            let remaining = remaining.clone();
            let sections = sections.clone();
            let apply = apply.clone();
            let BranchNode {
                branch_id,
                parents,
                head,
            } = node;
            let entry = (selected_branch == Some(branch_id))
                .then_some(selected_entry)
                .flatten();
            fetch_json(
                format!(
                    "/world/{world_id}/inspect/agents/{agent_id}/history?branch={branch_id}{}",
                    entry
                        .map(|entry| format!("&entry={entry}"))
                        .unwrap_or_default()
                ),
                "agent chat history",
                move |page: RawHistoryPageResponse| {
                    sections.borrow_mut()[index] = Some(BranchSection {
                        branch_id,
                        parents: parents.clone(),
                        head: head.clone(),
                        entries: page.entries,
                        cursor: page.cursor,
                    });
                    *remaining.borrow_mut() -= 1;
                    if *remaining.borrow() == 0 {
                        let branches = sections
                            .borrow_mut()
                            .iter_mut()
                            .map(|section| section.take().expect("every branch section was filled"))
                            .collect();
                        apply(InspectData::AgentHistory { agent_id, branches });
                    }
                },
                error,
            );
        }
    }

    #[cfg(feature = "hydrate")]
    Effect::new(move |_| {
        let Some(selected) = selection.get() else {
            data.set(None);
            error.set(None);
            resync_notice.set(None);
            return;
        };
        generation.update(|generation| *generation += 1);
        let for_generation = generation.get_untracked();
        data.set(None);
        error.set(None);
        resync_notice.set(None);
        live_tokens.set(std::collections::HashMap::new());
        if agents.get_untracked().is_empty() {
            fetch_json(
                format!("/world/{world_id}/inspect/agents"),
                "agent identities",
                move |items| agents.set(items),
                error,
            );
        }
        fetch_selection(world_id, selected, for_generation, fetch_state);
    });

    #[cfg(feature = "hydrate")]
    Effect::new(move |_| {
        let Some(scroll_top) = restore_scroll.get() else {
            return;
        };
        if data.get().is_some() {
            if let Some(element) = scroll.get() {
                let element: HtmlElement = element.unchecked_into();
                element.set_scroll_top(scroll_top);
            }
            restore_scroll.set(None);
        }
    });

    // fetch_json reports a failed/unparseable response only through `error`,
    // never through the success callback `fetch_selection` clears
    // `fetch_in_flight` in - clear it here too so a later Changed event does
    // not wait forever on a fetch that already failed.
    #[cfg(feature = "hydrate")]
    Effect::new(move |_| {
        if error.get().is_some() {
            fetch_in_flight.set(false);
        }
    });

    /// Whether an already-loaded `data` targets the same durable record a
    /// `Changed` event names, so an unrelated turn/inference completing
    /// elsewhere in the world does not trigger a refetch here.
    #[cfg(feature = "hydrate")]
    fn changed_matches(
        data: &InspectData,
        sequence_id: Option<i64>,
        inference_id: Option<i64>,
    ) -> bool {
        match data {
            InspectData::Sequence { nodes, .. } => {
                sequence_id.is_some_and(|id| {
                    nodes
                        .iter()
                        .any(|node| node.inference.sequence_id == Some(id))
                }) || inference_id
                    .is_some_and(|id| nodes.iter().any(|node| node.inference.id == id))
            }
            InspectData::Inference(inference) => inference_id == Some(inference.id),
            // A history page is a durable timeline for this world. Changed
            // events do not carry every affected branch, so refresh an open
            // history for any committed world change rather than leaving it
            // stale until the developer reopens it.
            InspectData::AgentHistory { .. } => true,
            InspectData::Agents(_) | InspectData::Objects(_) => false,
        }
    }

    #[cfg(feature = "hydrate")]
    Effect::new(move |_| {
        let Some(event) = dev_events.get() else {
            return;
        };
        match event {
            DevEvent::Token {
                inference_id,
                offset,
                text,
            } => {
                live_tokens.update(|tokens| {
                    let accumulated = tokens.entry(inference_id).or_default();
                    // A gap between what this viewer has accumulated and the
                    // server's offset means earlier chunks were dropped
                    // (broadcast lag): mark the loss visibly instead of
                    // silently rendering a short prefix as if it were whole.
                    if accumulated.len() < offset && !accumulated.ends_with("…missing…") {
                        accumulated.push_str("…missing…");
                    }
                    accumulated.push_str(&text);
                });
            }
            DevEvent::InferenceStarted {
                inference_id,
                agent_id,
                sequence_id,
                parent_inference_id,
            } => {
                let Some(InspectSelection::Sequence {
                    id: open_sequence_id,
                    ..
                }) = selection.get_untracked()
                else {
                    return;
                };
                if sequence_id != Some(open_sequence_id) {
                    return;
                }
                data.update(|data| {
                    if let Some(InspectData::Sequence { nodes, .. }) = data
                        && !nodes.iter().any(|node| node.inference.id == inference_id)
                    {
                        nodes.push(SequenceNode {
                            inference: RecordedInference {
                                id: inference_id,
                                agent_id,
                                branch_id: 0,
                                history_entry_id: 0,
                                sequence_id,
                                parent_inference_id,
                                cause_inference_id: None,
                                segments: Vec::new(),
                                request: Request {
                                    messages: Vec::new(),
                                    tools: Vec::new(),
                                    sampling: Default::default(),
                                },
                                model_prompt: None,
                                prompt_evidence: None,
                                outcome: RecordedOutcome::Running,
                                model: String::new(),
                                duration_ms: None,
                            },
                            messages: Vec::new(),
                            actions: Vec::new(),
                        });
                    }
                });
            }
            DevEvent::InferenceCompleted { inference_id, .. } => {
                live_tokens.update(|tokens| {
                    tokens.remove(&inference_id);
                });
                // The recorded outcome (exact text, reasoning, usage) only
                // exists once finish_inference's write commits; re-fetch
                // this node's real data rather than guessing it from tokens.
                // Completion also refreshes a standalone inference view
                // opened on this same inference (design.md: "Inference
                // completion refreshes either a selected turn or that
                // standalone inference").
                match selection.get_untracked() {
                    Some(InspectSelection::Sequence {
                        id: sequence_id, ..
                    }) => {
                        let for_generation = generation.get_untracked();
                        fetch_selection(
                            world_id,
                            InspectSelection::Sequence {
                                id: sequence_id,
                                entry_id: None,
                            },
                            for_generation,
                            fetch_state,
                        );
                    }
                    Some(InspectSelection::Inference(selected_id))
                        if selected_id == inference_id =>
                    {
                        let for_generation = generation.get_untracked();
                        fetch_selection(
                            world_id,
                            InspectSelection::Inference(inference_id),
                            for_generation,
                            fetch_state,
                        );
                    }
                    _ => {}
                }
            }
            DevEvent::Changed {
                sequence_id,
                inference_id,
            } => {
                let Some(selected) = selection.get_untracked() else {
                    return;
                };
                let matches = data.with_untracked(|data| {
                    data.as_ref()
                        .is_some_and(|data| changed_matches(data, sequence_id, inference_id))
                });
                if !matches {
                    return;
                }
                // A fetch for the current generation is already in flight:
                // record the invalidation so `fetch_selection` refetches
                // once more after that response lands, rather than racing
                // a second concurrent fetch against the first.
                if fetch_in_flight.get_untracked() {
                    invalidated_during_fetch.set(true);
                } else {
                    let for_generation = generation.get_untracked();
                    fetch_selection(world_id, selected, for_generation, fetch_state);
                }
            }
            DevEvent::ResyncRequired { skipped } => {
                resync_notice.set(Some(format!(
                    "{skipped} earlier live update{} were missed; reloaded the current view.",
                    if skipped == 1 { "" } else { "s" },
                )));
                live_tokens.set(std::collections::HashMap::new());
                if let Some(selected) = selection.get_untracked() {
                    let for_generation = generation.get_untracked();
                    fetch_selection(world_id, selected, for_generation, fetch_state);
                }
            }
            DevEvent::DeveloperModeChanged { enabled } => {
                // A disabled mode must clear all private pane data
                // client-side, not just hide it - the pane is about to
                // disappear (developer_mode gates its mount in PlayerChat),
                // but stale data must not linger for a later re-enable.
                if !enabled {
                    data.set(None);
                    error.set(None);
                    resync_notice.set(None);
                    live_tokens.set(std::collections::HashMap::new());
                    selection.set(None);
                }
            }
            DevEvent::AccessChanged => {}
        }
    });

    #[cfg(feature = "hydrate")]
    let _ = developer_mode;

    // Closed is a thin, always-visible strip on the edge - never removed from
    // the DOM and never requiring a separate reopen control elsewhere
    // (user_exerpts.md 2026-09-25/2026-09-28: options 1+3 combined, not
    // option 1's separate chat-header button alone). The last selection stays
    // live underneath so expanding shows exactly what Close left.
    let expand = move |_| {
        inspect_open.set(true);
        if selection.get_untracked().is_none() {
            selection.set(
                last_inspecting
                    .get_untracked()
                    .or(Some(InspectSelection::Agents)),
            );
        }
    };
    view! {
        <button
            id="inspect-strip"
            class=move || format!(
                "btn btn-ghost btn-xs self-start border-l border-base-300 [writing-mode:vertical-lr] {}",
                if inspect_open.get() { "hidden" } else { "" },
            )
            on:click=expand
        >
            "Inspect"
        </button>
        <aside node_ref=scroll class=move || format!(
            "min-h-0 w-full max-w-md flex-col gap-3 overflow-y-auto border-l border-base-300 p-3 sm:p-5 {}",
            if inspect_open.get() {
                if narrow_inspect.get() { "flex" } else { "hidden sm:flex" }
            } else {
                "hidden"
            },
        )>
            <div class="flex items-center justify-between">
                <h2 class="text-lg font-semibold">"Inspect"</h2>
                <div class="flex gap-1">
                    <button class="btn btn-ghost btn-xs" on:click=move |_| selection.set(Some(InspectSelection::Agents))>"Agents"</button>
                    <button class="btn btn-ghost btn-xs" on:click=move |_| selection.set(Some(InspectSelection::Objects { object_type: None, id: None }))>"Objects"</button>
                    <button class="btn btn-ghost btn-sm" on:click=move |_| inspect_open.set(false)>
                        "Close"
                    </button>
                </div>
            </div>
            {move || dev_error.get().map(|message| view! {
                <p class="alert alert-error text-sm">
                    {message}
                    " "
                    <button
                        class="btn btn-error btn-xs"
                        on:click=move |_| dev_reconnect_nonce.update(|nonce| *nonce += 1)
                    >
                        "Reconnect"
                    </button>
                </p>
            })}
            {move || error.get().map(|message| view! {
                <p class="alert alert-error text-sm">{message}</p>
            })}
            {move || resync_notice.get().map(|message| view! {
                <p class="alert alert-warning text-sm">{message}</p>
            })}
            {
                #[cfg(feature = "hydrate")]
                {
                    (move || match data.get() {
                        None => ().into_any(),
                        Some(InspectData::Sequence { id, trigger, caused_by, nodes }) => {
                            // Each inference appears once in this list (one
                            // SequenceNode per inference id), so summing
                            // it directly counts every inference exactly
                            // once - no separate dedup needed.
                            let (input_tokens, output_tokens, duration_ms) = nodes.iter().fold(
                                (0usize, 0usize, 0u64),
                                |(input, output, duration), node| {
                                    let (usage, node_duration) = match &node.inference.outcome {
                                        RecordedOutcome::Response(response) => {
                                            (Some(&response.usage), node.inference.duration_ms)
                                        }
                                        _ => (None, node.inference.duration_ms),
                                    };
                                    (
                                        input + usage.map(|u| u.input_tokens).unwrap_or(0),
                                        output + usage.map(|u| u.output_tokens).unwrap_or(0),
                                        duration + node_duration.unwrap_or(0),
                                    )
                                },
                            );
                            view! {
                                <div class=move || format!(
                                    "flex flex-col gap-2 {}",
                                    if matches!(selection.get(), Some(InspectSelection::Sequence { entry_id: Some(_), .. })) {
                                        "rounded ring-2 ring-primary"
                                    } else {
                                        ""
                                    },
                                )>
                                    <h3 class="text-sm font-semibold">"Turn"</h3>
                                    <details class="text-xs text-base-content/60">
                                        <summary class="cursor-pointer select-none">"Technical details"</summary>
                                        <div class="mt-1 flex flex-wrap gap-2">
                                            <span>{format!("{} inferences", nodes.len())}</span>
                                            <span>{format!("{input_tokens}+{output_tokens} tokens total")}</span>
                                            <span>{format!("{duration_ms}ms summed inference time")}</span>
                                            <span>{format!("record {id}")}</span>
                                            <span>{format!("trigger: {trigger}")}</span>
                                        </div>
                                        {(!caused_by.is_empty()).then(|| view! {
                                            <div class="mt-1 flex flex-wrap items-center gap-1">
                                                <span>"Caused by"</span>
                                                {caused_by.into_iter().map(|cause| view! {
                                                    <button class="btn btn-ghost btn-xs" on:click=move |_| selection.set(Some(InspectSelection::Sequence { id: cause, entry_id: None }))>
                                                        {format!("turn {cause}")}
                                                    </button>
                                                }).collect_view()}
                                            </div>
                                        })}
                                    </details>
                                    {nodes.into_iter().map(|node| view! {
                                        <SequenceNodeCard node selection agents live_tokens/>
                                    }).collect_view()}
                                </div>
                            }
                        }.into_any(),
                        Some(InspectData::Inference(inference)) => view! {
                            <InferenceCard inference selection agents live_tokens/>
                        }.into_any(),
                        Some(InspectData::AgentHistory { agent_id, branches }) => view! {
                            <AgentHistoryView world_id agent_id branches agents selection data/>
                        }.into_any(),
                        Some(InspectData::Agents(agents)) => view! {
                            <AgentsList agents selection/>
                        }.into_any(),
                        Some(InspectData::Objects(objects)) => view! {
                            <ObjectsView world_id initial=objects selection/>
                        }.into_any(),
                    }).into_any()
                }
                #[cfg(not(feature = "hydrate"))]
                {
                    ().into_any()
                }
            }
        </aside>
    }
    .into_any()
}

/// Renders request/history content without collapsing it into one string.
/// Tool-call identity remains a persistence relationship; the human view leads
/// with the tool name and arguments, and scene context becomes game facts.
#[cfg(feature = "hydrate")]
#[component]
fn ContentBlock(content: MessageContent) -> impl IntoView {
    match content {
        MessageContent::Text(text) => match text.strip_prefix("Current scene packet:\n") {
            Some(scene) => view! { <ScenePacket scene=scene.to_string()/> }.into_any(),
            None => view! { <CopyableText text/> }.into_any(),
        },
        MessageContent::ToolCalls(calls) => {
            view! { <ToolCallList calls actions=Vec::new() selection=RwSignal::new(None)/> }
                .into_any()
        }
        MessageContent::Speech(speech) => view! {
            <div class="flex flex-wrap items-start gap-1">
                <span class="badge badge-ghost badge-sm">
                    {format!("{} {}", speech.speaker, speech.kind)}
                </span>
                <CopyableText text=speech.display()/>
            </div>
        }
        .into_any(),
        MessageContent::ToolResult {
            tool_call_id: _,
            content,
        } if content.is_empty() => ().into_any(),
        MessageContent::ToolResult {
            tool_call_id: _,
            content,
        } => view! {
            <div class="flex flex-wrap items-start gap-1">
                <span class="badge badge-ghost badge-sm">"Tool result"</span>
                <CopyableText text=content/>
            </div>
        }
        .into_any(),
    }
}

/// A GM's scene context is structured state sent to the model. It is not a
/// second prompt: show the same stored packet as labelled game facts rather
/// than asking a developer to read its JSON encoding.
#[cfg(feature = "hydrate")]
#[component]
fn ScenePacket(scene: String) -> impl IntoView {
    fn label(key: &str) -> String {
        key.replace('_', " ")
    }

    fn value_text(value: &serde_json::Value) -> String {
        match value {
            serde_json::Value::Null => "none".to_string(),
            serde_json::Value::Bool(value) => value.to_string(),
            serde_json::Value::Number(value) => value.to_string(),
            serde_json::Value::String(value) => value.clone(),
            serde_json::Value::Array(values) => {
                values.iter().map(value_text).collect::<Vec<_>>().join("; ")
            }
            serde_json::Value::Object(fields) => fields
                .iter()
                .map(|(key, value)| format!("{}: {}", label(key), value_text(value)))
                .collect::<Vec<_>>()
                .join("; "),
        }
    }

    match serde_json::from_str::<serde_json::Value>(&scene) {
        Ok(serde_json::Value::Object(fields)) => view! {
            <section class="rounded-box border border-base-300 p-2">
                <h5 class="font-semibold">"Current scene"</h5>
                <dl class="mt-1 grid gap-1">
                    {fields.into_iter().map(|(key, value)| view! {
                        <div>
                            <dt class="font-semibold">{label(&key)}</dt>
                            <dd class="whitespace-pre-wrap">{value_text(&value)}</dd>
                        </div>
                    }).collect_view()}
                </dl>
                <CopyButton text=scene/>
            </section>
        }
        .into_any(),
        Ok(value) => view! {
            <div class="alert alert-error text-sm">
                {format!("Current scene packet must be an object, found {}", value_text(&value))}
            </div>
        }
        .into_any(),
        Err(error) => view! {
            <div class="alert alert-error text-sm">
                {format!("Could not decode current scene packet: {error}")}
            </div>
        }
        .into_any(),
    }
}

/// A response's content, distinct from `MessageContent` in never answering a
/// tool call itself (an inference produces tool calls; it does not receive
/// one).
#[cfg(feature = "hydrate")]
#[component]
fn ResponseContentBlock(content: Content) -> impl IntoView {
    match content {
        Content::Text(text) if text.is_empty() => view! { <EmptyResponse/> }.into_any(),
        Content::Text(text) => view! { <CopyableText text/> }.into_any(),
        Content::ToolCalls(calls) => {
            view! { <ToolCallList calls actions=Vec::new() selection=RwSignal::new(None)/> }
                .into_any()
        }
    }
}

/// An empty normal completion ends an agent turn without adding player chat.
/// It remains visible in inspection so a deliberate silent finish is distinct
/// from a missing or failed inference.
#[cfg(feature = "hydrate")]
#[component]
fn EmptyResponse() -> impl IntoView {
    view! { <p class="italic text-base-content/60">"No player-facing response."</p> }
}

#[cfg(feature = "hydrate")]
#[component]
fn ToolCallList(
    calls: Vec<ToolCall>,
    actions: Vec<RecordedAction>,
    selection: RwSignal<Option<InspectSelection>>,
) -> impl IntoView {
    view! {
        <ul class="space-y-1">
            {calls.into_iter().map(|call| view! {
                <li class="border-l-2 border-base-300 pl-2">
                    <div class="flex flex-wrap items-start gap-1">
                        <span class="font-semibold">{format!("Tool call: {}", call.name)}</span>
                        <CopyableText text=call.arguments/>
                    </div>
                    {actions.iter().filter(|action| action.call_id.as_deref() == Some(call.id.as_str())).cloned().map(|action| view! {
                        <div class="ml-3 mt-1 border-l-2 border-base-300 pl-2">
                            <span class="font-semibold">"Result"</span>
                            <CopyableText text=action.result/>
                            {action.approval_inference_id.map(|inference_id| view! {
                                <button class="btn btn-ghost btn-xs" on:click=move |_| selection.set(Some(InspectSelection::Inference(inference_id)))>"GM decision"</button>
                            })}
                        </div>
                    }).collect_view()}
                </li>
            }).collect_view()}
        </ul>
    }
}

/// One piece of exact recorded text, shown verbatim with a copy control for
/// that exact string (never a truncated or re-derived version of it).
#[cfg(feature = "hydrate")]
#[component]
fn CopyableText(text: String) -> impl IntoView {
    let copy_text = text.clone();
    view! {
        <span class="flex items-start gap-1">
            <span class="whitespace-pre-wrap">{text}</span>
            {(!copy_text.is_empty()).then(|| view! { <CopyButton text=copy_text/> })}
        </span>
    }
}

/// Copies `text` to the clipboard exactly as recorded - design.md: "Raw
/// fields are expandable and copyable without truncation; HTML escaping
/// must preserve copied text." `text` is the real string, not HTML, so
/// nothing needs unescaping first.
#[cfg(feature = "hydrate")]
#[component]
fn CopyButton(text: String) -> impl IntoView {
    let copied = RwSignal::new(false);
    let on_click = move |_| {
        let text = text.clone();
        let Some(window) = web_sys::window() else {
            return;
        };
        let promise = window.navigator().clipboard().write_text(&text);
        spawn_local(async move {
            if JsFuture::from(promise).await.is_ok() {
                copied.set(true);
                reset_after_delay(copied);
            }
        });
    };
    view! {
        <button
            type="button"
            class="btn btn-ghost btn-xs btn-square"
            aria-label=move || if copied.get() { "Copied text" } else { "Copy text" }
            title=move || if copied.get() { "Copied" } else { "Copy text" }
            on:click=on_click
        >
            {move || if copied.get() { "✓" } else { "⧉" }}
        </button>
    }
}

/// Human labels come from the durable owner relationship already returned by
/// the authorized agent list. Numeric IDs stay in links, never as a substitute
/// for the actor a developer is trying to understand.
#[cfg(feature = "hydrate")]
fn agent_label(agent_id: i64, agents: RwSignal<Vec<AgentInfo>>) -> String {
    let Some(agent) = agents.get().into_iter().find(|agent| agent.id == agent_id) else {
        return "Loading agent…".to_string();
    };
    agent_info_label(&agent)
}

#[cfg(feature = "hydrate")]
fn agent_info_label(agent: &AgentInfo) -> String {
    match agent.role.as_str() {
        "player" => agent
            .character_name
            .as_ref()
            .map(|name| format!("Player agent — {name}"))
            .unwrap_or_else(|| "Player agent".to_string()),
        "npc" => agent
            .character_name
            .as_ref()
            .map(|name| format!("NPC agent — {name}"))
            .unwrap_or_else(|| "NPC agent".to_string()),
        "world_gm" => "Game Master".to_string(),
        role => format!("Unknown agent role ({role})"),
    }
}

/// Clears the "Copied" confirmation after a couple of seconds.
#[cfg(feature = "hydrate")]
fn reset_after_delay(copied: RwSignal<bool>) {
    let Some(window) = web_sys::window() else {
        return;
    };
    let reset = Closure::<dyn FnMut()>::new(move || copied.set(false));
    let _ = window.set_timeout_with_callback_and_timeout_and_arguments_0(
        reset.as_ref().unchecked_ref(),
        2000,
    );
    reset.forget();
}

/// One node of the Turn execution story. The detailed Inference stays behind
/// its explicit link; the Turn only shows the actor's generated result and
/// any nested tool execution.
#[cfg(feature = "hydrate")]
#[component]
fn SequenceNodeCard(
    node: SequenceNode,
    selection: RwSignal<Option<InspectSelection>>,
    agents: RwSignal<Vec<AgentInfo>>,
    live_tokens: RwSignal<std::collections::HashMap<i64, String>>,
) -> impl IntoView {
    if let Some(cause_inference_id) = node.inference.cause_inference_id {
        let text = match node.inference.outcome {
            RecordedOutcome::Response(Response {
                content: Content::Text(text),
                ..
            }) => text,
            RecordedOutcome::Error(error) => error,
            RecordedOutcome::Running | RecordedOutcome::Response(_) => {
                "Compaction is running.".to_string()
            }
        };
        return view! { <CompactionCard inference_id=node.inference.id cause_inference_id=Some(cause_inference_id) text/> }.into_any();
    }
    let produced_ids: Vec<i64> = node.messages.iter().map(|message| message.id).collect();
    view! {
        <div class=move || format!(
            "card {}",
            if matches!(selection.get(), Some(InspectSelection::Sequence { entry_id: Some(entry_id), .. }) if produced_ids.contains(&entry_id)) {
                "bg-primary/20 ring-2 ring-primary"
            } else {
                "bg-base-200"
            },
        )>
            <div class="card-body gap-1 p-3 text-sm">
                <InferenceSummaryCard inference=node.inference actions=node.actions selection agents live_tokens/>
            </div>
        </div>
    }.into_any()
}

/// One collapsed, yellow representation of a recorded summary. Both the Turn
/// and raw history use it, so the model's summary is visibly the compaction
/// event wherever it is inspected.
#[cfg(feature = "hydrate")]
#[component]
fn CompactionCard(
    inference_id: i64,
    cause_inference_id: Option<i64>,
    text: String,
    #[prop(optional)] covered_ranges: Option<Vec<crate::inspection::PositionRange>>,
    #[prop(optional)] selection: Option<RwSignal<Option<InspectSelection>>>,
    #[prop(optional)] reason: Option<String>,
    #[prop(optional)] retained_messages: Option<i64>,
) -> impl IntoView {
    let label = covered_ranges
        .map(|ranges| {
            if ranges.len() == 1 {
                let range = &ranges[0];
                format!(
                    "Compacted history entries #{}-#{}",
                    range.first_position, range.last_position
                )
            } else {
                format!("Compacted history across {} histories", ranges.len())
            }
        })
        .unwrap_or_else(|| "Compacted history".to_string());
    view! {
        <details class="rounded border border-warning bg-warning/20 p-2 text-sm">
            <summary class="cursor-pointer select-none font-medium">{label}</summary>
            {cause_inference_id.map(|id| {
                let rejected = reason.as_deref() == Some("capacity_recovery");
                view! {
                    <p class="mt-1 text-xs text-base-content/60">
                        {if rejected { format!("Caused by inference {id}, which did not fit.") } else { format!("Deferred after inference {id}.") }}
                        {selection.map(|selection| view! {
                            <button class="badge badge-outline badge-sm ml-1" on:click=move |_| selection.set(Some(InspectSelection::Inference(id)))>"View it"</button>
                        })}
                    </p>
                }
            })}
            {reason.zip(retained_messages).map(|(reason, retained)| view! {
                <p class="mt-1 text-xs text-base-content/60">{if reason == "capacity_recovery" { format!("Capacity recovery: the request that caused this did not fit in the context, so only the newest {retained} messages stayed raw and the rest were summarised. Frequent entries like this mean messages are too long or the configured raw-message count is too high.") } else { format!("Threshold compaction retained {retained} newest messages.") }}</p>
            })}
            <p class="mt-1 whitespace-pre-wrap">{text}</p>
            {selection.map(|selection| view! {
                <button class="badge badge-outline badge-sm mt-1" on:click=move |_| selection.set(Some(InspectSelection::Inference(inference_id)))>"View inference"</button>
            })}
        </details>
    }
}

/// A Turn contains the execution story, not a repeated full inference panel.
/// Its compact cards lead with the actor and result and link to the exact
/// inference when the developer needs to inspect the input that led to it.
#[cfg(feature = "hydrate")]
#[component]
fn InferenceSummaryCard(
    inference: RecordedInference,
    actions: Vec<RecordedAction>,
    selection: RwSignal<Option<InspectSelection>>,
    agents: RwSignal<Vec<AgentInfo>>,
    live_tokens: RwSignal<std::collections::HashMap<i64, String>>,
) -> impl IntoView {
    let inference_id = inference.id;
    let agent_id = inference.agent_id;
    let branch_id = inference.branch_id;
    let history_entry_id = inference.history_entry_id;
    let is_running = matches!(inference.outcome, RecordedOutcome::Running);
    let (outcome_text, outcome_content) = match inference.outcome {
        RecordedOutcome::Running => (None, None),
        RecordedOutcome::Error(error) => (Some(format!("Error: {error}")), None),
        RecordedOutcome::Response(response) => match response.content {
            Content::Text(text) => (Some(text), None),
            content => (None, Some(content)),
        },
    };
    let output_for_copy = outcome_text.clone();
    view! {
        <div class="flex flex-col gap-1">
            <div class="flex flex-wrap items-center gap-1">
                <button class="font-semibold text-left hover:underline" on:click=move |_| selection.set(Some(InspectSelection::AgentHistory { agent_id, branch_id: Some(branch_id), entry_id: Some(history_entry_id) }))>
                    {move || agent_label(agent_id, agents)}
                </button>
                <button class="btn btn-ghost btn-xs" on:click=move |_| selection.set(Some(InspectSelection::Inference(inference_id)))>
                    "Inspect inference"
                </button>
            </div>
            {outcome_content.map(|content| match content {
                Content::ToolCalls(calls) => view! { <ToolCallList calls actions selection/> }.into_any(),
                content => view! { <ResponseContentBlock content/> }.into_any(),
            })}
            {if is_running {
                fn running_text(
                    inference_id: i64,
                    live_tokens: RwSignal<std::collections::HashMap<i64, String>>,
                ) -> String {
                    live_tokens
                        .get()
                        .get(&inference_id)
                        .cloned()
                        .unwrap_or_else(|| "Running…".to_string())
                }
                view! {
                    <div class="flex items-start gap-1">
                        <p class="whitespace-pre-wrap">{move || live_tokens.get().get(&inference_id).cloned().unwrap_or_else(|| "Running…".to_string())}</p>
                        <span class="text-xs text-base-content/60">"Streaming…"</span>
                        {move || view! { <CopyButton text=running_text(inference_id, live_tokens)/> }}
                    </div>
                }.into_any()
            } else {
                output_for_copy.map(|text| {
                    if text.is_empty() {
                        view! { <EmptyResponse/> }.into_any()
                    } else {
                        view! { <CopyableText text/> }.into_any()
                    }
                }).into_any()
            }}
        </div>
    }
}

/// Renders the detailed Inference view. It follows debugging order: the exact
/// reconstructed input first, then its recorded result; technical evidence is
/// available without obscuring either.
#[cfg(feature = "hydrate")]
#[component]
fn InferenceCard(
    inference: RecordedInference,
    selection: RwSignal<Option<InspectSelection>>,
    agents: RwSignal<Vec<AgentInfo>>,
    live_tokens: RwSignal<std::collections::HashMap<i64, String>>,
) -> impl IntoView {
    let is_running = matches!(inference.outcome, RecordedOutcome::Running);
    let (outcome_text, outcome_content, reasoning, usage) = match inference.outcome {
        RecordedOutcome::Running => (None, None, String::new(), None),
        RecordedOutcome::Error(error) => {
            (Some(format!("error: {error}")), None, String::new(), None)
        }
        RecordedOutcome::Response(response) => match response.content {
            // Plain text can still be streaming into view live; render it
            // through the same reactive path as the running/error cases.
            Content::Text(text) => (Some(text), None, response.reasoning, Some(response.usage)),
            // Tool calls never stream as partial text - render their full
            // structure (each call's id/name/arguments) once the response
            // has actually completed.
            content => (
                None,
                Some(content),
                response.reasoning,
                Some(response.usage),
            ),
        },
    };
    let request_messages = inference.request.messages;
    let model_prompt = inference.model_prompt;
    let inference_id = inference.id;
    let agent_id = inference.agent_id;
    let branch_id = inference.branch_id;
    let history_entry_id = inference.history_entry_id;
    let sequence_id = inference.sequence_id;
    let output_for_copy = outcome_text.clone();
    view! {
        <div class="flex flex-col gap-2">
            <div class="flex flex-wrap items-center gap-2">
                <h3 class="text-sm font-semibold">"Inference"</h3>
                <button class="btn btn-ghost btn-xs" on:click=move |_| selection.set(Some(InspectSelection::AgentHistory { agent_id, branch_id: Some(branch_id), entry_id: Some(history_entry_id) }))>
                    {move || agent_label(agent_id, agents)}
                </button>
                {inference.parent_inference_id.map(|parent| view! {
                    <button class="btn btn-ghost btn-xs" on:click=move |_| selection.set(Some(InspectSelection::Inference(parent)))>
                        "Parent inference"
                    </button>
                })}
                {sequence_id.map(|sequence_id| view! {
                    <button class="btn btn-ghost btn-xs" on:click=move |_| selection.set(Some(InspectSelection::Sequence { id: sequence_id, entry_id: None }))>
                        "View turn"
                    </button>
                })}
                <details class="dropdown dropdown-end ml-auto text-xs text-base-content/60">
                    <summary class="btn btn-ghost btn-xs btn-square" aria-label="Technical details" title="Technical details">"…"</summary>
                    <div class="dropdown-content z-10 mt-1 w-96 max-w-[80vw] rounded-box border border-base-300 bg-base-100 p-3 shadow">
                        <span>{format!("Model: {}", inference.model)}</span>
                        {inference.duration_ms.map(|ms| view! { <span class="block">{format!("Inference time: {ms}ms")}</span> })}
                        {usage.map(|usage| view! { <span class="block">{format!("Token use: {} input + {} output", usage.input_tokens, usage.output_tokens)}</span> })}
                        {inference.prompt_evidence.map(|_| view! { <span class="block">"Prompt reconstruction verified against generation evidence."</span> })}
                        {model_prompt.map(|prompt| view! {
                            <details class="mt-2 border-t border-base-300 pt-2">
                                <summary class="cursor-pointer select-none">"Exact formatted prompt"</summary>
                                <p class="mt-1 whitespace-pre-wrap font-mono text-xs">{prompt.clone()}</p>
                                <CopyButton text=prompt/>
                            </details>
                        })}
                    </div>
                </details>
            </div>
            <section class="border-t border-base-300 pt-1">
                <h4 class="font-semibold">"Input"</h4>
                <ul class="mt-1 space-y-1">
                    {request_messages.into_iter().map(|message| view! {
                        <li>
                            <span class="font-semibold">{if message.origin == MessageOrigin::Instruction { "instruction" } else { message.role.as_str() }}</span>
                            <ContentBlock content=message.content/>
                            {(!message.reasoning.is_empty()).then(|| view! {
                                <details class="text-base-content/60">
                                    <summary class="cursor-pointer select-none">"Model working text (not sent to the player)"</summary>
                                    <p class="mt-1 whitespace-pre-wrap">{message.reasoning}</p>
                                </details>
                            })}
                        </li>
                    }).collect_view()}
                </ul>
            </section>
            <section class="border-t border-base-300 pt-1">
                <h4 class="font-semibold">"Output"</h4>
                {outcome_content.map(|content| view! { <ResponseContentBlock content/> })}
                {if is_running {
                    fn running_text(
                        inference_id: i64,
                        live_tokens: RwSignal<std::collections::HashMap<i64, String>>,
                    ) -> String {
                        live_tokens
                            .get()
                            .get(&inference_id)
                            .cloned()
                            .unwrap_or_else(|| "Running…".to_string())
                    }
                    view! {
                        <div class="flex items-start gap-1">
                            <p class="whitespace-pre-wrap">{move || live_tokens.get().get(&inference_id).cloned().unwrap_or_else(|| "Running…".to_string())}</p>
                            <span class="text-xs text-base-content/60">"Streaming…"</span>
                            {move || view! { <CopyButton text=running_text(inference_id, live_tokens)/> }}
                        </div>
                    }.into_any()
            } else {
                output_for_copy.map(|text| {
                    if text.is_empty() {
                        view! { <EmptyResponse/> }.into_any()
                    } else {
                        view! { <CopyableText text/> }.into_any()
                    }
                }).into_any()
                }}
            </section>
            {(!reasoning.is_empty()).then(|| view! {
                <details class="border-t border-base-300 pt-1">
                    <summary class="cursor-pointer select-none">"Model working text (not sent to the player)"</summary>
                    <p class="mt-1 whitespace-pre-wrap text-base-content/60">{reasoning}</p>
                </details>
            })}
        </div>
    }
    .into_any()
}

/// The agent chat history view: linear raw messages, with compaction
/// summaries and inference failures inlined where they occurred. A plain
/// player/NPC/Storyteller agent has exactly one section and renders exactly
/// as the single linear view always has. A world_gm agent whose groups have
/// split/merged renders every branch in its DAG as its own linked section -
/// a shared ancestor branch appears once, named by every child section that
/// continues from it, never duplicated (user_declarations.md: "the GM's chat
/// history view needs to authentically display the DAG, including multiple
/// heads - one per Group").
#[cfg(feature = "hydrate")]
#[component]
fn AgentHistoryView(
    world_id: i64,
    agent_id: i64,
    branches: Vec<BranchSection>,
    agents: RwSignal<Vec<AgentInfo>>,
    selection: RwSignal<Option<InspectSelection>>,
    data: RwSignal<Option<InspectData>>,
) -> impl IntoView {
    let single = branches.len() == 1;
    view! {
        <div class="flex flex-col gap-3">
            <h3 class="text-sm font-semibold">{move || format!("{} chat history", agent_label(agent_id, agents))}</h3>
            {branches.into_iter().map(|section| view! {
                <BranchSectionView world_id agent_id section show_heading=!single selection data/>
            }).collect_view()}
        </div>
    }
}

/// One branch's own section: its entries, its own independent load-more
/// state, and - only when the parent view is showing more than one branch -
/// a heading naming its current group head (or "retired" for a split/merge
/// source) and links to the branches it continues from.
#[cfg(feature = "hydrate")]
#[component]
fn BranchSectionView(
    world_id: i64,
    agent_id: i64,
    section: BranchSection,
    show_heading: bool,
    selection: RwSignal<Option<InspectSelection>>,
    data: RwSignal<Option<InspectData>>,
) -> impl IntoView {
    let branch_id = section.branch_id;
    let loading_more = RwSignal::new(false);
    let load_more_error = RwSignal::new(None::<String>);
    let load_more = Rc::new(move |cursor: RawHistoryCursor| {
        loading_more.set(true);
        let query = serde_json::to_string(&cursor).expect("raw history cursor serializes");
        let encoded = js_sys::encode_uri_component(&query);
        fetch_json(
            format!("/world/{world_id}/inspect/agents/{agent_id}/history?after={encoded}"),
            "next agent chat history page",
            move |page: RawHistoryPageResponse| {
                loading_more.set(false);
                data.update(|data| {
                    if let Some(InspectData::AgentHistory { branches, .. }) = data
                        && let Some(section) = branches
                            .iter_mut()
                            .find(|section| section.branch_id == branch_id)
                    {
                        section.entries.extend(page.entries);
                        section.cursor = page.cursor;
                    }
                });
            },
            load_more_error,
        );
    });
    let sentinel = NodeRef::<leptos::html::Div>::new();
    if let Some(cursor) = section.cursor {
        let auto_load = load_more.clone();
        Effect::new(move |_| {
            let auto_load = auto_load.clone();
            let Some(element) = sentinel.get() else {
                return;
            };
            let callback = Closure::<dyn FnMut(js_sys::Array, IntersectionObserver)>::new(
                move |entries: js_sys::Array, _: IntersectionObserver| {
                    if entries
                        .iter()
                        .filter_map(|entry| entry.dyn_into::<IntersectionObserverEntry>().ok())
                        .any(|entry| entry.is_intersecting())
                        && !loading_more.get_untracked()
                    {
                        auto_load(cursor);
                    }
                },
            );
            match IntersectionObserver::new(callback.as_ref().unchecked_ref()) {
                Ok(observer) => {
                    observer.observe(&element);
                    callback.forget();
                    std::mem::forget(observer);
                }
                Err(error) => load_more_error.set(Some(format!(
                    "Could not observe more agent history: {error:?}"
                ))),
            }
        });
    }
    let heading = show_heading.then(|| {
        let label = match &section.head {
            Some(head) => format!("Branch {branch_id} - {} (current)", head.group_name),
            None => format!("Branch {branch_id} - retired"),
        };
        let parents = section.parents.clone();
        view! {
            <div class="flex flex-wrap items-baseline gap-2 border-t border-base-300 pt-2 text-xs">
                <span class="font-semibold">{label}</span>
                {(!parents.is_empty()).then(|| view! {
                    <span class="text-base-content/60">
                        {format!(
                            "continues from {}",
                            parents.iter()
                                .map(|parent| format!("branch {} @{}", parent.branch_id, parent.through_position))
                                .collect::<Vec<_>>()
                                .join(", "),
                        )}
                    </span>
                })}
            </div>
        }
    });
    view! {
        <div class="flex flex-col gap-2">
            {heading}
            <ul class="flex flex-col gap-1 text-sm">
                {section.entries.into_iter().map(|entry| view! { <RawHistoryEntryRow entry selection/> }).collect_view()}
            </ul>
            {section.cursor.map(|cursor| view! {
                <div node_ref=sentinel class="h-px" aria-hidden="true"></div>
                <button
                    class="btn btn-ghost btn-sm"
                    disabled=move || loading_more.get()
                    on:click=move |_| load_more(cursor)
                >
                    {move || if loading_more.get() { "Loading…" } else { "Load more" }}
                </button>
            })}
            {move || load_more_error.get().map(|message| view! {
                <div class="alert alert-error text-sm">{message}</div>
            })}
        </div>
    }
}

#[cfg(feature = "hydrate")]
#[component]
fn RawHistoryEntryRow(
    entry: RawHistoryEntry,
    selection: RwSignal<Option<InspectSelection>>,
) -> impl IntoView {
    match entry {
        RawHistoryEntry::Message { id, role, origin, content, reasoning, source_inference_id, answers_tool_execution_id } => {
            let silent_assistant_response = role == Role::Assistant
                && matches!(&content, MessageContent::Text(text) if text.is_empty());
            let empty_tool_result = matches!(&content, MessageContent::ToolResult { content, .. } if content.is_empty());
            if empty_tool_result {
                return ().into_any();
            }
            view! {
            <li class=move || history_entry_class(id, selection)>
                <div class="flex flex-wrap items-center gap-2">
                    <span class="badge badge-ghost badge-sm">{format!("message {id}")}</span>
                    <span class="font-semibold">{if origin == MessageOrigin::Instruction { "instruction" } else { role.as_str() }}</span>
                    {answers_tool_execution_id.map(|_| view! { <span class="badge badge-ghost badge-sm">"Tool result"</span> })}
                    {source_inference_id.filter(|_| answers_tool_execution_id.is_none()).map(|inference_id| view! {
                        <button class="badge badge-outline badge-sm" on:click=move |_| selection.set(Some(InspectSelection::Inference(inference_id)))>
                            "View inference"
                        </button>
                    })}
                </div>
                {if silent_assistant_response {
                    view! { <EmptyResponse/> }.into_any()
                } else {
                    view! { <ContentBlock content/> }.into_any()
                }}
                {(!reasoning.is_empty()).then(|| view! {
                    <details class="text-base-content/60">
                        <summary class="cursor-pointer select-none">"Model working text (not sent to the player)"</summary>
                        <p class="mt-1 whitespace-pre-wrap">{reasoning}</p>
                    </details>
                })}
            </li>
        }.into_any()
        },
        RawHistoryEntry::Summary { id, covered_ranges, inference_id, text, reason, retained_messages, cause_inference_id } => view! {
            <li class=move || history_entry_class(id, selection)>
                <CompactionCard inference_id cause_inference_id text covered_ranges selection reason retained_messages/>
            </li>
        }.into_any(),
        RawHistoryEntry::Inference { id, history_entry_id, error } => {
            let Some(error) = error else { return ().into_any(); };
            view! {
            <li class=move || format!("alert alert-error text-sm {}", if history_entry_selected(history_entry_id, selection) { "ring-2 ring-primary" } else { "" })>
                <button class="badge badge-outline badge-sm" on:click=move |_| selection.set(Some(InspectSelection::Inference(id)))>
                    "View inference"
                </button>
                <span>{error}</span>
            </li>
            }.into_any()
        },
    }
}

#[cfg(feature = "hydrate")]
fn history_entry_selected(id: i64, selection: RwSignal<Option<InspectSelection>>) -> bool {
    matches!(selection.get(), Some(InspectSelection::AgentHistory { entry_id: Some(entry_id), .. }) if entry_id == id)
}

#[cfg(feature = "hydrate")]
fn history_entry_class(id: i64, selection: RwSignal<Option<InspectSelection>>) -> String {
    format!(
        "rounded p-2 {}",
        if history_entry_selected(id, selection) {
            "bg-primary/20 ring-2 ring-primary"
        } else {
            "bg-base-200"
        },
    )
}

/// The world agents/GMs navigator (user_declarations.md: "Browseable world
/// objects, descriptions, tools, agents and links to their chat histories").
#[cfg(feature = "hydrate")]
#[component]
fn AgentsList(
    agents: Vec<AgentInfo>,
    selection: RwSignal<Option<InspectSelection>>,
) -> impl IntoView {
    view! {
        <ul class="flex flex-col gap-1 text-sm">
            {agents.into_iter().map(|agent| view! {
                <li>
                    <button class="btn btn-ghost btn-sm w-full justify-start" on:click=move |_| selection.set(Some(InspectSelection::AgentHistory { agent_id: agent.id, branch_id: None, entry_id: None }))>
                        {agent_info_label(&agent)}
                        {agent.location_name.map(|location| format!(" @ {location}"))}
                    </button>
                </li>
            }).collect_view()}
        </ul>
    }
}

/// The world object browser (user_declarations.md: present objects and their
/// relations without projecting them into a simplified display).
#[cfg(feature = "hydrate")]
#[component]
fn ObjectsView(
    world_id: i64,
    initial: WorldObjects,
    selection: RwSignal<Option<InspectSelection>>,
) -> impl IntoView {
    let world_title = format!("{} (time {})", initial.name, initial.time);
    let storyteller_summary = initial.storyteller_summary.clone();
    let initial_prompt = initial.initial_prompt.clone();
    let gm_notes = initial.gm_notes.clone();
    let storyteller_notes = initial.storyteller_notes.clone();
    let objects = RwSignal::new(initial);
    let search = RwSignal::new(String::new());
    let load_error = RwSignal::new(None::<String>);
    let object_rows = move || {
        objects.get().objects.into_iter().map(|object| {
        let object_type = object.object_type;
        let id = object.id;
        view! {
            <li class=move || if matches!(selection.get(), Some(InspectSelection::Objects { object_type: Some(kind), id: Some(selected_id) }) if kind == object_type && selected_id == id) { "rounded bg-primary/20 p-2" } else { "rounded bg-base-200 p-2" }>
                <button class="font-semibold text-left hover:underline" on:click=move |_| selection.set(Some(InspectSelection::Objects { object_type: Some(object_type), id: Some(id) }))>
                    {format!("{} {} ({})", object.object_type.as_str(), object.name, object.id)}
                </button>
                <p class="whitespace-pre-wrap">{object.description}</p>
                {(!object.fields.is_empty()).then(|| view! {
                    <dl class="mt-1 text-base-content/60">{object.fields.into_iter().map(|field| view! { <div>{format!("{}: {}", field.label, field.value)}</div> }).collect_view()}</dl>
                })}
                {(!object.relationships.is_empty()).then(|| view! {
                    <div class="mt-1 flex flex-wrap gap-1">{object.relationships.into_iter().map(|link| {
                        let link_type = link.object_type;
                        let link_id = link.id;
                        view! { <button class="badge badge-outline badge-sm" on:click=move |_| selection.set(Some(InspectSelection::Objects { object_type: Some(link_type), id: Some(link_id) }))>{format!("{} {} ({})", link_type.as_str(), link.label, link_id)}</button> }
                    }).collect_view()}</div>
                })}
                {(!object.gm_notes.is_empty()).then(|| view! { <p class="whitespace-pre-wrap text-base-content/60">{format!("GM notes: {}", object.gm_notes)}</p> })}
                {(!object.storyteller_notes.is_empty()).then(|| view! { <p class="whitespace-pre-wrap text-base-content/60">{format!("Storyteller notes: {}", object.storyteller_notes)}</p> })}
            </li>
        }
    }).collect_view()
    };
    view! {
        <div class="flex flex-col gap-3 text-sm">
            <div class="rounded bg-base-200 p-2">
                <p class="font-semibold">{world_title}</p>
                <p class="whitespace-pre-wrap text-base-content/60">{storyteller_summary}</p>
                {(!initial_prompt.is_empty()).then(|| view! {
                    <details class="mt-1">
                        <summary class="cursor-pointer select-none text-base-content/60">"Initial prompt"</summary>
                        <p class="whitespace-pre-wrap">{initial_prompt}</p>
                    </details>
                })}
                {(!gm_notes.is_empty()).then(|| view! { <p class="whitespace-pre-wrap text-base-content/60">{format!("GM notes: {gm_notes}")}</p> })}
                {(!storyteller_notes.is_empty()).then(|| view! { <p class="whitespace-pre-wrap text-base-content/60">{format!("Storyteller notes: {storyteller_notes}")}</p> })}
            </div>
            <form class="flex gap-1" on:submit=move |event| {
                event.prevent_default();
                let encoded = js_sys::encode_uri_component(&search.get_untracked()).as_string().expect("encoded world object search is a string");
                fetch_json(format!("/world/{world_id}/inspect/objects?search={encoded}"), "world object search", move |page| objects.set(page), load_error);
            }>
                <input class="input input-sm min-w-0 flex-1" placeholder="Search name, type, or ID" prop:value=move || search.get() on:input=move |event| search.set(event_target_value(&event))/>
                <button class="btn btn-sm" type="submit">"Search"</button>
            </form>
            {move || load_error.get().map(|error| view! { <p class="text-error">{error}</p> })}
            <ul class="flex flex-col gap-1">{object_rows}</ul>
            {move || objects.get().cursor.map(|cursor| view! {
                <button class="btn btn-outline btn-sm self-start" on:click=move |_| {
                    let cursor = js_sys::encode_uri_component(&serde_json::to_string(&cursor).expect("world object cursor serializes")).as_string().expect("encoded world object cursor is a string");
                    let search = js_sys::encode_uri_component(&search.get_untracked()).as_string().expect("encoded world object search is a string");
                    fetch_json(format!("/world/{world_id}/inspect/objects?after={cursor}&search={search}"), "next world object page", move |next: WorldObjects| objects.update(|current| { current.objects.extend(next.objects); current.cursor = next.cursor; }), load_error);
                }>
                    "Load more"
                </button>
            })}
        </div>
    }
}

/// Fetches one inspection route and deserializes its JSON body, reporting
/// any failure through `error`. Shared by every view the pane can show - the
/// fetch/validate/parse mechanics are identical, only the URL and the
/// deserialize target differ.
#[cfg(feature = "hydrate")]
fn fetch_json<T: serde::de::DeserializeOwned + 'static>(
    url: String,
    label: &'static str,
    on_success: impl Fn(T) + 'static,
    error: RwSignal<Option<String>>,
) {
    let Some(window) = web_sys::window() else {
        error.set(Some(format!(
            "Could not open the {label}: no browser window is available."
        )));
        return;
    };
    let request = window.fetch_with_str(&url);
    spawn_local(async move {
        let response = match JsFuture::from(request).await {
            Ok(response) => match response.dyn_into::<WebResponse>() {
                Ok(response) => response,
                Err(_) => {
                    error.set(Some(format!(
                        "The {label} returned an invalid browser response."
                    )));
                    return;
                }
            },
            Err(js_error) => {
                error.set(Some(format!("Could not open the {label}: {js_error:?}")));
                return;
            }
        };
        let body = match response.text() {
            Ok(body) => body,
            Err(js_error) => {
                error.set(Some(format!(
                    "Could not read the {label} response: {js_error:?}"
                )));
                return;
            }
        };
        let body = match JsFuture::from(body).await {
            Ok(body) => body.as_string().unwrap_or_default(),
            Err(js_error) => {
                error.set(Some(format!(
                    "Could not read the {label} response: {js_error:?}"
                )));
                return;
            }
        };
        if !response.ok() {
            error.set(Some(format!(
                "The {label} request failed (HTTP {}): {body}",
                response.status()
            )));
            return;
        }
        match serde_json::from_str::<T>(&body) {
            Ok(fetched) => {
                error.set(None);
                on_success(fetched);
            }
            Err(parse_error) => {
                error.set(Some(format!("Could not parse the {label}: {parse_error}")));
            }
        }
    });
}

/// Waits once for the server's model lifecycle transition, then reloads into
/// the server-rendered game page with its stored history.
#[island]
pub fn GameLoading() -> impl IntoView {
    let message = RwSignal::new("Preparing the game world…".to_string());

    #[cfg(feature = "hydrate")]
    wait_for_game(message);

    view! {
        <main class="min-h-dvh bg-base-200 p-4 sm:p-8">
            <section class="card mx-auto max-w-xl bg-base-100 shadow-xl">
                <div class="card-body">
                    <h1 class="card-title text-2xl">"Cairnworld"</h1>
                    <span class="loading loading-spinner loading-md text-primary" aria-hidden="true"></span>
                    <p role="status">{move || message.get()}</p>
                </div>
            </section>
        </main>
    }
}

#[cfg(feature = "hydrate")]
fn wait_for_game(message: RwSignal<String>) {
    let Some(window) = web_sys::window() else {
        message.set("Could not start the game-ready check: no browser window is available.".into());
        return;
    };
    let request = window.fetch_with_str("/game-status");
    spawn_local(async move {
        let response = match JsFuture::from(request).await {
            Ok(response) => match response.dyn_into::<WebResponse>() {
                Ok(response) => response,
                Err(_) => {
                    message
                        .set("The game-ready check returned an invalid browser response.".into());
                    return;
                }
            },
            Err(error) => {
                message.set(format!(
                    "Could not check whether the game is ready: {error:?}"
                ));
                return;
            }
        };
        if response.ok() {
            if let Err(error) = window.location().reload() {
                message.set(format!(
                    "The game is ready, but this page could not reload: {error:?}"
                ));
            }
        } else {
            let status = response.status();
            let body = match response.text() {
                Ok(body) => body,
                Err(error) => {
                    message.set(format!(
                        "The game could not start (HTTP {status}); obtaining the server error failed: {error:?}"
                    ));
                    return;
                }
            };
            match JsFuture::from(body).await {
                Ok(body) => message.set(format!(
                    "The game could not start (HTTP {status}): {}",
                    body.as_string().unwrap_or_else(|| "the server returned a non-text error".into())
                )),
                Err(error) => message.set(format!(
                    "The game could not start (HTTP {status}); reading the server error failed: {error:?}"
                )),
            }
        }
    });
}

#[island]
pub fn PlayerChat(
    world_id: i64,
    character_id: i64,
    after_message_id: Option<i64>,
    developer_mode: bool,
    children: Children,
) -> impl IntoView {
    #[cfg(not(feature = "hydrate"))]
    let _ = (world_id, character_id, after_message_id, developer_mode);
    let entries = RwSignal::new(Vec::<ChatEntry>::new());
    let can_act = RwSignal::new(false);
    let activity = RwSignal::new(None::<ChatActivity>);
    let disconnected = RwSignal::new(false);
    let chat = NodeRef::<leptos::html::Ol>::new();
    let inspect_scroll = NodeRef::<leptos::html::Aside>::new();
    let input = NodeRef::<leptos::html::Input>::new();
    let dev_error = RwSignal::new(None::<String>);
    #[cfg(feature = "hydrate")]
    let initial_inspecting = match InspectSelection::from_location() {
        Ok(selection) => selection,
        Err(error) => {
            dev_error.set(Some(format!("Invalid inspect URL: {error}")));
            None
        }
    };
    #[cfg(not(feature = "hydrate"))]
    let initial_inspecting = None;
    let inspecting = RwSignal::new(initial_inspecting);
    let restore_inspect_scroll = RwSignal::new(None::<i32>);
    // Hydrate the same closed layout the server rendered; the selection
    // effect opens it afterward, including when restoring an inspect URL.
    let inspect_open = RwSignal::new(false);
    let last_inspecting = RwSignal::new(initial_inspecting);
    // Narrow screens show one of the already-mounted panes. This is distinct
    // from `inspect_open`: Close still collapses Inspect to its edge strip,
    // whereas these tabs only choose which pane CSS makes visible below `sm`.
    let narrow_inspect = RwSignal::new(false);
    // A signal, not the plain prop: an owner may toggle Developer Mode while
    // this page stays open, and every active member's page must reflect it
    // immediately (user_declarations.md), not only on reload.
    let developer_mode = RwSignal::new(developer_mode);
    // The most recent live event from /inspect/events, for the currently
    // open inspect pane to patch itself with instead of a network re-fetch.
    let dev_events = RwSignal::new(None::<DevEvent>);
    let dev_reconnect_nonce = RwSignal::new(0u32);

    #[cfg(feature = "hydrate")]
    let socket = connect_player_chat(
        world_id,
        character_id,
        after_message_id,
        entries,
        can_act,
        activity,
        disconnected,
    );

    #[cfg(feature = "hydrate")]
    connect_dev_events(
        world_id,
        developer_mode,
        dev_events,
        dev_error,
        dev_reconnect_nonce,
    );

    #[cfg(feature = "hydrate")]
    {
        // A normal selection writes one browser-history entry. A popstate
        // selection skips that write, so Back/Forward changes only the pane
        // and never makes another entry to bounce through.
        let applying_popstate = RwSignal::new(false);
        Effect::new(move |_| {
            let selected = inspecting.get();
            if applying_popstate.get_untracked() {
                applying_popstate.set(false);
                return;
            }
            if let Some(inspect) = inspect_scroll.get() {
                let inspect: HtmlElement = inspect.unchecked_into();
                if let Err(error) = InspectSelection::remember_scroll(inspect.scroll_top()) {
                    dev_error.set(Some(error));
                    return;
                }
            }
            if let Err(error) = InspectSelection::write_to_location(selected) {
                dev_error.set(Some(error));
            }
        });
        Effect::new(move |_| {
            if let Some(selected) = inspecting.get() {
                last_inspecting.set(Some(selected));
                inspect_open.set(true);
                narrow_inspect.set(true);
            }
        });
        Effect::new(move |_| {
            if dev_error.get().is_some() {
                inspect_open.set(true);
            }
        });
        Effect::new(move |_| {
            if !developer_mode.get() {
                inspect_open.set(false);
                inspecting.set(None);
            }
        });
        let on_popstate = Closure::<dyn Fn(Event)>::new(move |_| {
            applying_popstate.set(true);
            restore_inspect_scroll.set(InspectSelection::scroll_position());
            match InspectSelection::from_location() {
                Ok(selection) => {
                    dev_error.set(None);
                    inspecting.set(selection);
                }
                Err(error) => {
                    dev_error.set(Some(format!("Invalid inspect URL: {error}")));
                    inspecting.set(None);
                }
            }
        });
        if let Some(window) = web_sys::window() {
            match window
                .add_event_listener_with_callback("popstate", on_popstate.as_ref().unchecked_ref())
            {
                Ok(()) => on_popstate.forget(),
                Err(error) => dev_error.set(Some(format!(
                    "Could not listen for browser history changes: {error:?}"
                ))),
            }
        } else {
            dev_error.set(Some(
                "Could not listen for browser history changes: no browser window is available."
                    .into(),
            ));
        }
    }

    #[cfg(feature = "hydrate")]
    Effect::new(move |_| {
        entries.track();
        activity.track();
        if let Some(chat) = chat.get() {
            let chat: HtmlElement = chat.unchecked_into();
            chat.set_scroll_top(chat.scroll_height());
        }
    });

    let on_submit = move |event: leptos::ev::SubmitEvent| {
        event.prevent_default();
        #[cfg(feature = "hydrate")]
        {
            if !can_act.get() || disconnected.get() {
                return;
            }
            let Some(input_element) = input.get() else {
                return;
            };
            let text = input_element.value();
            if text.trim().is_empty() {
                return;
            }
            let socket = socket.borrow();
            let Some(socket) = socket.as_ref() else {
                return;
            };
            if socket.ready_state() != WebSocket::OPEN {
                return;
            }
            let event = ClientEvent::Message { text: text.clone() };
            match serde_json::to_string(&event).and_then(|event| {
                socket.send_with_str(&event).map_err(|error| {
                    serde_json::Error::io(std::io::Error::other(format!(
                        "sending browser chat message: {error:?}"
                    )))
                })
            }) {
                Ok(()) => {
                    entries.update(|entries| {
                        entries.push(ChatEntry {
                            role: ChatRole::User,
                            text,
                            speaker: None,
                            id: None,
                            sequence_id: None,
                        })
                    });
                    input_element.set_value("");
                    can_act.set(false);
                }
                Err(error) => add_notice(entries, format!("Could not send message: {error}")),
            }
        }
    };

    #[cfg(feature = "hydrate")]
    watch_inspect_clicks(chat, inspecting, developer_mode, dev_error);

    let body = view! {
        <div class="flex min-h-0 flex-1 gap-3">
            <div class=move || if developer_mode.get() && narrow_inspect.get() { "hidden min-h-0 flex-1 flex-col sm:flex" } else { "flex min-h-0 flex-1 flex-col" }>
                <ol id="chat" class="flex min-h-0 flex-1 flex-col gap-3 overflow-y-auto px-1 py-4" node_ref=chat aria-live="polite">
                    {children()}
                    {move || {
                        let developer_mode = developer_mode.get();
                        entries.get().into_iter().map(move |entry| {
                            view! { <ChatEntryView entry developer_mode/> }
                        }).collect_view()
                    }}
                </ol>
                {chat_status_line(activity, disconnected)}
                <form id="message" class="join w-full" on:submit=on_submit>
                    <input class="input join-item min-w-0 flex-1" node_ref=input name="text" autocomplete="off" placeholder="Write a message…" disabled=move || disconnected.get()/>
                    <button class="btn btn-primary join-item" type="submit" disabled=move || !can_act.get() || disconnected.get()>"Send"</button>
                </form>
            </div>
            {move || {
                let pane = PaneState {
                    selection: inspecting,
                    open: inspect_open,
                    last_selection: last_inspecting,
                    scroll: inspect_scroll,
                    restore_scroll: restore_inspect_scroll,
                };
                developer_mode.get().then(|| {
                    inspect_pane(world_id, pane, narrow_inspect, dev_events, developer_mode, dev_error, dev_reconnect_nonce)
                })
            }}
        </div>
    }
    .into_any();

    view! {
        <main class="h-dvh bg-base-200 p-3 sm:p-6">
            <section class=move || format!(
                "mx-auto flex h-[calc(100dvh-1.5rem)] {} flex-col rounded-box bg-base-100 shadow-xl sm:h-[calc(100dvh-3rem)]",
                if developer_mode.get() { "max-w-7xl" } else { "max-w-5xl" },
            )>
                <header class="navbar border-b border-base-300 px-4">
                    <h1 class="text-xl font-semibold">"Cairnworld"</h1>
                    <span class="ml-auto badge badge-primary badge-outline">"Adventure chat"</span>
                    {move || developer_mode.get().then(|| view! { <span class="badge badge-error ml-2">"Developer Mode"</span> })}
                </header>
                {move || developer_mode.get().then(|| view! {
                    <div class="join mx-3 mt-3 sm:hidden" role="tablist" aria-label="Game panes">
                        <button id="chat-tab" class="btn join-item flex-1" class:btn-primary=move || !narrow_inspect.get() on:click=move |_| narrow_inspect.set(false) role="tab" aria-selected=move || !narrow_inspect.get()>"Chat"</button>
                        <button id="inspect-tab" class="btn join-item flex-1" class:btn-primary=move || narrow_inspect.get() on:click=move |_| {
                            narrow_inspect.set(true);
                            if !inspect_open.get_untracked() {
                                inspect_open.set(true);
                                if inspecting.get_untracked().is_none() {
                                    inspecting.set(last_inspecting.get_untracked().or(Some(InspectSelection::Agents)));
                                }
                            }
                        } role="tab" aria-selected=move || narrow_inspect.get()>"Inspect"</button>
                    </div>
                })}
                <div class="flex min-h-0 flex-1 p-3 sm:p-5">{body}</div>
            </section>
        </main>
    }
    .into_any()
}

/// A single delegated click listener on the chat container: `#chat`'s live
/// entries and its opaque, non-reactive SSR history (see `ChatTranscript`)
/// both render a plain `data-inspect-sequence` attribute, so one listener
/// covers both without the static history needing its own reactivity.
#[cfg(feature = "hydrate")]
fn watch_inspect_clicks(
    chat: NodeRef<leptos::html::Ol>,
    inspecting: RwSignal<Option<InspectSelection>>,
    developer_mode: RwSignal<bool>,
    dev_error: RwSignal<Option<String>>,
) {
    Effect::new(move |_| {
        let Some(chat) = chat.get() else { return };
        let chat: HtmlElement = chat.unchecked_into();
        let on_click = Closure::<dyn Fn(Event)>::new(move |event: Event| {
            // The static SSR history's data-inspect-sequence attributes do
            // not repaint on a live mode toggle (see PlayerChat's doc
            // comment); this check is what actually makes a live-disabled
            // page stop honoring them, not just hide the pane chrome.
            if !developer_mode.get_untracked() {
                return;
            }
            let Some(target) = event
                .target()
                .and_then(|target| target.dyn_into::<web_sys::Element>().ok())
            else {
                return;
            };
            let Some(entry) = target.closest("[data-inspect-sequence]").ok().flatten() else {
                return;
            };
            let Some(sequence_id) = entry
                .get_attribute("data-inspect-sequence")
                .and_then(|id| id.parse().ok())
            else {
                return;
            };
            let entry_id = entry
                .get_attribute("data-inspect-entry")
                .and_then(|id| id.parse().ok());
            inspecting.set(Some(InspectSelection::Sequence {
                id: sequence_id,
                entry_id,
            }));
        });
        match chat.add_event_listener_with_callback("click", on_click.as_ref().unchecked_ref()) {
            Ok(()) => on_click.forget(),
            Err(error) => dev_error.set(Some(format!(
                "Could not listen for inspection links: {error:?}"
            ))),
        }
    });
}

fn chat_status_line(
    activity: RwSignal<Option<ChatActivity>>,
    disconnected: RwSignal<bool>,
) -> AnyView {
    (move || {
        if disconnected.get() {
            view! {
                <p id="chat-status" class="px-2 pb-2 text-sm text-base-content/60" role="status">
                    "Connection lost. Reload to reconnect."
                </p>
            }
            .into_any()
        } else {
            activity
                .get()
                .map(|activity| view! {
                    <p id="chat-status" class="flex items-center gap-2 px-2 pb-2 text-sm text-base-content/60" role="status">
                        <span class="loading loading-dots loading-xs" aria-hidden="true"></span>
                        {chat_activity_text(activity)}
                    </p>
                })
                .into_any()
        }
    })
    .into_any()
}

fn chat_activity_text(activity: ChatActivity) -> &'static str {
    match activity {
        ChatActivity::PreparingOpening => "The game is preparing its first response…",
        ChatActivity::Responding => "The game is responding…",
    }
}

/// Owner-only Developer Mode toggle. Enabling requires an explicit warning
/// modal, shown with the real `<dialog>` element (`showModal`/`close`) so it
/// is a true modal - focus-trapped, dismissible with Escape, and rendered
/// above the rest of the page - not an inline block sharing the page flow.
/// Disabling needs no warning, so it is a plain form outside this island
/// (see `WorldDetail`). Recording always runs regardless of this toggle -
/// only visibility to active members changes.
#[component]
pub fn DeveloperModeEnableButton(world_id: i64) -> impl IntoView {
    let dialog_id = format!("developer-mode-warning-{world_id}");
    let action_path = format!("/world/{world_id}/developer-mode");
    view! {
        <button class="btn btn-warning btn-sm" type="button" command="show-modal" commandfor=dialog_id.clone()>"Enable Developer Mode"</button>
        <dialog id=dialog_id.clone() class="modal">
            <div class="modal-box">
                <h3 class="text-lg font-bold">"Enable Developer Mode?"</h3>
                <p class="py-2">
                    "Enabling Developer Mode exposes complete internal game state - "
                    "every agent's raw chat history, exact model input/output, and "
                    "world objects - to every active player. This is effectively "
                    "cheating for a real game. It becomes visible to all players "
                    "immediately."
                </p>
                <div class="modal-action">
                    <form action=action_path.clone() method="post">
                        <input type="hidden" name="enabled" value="true"/>
                        <button class="btn btn-warning btn-sm" type="submit">"Enable anyway"</button>
                    </form>
                    <button class="btn btn-ghost btn-sm" type="button" command="close" commandfor=dialog_id.clone()>"Cancel"</button>
                </div>
            </div>
            <form method="dialog" class="modal-backdrop">
                <button>"close"</button>
            </form>
        </dialog>
    }
}

#[component]
pub fn ChatTranscript(
    history: Vec<ChatEntry>,
    #[prop(default = false)] developer_mode: bool,
) -> impl IntoView {
    history
        .into_iter()
        .map(|entry| view! { <ChatEntryView entry developer_mode/> })
        .collect_view()
}

#[component]
fn ChatEntryView(entry: ChatEntry, #[prop(default = false)] developer_mode: bool) -> impl IntoView {
    let class = match entry.role {
        ChatRole::User => "chat chat-end",
        ChatRole::Assistant => "chat chat-start",
        ChatRole::Narration => "rounded-box bg-base-200 px-4 py-3 text-sm text-base-content/80",
        ChatRole::Speech => "chat chat-start",
        ChatRole::Notice => "alert alert-warning text-sm",
    };
    let bubble = match entry.role {
        ChatRole::User => "chat-bubble chat-bubble-primary",
        ChatRole::Assistant => "chat-bubble",
        ChatRole::Speech => "chat-bubble chat-bubble-secondary",
        ChatRole::Narration | ChatRole::Notice => "",
    };
    let gm = entry.role == ChatRole::Narration;
    // Only present when Developer Mode is on: an ordinary player's page never
    // carries a sequence id, matching design.md - enabling changes visibility,
    // not what is recorded. A delegated click listener on #chat reads this.
    let inspect = developer_mode.then_some(entry.sequence_id).flatten();
    let class = if inspect.is_some() {
        format!("{class} cursor-pointer hover:opacity-80")
    } else {
        class.to_string()
    };
    view! {
        <li class=class data-role=chat_role_name(entry.role) data-speaker=entry.speaker data-inspect-sequence=inspect.map(|id| id.to_string()) data-inspect-entry=entry.id.map(|id| id.to_string())>
            {gm.then(|| view! { <span class="badge badge-outline badge-sm mb-2">"GM"</span> })}
            <div class=bubble>{entry.text}</div>
        </li>
    }
}

pub fn chat_role_name(role: ChatRole) -> &'static str {
    match role {
        ChatRole::User => "user",
        ChatRole::Assistant => "assistant",
        ChatRole::Narration => "narration",
        ChatRole::Speech => "speech",
        ChatRole::Notice => "notice",
    }
}

#[cfg(feature = "hydrate")]
fn connect_player_chat(
    world_id: i64,
    character_id: i64,
    after_message_id: Option<i64>,
    entries: RwSignal<Vec<ChatEntry>>,
    can_act: RwSignal<bool>,
    activity: RwSignal<Option<ChatActivity>>,
    disconnected: RwSignal<bool>,
) -> Rc<RefCell<Option<WebSocket>>> {
    let socket = Rc::new(RefCell::new(None));
    let socket_for_connection = socket.clone();
    let Some(window) = web_sys::window() else {
        add_notice(
            entries,
            "Could not open chat: no browser window is available.".into(),
        );
        disconnected.set(true);
        return socket;
    };
    let location = window.location();
    let scheme = if location.protocol().ok().as_deref() == Some("https:") {
        "wss"
    } else {
        "ws"
    };
    let host = match location.host() {
        Ok(host) => host,
        Err(_) => {
            add_notice(
                entries,
                "Could not determine the browser host for chat.".into(),
            );
            disconnected.set(true);
            return socket;
        }
    };
    let url = match after_message_id {
        Some(after_message_id) => {
            format!(
                "{scheme}://{host}/world/{world_id}/characters/{character_id}/ws?after_message_id={after_message_id}"
            )
        }
        None => format!("{scheme}://{host}/world/{world_id}/characters/{character_id}/ws"),
    };
    let websocket = match WebSocket::new(&url) {
        Ok(socket) => socket,
        Err(error) => {
            add_notice(
                entries,
                format!("Could not open chat connection: {error:?}"),
            );
            disconnected.set(true);
            return socket;
        }
    };

    let on_message_entries = entries;
    let on_message_can_act = can_act;
    let on_message_activity = activity;
    let on_message = Closure::<dyn FnMut(Event)>::new(move |event: Event| {
        let Some(message) = event.dyn_ref::<MessageEvent>() else {
            return;
        };
        let text: String = match message.data().as_string() {
            Some(text) => text,
            None => {
                add_notice(
                    on_message_entries,
                    "Chat server sent a non-text message.".into(),
                );
                return;
            }
        };
        match serde_json::from_str::<ServerEvent>(&text) {
            Ok(ServerEvent::Entry {
                role,
                text,
                speaker,
                id,
                sequence_id,
            }) => upsert_chat_entry(
                on_message_entries,
                ChatEntry {
                    role,
                    text,
                    speaker,
                    id,
                    sequence_id,
                },
            ),
            Ok(ServerEvent::Activity { activity }) => {
                on_message_activity.set(Some(activity));
            }
            Ok(ServerEvent::CanAct { value }) => {
                on_message_can_act.set(value);
                if value {
                    on_message_activity.set(None);
                }
            }
            Ok(ServerEvent::Error { message }) => add_notice(on_message_entries, message),
            Ok(ServerEvent::Stopping) => add_notice(
                on_message_entries,
                "The server is stopping. Reload this page once it is running again.".into(),
            ),
            Err(error) => add_notice(
                on_message_entries,
                format!("Chat server sent an invalid message: {error}"),
            ),
        }
    });
    websocket.set_onmessage(Some(on_message.as_ref().unchecked_ref()));
    // The browser owns this callback until the document unloads with its
    // WebSocket. wasm-bindgen otherwise drops it as this function returns.
    on_message.forget();

    let on_close_can_act = can_act;
    let on_close_activity = activity;
    let on_close_disconnected = disconnected;
    let on_close = Closure::<dyn FnMut(Event)>::new(move |_| {
        on_close_can_act.set(false);
        on_close_activity.set(None);
        on_close_disconnected.set(true);
    });
    websocket.set_onclose(Some(on_close.as_ref().unchecked_ref()));
    on_close.forget();

    *socket_for_connection.borrow_mut() = Some(websocket);
    socket
}

/// A submitted player message is shown immediately, then the server sends the
/// same durable event back with its database identity. Replace that one local
/// placeholder so the visible message becomes inspectable without a duplicate.
#[cfg(feature = "hydrate")]
fn upsert_chat_entry(entries: RwSignal<Vec<ChatEntry>>, entry: ChatEntry) {
    entries.update(|entries| {
        if let Some(id) = entry.id
            && let Some(existing) = entries.iter_mut().rfind(|existing| {
                existing.id.is_none() && existing.role == entry.role && existing.text == entry.text
            })
        {
            existing.id = Some(id);
            existing.sequence_id = entry.sequence_id;
        } else {
            entries.push(entry);
        }
    });
}

/// One live inspection socket and the closures keeping its handlers alive.
/// `close` is called manually before every reconnect (replacing the
/// connection), tearing the previous socket down before opening the next -
/// coding_standards.md forbids `unsafe`, which is what holding a
/// `wasm_bindgen::closure::Closure` in Leptos's `Send + Sync`-bound
/// `on_cleanup` would require, so final teardown on navigation/unload is the
/// browser's own (it always closes open sockets itself), matching this
/// island's existing player-chat socket, which the codebase already leaves
/// to the same browser behavior rather than an explicit unmount hook.
#[cfg(feature = "hydrate")]
struct DevSocket {
    socket: WebSocket,
    _on_message: Closure<dyn FnMut(Event)>,
    _on_error: Closure<dyn FnMut(Event)>,
    _on_close: Closure<dyn FnMut(Event)>,
}

#[cfg(feature = "hydrate")]
impl DevSocket {
    fn close(&self) {
        // Clear handlers before closing so a close fired by this teardown
        // itself cannot report a stale "connection closed" error for the
        // socket a fresh reconnect is about to replace.
        self.socket.set_onmessage(None);
        self.socket.set_onerror(None);
        self.socket.set_onclose(None);
        let _ = self.socket.close();
    }
}

/// Connects to `/world/:id/inspect/events` so an already-open page reflects
/// a live Developer Mode toggle and streamed inference progress without a
/// reload. Unlike the player chat socket's own connection, a failure here is
/// shown explicitly (`dev_error`) with a `Reconnect` control - Developer Mode
/// data must never look current when the live channel silently died (see
/// design.md's Live inspection protocol: "fail-loudly").
///
/// `reconnect_nonce` re-runs this whenever bumped (initial mount counts as
/// one run); each run closes the previous connection (if any) before opening
/// a new one, so reconnecting always repeats the same subscribe/snapshot
/// handshake as the initial connect.
#[cfg(feature = "hydrate")]
fn connect_dev_events(
    world_id: i64,
    developer_mode: RwSignal<bool>,
    dev_events: RwSignal<Option<DevEvent>>,
    dev_error: RwSignal<Option<String>>,
    reconnect_nonce: RwSignal<u32>,
) {
    let current = Rc::new(RefCell::new(None::<DevSocket>));

    Effect::new(move |_| {
        reconnect_nonce.track();
        if let Some(socket) = current.borrow_mut().take() {
            socket.close();
        }
        dev_error.set(None);
        let Some(window) = web_sys::window() else {
            dev_error.set(Some(
                "Could not open live updates: no browser window is available.".into(),
            ));
            return;
        };
        let location = window.location();
        let scheme = if location.protocol().ok().as_deref() == Some("https:") {
            "wss"
        } else {
            "ws"
        };
        let Ok(host) = location.host() else {
            dev_error.set(Some(
                "Could not determine the browser host for live updates.".into(),
            ));
            return;
        };
        let websocket = match WebSocket::new(&format!(
            "{scheme}://{host}/world/{world_id}/inspect/events"
        )) {
            Ok(websocket) => websocket,
            Err(error) => {
                dev_error.set(Some(format!("Could not open live updates: {error:?}")));
                return;
            }
        };

        let on_message = Closure::<dyn FnMut(Event)>::new(move |event: Event| {
            let Some(message) = event.dyn_ref::<MessageEvent>() else {
                return;
            };
            let Some(text) = message.data().as_string() else {
                dev_error.set(Some("Live updates sent a non-text message.".into()));
                return;
            };
            match serde_json::from_str::<DevEvent>(&text) {
                Ok(event) => {
                    if let DevEvent::DeveloperModeChanged { enabled } = &event {
                        developer_mode.set(*enabled);
                    }
                    dev_events.set(Some(event));
                }
                Err(error) => {
                    dev_error.set(Some(format!(
                        "Live updates sent an invalid message: {error}"
                    )));
                }
            }
        });
        websocket.set_onmessage(Some(on_message.as_ref().unchecked_ref()));

        let on_error = Closure::<dyn FnMut(Event)>::new(move |_| {
            dev_error.set(Some("Live updates connection failed.".into()));
        });
        websocket.set_onerror(Some(on_error.as_ref().unchecked_ref()));

        let on_close = Closure::<dyn FnMut(Event)>::new(move |_| {
            dev_error.set(Some("Live updates connection closed.".into()));
        });
        websocket.set_onclose(Some(on_close.as_ref().unchecked_ref()));

        *current.borrow_mut() = Some(DevSocket {
            socket: websocket,
            _on_message: on_message,
            _on_error: on_error,
            _on_close: on_close,
        });
    });
}

#[cfg(feature = "hydrate")]
fn add_notice(entries: RwSignal<Vec<ChatEntry>>, text: String) {
    entries.update(|entries| {
        entries.push(ChatEntry {
            role: ChatRole::Notice,
            text,
            speaker: None,
            id: None,
            sequence_id: None,
        })
    });
}
