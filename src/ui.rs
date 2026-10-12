use leptos::prelude::*;
use serde::{Deserialize, Serialize};

#[cfg(feature = "hydrate")]
use {
    crate::inspection::{
        AgentInfo, BranchHead, BranchNode, BranchParent, Content, GameObject, MessageContent,
        RawHistoryCursor, RawHistoryEntry, RecordedInference, RecordedOutcome, Request,
        SequenceNode, ToolCall, WorldObjects,
    },
    std::{cell::RefCell, rc::Rc},
    wasm_bindgen::{JsCast, closure::Closure},
    wasm_bindgen_futures::{JsFuture, spawn_local},
    web_sys::{Event, HtmlElement, MessageEvent, Response as WebResponse, WebSocket},
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
    Notice,
}

/// A stored or live entry rendered in the player's chat transcript.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct ChatEntry {
    pub role: ChatRole,
    pub text: String,
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
    Sequence(i64),
    Inference(i64),
    AgentHistory(i64),
    Agents,
    Objects,
}

/// Mirrors `store::DevEvent`, the wire format for `/world/:id/inspect/events`.
#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DevEvent {
    DeveloperModeChanged {
        enabled: bool,
    },
    /// A wake-up only: never itself an authorization decision. The server
    /// already rechecked access before sending this; the client still
    /// treats it as informational (mode toggles arrive as
    /// `DeveloperModeChanged`, revocation as the socket closing).
    AccessChanged,
    InferenceStarted {
        inference_id: i64,
        agent_id: i64,
        sequence_id: Option<i64>,
        parent_inference_id: Option<i64>,
    },
    Token {
        inference_id: i64,
        offset: usize,
        text: String,
    },
    InferenceCompleted {
        inference_id: i64,
        error: Option<String>,
    },
    Changed {
        sequence_id: Option<i64>,
        inference_id: Option<i64>,
    },
    ResyncRequired {
        skipped: u64,
    },
}

/// The JSON body `/inspect/agents/:id/history` returns: a page of entries
/// plus the cursor to resume after, or `None` when the page reached the head.
/// Mirrors `web::RawHistoryPageResponse`.
#[cfg(feature = "hydrate")]
#[derive(Clone, Debug, Deserialize)]
struct RawHistoryPageView {
    entries: Vec<RawHistoryEntry>,
    cursor: Option<RawHistoryCursor>,
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
    head: Option<BranchHead>,
    entries: Vec<RawHistoryEntry>,
    cursor: Option<RawHistoryCursor>,
}

/// Whichever view is currently loaded in the pane, keyed by `InspectSelection`.
#[cfg(feature = "hydrate")]
#[derive(Clone, Debug)]
enum InspectData {
    Sequence(Vec<SequenceNode>),
    Inference(RecordedInference),
    /// `branches` is never empty: a plain player/NPC/Storyteller agent has
    /// exactly one section here, with no parents and no head, rendered
    /// exactly as the single linear view always has been. A world_gm agent
    /// whose groups have split/merged has one section per branch in its DAG.
    AgentHistory { agent_id: i64, branches: Vec<BranchSection> },
    Agents(Vec<AgentInfo>),
    Objects(WorldObjects),
}

/// The pane is owned by the chat island, so switching views only changes
/// this local state; the mounted chat and its websocket stay intact
/// (user_declarations.md: "Links between views are important for
/// navigation" - none of them navigate away from the game page).
fn inspect_pane(
    world_id: i64,
    selection: RwSignal<Option<InspectSelection>>,
    dev_events: RwSignal<Option<DevEvent>>,
    developer_mode: RwSignal<bool>,
    dev_error: RwSignal<Option<String>>,
    dev_reconnect_nonce: RwSignal<u32>,
) -> AnyView {
    #[cfg(not(feature = "hydrate"))]
    let _ = (world_id, dev_events, developer_mode, dev_error, dev_reconnect_nonce);
    #[cfg(feature = "hydrate")]
    let data = RwSignal::new(None::<InspectData>);
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

    /// Fetches the view for `selected`, tagging the request with `generation`
    /// so a response arriving after the pane has moved to a different
    /// selection is silently dropped instead of overwriting newer state.
    #[cfg(feature = "hydrate")]
    fn fetch_selection(
        world_id: i64,
        selected: InspectSelection,
        generation: RwSignal<u64>,
        for_generation: u64,
        data: RwSignal<Option<InspectData>>,
        error: RwSignal<Option<String>>,
        invalidated_during_fetch: RwSignal<bool>,
        fetch_in_flight: RwSignal<bool>,
    ) {
        invalidated_during_fetch.set(false);
        fetch_in_flight.set(true);
        let apply = move |result: InspectData| {
            // Stale: the pane has since moved to a different selection.
            if generation.get_untracked() != for_generation {
                return;
            }
            fetch_in_flight.set(false);
            data.set(Some(result));
            // A Changed event for this exact target arrived while this
            // fetch was in flight - the snapshot that just landed may
            // already be behind it, so reload once more.
            if invalidated_during_fetch.get_untracked() {
                fetch_selection(
                    world_id, selected, generation, for_generation, data, error,
                    invalidated_during_fetch, fetch_in_flight,
                );
            }
        };
        match selected {
            InspectSelection::Sequence(sequence_id) => fetch_json(
                format!("/world/{world_id}/inspect/sequences/{sequence_id}"),
                "turn view",
                move |nodes| apply(InspectData::Sequence(nodes)),
                error,
            ),
            InspectSelection::Inference(inference_id) => fetch_json(
                format!("/world/{world_id}/inspect/inferences/{inference_id}"),
                "inference view",
                move |inference| apply(InspectData::Inference(inference)),
                error,
            ),
            InspectSelection::AgentHistory(agent_id) => fetch_json(
                format!("/world/{world_id}/inspect/agents/{agent_id}/branches"),
                "agent branch graph",
                move |nodes: Vec<BranchNode>| {
                    fetch_branch_sections(world_id, agent_id, nodes, apply, error);
                },
                error,
            ),
            InspectSelection::Agents => fetch_json(
                format!("/world/{world_id}/inspect/agents"),
                "agent list",
                move |agents| apply(InspectData::Agents(agents)),
                error,
            ),
            InspectSelection::Objects => fetch_json(
                format!("/world/{world_id}/inspect/objects"),
                "world objects",
                move |objects| apply(InspectData::Objects(objects)),
                error,
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
            let BranchNode { branch_id, parents, head } = node;
            fetch_json(
                format!("/world/{world_id}/inspect/agents/{agent_id}/history?branch={branch_id}"),
                "agent chat history",
                move |page: RawHistoryPageView| {
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
        fetch_selection(
            world_id, selected, generation, for_generation, data, error,
            invalidated_during_fetch, fetch_in_flight,
        );
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
    fn changed_matches(data: &InspectData, sequence_id: Option<i64>, inference_id: Option<i64>) -> bool {
        match data {
            InspectData::Sequence(nodes) => {
                sequence_id.is_some_and(|id| nodes.iter().any(|node| node.inference.sequence_id == Some(id)))
                    || inference_id.is_some_and(|id| nodes.iter().any(|node| node.inference.id == id))
            }
            InspectData::Inference(inference) => inference_id == Some(inference.id),
            InspectData::AgentHistory { .. } | InspectData::Agents(_) | InspectData::Objects(_) => false,
        }
    }

    #[cfg(feature = "hydrate")]
    Effect::new(move |_| {
        let Some(event) = dev_events.get() else { return };
        match event {
            DevEvent::Token { inference_id, offset, text } => {
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
                let Some(InspectSelection::Sequence(open_sequence_id)) = selection.get_untracked()
                else {
                    return;
                };
                if sequence_id != Some(open_sequence_id) {
                    return;
                }
                data.update(|data| {
                    if let Some(InspectData::Sequence(nodes)) = data
                        && !nodes.iter().any(|node| node.inference.id == inference_id)
                    {
                        nodes.push(SequenceNode {
                            inference: RecordedInference {
                                id: inference_id,
                                agent_id,
                                sequence_id,
                                parent_inference_id,
                                segments: Vec::new(),
                                request: Request {
                                    messages: Vec::new(),
                                    tools: Vec::new(),
                                    sampling: Default::default(),
                                },
                                model_prompt: None,
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
                    Some(InspectSelection::Sequence(sequence_id)) => {
                        let for_generation = generation.get_untracked();
                        fetch_selection(
                            world_id, InspectSelection::Sequence(sequence_id), generation,
                            for_generation, data, error, invalidated_during_fetch, fetch_in_flight,
                        );
                    }
                    Some(InspectSelection::Inference(selected_id)) if selected_id == inference_id => {
                        let for_generation = generation.get_untracked();
                        fetch_selection(
                            world_id, InspectSelection::Inference(inference_id), generation,
                            for_generation, data, error, invalidated_during_fetch, fetch_in_flight,
                        );
                    }
                    _ => {}
                }
            }
            DevEvent::Changed { sequence_id, inference_id } => {
                let Some(selected) = selection.get_untracked() else { return };
                let matches = data.with_untracked(|data| {
                    data.as_ref().is_some_and(|data| changed_matches(data, sequence_id, inference_id))
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
                    fetch_selection(
                        world_id, selected, generation, for_generation, data, error,
                        invalidated_during_fetch, fetch_in_flight,
                    );
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
                    fetch_selection(
                        world_id, selected, generation, for_generation, data, error,
                        invalidated_during_fetch, fetch_in_flight,
                    );
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

    view! {
        <aside class="flex min-h-0 w-full max-w-md flex-col gap-3 overflow-y-auto border-l border-base-300 p-3 sm:p-5">
            <div class="flex items-center justify-between">
                <h2 class="text-lg font-semibold">"Inspect"</h2>
                <div class="flex gap-1">
                    <button class="btn btn-ghost btn-xs" on:click=move |_| selection.set(Some(InspectSelection::Agents))>"Agents"</button>
                    <button class="btn btn-ghost btn-xs" on:click=move |_| selection.set(Some(InspectSelection::Objects))>"Objects"</button>
                    <button class="btn btn-ghost btn-sm" on:click=move |_| selection.set(None)>"Close"</button>
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
                        Some(InspectData::Sequence(nodes)) => {
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
                                <div class="flex flex-col gap-2">
                                    <div class="flex flex-wrap gap-2 text-xs text-base-content/60">
                                        <span>{format!("{} inferences", nodes.len())}</span>
                                        <span>{format!("{input_tokens}+{output_tokens} tokens total")}</span>
                                        <span>{format!("{duration_ms}ms summed inference time")}</span>
                                    </div>
                                    {nodes.into_iter().map(|node| view! {
                                        <SequenceNodeCard node selection live_tokens/>
                                    }).collect_view()}
                                </div>
                            }
                        }.into_any(),
                        Some(InspectData::Inference(inference)) => view! {
                            <InferenceCard inference selection standalone=true live_tokens/>
                        }.into_any(),
                        Some(InspectData::AgentHistory { agent_id, branches }) => view! {
                            <AgentHistoryView world_id agent_id branches selection data/>
                        }.into_any(),
                        Some(InspectData::Agents(agents)) => view! {
                            <AgentsList agents selection/>
                        }.into_any(),
                        Some(InspectData::Objects(objects)) => view! {
                            <ObjectsView objects/>
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

/// Renders `content` without collapsing it into one string: each tool call
/// keeps its own `id`/`name`/`arguments` as a separate row, and every piece
/// gets its own copy control for the exact recorded text (design.md: "Raw
/// fields are expandable and copyable without truncation").
#[cfg(feature = "hydrate")]
#[component]
fn ContentBlock(content: MessageContent) -> impl IntoView {
    match content {
        MessageContent::Text(text) => view! { <CopyableText text/> }.into_any(),
        MessageContent::ToolCalls(calls) => view! { <ToolCallList calls/> }.into_any(),
        MessageContent::ToolResult { tool_call_id, content } => view! {
            <div class="flex flex-wrap items-start gap-1">
                <span class="badge badge-ghost badge-sm">{format!("answers call {tool_call_id}")}</span>
                <CopyableText text=content/>
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
        Content::Text(text) => view! { <CopyableText text/> }.into_any(),
        Content::ToolCalls(calls) => view! { <ToolCallList calls/> }.into_any(),
    }
}

#[cfg(feature = "hydrate")]
#[component]
fn ToolCallList(calls: Vec<ToolCall>) -> impl IntoView {
    view! {
        <ul class="space-y-1">
            {calls.into_iter().map(|call| view! {
                <li class="flex flex-wrap items-start gap-1">
                    <span class="badge badge-ghost badge-sm">{format!("call {}", call.id)}</span>
                    <span class="font-semibold">{call.name}</span>
                    <CopyableText text=call.arguments/>
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
        let Some(window) = web_sys::window() else { return };
        let promise = window.navigator().clipboard().write_text(&text);
        spawn_local(async move {
            if JsFuture::from(promise).await.is_ok() {
                copied.set(true);
                reset_after_delay(copied);
            }
        });
    };
    view! {
        <button type="button" class="btn btn-ghost btn-xs" on:click=on_click>
            {move || if copied.get() { "Copied" } else { "Copy" }}
        </button>
    }
}

/// Clears the "Copied" confirmation after a couple of seconds.
#[cfg(feature = "hydrate")]
fn reset_after_delay(copied: RwSignal<bool>) {
    let Some(window) = web_sys::window() else { return };
    let reset = Closure::<dyn FnMut()>::new(move || copied.set(false));
    let _ = window.set_timeout_with_callback_and_timeout_and_arguments_0(
        reset.as_ref().unchecked_ref(),
        2000,
    );
    reset.forget();
}

/// One node of the turn view: an inference plus what it produced. Shares
/// `InferenceCard`'s rendering of the inference itself, since the turn view
/// and the dedicated inference view show the identical verbatim data.
#[cfg(feature = "hydrate")]
#[component]
fn SequenceNodeCard(
    node: SequenceNode,
    selection: RwSignal<Option<InspectSelection>>,
    live_tokens: RwSignal<std::collections::HashMap<i64, String>>,
) -> impl IntoView {
    let produced = node.messages;
    view! {
        <div class="card bg-base-200">
            <div class="card-body gap-1 p-3 text-sm">
                <InferenceCard inference=node.inference selection standalone=false live_tokens/>
                {(!node.actions.is_empty()).then(|| view! {
                    <ul class="mt-1 space-y-1 border-t border-base-300 pt-1">
                        {node.actions.into_iter().map(|action| view! {
                            <li><span class="font-semibold">{action.tool}</span>{format!("({}) -> {}", action.args, action.result)}</li>
                        }).collect_view()}
                    </ul>
                })}
                {(!produced.is_empty()).then(|| view! {
                    <ul class="mt-1 space-y-1 border-t border-base-300 pt-1 text-base-content/60">
                        {produced.into_iter().map(|message| view! {
                            <li>{match message.answers_tool_execution_id {
                                Some(tool_execution_id) => format!("{} (answers tool_execution {tool_execution_id})", message.role.as_str()),
                                None => message.role.as_str().to_string(),
                            }}</li>
                        }).collect_view()}
                    </ul>
                })}
            </div>
        </div>
    }
}

/// Renders one inference's verbatim input/output - the "inference view" of
/// user_declarations.md, shared between the turn view's per-node display and
/// the dedicated `/inspect/inferences/:id` view. Badges link to the agent's
/// raw chat history and (when standalone) back to the turn it belongs to,
/// matching "links between views are important for navigation".
#[cfg(feature = "hydrate")]
#[component]
fn InferenceCard(
    inference: RecordedInference,
    selection: RwSignal<Option<InspectSelection>>,
    standalone: bool,
    live_tokens: RwSignal<std::collections::HashMap<i64, String>>,
) -> impl IntoView {
    let is_running = matches!(inference.outcome, RecordedOutcome::Running);
    let (outcome_text, outcome_content, reasoning, usage) = match inference.outcome {
        RecordedOutcome::Running => (None, None, String::new(), None),
        RecordedOutcome::Error(error) => (Some(format!("error: {error}")), None, String::new(), None),
        RecordedOutcome::Response(response) => match response.content {
            // Plain text can still be streaming into view live; render it
            // through the same reactive path as the running/error cases.
            Content::Text(text) => (Some(text), None, response.reasoning, Some(response.usage)),
            // Tool calls never stream as partial text - render their full
            // structure (each call's id/name/arguments) once the response
            // has actually completed.
            content => (None, Some(content), response.reasoning, Some(response.usage)),
        },
    };
    let request_messages = inference.request.messages;
    let model_prompt = inference.model_prompt;
    let inference_id = inference.id;
    let agent_id = inference.agent_id;
    let sequence_id = inference.sequence_id;
    view! {
        <div>
            <div class="flex flex-wrap items-center gap-2">
                {if standalone {
                    view! { <span class="badge badge-outline">{format!("inference {inference_id}")}</span> }.into_any()
                } else {
                    view! {
                        <button class="badge badge-outline" on:click=move |_| selection.set(Some(InspectSelection::Inference(inference_id)))>
                            {format!("inference {inference_id}")}
                        </button>
                    }.into_any()
                }}
                <button class="badge badge-ghost" on:click=move |_| selection.set(Some(InspectSelection::AgentHistory(agent_id)))>
                    {format!("agent {agent_id}")}
                </button>
                <span class="badge badge-ghost">{inference.model}</span>
                {inference.duration_ms.map(|ms| view! { <span class="badge badge-ghost">{format!("{ms}ms")}</span> })}
                {usage.map(|usage| view! {
                    <span class="badge badge-ghost">{format!("{}+{} tokens", usage.input_tokens, usage.output_tokens)}</span>
                })}
                {inference.parent_inference_id.map(|parent| view! {
                    <button class="badge badge-ghost" on:click=move |_| selection.set(Some(InspectSelection::Inference(parent)))>
                        {format!("parent {parent}")}
                    </button>
                })}
                {standalone.then(|| sequence_id.map(|sequence_id| view! {
                    <button class="badge badge-ghost" on:click=move |_| selection.set(Some(InspectSelection::Sequence(sequence_id)))>
                        {format!("turn {sequence_id}")}
                    </button>
                }))}
            </div>
            {
                fn running_text(
                    outcome_text: &Option<String>,
                    is_running: bool,
                    inference_id: i64,
                    live_tokens: RwSignal<std::collections::HashMap<i64, String>>,
                ) -> String {
                    match outcome_text {
                        Some(text) => text.clone(),
                        None if is_running => live_tokens
                            .get()
                            .get(&inference_id)
                            .cloned()
                            .map(|streamed| format!("{streamed}…"))
                            .unwrap_or_else(|| "running…".to_string()),
                        None => String::new(),
                    }
                }
                let outcome_for_button = outcome_text.clone();
                view! {
                    {outcome_content.map(|content| view! { <ResponseContentBlock content/> })}
                    <div class="flex items-start gap-1">
                        <p class="whitespace-pre-wrap">
                            {move || running_text(&outcome_text, is_running, inference_id, live_tokens)}
                        </p>
                        {move || {
                            let text = running_text(&outcome_for_button, is_running, inference_id, live_tokens);
                            (!text.is_empty()).then(|| view! { <CopyButton text/> })
                        }}
                    </div>
                }
            }
            {(!reasoning.is_empty()).then(|| view! {
                <p class="whitespace-pre-wrap text-base-content/60">{format!("reasoning: {reasoning}")}</p>
            })}
            <details class="mt-1 border-t border-base-300 pt-1">
                <summary class="cursor-pointer select-none">"Input"</summary>
                <ul class="mt-1 space-y-1">
                    {request_messages.into_iter().map(|message| view! {
                        <li>
                            <span class="font-semibold">{message.role.as_str()}</span>
                            <ContentBlock content=message.content/>
                            {(!message.reasoning.is_empty()).then(|| view! {
                                <span class="block whitespace-pre-wrap text-base-content/60">{format!("reasoning: {}", message.reasoning)}</span>
                            })}
                        </li>
                    }).collect_view()}
                </ul>
                {model_prompt.map(|prompt| view! {
                    <div class="mt-1 flex items-start gap-1 border-t border-base-300 pt-1">
                        <p class="whitespace-pre-wrap font-mono text-xs">{prompt.clone()}</p>
                        <CopyButton text=prompt/>
                    </div>
                })}
            </details>
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
    selection: RwSignal<Option<InspectSelection>>,
    data: RwSignal<Option<InspectData>>,
) -> impl IntoView {
    let single = branches.len() == 1;
    view! {
        <div class="flex flex-col gap-3">
            <h3 class="text-sm font-semibold">{format!("Agent {agent_id} chat history")}</h3>
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
    let load_more = move |cursor: RawHistoryCursor| {
        loading_more.set(true);
        let query = serde_json::to_string(&cursor).unwrap_or_default();
        let encoded = js_sys::encode_uri_component(&query);
        fetch_json(
            format!("/world/{world_id}/inspect/agents/{agent_id}/history?after={encoded}"),
            "next agent chat history page",
            move |page: RawHistoryPageView| {
                loading_more.set(false);
                data.update(|data| {
                    if let Some(InspectData::AgentHistory { branches, .. }) = data
                        && let Some(section) = branches.iter_mut().find(|section| section.branch_id == branch_id)
                    {
                        section.entries.extend(page.entries);
                        section.cursor = page.cursor;
                    }
                });
            },
            load_more_error,
        );
    };
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
fn RawHistoryEntryRow(entry: RawHistoryEntry, selection: RwSignal<Option<InspectSelection>>) -> impl IntoView {
    match entry {
        RawHistoryEntry::Message { id, role, content, reasoning, source_inference_id, answers_tool_execution_id } => view! {
            <li class="rounded bg-base-200 p-2">
                <div class="flex flex-wrap items-center gap-2">
                    <span class="badge badge-ghost badge-sm">{format!("message {id}")}</span>
                    <span class="font-semibold">{role.as_str()}</span>
                    {answers_tool_execution_id.map(|tool_execution_id| view! { <span class="badge badge-ghost badge-sm">{format!("answers tool_execution {tool_execution_id}")}</span> })}
                    {source_inference_id.map(|inference_id| view! {
                        <button class="badge badge-outline badge-sm" on:click=move |_| selection.set(Some(InspectSelection::Inference(inference_id)))>
                            {format!("inference {inference_id}")}
                        </button>
                    })}
                </div>
                <ContentBlock content/>
                {(!reasoning.is_empty()).then(|| view! {
                    <p class="whitespace-pre-wrap text-base-content/60">{format!("reasoning: {reasoning}")}</p>
                })}
            </li>
        }.into_any(),
        RawHistoryEntry::Notice { id, content, source_inference_id } => view! {
            <li class="alert alert-warning text-sm">
                <span class="badge badge-ghost badge-sm">{format!("notice {id}")}</span>
                <span>{content}</span>
                {source_inference_id.map(|inference_id| view! {
                    <button class="badge badge-outline badge-sm" on:click=move |_| selection.set(Some(InspectSelection::Inference(inference_id)))>
                        {format!("inference {inference_id}")}
                    </button>
                })}
            </li>
        }.into_any(),
        RawHistoryEntry::Summary { id, covered_ranges, inference_id, text } => view! {
            <li>
                <details class="rounded border border-base-300 p-2">
                    <summary class="cursor-pointer select-none text-base-content/60">
                        {format!(
                            "summary {id} covers {}",
                            covered_ranges
                                .iter()
                                .map(|range| format!(
                                    "branch {} #{}-#{}",
                                    range.branch_id, range.first_position, range.last_position,
                                ))
                                .collect::<Vec<_>>()
                                .join(", "),
                        )}
                    </summary>
                    <p class="mt-1 whitespace-pre-wrap">{text}</p>
                    <button class="badge badge-outline badge-sm mt-1" on:click=move |_| selection.set(Some(InspectSelection::Inference(inference_id)))>
                        {format!("inference {inference_id}")}
                    </button>
                </details>
            </li>
        }.into_any(),
        RawHistoryEntry::FailedInference { id, error } => view! {
            <li class="alert alert-error text-sm">
                <button class="badge badge-outline badge-sm" on:click=move |_| selection.set(Some(InspectSelection::Inference(id)))>
                    {format!("inference {id} failed")}
                </button>
                <span>{error}</span>
            </li>
        }.into_any(),
    }
}

/// The world agents/GMs navigator (user_declarations.md: "Browseable world
/// objects, descriptions, tools, agents and links to their chat histories").
#[cfg(feature = "hydrate")]
#[component]
fn AgentsList(agents: Vec<AgentInfo>, selection: RwSignal<Option<InspectSelection>>) -> impl IntoView {
    view! {
        <ul class="flex flex-col gap-1 text-sm">
            {agents.into_iter().map(|agent| view! {
                <li>
                    <button class="btn btn-ghost btn-sm w-full justify-start" on:click=move |_| selection.set(Some(InspectSelection::AgentHistory(agent.id)))>
                        <span class="badge badge-ghost badge-sm">{agent.role}</span>
                        {agent.character_name.unwrap_or_else(|| format!("agent {}", agent.id))}
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
fn ObjectsView(objects: WorldObjects) -> impl IntoView {
    fn object_row(object: GameObject) -> impl IntoView {
        view! {
            <li class="rounded bg-base-200 p-2">
                <p class="font-semibold">{format!("{} ({})", object.name, object.id)}</p>
                <p class="whitespace-pre-wrap">{object.description}</p>
                {(!object.gm_notes.is_empty()).then(|| view! {
                    <p class="whitespace-pre-wrap text-base-content/60">{format!("GM notes: {}", object.gm_notes)}</p>
                })}
                {(!object.storyteller_notes.is_empty()).then(|| view! {
                    <p class="whitespace-pre-wrap text-base-content/60">{format!("Storyteller notes: {}", object.storyteller_notes)}</p>
                })}
            </li>
        }
    }
    view! {
        <div class="flex flex-col gap-3 text-sm">
            <div class="rounded bg-base-200 p-2">
                <p class="font-semibold">{format!("{} (time {})", objects.name, objects.time)}</p>
                <p class="whitespace-pre-wrap text-base-content/60">{objects.storyteller_summary}</p>
                {(!objects.initial_prompt.is_empty()).then(|| view! {
                    <details class="mt-1">
                        <summary class="cursor-pointer select-none text-base-content/60">"Initial prompt"</summary>
                        <p class="whitespace-pre-wrap">{objects.initial_prompt}</p>
                    </details>
                })}
                {(!objects.gm_notes.is_empty()).then(|| view! {
                    <p class="whitespace-pre-wrap text-base-content/60">{format!("GM notes: {}", objects.gm_notes)}</p>
                })}
                {(!objects.storyteller_notes.is_empty()).then(|| view! {
                    <p class="whitespace-pre-wrap text-base-content/60">{format!("Storyteller notes: {}", objects.storyteller_notes)}</p>
                })}
            </div>
            <div>
                <h3 class="text-sm font-semibold">"Characters"</h3>
                <ul class="mt-1 flex flex-col gap-1">{objects.characters.into_iter().map(object_row).collect_view()}</ul>
            </div>
            <div>
                <h3 class="text-sm font-semibold">"Locations"</h3>
                <ul class="mt-1 flex flex-col gap-1">{objects.locations.into_iter().map(object_row).collect_view()}</ul>
            </div>
            <div>
                <h3 class="text-sm font-semibold">"Items"</h3>
                <ul class="mt-1 flex flex-col gap-1">{objects.items.into_iter().map(object_row).collect_view()}</ul>
            </div>
            <div>
                <h3 class="text-sm font-semibold">"Connections"</h3>
                <ul class="mt-1 flex flex-col gap-1">
                    {objects.connections.into_iter().map(|connection| view! {
                        <li class="rounded bg-base-200 p-2">
                            <p class="font-semibold">{format!("{} <-> {} ({})", connection.from, connection.to, connection.id)}</p>
                            <p class="whitespace-pre-wrap">{connection.description}</p>
                            <p class="text-base-content/60">{format!("travel time: {}", connection.travel_time)}</p>
                            {(!connection.gm_notes.is_empty()).then(|| view! {
                                <p class="whitespace-pre-wrap text-base-content/60">{format!("GM notes: {}", connection.gm_notes)}</p>
                            })}
                            {(!connection.storyteller_notes.is_empty()).then(|| view! {
                                <p class="whitespace-pre-wrap text-base-content/60">{format!("Storyteller notes: {}", connection.storyteller_notes)}</p>
                            })}
                        </li>
                    }).collect_view()}
                </ul>
            </div>
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
    let input = NodeRef::<leptos::html::Input>::new();
    let inspecting = RwSignal::new(None::<InspectSelection>);
    // A signal, not the plain prop: an owner may toggle Developer Mode while
    // this page stays open, and every active member's page must reflect it
    // immediately (user_declarations.md), not only on reload.
    let developer_mode = RwSignal::new(developer_mode);
    // The most recent live event from /inspect/events, for the currently
    // open inspect pane to patch itself with instead of a network re-fetch.
    let dev_events = RwSignal::new(None::<DevEvent>);
    let dev_error = RwSignal::new(None::<String>);
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
    connect_dev_events(world_id, developer_mode, dev_events, dev_error, dev_reconnect_nonce);

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
    watch_inspect_clicks(chat, inspecting, developer_mode);

    let body = view! {
        <div class="flex min-h-0 flex-1 gap-3">
            <div class="flex min-h-0 flex-1 flex-col">
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
            {move || developer_mode.get().then(|| inspect_pane(world_id, inspecting, dev_events, developer_mode, dev_error, dev_reconnect_nonce))}
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
            inspecting.set(Some(InspectSelection::Sequence(sequence_id)));
        });
        let _ = chat.add_event_listener_with_callback("click", on_click.as_ref().unchecked_ref());
        on_click.forget();
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
#[island]
pub fn DeveloperModeEnableButton(world_id: i64) -> impl IntoView {
    let dialog = NodeRef::<leptos::html::Dialog>::new();
    let action_path = format!("/world/{world_id}/developer-mode");
    let open_dialog = move |_| {
        #[cfg(feature = "hydrate")]
        if let Some(dialog) = dialog.get() {
            let dialog: web_sys::HtmlDialogElement = dialog.unchecked_into();
            let _ = dialog.show_modal();
        }
    };
    let close_dialog = move |_| {
        #[cfg(feature = "hydrate")]
        if let Some(dialog) = dialog.get() {
            let dialog: web_sys::HtmlDialogElement = dialog.unchecked_into();
            dialog.close();
        }
    };
    view! {
        <button class="btn btn-warning btn-sm" on:click=open_dialog>"Enable Developer Mode"</button>
        <dialog node_ref=dialog class="modal">
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
                    <button class="btn btn-ghost btn-sm" on:click=close_dialog>"Cancel"</button>
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
        ChatRole::Notice => "alert alert-warning text-sm",
    };
    let bubble = match entry.role {
        ChatRole::User => "chat-bubble chat-bubble-primary",
        ChatRole::Assistant => "chat-bubble",
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
        <li class=class data-role=chat_role_name(entry.role) data-inspect-sequence=inspect.map(|id| id.to_string())>
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
                id,
                sequence_id,
            }) => on_message_entries.update(|entries| {
                entries.push(ChatEntry {
                    role,
                    text,
                    id,
                    sequence_id,
                });
            }),
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
            dev_error.set(Some("Could not open live updates: no browser window is available.".into()));
            return;
        };
        let location = window.location();
        let scheme = if location.protocol().ok().as_deref() == Some("https:") {
            "wss"
        } else {
            "ws"
        };
        let Ok(host) = location.host() else {
            dev_error.set(Some("Could not determine the browser host for live updates.".into()));
            return;
        };
        let websocket = match WebSocket::new(&format!("{scheme}://{host}/world/{world_id}/inspect/events")) {
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
                    dev_error.set(Some(format!("Live updates sent an invalid message: {error}")));
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
            id: None,
            sequence_id: None,
        })
    });
}
