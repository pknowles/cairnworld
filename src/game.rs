use std::{
    collections::{HashMap, HashSet},
    sync::{
        Arc,
        atomic::{AtomicI64, Ordering},
    },
};

use anyhow::{Context, Result, ensure};
use schemars::{JsonSchema, schema_for};
use serde::Deserialize;
use tokio::sync::{Mutex, broadcast, mpsc, oneshot, watch};

use crate::{
    agent::{self, Budget, CallContext},
    inference::ScheduledBackend,
    llm::{Backend, Content, Message, Response, Role, Sampling, ToolDefinition},
    settings::Limits,
    store::{PendingAction, PlayerAgent, PresenceNarration, Store},
    tools::{GroupPartition, Tool, ToolFuture, ToolOutcome, TopologyChange},
};

/// The one game service that a browser event will invoke. It derives the player
/// history from a previously authenticated membership; no tool accepts a
/// client-selected agent or character id.
pub struct Game<B> {
    store: Store,
    backend: ScheduledBackend<B>,
    limits: Limits,
    model: String,
    sampling: Sampling,
    worlds: Mutex<HashMap<i64, WorldEvents>>,
    openings: Mutex<HashMap<i64, watch::Receiver<OpeningState>>>,
    /// Process-local, never restored after restart (design.md's Membership
    /// and presence: "Live connection IDs are process-local").
    next_connection_id: AtomicI64,
}

#[derive(Clone, Debug)]
enum OpeningState {
    Pending,
    Ready,
    Failed(String),
}

#[derive(Clone)]
struct WorldEvents {
    sender: mpsc::Sender<WorldEvent>,
    broadcasts: Arc<Mutex<HashMap<i64, broadcast::Sender<String>>>>,
    /// The short-lock-protected set of live connection IDs per character
    /// (design.md's Membership and presence). A separate lock from
    /// `broadcasts`/`sender` so connect/disconnect never wait on an ongoing
    /// model turn or narration delivery.
    connections: Arc<Mutex<HashMap<i64, HashSet<i64>>>>,
}

type GroupBroadcasts = Arc<Mutex<HashMap<i64, broadcast::Sender<String>>>>;

struct Narration {
    location_id: i64,
    group_id: i64,
    text: String,
    /// The recorded_content row the GM's own settled reply message already
    /// references. Every present character's delivered copy points at this
    /// same reference rather than re-recording the narration text once per
    /// recipient (design.md's Chat schema: "every recipient references that
    /// same selection").
    content_id: i64,
}

/// Everything a player-facing action tool needs to submit one action to its
/// location GM. The tool owns this invocation scope until a model calls it.
struct PlayerActionTool {
    member: PlayerAgent,
    sequence_id: i64,
    definition: ToolDefinition,
    validate: fn(&str) -> Result<()>,
    location_id: i64,
    budget: Budget,
    call: CallContext,
    broadcasts: GroupBroadcasts,
}

enum WorldEvent {
    PlayerMessage {
        member: PlayerAgent,
        text: String,
        reply: oneshot::Sender<Result<Response>>,
    },
    Enter {
        member: PlayerAgent,
        reply: oneshot::Sender<Result<Option<Response>>>,
    },
    /// A character's connected/disconnected transition. This is only a
    /// wake-up carrying the identity that changed; the handler reads current
    /// presence/group state when it actually acts, not this event's own
    /// payload, since further changes may have queued behind it.
    PresenceChanged { character_id: i64 },
    /// A previously scheduled arrival/departure deadline has elapsed. `group_id`
    /// and `generation` let the handler recognize a deadline superseded by a
    /// later change (design.md: "stale timer events do nothing").
    PresenceDeadline { group_id: i64, generation: i64 },
}

impl<B> Game<B>
where
    B: Backend + Send + Sync + 'static,
{
    pub fn new(
        store: Store,
        backend: ScheduledBackend<B>,
        limits: Limits,
        model: String,
        sampling: Sampling,
    ) -> Self {
        Self {
            store,
            backend,
            limits,
            model,
            sampling,
            worlds: Mutex::new(HashMap::new()),
            openings: Mutex::new(HashMap::new()),
            next_connection_id: AtomicI64::new(1),
        }
    }

    /// Queue a player message behind all earlier events in this world.
    pub async fn player_message(
        self: &Arc<Self>,
        member: PlayerAgent,
        text: &str,
    ) -> Result<Response> {
        let world = self.world_events(member.world_id).await;
        let (reply, receive) = oneshot::channel();
        world
            .sender
            .send(WorldEvent::PlayerMessage {
                member,
                text: text.to_string(),
                reply,
            })
            .await
            .context("queueing player message")?;
        receive
            .await
            .context("world stopped processing player message")?
    }

    /// Queue opening a world behind all earlier events in that world.
    pub async fn enter(self: &Arc<Self>, member: PlayerAgent) -> Result<Option<Response>> {
        let world = self.world_events(member.world_id).await;
        let (reply, receive) = oneshot::channel();
        world
            .sender
            .send(WorldEvent::Enter { member, reply })
            .await
            .context("queueing world entry")?;
        receive
            .await
            .context("world stopped processing world entry")?
    }

    /// Wait for the one server-owned opening turn for a blank Adventurer.
    /// Multiple viewers share this operation; only its first observer queues
    /// game work. Once the player agent has a stored reply, reconnects return
    /// immediately and merely view that history.
    pub async fn wait_for_opening(self: &Arc<Self>, member: PlayerAgent) -> Result<()> {
        if self.opening_complete(&member).await? {
            return Ok(());
        }
        let character_id = member.character_id;
        let mut receiver = {
            let mut openings = self.openings.lock().await;
            if let Some(receiver) = openings.get(&character_id) {
                receiver.clone()
            } else {
                let (sender, receiver) = watch::channel(OpeningState::Pending);
                openings.insert(character_id, receiver.clone());
                let game = Arc::clone(self);
                tokio::spawn(async move {
                    tracing::info!(
                        world_id = member.world_id,
                        user_id = member.user_id,
                        "starting server-owned player agent opening turn"
                    );
                    let state = match game.enter(member).await {
                        Ok(_) => OpeningState::Ready,
                        Err(error) => OpeningState::Failed(format!("{error:#}")),
                    };
                    let _ = sender.send(state);
                    game.openings.lock().await.remove(&character_id);
                });
                receiver
            }
        };
        loop {
            let state = receiver.borrow_and_update().clone();
            match state {
                OpeningState::Pending => receiver
                    .changed()
                    .await
                    .context("opening player agent ended before completing")?,
                OpeningState::Ready => return Ok(()),
                OpeningState::Failed(error) => anyhow::bail!(error),
            }
        }
    }

    /// Subscribe a connected browser to live narration from the character's
    /// current group. World processing remains serialized across groups.
    pub async fn subscribe(
        self: &Arc<Self>,
        member: &PlayerAgent,
    ) -> Result<broadcast::Receiver<String>> {
        let group_id = self.store.character_group_id(member.character_id).await?;
        Ok(group_broadcast(
            &self.world_events(member.world_id).await.broadcasts,
            group_id,
        )
        .await
        .subscribe())
    }

    /// Register one live connection for a character, returning its id for the
    /// matching `disconnect` call. A durable zero/one transition (design.md's
    /// Membership and presence) persists `character_presence` and wakes the
    /// world's serialized queue with a `PresenceChanged` event; this never
    /// waits for that queue to actually process it.
    pub async fn connect(self: &Arc<Self>, member: &PlayerAgent) -> Result<i64> {
        let connection_id = self.next_connection_id.fetch_add(1, Ordering::Relaxed);
        let world = self.world_events(member.world_id).await;
        let became_present = {
            let mut connections = world.connections.lock().await;
            let characters = connections.entry(member.character_id).or_default();
            let was_empty = characters.is_empty();
            characters.insert(connection_id);
            was_empty
        };
        if became_present {
            self.store
                .mark_character_present(member.character_id)
                .await
                .context("recording character presence")?;
            let _ = world.sender.try_send(WorldEvent::PresenceChanged {
                character_id: member.character_id,
            });
        }
        Ok(connection_id)
    }

    /// Release one live connection, reversing `connect`. A connection's owner
    /// releases it exactly once; disconnecting an id already removed (e.g. a
    /// double-release race) is a no-op rather than an error, matching a
    /// one/zero set membership check rather than a counter.
    pub async fn disconnect(self: &Arc<Self>, member: &PlayerAgent, connection_id: i64) -> Result<()> {
        let world = self.world_events(member.world_id).await;
        let became_absent = {
            let mut connections = world.connections.lock().await;
            let Some(characters) = connections.get_mut(&member.character_id) else {
                return Ok(());
            };
            characters.remove(&connection_id);
            let now_empty = characters.is_empty();
            if now_empty {
                connections.remove(&member.character_id);
            }
            now_empty
        };
        if became_absent {
            self.store
                .mark_character_absent(member.character_id)
                .await
                .context("recording character absence")?;
            let _ = world.sender.try_send(WorldEvent::PresenceChanged {
                character_id: member.character_id,
            });
        }
        Ok(())
    }

    async fn world_events(self: &Arc<Self>, world_id: i64) -> WorldEvents {
        let mut worlds = self.worlds.lock().await;
        if let Some(world) = worlds.get(&world_id) {
            return world.clone();
        }
        let (sender, receiver) = mpsc::channel(64);
        let world = WorldEvents {
            sender,
            broadcasts: Arc::new(Mutex::new(HashMap::new())),
            connections: Arc::new(Mutex::new(HashMap::new())),
        };
        worlds.insert(world_id, world.clone());
        drop(worlds);
        let game = Arc::clone(self);
        let broadcasts = Arc::clone(&world.broadcasts);
        let sender = world.sender.clone();
        tokio::spawn(async move { game.process_world(receiver, sender, broadcasts).await });
        world
    }

    async fn process_world(
        self: Arc<Self>,
        mut events: mpsc::Receiver<WorldEvent>,
        events_sender: mpsc::Sender<WorldEvent>,
        broadcasts: Arc<Mutex<HashMap<i64, broadcast::Sender<String>>>>,
    ) {
        while let Some(event) = events.recv().await {
            match event {
                WorldEvent::PlayerMessage {
                    member,
                    text,
                    reply,
                } => {
                    let _ = reply.send(
                        self.resolve_player_message(member, &text, &broadcasts)
                            .await,
                    );
                }
                WorldEvent::Enter { member, reply } => {
                    let _ = reply.send(self.resolve_enter(member, &broadcasts).await);
                }
                WorldEvent::PresenceChanged { character_id } => {
                    if let Err(error) = self
                        .handle_presence_changed(character_id, &events_sender, &broadcasts)
                        .await
                    {
                        tracing::error!(character_id, error = %format!("{error:#}"), "handling presence change");
                    }
                }
                WorldEvent::PresenceDeadline { group_id, generation } => {
                    if let Err(error) = self
                        .handle_presence_deadline(group_id, generation, &broadcasts)
                        .await
                    {
                        tracing::error!(group_id, generation, error = %format!("{error:#}"), "handling presence deadline");
                    }
                }
            }
        }
    }

    /// React to a character's connect/disconnect: on first arrival into an
    /// occupied group, narrate immediately; on a departure, or first arrival
    /// after an empty period, schedule (or leave already-scheduled) a
    /// 60-second deadline. Reads current presence/group state rather than
    /// trusting the event's own payload, since further changes may have
    /// queued behind it (design.md's Membership and presence).
    async fn handle_presence_changed(
        self: &Arc<Self>,
        character_id: i64,
        events_sender: &mpsc::Sender<WorldEvent>,
        broadcasts: &GroupBroadcasts,
    ) -> Result<()> {
        let group_id = self.store.character_group_id(character_id).await?;
        let online = self.store.group_online_character_ids(group_id).await?;
        let narration = self.store.presence_narration(group_id).await?;
        if narration.due_at.is_some() {
            // A departure or empty-period arrival is already pending for
            // this group; subsequent changes within that window share the
            // existing deadline rather than rescheduling it.
            return Ok(());
        }
        let group_already_occupied = !narration.last_announced_character_ids.is_empty();
        if group_already_occupied && online.len() > narration.last_announced_character_ids.len() {
            // Arrival into a group already shown (by its last narration) as
            // occupied: narrate immediately, no delay.
            return self
                .narrate_presence_transition(group_id, &narration, &online, broadcasts)
                .await;
        }
        if online.len() != narration.last_announced_character_ids.len()
            || online != narration.last_announced_character_ids
        {
            // A departure (or an arrival into an already-occupied group,
            // which design.md also delays behind the shared departure
            // window once the group is non-empty) - schedule the deadline.
            let after_seconds = self.limits.presence_departure_seconds;
            let generation = self
                .store
                .schedule_presence_deadline(group_id, i64::try_from(after_seconds)?, narration.generation)
                .await?;
            let sender = events_sender.clone();
            tokio::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_secs(after_seconds)).await;
                let _ = sender
                    .send(WorldEvent::PresenceDeadline { group_id, generation })
                    .await;
            });
        }
        Ok(())
    }

    /// Act on a scheduled deadline if it is still current. A stale deadline
    /// (superseded by a later change that already rescheduled or resolved
    /// it) does nothing (design.md: "stale timer events do nothing").
    async fn handle_presence_deadline(
        self: &Arc<Self>,
        group_id: i64,
        generation: i64,
        broadcasts: &GroupBroadcasts,
    ) -> Result<()> {
        let narration = self.store.presence_narration(group_id).await?;
        if narration.generation != generation {
            return Ok(());
        }
        let online = self.store.group_online_character_ids(group_id).await?;
        if online.is_empty() {
            // Nobody remains online: retain the difference without running
            // an inference (design.md).
            self.store
                .announce_presence(group_id, &online, narration.generation)
                .await?;
            return Ok(());
        }
        self.narrate_presence_transition(group_id, &narration, &online, broadcasts)
            .await
    }

    /// Ask the group's world_gm to narrate the difference between the last
    /// announced set and the current online set, deliver it to the group,
    /// and persist the new announced set with that narration (design.md:
    /// "persist the new announced set with the narration").
    async fn narrate_presence_transition(
        self: &Arc<Self>,
        group_id: i64,
        narration: &PresenceNarration,
        online: &[i64],
        broadcasts: &GroupBroadcasts,
    ) -> Result<()> {
        let world_id = self.store.world_id_for_group(group_id).await?;
        let location_id = self.store.group_location_id(group_id).await?;
        let gm_history = self.store.group_gm_history(group_id).await?;
        let arrived_ids: Vec<i64> = online
            .iter()
            .copied()
            .filter(|id| !narration.last_announced_character_ids.contains(id))
            .collect();
        let left_ids: Vec<i64> = narration
            .last_announced_character_ids
            .iter()
            .copied()
            .filter(|id| !online.contains(id))
            .collect();
        let arrived = self.store.character_names(&arrived_ids).await?;
        let left = self.store.character_names(&left_ids).await?;
        let mut change = String::new();
        if !arrived.is_empty() {
            change.push_str(&format!("{} arrived. ", arrived.join(", ")));
        }
        if !left.is_empty() {
            change.push_str(&format!("{} left.", left.join(", ")));
        }
        let scene = serde_json::to_string(
            &self
                .store
                .gm_scene(world_id, gm_history.branch_id)
                .await
                .context("loading group GM scene for presence narration")?,
        )
        .context("serializing group GM presence scene")?;
        let role = Message::text(
            Role::System,
            format!(
                "You narrate play in this location for the characters present, listed in the scene packet. Describe only what the packet supports, addressing the characters as a group. The current scene packet is {scene}."
            ),
        );
        let request = Message::text(
            Role::User,
            format!("{change} Narrate this to the characters present."),
        );
        let sequence = self
            .store
            .begin_sequence(world_id, "presence transition")
            .await
            .context("starting presence narration sequence")?;
        let result: Result<()> = async {
            let call = CallContext::root(Some(sequence.id));
            let budget = Budget::new(self.limits);
            let settled = agent::complete_with_call_context(
                &self.store,
                &self.backend,
                &budget,
                agent::Turn {
                    agent_id: gm_history.agent_id,
                    branch_id: Some(gm_history.branch_id),
                    static_messages: &[role, request],
                    tools: &[],
                    sampling: self.sampling.clone(),
                    model: &self.model,
                },
                &call,
                |_| {},
                |_| {},
            )
            .await
            .context("asking group GM for presence narration")?;
            let Content::Text(text) = settled.response.content else {
                anyhow::bail!("group GM did not settle while narrating presence");
            };
            self.store
                .append_location_narration(world_id, location_id, settled.output_content_id)
                .await
                .context("storing presence narration")?;
            let _ = group_broadcast(broadcasts, group_id).await.send(text);
            self.store
                .announce_presence(group_id, online, narration.generation)
                .await
        }
        .await;
        if let Err(finish_error) = self
            .store
            .finish_sequence(
                sequence.id,
                result.as_ref().err().map(|error| format!("{error:#}")).as_deref(),
            )
            .await
        {
            tracing::error!(
                sequence_id = sequence.id,
                error = %format!("{finish_error:#}"),
                "finishing presence narration sequence"
            );
        }
        result
    }

    /// Resolve one player message through its membership-owned player agent.
    async fn resolve_player_message(
        self: &Arc<Self>,
        member: PlayerAgent,
        text: &str,
        broadcasts: &GroupBroadcasts,
    ) -> Result<Response> {
        let sequence = self
            .store
            .begin_sequence(member.world_id, "player message")
            .await
            .context("starting player message sequence")?;
        // Everything from here on must finish the sequence on every exit,
        // including a failure in setup before the agent ever runs - not only
        // agent::complete_with_call_context's own result - so the whole body
        // is captured as one Result rather than using `?` directly.
        let result: Result<Response> = async {
            self.store
                .append_message_with_sequence(
                    member.agent_id,
                    &Message::text(Role::User, text),
                    sequence.id,
                )
                .await
                .context("storing player message")?;
            let ready = self
                .store
                .is_ready_to_begin(&member)
                .await
                .context("checking character creation state")?;
            let budget = Budget::new(self.limits);
            let call = CallContext::root(Some(sequence.id));
            let tools = if ready {
                let location_id = self
                    .store
                    .player_location_id(&member)
                    .await
                    .context("resolving player location")?;
                self.action_tools(
                    member.clone(),
                    sequence.id,
                    location_id,
                    budget.clone(),
                    call.clone(),
                    Arc::clone(broadcasts),
                )
            } else {
                self.creation_tools(
                    member.clone(),
                    sequence.id,
                    budget.clone(),
                    call.clone(),
                    Arc::clone(broadcasts),
                )
            };
            let prompt = self
                .player_prompt(&member, ready)
                .await
                .context("building player prompt")?;
            let settled = agent::complete_with_call_context(
                &self.store,
                &self.backend,
                &budget,
                agent::Turn {
                    agent_id: member.agent_id,
                    branch_id: None,
                    static_messages: &[prompt],
                    tools: &tools,
                    sampling: self.sampling.clone(),
                    model: &self.model,
                },
                &call,
                |_| {},
                |_| {},
            )
            .await
            .context("resolving player agent")?;
            Ok(settled.response)
        }
        .await;
        // This is the root, player-triggered invocation: its return finishes
        // the whole sequence's wall time (design.md: "Finish sequence on
        // foreground invocation return"). Nested/deferred invocations finish
        // only their own row. A failure to finish the sequence itself is a
        // secondary bookkeeping problem, logged rather than allowed to
        // replace `result`'s own error as the reason this call failed.
        if let Err(finish_error) = self
            .store
            .finish_sequence(
                sequence.id,
                result.as_ref().err().map(|error| format!("{error:#}")).as_deref(),
            )
            .await
        {
            tracing::error!(
                sequence_id = sequence.id,
                error = %format!("{finish_error:#}"),
                "failed to finish player message sequence"
            );
        }
        let response = result?;
        ensure!(
            matches!(response.content, Content::Text(_)),
            "player agent did not settle after its action"
        );
        Ok(response)
    }

    /// The player agent starts the character-creation conversation when the
    /// browser opens a new Adventurer, before the player needs a special
    /// command. Its complete creation interface is visible from the start;
    /// the agent uses the conversation to wait for the player's choice to
    /// roll.
    async fn resolve_enter(
        self: &Arc<Self>,
        member: PlayerAgent,
        broadcasts: &GroupBroadcasts,
    ) -> Result<Option<Response>> {
        let ready = self
            .store
            .is_ready_to_begin(&member)
            .await
            .context("checking character creation state")?;
        if ready {
            return Ok(None);
        }
        if self.opening_complete(&member).await? {
            return Ok(None);
        }
        let sequence = self
            .store
            .begin_sequence(member.world_id, "player entered world")
            .await?;
        let budget = Budget::new(self.limits);
        let call = CallContext::root(Some(sequence.id));
        let tools = self.creation_tools(
            member.clone(),
            sequence.id,
            budget.clone(),
            call.clone(),
            Arc::clone(broadcasts),
        );
        // Everything from here on must finish the sequence on every exit,
        // including a failure building the prompt before the agent ever
        // runs - not only agent::complete_with_call_context's own result.
        let result: Result<Response> = async {
            let prompt = self
                .player_prompt(&member, false)
                .await
                .context("building player prompt")?;
            // This is the game event that starts the agent's first turn. It
            // is deliberately static context, not a player-visible or stored
            // chat message: the agent must speak first, while tool-capable
            // templates require a user message before their declarations.
            let entered = Message::text(
                Role::User,
                "The player has entered the world and has not chosen to roll yet. Speak directly to them.",
            );
            let settled = agent::complete_with_call_context(
                &self.store,
                &self.backend,
                &budget,
                agent::Turn {
                    agent_id: member.agent_id,
                    branch_id: None,
                    static_messages: &[prompt, entered],
                    tools: &tools,
                    sampling: self.sampling.clone(),
                    model: &self.model,
                },
                &call,
                |_| {},
                |_| {},
            )
            .await
            .context("opening player-agent conversation")?;
            Ok(settled.response)
        }
        .await;
        // A failure to finish the sequence itself is a secondary bookkeeping
        // problem, logged rather than allowed to replace `result`'s own
        // error as the reason this call failed.
        if let Err(finish_error) = self
            .store
            .finish_sequence(
                sequence.id,
                result.as_ref().err().map(|error| format!("{error:#}")).as_deref(),
            )
            .await
        {
            tracing::error!(
                sequence_id = sequence.id,
                error = %format!("{finish_error:#}"),
                "failed to finish player entered sequence"
            );
        }
        let response = result?;
        ensure!(
            matches!(response.content, Content::Text(_)),
            "player agent did not settle while opening the conversation"
        );
        Ok(Some(response))
    }

    /// Whether the player agent has already produced its stored opening reply.
    pub async fn opening_complete(&self, member: &PlayerAgent) -> Result<bool> {
        Ok(self
            .store
            .player_chat(member)
            .await
            .context("checking stored player-agent opening reply")?
            .iter()
            .any(|entry| entry.role == Role::Assistant))
    }

    /// Return the stored player-visible delta a browser missed after rendering
    /// its page snapshot and before its websocket became available.
    pub async fn player_chat_after(
        &self,
        member: &PlayerAgent,
        after_message_id: Option<i64>,
    ) -> Result<Vec<crate::store::PlayerChatEntry>> {
        self.store.player_chat_after(member, after_message_id).await
    }

    async fn player_prompt(&self, member: &PlayerAgent, ready: bool) -> Result<Message> {
        let text = if ready {
            let scene = serde_json::to_string(
                &self
                    .store
                    .player_scene(member)
                    .await
                    .context("loading player scene for prompt")?,
            )
            .context("serializing player scene for prompt")?;
            format!(
                "Guide the player through the current scene. Use a declared action tool when the player asks their character to look or speak. The following is the current player-visible scene, derived from the world state; do not invent items, people, or facts outside it:\n{scene}"
            )
        } else {
            "Speak first and guide the player through their Adventurer's Cairn character creation. Character creation requires Hit Protection first, then attributes. Introduce the next required step in short thematic language, and wait until the player is ready before using its roll tool. The roll tools make the real, recorded results. Immediately after attributes are rolled, call ready_to_begin; do not invent an opening scene or wait for another confirmation. Its result is the location GM's narration, already delivered verbatim to the player as a GM chat entry. Do not repeat or paraphrase it. Ask what the player wants to do next.".to_string()
        };
        Ok(Message::text(Role::System, text))
    }

    fn creation_tools(
        self: &Arc<Self>,
        member: PlayerAgent,
        sequence_id: i64,
        budget: Budget,
        call: CallContext,
        broadcasts: GroupBroadcasts,
    ) -> Vec<Tool> {
        let game = Arc::clone(self);
        let hp_member = member.clone();
        let hit_protection = Tool::new(
            ToolDefinition {
                name: "roll_hit_protection".to_string(),
                description: "Roll the Adventurer's starting 1d6 Hit Protection.".to_string(),
                schema: no_argument_schema(),
            },
            move |context| {
                let arguments = context.arguments.to_string();
                let tool_execution_id = context.tool_execution_id;
                let game = Arc::clone(&game);
                let member = hp_member.clone();
                Box::pin(async move {
                    if let Err(error) = parse_no_arguments(&arguments) {
                        return Ok(ToolOutcome::rejected(error));
                    }
                    Ok(ToolOutcome::Completed(format!(
                        "Hit Protection: {}",
                        game.store
                            .roll_hit_protection(&member, sequence_id, Some(tool_execution_id))
                            .await?
                    )))
                }) as ToolFuture
            },
        );
        let game = Arc::clone(self);
        let attributes_member = member.clone();
        let attributes = Tool::new(
            ToolDefinition {
                name: "roll_attributes".to_string(),
                description: "Roll the Adventurer's STR, DEX, and WIL as 3d6 each.".to_string(),
                schema: no_argument_schema(),
            },
            move |context| {
                let arguments = context.arguments.to_string();
                let tool_execution_id = context.tool_execution_id;
                let game = Arc::clone(&game);
                let member = attributes_member.clone();
                Box::pin(async move {
                    if let Err(error) = parse_no_arguments(&arguments) {
                        return Ok(ToolOutcome::rejected(error));
                    }
                    let (str, dex, wil) = game
                        .store
                        .roll_attributes(&member, sequence_id, Some(tool_execution_id))
                        .await?;
                    Ok(ToolOutcome::Completed(format!(
                        "Attributes: STR {str}, DEX {dex}, WIL {wil}"
                    )))
                }) as ToolFuture
            },
        );
        let game = Arc::clone(self);
        let ready_member = member;
        let ready_budget = budget;
        let ready_call = call;
        let ready_broadcasts = broadcasts;
        let ready = Tool::new(
            ToolDefinition {
                name: "ready_to_begin".to_string(),
                description: "Mark character creation complete after the required rolls."
                    .to_string(),
                schema: no_argument_schema(),
            },
            move |context| {
                let arguments = context.arguments.to_string();
                let tool_execution_id = context.tool_execution_id;
                let game = Arc::clone(&game);
                let member = ready_member.clone();
                let budget = ready_budget.clone();
                let call = ready_call.clone();
                let broadcasts = Arc::clone(&ready_broadcasts);
                Box::pin(async move {
                    if let Err(error) = parse_no_arguments(&arguments) {
                        return Ok(ToolOutcome::rejected(error));
                    }
                    game.store
                        .ready_to_begin(&member, sequence_id, Some(tool_execution_id))
                        .await?;
                    let narration = game
                        .opening_narration(&member, budget, call.child(tool_execution_id))
                        .await?;
                    let delivery_game = Arc::clone(&game);
                    Ok(ToolOutcome::CompletedAfter {
                        content: "The GM's narration reached the player as its own entry. Ask what they want to do next.".to_string(),
                        after_result: Box::pin(async move {
                            delivery_game.deliver_narration(&member, &narration, &broadcasts).await
                        }),
                    })
                }) as ToolFuture
            },
        );
        vec![hit_protection, attributes, ready]
    }

    fn action_tools(
        self: &Arc<Self>,
        member: PlayerAgent,
        sequence_id: i64,
        location_id: i64,
        budget: Budget,
        call: CallContext,
        broadcasts: GroupBroadcasts,
    ) -> Vec<Tool> {
        let game = Arc::clone(self);
        let presence_member = member.clone();
        let party_presence = Tool::new(
            ToolDefinition {
                name: "party_presence".to_string(),
                description: "List your current group's characters and whether each is online."
                    .to_string(),
                schema: no_argument_schema(),
            },
            move |context| {
                let arguments = context.arguments.to_string();
                let game = Arc::clone(&game);
                let member = presence_member.clone();
                Box::pin(async move {
                    if let Err(error) = parse_no_arguments(&arguments) {
                        return Ok(ToolOutcome::rejected(error));
                    }
                    let party = game.store.party_presence(member.character_id).await?;
                    Ok(ToolOutcome::Completed(
                        serde_json::to_string(&party).context("serializing party presence")?,
                    ))
                }) as ToolFuture
            },
        );
        vec![
            party_presence,
            self.action_tool(PlayerActionTool {
                member: member.clone(),
                sequence_id,
                definition: ToolDefinition {
                    name: "look".to_string(),
                    description: "Examine or ask about something in the current scene.".to_string(),
                    schema: serde_json::to_value(schema_for!(CharacterAction))
                        .expect("character action schema should serialize"),
                },
                validate: validate_character_action,
                location_id,
                budget: budget.clone(),
                call: call.clone(),
                broadcasts: Arc::clone(&broadcasts),
            }),
            self.action_tool(PlayerActionTool {
                member: member.clone(),
                sequence_id,
                definition: ToolDefinition {
                    name: "say".to_string(),
                    description: "Speak words your character says in the current scene.".to_string(),
                    schema: serde_json::to_value(schema_for!(CharacterAction))
                        .expect("character action schema should serialize"),
                },
                validate: validate_character_action,
                location_id,
                budget: budget.clone(),
                call: call.clone(),
                broadcasts: Arc::clone(&broadcasts),
            }),
            self.action_tool(PlayerActionTool {
                member,
                sequence_id,
                definition: ToolDefinition {
                    name: "take".to_string(),
                    description: "Take one named item currently visible in the scene. The GM must approve it before Rust transfers the item.".to_string(),
                    schema: serde_json::to_value(schema_for!(TakeAction))
                        .expect("take action schema should serialize"),
                },
                validate: validate_take_action,
                location_id,
                budget,
                call,
                broadcasts,
            }),
        ]
    }

    fn action_tool(self: &Arc<Self>, action: PlayerActionTool) -> Tool {
        let PlayerActionTool {
            member,
            sequence_id,
            definition,
            validate,
            location_id,
            budget,
            call,
            broadcasts,
        } = action;
        let tool_name = definition.name.clone();
        let game = Arc::clone(self);
        Tool::new(definition, move |context| {
            let arguments = context.arguments.to_string();
            let tool_execution_id = context.tool_execution_id;
            let game = Arc::clone(&game);
            let member = member.clone();
            let budget = budget.clone();
            let call = call.clone();
            let broadcasts = Arc::clone(&broadcasts);
            let tool_name = tool_name.clone();
            Box::pin(async move {
                if let Err(error) = validate(&arguments) {
                    return Ok(ToolOutcome::rejected(error));
                }
                let pending = game
                    .store
                    .create_pending_action(
                        &member,
                        sequence_id,
                        tool_execution_id,
                        &tool_name,
                        &arguments,
                    )
                    .await
                    .with_context(|| format!("submitting {tool_name} to location GM"))?;
                let narration = game.arbitrate(pending, location_id, budget, call).await?;
                let delivery_game = Arc::clone(&game);
                Ok(ToolOutcome::CompletedAfter {
                    content: "The GM's narration reached the player as its own entry. Respond to the player's next choice.".to_string(),
                    after_result: Box::pin(async move {
                        delivery_game.deliver_narration(&member, &narration, &broadcasts).await
                    }),
                })
            }) as ToolFuture
        })
    }

    /// The location GM, not the player agent, introduces a character who has
    /// just completed creation. The opening is a world transition, so it runs
    /// immediately from ready_to_begin instead of waiting for another player
    /// message to select normal scene tools.
    async fn opening_narration(
        self: &Arc<Self>,
        member: &PlayerAgent,
        budget: Budget,
        call: CallContext,
    ) -> Result<Narration> {
        let location_id = self.store.player_location_id(member).await?;
        let group_id = self.store.character_group_id(member.character_id).await?;
        let gm_history = self.store.player_group_history(member).await?;
        let scene = serde_json::to_string(
            &self
                .store
                .gm_scene(member.world_id, gm_history.branch_id)
                .await
                .context("loading group GM scene for the opening")?,
        )
        .context("serializing group GM opening scene")?;
        let role = Message::text(
            Role::System,
            format!(
                "You narrate play in this location for the characters present, listed in the scene packet. Describe only what the packet supports, addressing the characters as a group. The current scene packet is {scene}."
            ),
        );
        // The event that starts this turn: tool-capable chat templates need a
        // user message before the agent speaks.
        let request = Message::text(
            Role::User,
            "A player character has just begun play here. Narrate the scene, surroundings, and plot-relevant details the characters present can perceive.",
        );
        let settled = agent::complete_with_call_context(
            &self.store,
            &self.backend,
            &budget,
            agent::Turn {
                agent_id: gm_history.agent_id,
                branch_id: Some(gm_history.branch_id),
                static_messages: &[role, request],
                tools: &[],
                sampling: self.sampling.clone(),
                model: &self.model,
            },
            &call,
            |_| {},
            |_| {},
        )
        .await
        .context("asking group GM for opening narration")?;
        let Content::Text(narration) = settled.response.content else {
            anyhow::bail!("group GM did not settle while opening play");
        };
        Ok(Narration {
            location_id,
            group_id,
            text: narration,
            content_id: settled.output_content_id,
        })
    }

    async fn arbitrate(
        self: &Arc<Self>,
        pending: PendingAction,
        location_id: i64,
        budget: Budget,
        call: CallContext,
    ) -> Result<Narration> {
        let group_id = self
            .store
            .character_group_id(pending.character_id)
            .await
            .context("resolving acting character's group for arbitration")?;
        // approve_action and reject_action both resolve the same pending
        // action. resolve_pending_action's transaction is the sole authority
        // (design.md: "They cannot be approved twice") - no caller-side flag
        // mirrors that durable state. Each tool compares the decision it
        // requested against the decision resolve_pending_action actually
        // returns (freshly applied or already resolved by an earlier call in
        // this same turn): a match reports that recorded outcome, a mismatch
        // is an ordinary tool rejection describing the existing resolution,
        // not an error - a later rejection reason can never replace an
        // earlier approval's result, or vice versa.
        let game = Arc::clone(self);
        let action_id = pending.id;
        let approval_pending = pending.clone();
        let gm_tool = Tool::new(
            ToolDefinition {
                name: "approve_action".to_string(),
                description:
                    "Approve the presented action by its id after judging its visible arguments."
                        .to_string(),
                schema: serde_json::to_value(schema_for!(ApproveAction))
                    .expect("approval schema should serialize"),
            },
            move |context| {
                let arguments = context.arguments.to_string();
                let tool_execution_id = context.tool_execution_id;
                let game = Arc::clone(&game);
                Box::pin(async move {
                    let approval: ApproveAction =
                        match serde_json::from_str(&arguments).context("parsing action approval") {
                            Ok(approval) => approval,
                            Err(error) => return Ok(ToolOutcome::rejected(error)),
                        };
                    if approval.action_id != action_id {
                        return Ok(ToolOutcome::Rejected(format!(
                            "Tool arguments were rejected: GM approved action {} but was presented action {action_id}",
                            approval.action_id
                        )));
                    }
                    let resolved = game
                        .store
                        .resolve_pending_action(
                            approval_pending.gm_agent_id,
                            approval_pending.world_id,
                            action_id,
                            tool_execution_id,
                            "approved",
                        )
                        .await
                        .with_context(|| format!("approving action {action_id}"))?;
                    if !resolved.approved {
                        return Ok(ToolOutcome::Rejected(format!(
                            "Tool arguments were rejected: action {action_id} was already rejected, it cannot now be approved"
                        )));
                    }
                    Ok(ToolOutcome::Completed(format!(
                        "Action {action_id} approved."
                    )))
                }) as ToolFuture
            },
        );
        let game = Arc::clone(self);
        let reject_tool = Tool::new(
            ToolDefinition {
                name: "reject_action".to_string(),
                description:
                    "Reject the presented action by its id and give the player a concise reason."
                        .to_string(),
                schema: serde_json::to_value(schema_for!(RejectAction))
                    .expect("rejection schema should serialize"),
            },
            move |context| {
                let arguments = context.arguments.to_string();
                let tool_execution_id = context.tool_execution_id;
                let game = Arc::clone(&game);
                Box::pin(async move {
                    let rejection: RejectAction = match serde_json::from_str(&arguments)
                        .context("parsing action rejection")
                    {
                        Ok(rejection) => rejection,
                        Err(error) => return Ok(ToolOutcome::rejected(error)),
                    };
                    if rejection.action_id != action_id {
                        return Ok(ToolOutcome::Rejected(format!(
                            "Tool arguments were rejected: GM rejected action {} but was presented action {action_id}",
                            rejection.action_id
                        )));
                    }
                    if rejection.reason.trim().is_empty() {
                        return Ok(ToolOutcome::Rejected(
                            "Tool arguments were rejected: GM rejection reason is empty"
                                .to_string(),
                        ));
                    }
                    let resolved = game
                        .store
                        .resolve_pending_action(
                            pending.gm_agent_id,
                            pending.world_id,
                            action_id,
                            tool_execution_id,
                            &format!("rejected: {}", rejection.reason),
                        )
                        .await
                        .with_context(|| format!("rejecting action {action_id}"))?;
                    if resolved.approved {
                        return Ok(ToolOutcome::Rejected(format!(
                            "Tool arguments were rejected: action {action_id} was already approved, it cannot now be rejected"
                        )));
                    }
                    Ok(ToolOutcome::Completed(format!(
                        "Action {action_id} rejected: {}",
                        rejection.reason
                    )))
                }) as ToolFuture
            },
        );
        let split_tool = Tool::new(
            ToolDefinition {
                name: "split_group".to_string(),
                description: "Split this group into separate groups when characters go their own ways. Applies after every other call in this turn's batch has run, against the group the characters were in when this turn began; result is deferred and confirmed in the reply.".to_string(),
                schema: serde_json::to_value(schema_for!(SplitGroup))
                    .expect("split_group schema should serialize"),
            },
            move |context| {
                let arguments = context.arguments.to_string();
                Box::pin(async move {
                    let split: SplitGroup = match serde_json::from_str(&arguments)
                        .context("parsing split_group arguments")
                    {
                        Ok(split) => split,
                        Err(error) => return Ok(ToolOutcome::rejected(error)),
                    };
                    if split.partitions.is_empty() {
                        return Ok(ToolOutcome::Rejected(
                            "Tool arguments were rejected: split_group requires at least one partition"
                                .to_string(),
                        ));
                    }
                    Ok(ToolOutcome::PendingTopologyChange(TopologyChange::Split {
                        group_id: split.group_id,
                        partitions: split
                            .partitions
                            .into_iter()
                            .map(|partition| GroupPartition {
                                character_ids: partition.character_ids,
                                name: partition.name,
                                description: partition.description,
                            })
                            .collect(),
                        continue_with_character_id: split.continue_with_character_id,
                    }))
                }) as ToolFuture
            },
        );
        let merge_tool = Tool::new(
            ToolDefinition {
                name: "merge_groups".to_string(),
                description: "Merge separate groups at one location back into a single group when characters reunite. Applies after every other call in this turn's batch has run; result is deferred and confirmed in the reply.".to_string(),
                schema: serde_json::to_value(schema_for!(MergeGroups))
                    .expect("merge_groups schema should serialize"),
            },
            move |context| {
                let arguments = context.arguments.to_string();
                Box::pin(async move {
                    let merge: MergeGroups = match serde_json::from_str(&arguments)
                        .context("parsing merge_groups arguments")
                    {
                        Ok(merge) => merge,
                        Err(error) => return Ok(ToolOutcome::rejected(error)),
                    };
                    Ok(ToolOutcome::PendingTopologyChange(TopologyChange::Merge {
                        group_ids: merge.group_ids,
                        name: merge.name,
                        description: merge.description,
                    }))
                }) as ToolFuture
            },
        );
        let scene = serde_json::to_string(
            &self
                .store
                .gm_scene(pending.world_id, pending.gm_branch_id)
                .await
                .context("loading group GM scene for prompt")?,
        )
        .context("serializing group GM scene for prompt")?;
        let actor = self.store.character_name(pending.character_id).await?;
        let role = Message::text(
            Role::System,
            format!(
                "You arbitrate this location for the characters present, listed in the scene packet. Judge each action from that packet, call approve_action with its id if it can happen or reject it with a reason, then narrate the outcome to the characters present. If the characters split up or a separated group reunites here, call split_group or merge_groups; either takes effect after this turn's other calls, against the group as it stood when this turn began. The current scene packet is {scene}."
            ),
        );
        // The event that starts this turn: tool-capable chat templates need a
        // user message before the agent speaks.
        let request = Message::text(
            Role::User,
            format!(
                "{actor} takes action {action_id}: a `{}` request with arguments {}.",
                pending.tool, pending.args,
            ),
        );
        let settled = agent::complete_with_call_context(
            &self.store,
            &self.backend,
            &budget,
            agent::Turn {
                agent_id: pending.gm_agent_id,
                branch_id: Some(pending.gm_branch_id),
                static_messages: &[role, request],
                tools: &[gm_tool, reject_tool, split_tool, merge_tool],
                sampling: self.sampling.clone(),
                model: &self.model,
            },
            &call.child(pending.requesting_tool_execution_id),
            |_| {},
            |_| {},
        )
        .await
        .with_context(|| format!("arbitrating action {action_id}"))?;
        let Content::Text(narration) = settled.response.content else {
            anyhow::bail!("GM did not settle after action {action_id}");
        };
        ensure!(
            !self
                .store
                .has_pending_action(pending.world_id, action_id)
                .await
                .context("checking GM action resolution")?,
            "GM narrated action {action_id} without approving or rejecting it"
        );
        Ok(Narration {
            location_id,
            group_id,
            text: narration,
            content_id: settled.output_content_id,
        })
    }

    async fn deliver_narration(
        &self,
        member: &PlayerAgent,
        narration: &Narration,
        broadcasts: &GroupBroadcasts,
    ) -> Result<()> {
        self.store
            .append_location_narration(member.world_id, narration.location_id, narration.content_id)
            .await
            .context("storing GM narration before the player agent follows up")?;
        // A player may disconnect while the GM resolves. The narration remains
        // stored for reload; active viewers receive this exact same text.
        let _ = group_broadcast(broadcasts, narration.group_id)
            .await
            .send(narration.text.clone());
        Ok(())
    }
}

async fn group_broadcast(
    broadcasts: &Arc<Mutex<HashMap<i64, broadcast::Sender<String>>>>,
    group_id: i64,
) -> broadcast::Sender<String> {
    let mut broadcasts = broadcasts.lock().await;
    broadcasts
        .entry(group_id)
        .or_insert_with(|| broadcast::channel(64).0)
        .clone()
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct CharacterAction {
    /// What the character attempts or says.
    description: String,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct TakeAction {
    /// The exact item name from the current scene.
    item: String,
}

fn validate_character_action(arguments: &str) -> Result<()> {
    let action: CharacterAction =
        serde_json::from_str(arguments).context("parsing character action arguments")?;
    ensure!(
        !action.description.trim().is_empty(),
        "action description is empty"
    );
    Ok(())
}

fn validate_take_action(arguments: &str) -> Result<()> {
    let action: TakeAction =
        serde_json::from_str(arguments).context("parsing take action arguments")?;
    ensure!(!action.item.trim().is_empty(), "item name is empty");
    Ok(())
}

fn no_argument_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {},
    })
}

fn parse_no_arguments(arguments: &str) -> Result<()> {
    let arguments: serde_json::Map<String, serde_json::Value> =
        serde_json::from_str(arguments).context("parsing empty tool arguments")?;
    ensure!(
        arguments.is_empty(),
        "empty tool arguments must be an empty object"
    );
    Ok(())
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ApproveAction {
    /// The visible action id to approve.
    action_id: i64,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct RejectAction {
    /// The visible action id to reject.
    action_id: i64,
    /// Why the action cannot happen as requested.
    reason: String,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct SplitGroupPartition {
    /// The character IDs going to this resulting group. Every currently
    /// present character must appear in exactly one partition.
    character_ids: Vec<i64>,
    /// A short name for the resulting group.
    name: String,
    /// What distinguishes this resulting group's situation.
    description: String,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct SplitGroup {
    /// The group id being split - the id of the group currently arbitrating.
    group_id: i64,
    /// A complete, nonempty, disjoint partition of the group's current
    /// characters. Every character present must appear in exactly one
    /// partition; none may be omitted or duplicated.
    partitions: Vec<SplitGroupPartition>,
    /// Required when this call has no single triggering character (a GM
    /// event, not a player action): which resulting group the next turn in
    /// this same invocation continues as.
    #[serde(default)]
    continue_with_character_id: Option<i64>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct MergeGroups {
    /// At least two distinct group IDs at the same location to merge.
    group_ids: Vec<i64>,
    /// A short name for the resulting merged group.
    name: String,
    /// What distinguishes the merged group's situation.
    description: String,
}

#[cfg(test)]
mod tests {
    use std::{
        collections::VecDeque,
        sync::{
            Mutex,
            atomic::{AtomicUsize, Ordering},
        },
    };

    use super::*;
    use crate::{
        inference::InferenceScheduler,
        llm::{MessageContent, Response, ToolCall, Usage},
        mistralrs_backend::MistralRsBackend,
        scenario::Scenario,
        settings::Settings,
    };

    struct ScriptedBackend {
        responses: Mutex<VecDeque<Response>>,
        requests: Arc<Mutex<Vec<crate::llm::Request>>>,
    }

    impl ScriptedBackend {
        fn recording(
            responses: impl IntoIterator<Item = Response>,
        ) -> (Self, Arc<Mutex<Vec<crate::llm::Request>>>) {
            let requests = Arc::new(Mutex::new(vec![]));
            (
                Self {
                    responses: Mutex::new(responses.into_iter().collect()),
                    requests: Arc::clone(&requests),
                },
                requests,
            )
        }
    }

    impl Backend for ScriptedBackend {
        async fn complete(
            &self,
            request: crate::llm::Request,
            _on_token: impl FnMut(&str) + Send,
        ) -> Result<Response> {
            self.requests.lock().unwrap().push(request);
            self.responses
                .lock()
                .unwrap()
                .pop_front()
                .context("scripted backend received an unexpected inference")
        }
    }

    fn response(content: Content) -> Response {
        Response {
            content,
            reasoning: String::new(),
            usage: Usage {
                input_tokens: 1,
                output_tokens: 1,
            },
        }
    }

    struct DelayedBackend {
        started: Arc<tokio::sync::Notify>,
        release: Arc<tokio::sync::Notify>,
        requests: Arc<AtomicUsize>,
    }

    impl DelayedBackend {
        fn new() -> (
            Self,
            Arc<tokio::sync::Notify>,
            Arc<tokio::sync::Notify>,
            Arc<AtomicUsize>,
        ) {
            let started = Arc::new(tokio::sync::Notify::new());
            let release = Arc::new(tokio::sync::Notify::new());
            let requests = Arc::new(AtomicUsize::new(0));
            (
                Self {
                    started: Arc::clone(&started),
                    release: Arc::clone(&release),
                    requests: Arc::clone(&requests),
                },
                started,
                release,
                requests,
            )
        }
    }

    impl Backend for DelayedBackend {
        async fn complete(
            &self,
            _request: crate::llm::Request,
            _on_token: impl FnMut(&str) + Send,
        ) -> Result<Response> {
            self.requests.fetch_add(1, Ordering::SeqCst);
            self.started.notify_one();
            self.release.notified().await;
            Ok(response(Content::Text("Welcome.".into())))
        }
    }

    async fn blank_opening_game<B>(
        backend: B,
    ) -> (Arc<Game<B>>, Store, crate::store::TestDatabase, PlayerAgent)
    where
        B: Backend + Send + Sync + 'static,
    {
        opening_game(
            backend,
            "scripted".into(),
            Sampling {
                temperature: 0.0,
                ..Default::default()
            },
        )
        .await
    }

    async fn opening_game<B>(
        backend: B,
        model: String,
        sampling: Sampling,
    ) -> (Arc<Game<B>>, Store, crate::store::TestDatabase, PlayerAgent)
    where
        B: Backend + Send + Sync + 'static,
    {
        opening_game_with_limits(backend, model, sampling, Limits::default()).await
    }

    async fn device_test_game() -> (MistralRsBackend, String, Sampling, Limits) {
        let _ = tracing_subscriber::fmt()
            .with_env_filter("cairnworld=info")
            .with_test_writer()
            .try_init();
        let settings = Settings::for_device_test("dev-qwen35").expect("default.toml should load");
        let model = settings
            .model(None)
            .expect("default.toml should configure the device-test model");
        let backend = MistralRsBackend::load(
            &model.path,
            model.chat_template.as_deref(),
            model.source_model.as_deref(),
            settings.limits,
            false,
        )
        .await
        .expect("device-test model should load");
        let sampling = settings.sampling(&model);
        (backend, model.path, sampling, settings.limits)
    }

    async fn opening_game_with_limits<B>(
        backend: B,
        model: String,
        sampling: Sampling,
        limits: Limits,
    ) -> (Arc<Game<B>>, Store, crate::store::TestDatabase, PlayerAgent)
    where
        B: Backend + Send + Sync + 'static,
    {
        let db = crate::store::TestDatabase::new("opening-test");
        let store = Store::open(db.path()).await.unwrap();
        let owner = store
            .find_or_create_user("opening@example.test", "Opening")
            .await
            .unwrap();
        let scenario = Scenario::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/scenarios/bread_thief.json"
        ))
        .unwrap();
        let installed = store.install_scenario(&owner, &scenario).await.unwrap();
        let scheduler = InferenceScheduler::new(backend, limits).unwrap();
        let game = Arc::new(Game::new(
            store.clone(),
            scheduler.foreground(),
            limits,
            model,
            sampling,
        ));
        (game, store, db, installed.member)
    }

    #[tokio::test]
    async fn player_take_is_approved_by_its_location_gm_and_transferred_by_rust() {
        let db = crate::store::TestDatabase::new("game-test");
        let store = Store::open(db.path()).await.unwrap();
        let owner = store
            .find_or_create_user("warden@example.test", "Warden")
            .await
            .unwrap();
        let scenario = Scenario::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/scenarios/bread_thief.json"
        ))
        .unwrap();
        let installed = store.install_scenario(&owner, &scenario).await.unwrap();
        // Narration recipients are connected PCs (design.md's Membership and
        // presence); the player must be online to receive the GM's reply.
        store
            .mark_character_present(installed.member.character_id)
            .await
            .unwrap();
        let companion = store
            .create_player_character(owner.id, installed.world_id)
            .await
            .unwrap();
        store
            .mark_character_present(companion.character_id)
            .await
            .unwrap();
        let player_names = [
            store
                .player_scene(&installed.member)
                .await
                .unwrap()
                .character_name,
            store.player_scene(&companion).await.unwrap().character_name,
        ];
        let creation = store
            .begin_sequence(installed.world_id, "creation test")
            .await
            .unwrap();
        store
            .roll_hit_protection(&installed.member, creation.id, None)
            .await
            .unwrap();
        store
            .roll_attributes(&installed.member, creation.id, None)
            .await
            .unwrap();
        store
            .ready_to_begin(&installed.member, creation.id, None)
            .await
            .unwrap();
        let (scripted_backend, recorded_requests) = ScriptedBackend::recording([
            response(Content::ToolCalls(vec![ToolCall {
                id: "take-1".into(),
                name: "take".into(),
                arguments: r#"{"item":"flour sack"}"#.into(),
            }])),
            response(Content::ToolCalls(vec![ToolCall {
                id: "approve-1".into(),
                name: "approve_action".into(),
                arguments: r#"{"action_id":1}"#.into(),
            }])),
            response(Content::Text(
                "You lift the flour sack while Toma watches.".into(),
            )),
            response(Content::Text("What do you do next?".into())),
        ]);
        let backend = InferenceScheduler::new(scripted_backend, Limits::default()).unwrap();
        let game = Arc::new(Game::new(
            store.clone(),
            backend.foreground(),
            Limits::default(),
            "scripted".into(),
            Sampling {
                temperature: 0.0,
                ..Default::default()
            },
        ));
        let mut broadcasts = game.subscribe(&installed.member).await.unwrap();
        let (actions_before, pending_before) = store.action_counts().await.unwrap();
        game.player_message(installed.member.clone(), "I take the flour sack.")
            .await
            .unwrap();
        assert_eq!(
            tokio::time::timeout(std::time::Duration::from_secs(1), broadcasts.recv())
                .await
                .expect("GM narration was not broadcast")
                .expect("world broadcast channel closed"),
            "You lift the flour sack while Toma watches."
        );
        let (actions, pending) = store.action_counts().await.unwrap();
        assert_eq!(actions, actions_before + 1);
        assert_eq!(pending, pending_before);
        let chat = store.player_chat(&installed.member).await.unwrap();
        let narration = chat
            .iter()
            .position(|entry| {
                entry.role == Role::Narration
                    && entry.text == "You lift the flour sack while Toma watches."
            })
            .expect("the GM narration must be stored for a browser reload");
        assert!(
            matches!(
                chat.get(narration + 1),
                Some(entry) if entry.role == Role::Assistant && entry.text == "What do you do next?"
            ),
            "the player agent must follow already-delivered GM narration instead of preceding it"
        );
        assert!(
            store
                .player_chat(&companion)
                .await
                .unwrap()
                .iter()
                .any(|entry| entry.role == Role::Narration
                    && entry.text == "You lift the flour sack while Toma watches."),
            "the same GM narration must reach every co-located character's history"
        );
        let request = store
            .request_for_segments(
                installed.member.agent_id,
                &store
                    .live_segments(installed.member.agent_id, None)
                    .await
                    .unwrap(),
                Sampling {
                    temperature: 0.0,
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        let narration = request
            .messages
            .iter()
            .position(|message| {
                matches!(
                    &message.content,
                    MessageContent::Text(text) if text == "You lift the flour sack while Toma watches."
                )
            })
            .expect("the GM narration must be in the player agent's next context");
        assert!(
            matches!(
                request.messages.get(narration - 1).map(|message| &message.content),
                Some(MessageContent::ToolResult { content, .. }) if content.contains("narration reached the player as its own entry")
            ),
            "the player agent must receive its tool result before the GM narration"
        );
        let player_inference = store.reconstruct_inference(1).await.unwrap();
        let gm_inference = store.reconstruct_inference(2).await.unwrap();
        assert_eq!(player_inference.sequence_id, Some(creation.id + 1));
        assert_eq!(player_inference.parent_inference_id, None);
        assert_eq!(gm_inference.sequence_id, player_inference.sequence_id);
        assert_eq!(gm_inference.parent_inference_id, Some(player_inference.id));
        // A chat-entry inspect button needs the same sequence whether it
        // resolves through the player's own triggering message or through the
        // inference that produced a later reply.
        let player_line = chat
            .iter()
            .find(|entry| entry.text == "I take the flour sack.")
            .expect("the player's own message is stored");
        assert_eq!(player_line.sequence_id, player_inference.sequence_id);
        let assistant_line = chat
            .iter()
            .find(|entry| entry.text == "What do you do next?")
            .expect("the player agent's reply is stored");
        assert_eq!(assistant_line.sequence_id, player_inference.sequence_id);
        // The live socket handler re-fetches from a cursor after each turn
        // instead of reloading the page; that must resolve the same sequence
        // ids as the SSR path above, not the placeholder None a raw broadcast
        // string would carry.
        let live_entries = game
            .player_chat_after(&installed.member, Some(player_line.id - 1))
            .await
            .unwrap();
        assert_eq!(
            live_entries
                .iter()
                .find(|entry| entry.text == "I take the flour sack.")
                .expect("the live cursor sees the player's own message")
                .sequence_id,
            player_inference.sequence_id
        );
        assert_eq!(
            live_entries
                .iter()
                .find(|entry| entry.text == "What do you do next?")
                .expect("the live cursor sees the player agent's reply")
                .sequence_id,
            player_inference.sequence_id
        );
        {
            let requests = recorded_requests.lock().unwrap();
            let gm_packet = requests
                .iter()
                .flat_map(|request| request.messages.iter())
                .find_map(|message| match &message.content {
                    MessageContent::Text(text)
                        if text.starts_with("You arbitrate this location") =>
                    {
                        Some(text)
                    }
                    _ => None,
                })
                .expect("the GM arbitration must receive its actual location packet");
            for name in &player_names {
                assert!(
                    gm_packet.contains(&format!("\"name\":\"{name}\"")),
                    "the GM packet must name every player character in its location"
                );
            }
            let acting = &player_names[0];
            assert!(
                requests
                    .iter()
                    .flat_map(|request| request.messages.iter())
                    .any(|message| {
                        matches!(&message.content, MessageContent::Text(text)
                        if text.starts_with(&format!("{acting} takes action")))
                    }),
                "the GM must be told which character took the action"
            );
        }

        // Provenance: the take action links to the player call that submitted
        // it and the GM call that approved it. The sequence tree walks from the
        // player message trigger through both inferences.
        let take_actions = store.inference_actions(player_inference.id).await.unwrap();
        assert_eq!(
            take_actions.len(),
            1,
            "the player turn performed one action"
        );
        assert_eq!(take_actions[0].tool, "take");
        assert_eq!(take_actions[0].call_id.as_deref(), Some("take-1"));
        assert_eq!(
            take_actions[0].approval_inference_id,
            Some(gm_inference.id),
            "the recorded action names the GM inference that approved it"
        );
        assert!(
            store
                .inference_actions(gm_inference.id)
                .await
                .unwrap()
                .is_empty(),
            "the GM inference approved an action but performed none of its own"
        );
        let sequence_inferences = store
            .sequence_inference_ids(player_inference.sequence_id.unwrap())
            .await
            .unwrap();
        assert_eq!(
            sequence_inferences.first().copied(),
            Some(player_inference.id),
            "the player message inference opens its sequence"
        );
        assert!(
            sequence_inferences.contains(&gm_inference.id),
            "the GM inference belongs to the same sequence"
        );
        assert!(
            sequence_inferences.windows(2).all(|pair| pair[0] < pair[1]),
            "sequence inferences are returned in recorded order"
        );
        let gm_messages = store.inference_message_ids(gm_inference.id).await.unwrap();
        assert_eq!(
            gm_messages
                .iter()
                .map(|message| message.role.clone())
                .collect::<Vec<_>>(),
            vec![Role::Assistant, Role::Tool],
            "the GM inference's approve_action call and its result are recorded in the GM's history"
        );
        assert!(
            gm_messages[1].answers_tool_execution_id.is_some(),
            "the GM's tool result names the exact approve_action call it answers"
        );
    }

    /// A GM turn cannot resolve the same pending action twice with different
    /// outcomes: calling approve_action then reject_action for the same
    /// action_id in one batch must report the conflicting call as an
    /// ordinary tool rejection, not silently succeed or error out - and the
    /// first call's approval (and its state mutation) stands.
    #[tokio::test]
    async fn a_gm_turn_cannot_both_approve_and_reject_the_same_action() {
        let db = crate::store::TestDatabase::new("game-test");
        let store = Store::open(db.path()).await.unwrap();
        let owner = store
            .find_or_create_user("warden@example.test", "Warden")
            .await
            .unwrap();
        let scenario = Scenario::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/scenarios/bread_thief.json"
        ))
        .unwrap();
        let installed = store.install_scenario(&owner, &scenario).await.unwrap();
        // Narration recipients are connected PCs (design.md's Membership and
        // presence); the player must be online to receive the GM's reply.
        store
            .mark_character_present(installed.member.character_id)
            .await
            .unwrap();
        let creation = store
            .begin_sequence(installed.world_id, "creation test")
            .await
            .unwrap();
        store
            .roll_hit_protection(&installed.member, creation.id, None)
            .await
            .unwrap();
        store
            .roll_attributes(&installed.member, creation.id, None)
            .await
            .unwrap();
        store
            .ready_to_begin(&installed.member, creation.id, None)
            .await
            .unwrap();
        let (scripted_backend, recorded_requests) = ScriptedBackend::recording([
            response(Content::ToolCalls(vec![ToolCall {
                id: "take-1".into(),
                name: "take".into(),
                arguments: r#"{"item":"flour sack"}"#.into(),
            }])),
            // Both calls answer the same action_id in one batch; the GM
            // model reversing its own decision mid-turn is exactly the case
            // this guards - not a network retry of an identical call.
            response(Content::ToolCalls(vec![
                ToolCall {
                    id: "approve-1".into(),
                    name: "approve_action".into(),
                    arguments: r#"{"action_id":1}"#.into(),
                },
                ToolCall {
                    id: "reject-1".into(),
                    name: "reject_action".into(),
                    arguments: r#"{"action_id":1,"reason":"changed my mind"}"#.into(),
                },
            ])),
            response(Content::Text(
                "You lift the flour sack while Toma watches.".into(),
            )),
            response(Content::Text("What do you do next?".into())),
        ]);
        let backend = InferenceScheduler::new(scripted_backend, Limits::default()).unwrap();
        let game = Arc::new(Game::new(
            store.clone(),
            backend.foreground(),
            Limits::default(),
            "scripted".into(),
            Sampling {
                temperature: 0.0,
                ..Default::default()
            },
        ));
        let (actions_before, pending_before) = store.action_counts().await.unwrap();
        game.player_message(installed.member.clone(), "I take the flour sack.")
            .await
            .unwrap();
        let (actions, pending) = store.action_counts().await.unwrap();
        assert_eq!(
            actions,
            actions_before + 1,
            "exactly one resolution must be recorded, not two conflicting ones"
        );
        assert_eq!(pending, pending_before);
        let holders: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM item_character JOIN item ON item.id = item_character.item_id \
             WHERE item.world_id = ? AND item.name = 'flour sack'",
        )
        .bind(installed.world_id)
        .fetch_one(&store.pool())
        .await
        .unwrap();
        assert_eq!(
            holders, 1,
            "the first call's approval must stand: the item is transferred exactly once"
        );
        let requests = recorded_requests.lock().unwrap();
        let reject_result = requests
            .iter()
            .flat_map(|request| request.messages.iter())
            .find_map(|message| match &message.content {
                MessageContent::ToolResult { tool_call_id, content } if tool_call_id == "reject-1" => {
                    Some(content.clone())
                }
                _ => None,
            })
            .expect("the GM's own next turn must see reject-1's tool result");
        assert!(
            reject_result.contains("already approved") && reject_result.contains("cannot now be rejected"),
            "the conflicting reject_action call must be reported as an ordinary tool rejection naming \
             the existing resolution, not silently succeed or surface a generic error: {reject_result}"
        );
    }

    #[tokio::test]
    async fn split_group_applies_after_its_batch_and_sibling_calls_see_the_pre_split_group() {
        let db = crate::store::TestDatabase::new("game-test");
        let store = Store::open(db.path()).await.unwrap();
        let owner = store
            .find_or_create_user("warden@example.test", "Warden")
            .await
            .unwrap();
        let scenario = Scenario::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/scenarios/bread_thief.json"
        ))
        .unwrap();
        let installed = store.install_scenario(&owner, &scenario).await.unwrap();
        store
            .mark_character_present(installed.member.character_id)
            .await
            .unwrap();
        let original_group_id = store
            .character_group_id(installed.member.character_id)
            .await
            .unwrap();
        let toma_id: i64 = sqlx::query_scalar("SELECT id FROM character WHERE name = 'Toma'")
            .fetch_one(&store.pool())
            .await
            .unwrap();
        let mara_id: i64 = sqlx::query_scalar("SELECT id FROM character WHERE name = 'Mara'")
            .fetch_one(&store.pool())
            .await
            .unwrap();
        let creation = store
            .begin_sequence(installed.world_id, "creation test")
            .await
            .unwrap();
        store
            .roll_hit_protection(&installed.member, creation.id, None)
            .await
            .unwrap();
        store
            .roll_attributes(&installed.member, creation.id, None)
            .await
            .unwrap();
        store
            .ready_to_begin(&installed.member, creation.id, None)
            .await
            .unwrap();
        let split_args = serde_json::json!({
            "group_id": original_group_id,
            "partitions": [
                {"character_ids": [installed.member.character_id, mara_id], "name": "with Mara", "description": "outside the hut"},
                {"character_ids": [toma_id], "name": "Toma alone", "description": "inside the hut"},
            ],
        })
        .to_string();
        let (scripted_backend, recorded_requests) = ScriptedBackend::recording([
            response(Content::ToolCalls(vec![ToolCall {
                id: "look-1".into(),
                name: "look".into(),
                arguments: r#"{"description":"the room"}"#.into(),
            }])),
            // approve_action and split_group in one batch: split_group must
            // not retarget approve_action's own group mid-batch (design.md's
            // "Tool boundary": other calls execute against the pre-transition
            // group).
            response(Content::ToolCalls(vec![
                ToolCall {
                    id: "approve-1".into(),
                    name: "approve_action".into(),
                    arguments: r#"{"action_id":1}"#.into(),
                },
                ToolCall {
                    id: "split-1".into(),
                    name: "split_group".into(),
                    arguments: split_args,
                },
            ])),
            response(Content::Text("Toma watches as you step outside with Mara.".into())),
            response(Content::Text("What do you do next?".into())),
        ]);
        let backend = InferenceScheduler::new(scripted_backend, Limits::default()).unwrap();
        let game = Arc::new(Game::new(
            store.clone(),
            backend.foreground(),
            Limits::default(),
            "scripted".into(),
            Sampling {
                temperature: 0.0,
                ..Default::default()
            },
        ));

        let (actions_before, pending_before) = store.action_counts().await.unwrap();
        game.player_message(installed.member.clone(), "I look around, then we split up.")
            .await
            .unwrap();

        let split_result = {
            let requests = recorded_requests.lock().unwrap();
            requests
                .iter()
                .flat_map(|request| request.messages.iter())
                .find_map(|message| match &message.content {
                    MessageContent::ToolResult { tool_call_id, content } if tool_call_id == "split-1" => {
                        Some(content.clone())
                    }
                    _ => None,
                })
                .expect("the GM's own next turn must see split-1's tool result")
        };
        assert!(
            split_result.contains("split into groups"),
            "split_group must report success: {split_result}"
        );

        let adventurer_group = store
            .character_group_id(installed.member.character_id)
            .await
            .unwrap();
        let mara_group = store.character_group_id(mara_id).await.unwrap();
        let toma_group = store.character_group_id(toma_id).await.unwrap();
        assert_eq!(
            adventurer_group, mara_group,
            "the partition naming both must land in the same resulting group"
        );
        assert_ne!(
            adventurer_group, toma_group,
            "the two partitions must be different resulting groups"
        );
        assert_ne!(
            adventurer_group, original_group_id,
            "a split retires the source group rather than reusing its id"
        );
        let (retired_at,): (Option<String>,) =
            sqlx::query_as("SELECT retired_at FROM \"group\" WHERE id = ?")
                .bind(original_group_id)
                .fetch_one(&store.pool())
                .await
                .unwrap();
        assert!(retired_at.is_some(), "the source group must retire");
        let parent_branch_id: i64 = sqlx::query_scalar("SELECT branch_id FROM \"group\" WHERE id = ?")
            .bind(original_group_id)
            .fetch_one(&store.pool())
            .await
            .unwrap();
        let new_branch_id: i64 = sqlx::query_scalar("SELECT branch_id FROM \"group\" WHERE id = ?")
            .bind(adventurer_group)
            .fetch_one(&store.pool())
            .await
            .unwrap();
        let linked_parent: i64 = sqlx::query_scalar(
            "SELECT parent_branch_id FROM history_parent WHERE branch_id = ?",
        )
        .bind(new_branch_id)
        .fetch_one(&store.pool())
        .await
        .unwrap();
        assert_eq!(
            linked_parent, parent_branch_id,
            "each resulting group's branch must be parented on the source's branch"
        );
        // approve_action's own effect (the item-approval action row) proves
        // its call in the same batch as split_group still ran and committed
        // normally, rather than being disrupted by the deferred topology
        // change alongside it.
        let (actions, pending) = store.action_counts().await.unwrap();
        assert_eq!(
            actions,
            actions_before + 1,
            "approve_action's own resolution must be recorded"
        );
        assert_eq!(pending, pending_before);
    }

    /// Drives a real split_group then merge_groups through the actual GM
    /// tool-call machinery (a `ScriptedBackend` stands in for the model's own
    /// reasoning, exactly as the rest of the tool-call surface is tested) and
    /// returns the resulting state: the store/fixture, the player member,
    /// and the two source groups plus the merged group/branch the merge
    /// produced. Shared by the structural merge test and the real-model
    /// context-assembly test, which both need the identical merged branch
    /// DAG and differ only in what they do with it afterward.
    async fn split_then_merge_groups() -> (
        Store,
        crate::store::TestDatabase,
        PlayerAgent,
        i64,
        i64,
        i64,
        i64,
    ) {
        let db = crate::store::TestDatabase::new("game-test");
        let store = Store::open(db.path()).await.unwrap();
        let owner = store
            .find_or_create_user("warden@example.test", "Warden")
            .await
            .unwrap();
        let scenario = Scenario::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/scenarios/bread_thief.json"
        ))
        .unwrap();
        let installed = store.install_scenario(&owner, &scenario).await.unwrap();
        store
            .mark_character_present(installed.member.character_id)
            .await
            .unwrap();
        let original_group_id = store
            .character_group_id(installed.member.character_id)
            .await
            .unwrap();
        let toma_id: i64 = sqlx::query_scalar("SELECT id FROM character WHERE name = 'Toma'")
            .fetch_one(&store.pool())
            .await
            .unwrap();
        let mara_id: i64 = sqlx::query_scalar("SELECT id FROM character WHERE name = 'Mara'")
            .fetch_one(&store.pool())
            .await
            .unwrap();
        let creation = store
            .begin_sequence(installed.world_id, "creation test")
            .await
            .unwrap();
        store
            .roll_hit_protection(&installed.member, creation.id, None)
            .await
            .unwrap();
        store
            .roll_attributes(&installed.member, creation.id, None)
            .await
            .unwrap();
        store
            .ready_to_begin(&installed.member, creation.id, None)
            .await
            .unwrap();

        let split_args = serde_json::json!({
            "group_id": original_group_id,
            "partitions": [
                {"character_ids": [installed.member.character_id, mara_id], "name": "with Mara", "description": "outside"},
                {"character_ids": [toma_id], "name": "Toma alone", "description": "inside"},
            ],
        })
        .to_string();
        let (split_backend, _split_requests) = ScriptedBackend::recording([
            response(Content::ToolCalls(vec![ToolCall {
                id: "look-1".into(),
                name: "look".into(),
                arguments: r#"{"description":"the room"}"#.into(),
            }])),
            response(Content::ToolCalls(vec![
                ToolCall {
                    id: "approve-1".into(),
                    name: "approve_action".into(),
                    arguments: r#"{"action_id":1}"#.into(),
                },
                ToolCall {
                    id: "split-1".into(),
                    name: "split_group".into(),
                    arguments: split_args,
                },
            ])),
            response(Content::Text("You step outside with Mara.".into())),
            response(Content::Text("What do you do next?".into())),
        ]);
        let split_backend = InferenceScheduler::new(split_backend, Limits::default()).unwrap();
        let split_game = Arc::new(Game::new(
            store.clone(),
            split_backend.foreground(),
            Limits::default(),
            "scripted".into(),
            Sampling {
                temperature: 0.0,
                ..Default::default()
            },
        ));
        split_game
            .player_message(installed.member.clone(), "I look, then we split up.")
            .await
            .unwrap();

        let adventurer_group = store
            .character_group_id(installed.member.character_id)
            .await
            .unwrap();
        let toma_group = store.character_group_id(toma_id).await.unwrap();
        assert_ne!(adventurer_group, toma_group, "the split must have taken effect");

        let merge_args = serde_json::json!({
            "group_ids": [adventurer_group, toma_group],
            "name": "reunited",
            "description": "back together at the hut",
        })
        .to_string();
        let (merge_backend, merge_requests) = ScriptedBackend::recording([
            response(Content::ToolCalls(vec![ToolCall {
                id: "look-2".into(),
                name: "look".into(),
                arguments: r#"{"description":"Toma"}"#.into(),
            }])),
            response(Content::ToolCalls(vec![
                ToolCall {
                    id: "approve-2".into(),
                    name: "approve_action".into(),
                    arguments: r#"{"action_id":2}"#.into(),
                },
                ToolCall {
                    id: "merge-1".into(),
                    name: "merge_groups".into(),
                    arguments: merge_args,
                },
            ])),
            response(Content::Text("You and Mara rejoin Toma.".into())),
            response(Content::Text("What do you do next?".into())),
        ]);
        let merge_backend = InferenceScheduler::new(merge_backend, Limits::default()).unwrap();
        let merge_game = Arc::new(Game::new(
            store.clone(),
            merge_backend.foreground(),
            Limits::default(),
            "scripted".into(),
            Sampling {
                temperature: 0.0,
                ..Default::default()
            },
        ));
        merge_game
            .player_message(installed.member.clone(), "I look at Toma, then we regroup.")
            .await
            .unwrap();

        let merge_result = {
            let requests = merge_requests.lock().unwrap();
            requests
                .iter()
                .flat_map(|request| request.messages.iter())
                .find_map(|message| match &message.content {
                    MessageContent::ToolResult { tool_call_id, content } if tool_call_id == "merge-1" => {
                        Some(content.clone())
                    }
                    _ => None,
                })
                .expect("the GM's own next turn must see merge-1's tool result")
        };
        assert!(merge_result.contains("merged into group"), "{merge_result}");

        let merged_adventurer = store
            .character_group_id(installed.member.character_id)
            .await
            .unwrap();
        let merged_toma = store.character_group_id(toma_id).await.unwrap();
        let merged_mara = store.character_group_id(mara_id).await.unwrap();
        assert_eq!(merged_adventurer, merged_toma, "the merge must reunite all characters");
        assert_eq!(merged_adventurer, merged_mara);

        (
            store,
            db,
            installed.member,
            adventurer_group,
            toma_group,
            merged_adventurer,
            original_group_id,
        )
    }

    #[tokio::test]
    async fn merge_groups_unions_membership_and_bounds_both_source_branches() {
        let (store, _db, _member, adventurer_group, toma_group, merged_adventurer, _original_group_id) =
            split_then_merge_groups().await;
        assert_ne!(merged_adventurer, adventurer_group, "merge retires the sources, not reuses one");
        assert_ne!(merged_adventurer, toma_group);

        for source in [adventurer_group, toma_group] {
            let (retired_at,): (Option<String>,) =
                sqlx::query_as("SELECT retired_at FROM \"group\" WHERE id = ?")
                    .bind(source)
                    .fetch_one(&store.pool())
                    .await
                    .unwrap();
            assert!(retired_at.is_some(), "source group {source} must retire");
        }
        let merged_branch: i64 = sqlx::query_scalar("SELECT branch_id FROM \"group\" WHERE id = ?")
            .bind(merged_adventurer)
            .fetch_one(&store.pool())
            .await
            .unwrap();
        let mut parents: Vec<i64> = sqlx::query_as::<_, (i64,)>(
            "SELECT parent_branch_id FROM history_parent WHERE branch_id = ? ORDER BY ordinal",
        )
        .bind(merged_branch)
        .fetch_all(&store.pool())
        .await
        .unwrap()
        .into_iter()
        .map(|(id,)| id)
        .collect();
        assert_eq!(parents.len(), 2, "the merged branch must bound both source branches");
        parents.sort_unstable();
        let mut source_branches: Vec<i64> = vec![
            sqlx::query_scalar::<_, i64>("SELECT branch_id FROM \"group\" WHERE id = ?")
                .bind(adventurer_group)
                .fetch_one(&store.pool())
                .await
                .unwrap(),
            sqlx::query_scalar::<_, i64>("SELECT branch_id FROM \"group\" WHERE id = ?")
                .bind(toma_group)
                .fetch_one(&store.pool())
                .await
                .unwrap(),
        ];
        source_branches.sort_unstable();
        assert_eq!(parents, source_branches);
    }

    /// The new part beyond `merge_groups_unions_membership_and_bounds_both_source_branches`
    /// (which already proves the merge structurally): attaching a REAL model
    /// to the resulting branch DAG for its next turn, so
    /// live_segments/request_for_segments must actually assemble a valid
    /// context (summary + uncovered tail per branch, as bounded history
    /// ranges' step 3 changed) that a real chat template accepts - not just
    /// that the scripted test backend received whatever `Vec<Segment>` was
    /// handed to it.
    #[tokio::test]
    #[ignore = "device test: needs a CUDA GPU and the models/ GGUF from default.toml"]
    async fn real_model_assembles_context_after_a_split_and_merge() {
        let (store, _db, member, ..) = split_then_merge_groups().await;
        let (real_backend, model_path, sampling, _limits) = device_test_game().await;
        // This test's own point is context assembly succeeding across
        // however many real inferences one player turn takes on the merged
        // branch DAG, not constraining the model's agentic tool-use choices
        // (Look/Say/etc, all legitimate) - a small local model reliably
        // takes several tool round trips before settling on a reply, so
        // max_inferences_per_chat (a Game-level per-chat budget, independent
        // of the backend's own loaded max_context_tokens/KV cache) needs
        // real gameplay's headroom, not the tight budget other tests use to
        // assert a specific short interaction.
        let limits = Limits {
            max_inferences_per_chat: 24,
            ..Limits::default()
        };
        let real_game = Arc::new(Game::new(
            store.clone(),
            InferenceScheduler::new(real_backend, Limits::default())
                .unwrap()
                .foreground(),
            limits,
            model_path,
            sampling,
        ));
        real_game
            .player_message(member.clone(), "I say: \"Good to have us all back together.\"")
            .await
            .expect("a real model turn must succeed on the merged branch DAG");
        let player_chat = store
            .player_chat(&member)
            .await
            .expect("player chat should load after the real turn");
        assert!(
            player_chat
                .iter()
                .any(|entry| entry.role == Role::Assistant && !entry.text.trim().is_empty()),
            "the real model turn must leave a player-visible assistant reply"
        );
    }

    #[tokio::test]
    async fn an_incomplete_split_partition_is_rejected_and_changes_nothing() {
        let db = crate::store::TestDatabase::new("game-test");
        let store = Store::open(db.path()).await.unwrap();
        let owner = store
            .find_or_create_user("warden@example.test", "Warden")
            .await
            .unwrap();
        let scenario = Scenario::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/scenarios/bread_thief.json"
        ))
        .unwrap();
        let installed = store.install_scenario(&owner, &scenario).await.unwrap();
        store
            .mark_character_present(installed.member.character_id)
            .await
            .unwrap();
        let original_group_id = store
            .character_group_id(installed.member.character_id)
            .await
            .unwrap();
        let creation = store
            .begin_sequence(installed.world_id, "creation test")
            .await
            .unwrap();
        store
            .roll_hit_protection(&installed.member, creation.id, None)
            .await
            .unwrap();
        store
            .roll_attributes(&installed.member, creation.id, None)
            .await
            .unwrap();
        store
            .ready_to_begin(&installed.member, creation.id, None)
            .await
            .unwrap();
        // Omits Mara and Toma entirely - not a complete partition of the
        // group's current membership.
        let incomplete_split = serde_json::json!({
            "group_id": original_group_id,
            "partitions": [
                {"character_ids": [installed.member.character_id], "name": "alone", "description": "outside"},
            ],
        })
        .to_string();
        let (backend, recorded_requests) = ScriptedBackend::recording([
            response(Content::ToolCalls(vec![ToolCall {
                id: "look-1".into(),
                name: "look".into(),
                arguments: r#"{"description":"the room"}"#.into(),
            }])),
            response(Content::ToolCalls(vec![
                ToolCall {
                    id: "approve-1".into(),
                    name: "approve_action".into(),
                    arguments: r#"{"action_id":1}"#.into(),
                },
                ToolCall {
                    id: "split-1".into(),
                    name: "split_group".into(),
                    arguments: incomplete_split,
                },
            ])),
            response(Content::Text("You step outside alone.".into())),
            response(Content::Text("What do you do next?".into())),
        ]);
        let backend = InferenceScheduler::new(backend, Limits::default()).unwrap();
        let game = Arc::new(Game::new(
            store.clone(),
            backend.foreground(),
            Limits::default(),
            "scripted".into(),
            Sampling {
                temperature: 0.0,
                ..Default::default()
            },
        ));
        game.player_message(installed.member.clone(), "I look around, then leave alone.")
            .await
            .unwrap();

        let split_result = {
            let requests = recorded_requests.lock().unwrap();
            requests
                .iter()
                .flat_map(|request| request.messages.iter())
                .find_map(|message| match &message.content {
                    MessageContent::ToolResult { tool_call_id, content } if tool_call_id == "split-1" => {
                        Some(content.clone())
                    }
                    _ => None,
                })
                .expect("the GM's own next turn must see split-1's tool result")
        };
        assert!(
            split_result.contains("Tool arguments were rejected"),
            "an incomplete partition must be rejected: {split_result}"
        );
        let unchanged_group = store
            .character_group_id(installed.member.character_id)
            .await
            .unwrap();
        assert_eq!(
            unchanged_group, original_group_id,
            "a rejected split must leave membership unchanged"
        );
        let (retired_at,): (Option<String>,) =
            sqlx::query_as("SELECT retired_at FROM \"group\" WHERE id = ?")
                .bind(original_group_id)
                .fetch_one(&store.pool())
                .await
                .unwrap();
        assert!(retired_at.is_none(), "a rejected split must not retire the group");
    }

    #[tokio::test]
    async fn a_second_topology_proposal_in_one_batch_against_an_already_split_group_is_rejected() {
        let db = crate::store::TestDatabase::new("game-test");
        let store = Store::open(db.path()).await.unwrap();
        let owner = store
            .find_or_create_user("warden@example.test", "Warden")
            .await
            .unwrap();
        let scenario = Scenario::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/scenarios/bread_thief.json"
        ))
        .unwrap();
        let installed = store.install_scenario(&owner, &scenario).await.unwrap();
        store
            .mark_character_present(installed.member.character_id)
            .await
            .unwrap();
        let original_group_id = store
            .character_group_id(installed.member.character_id)
            .await
            .unwrap();
        let toma_id: i64 = sqlx::query_scalar("SELECT id FROM character WHERE name = 'Toma'")
            .fetch_one(&store.pool())
            .await
            .unwrap();
        let mara_id: i64 = sqlx::query_scalar("SELECT id FROM character WHERE name = 'Mara'")
            .fetch_one(&store.pool())
            .await
            .unwrap();
        let creation = store
            .begin_sequence(installed.world_id, "creation test")
            .await
            .unwrap();
        store
            .roll_hit_protection(&installed.member, creation.id, None)
            .await
            .unwrap();
        store
            .roll_attributes(&installed.member, creation.id, None)
            .await
            .unwrap();
        store
            .ready_to_begin(&installed.member, creation.id, None)
            .await
            .unwrap();
        // Two split_group calls against the same source group in one batch:
        // the first retires it, so the second (still describing the
        // now-stale original membership) must be rejected rather than
        // applied against a group that no longer exists as an active split
        // source (design.md: "A stale second topology proposal gets a
        // rejected result").
        let first_split = serde_json::json!({
            "group_id": original_group_id,
            "partitions": [
                {"character_ids": [installed.member.character_id, mara_id], "name": "with Mara", "description": "outside"},
                {"character_ids": [toma_id], "name": "Toma alone", "description": "inside"},
            ],
        })
        .to_string();
        let second_split = serde_json::json!({
            "group_id": original_group_id,
            "partitions": [
                {"character_ids": [installed.member.character_id], "name": "alone", "description": "elsewhere"},
                {"character_ids": [mara_id, toma_id], "name": "the other two", "description": "elsewhere"},
            ],
        })
        .to_string();
        let (backend, recorded_requests) = ScriptedBackend::recording([
            response(Content::ToolCalls(vec![ToolCall {
                id: "look-1".into(),
                name: "look".into(),
                arguments: r#"{"description":"the room"}"#.into(),
            }])),
            response(Content::ToolCalls(vec![
                ToolCall {
                    id: "approve-1".into(),
                    name: "approve_action".into(),
                    arguments: r#"{"action_id":1}"#.into(),
                },
                ToolCall {
                    id: "split-1".into(),
                    name: "split_group".into(),
                    arguments: first_split,
                },
                ToolCall {
                    id: "split-2".into(),
                    name: "split_group".into(),
                    arguments: second_split,
                },
            ])),
            response(Content::Text("You step outside with Mara.".into())),
            response(Content::Text("What do you do next?".into())),
        ]);
        let backend = InferenceScheduler::new(backend, Limits::default()).unwrap();
        let game = Arc::new(Game::new(
            store.clone(),
            backend.foreground(),
            Limits::default(),
            "scripted".into(),
            Sampling {
                temperature: 0.0,
                ..Default::default()
            },
        ));
        game.player_message(installed.member.clone(), "I look, then someone tries to split us twice.")
            .await
            .unwrap();

        let (first_result, second_result) = {
            let requests = recorded_requests.lock().unwrap();
            let find = |call_id: &str| {
                requests
                    .iter()
                    .flat_map(|request| request.messages.iter())
                    .find_map(|message| match &message.content {
                        MessageContent::ToolResult { tool_call_id, content } if tool_call_id == call_id => {
                            Some(content.clone())
                        }
                        _ => None,
                    })
            };
            (
                find("split-1").expect("the GM's own next turn must see split-1's tool result"),
                find("split-2").expect("the GM's own next turn must see split-2's tool result"),
            )
        };
        assert!(
            first_result.contains("split into groups"),
            "the first proposal against the still-active source must succeed: {first_result}"
        );
        assert!(
            second_result.contains("Tool arguments were rejected"),
            "the second proposal against the now-retired source must be rejected, not applied: {second_result}"
        );
        let adventurer_group = store
            .character_group_id(installed.member.character_id)
            .await
            .unwrap();
        let mara_group = store.character_group_id(mara_id).await.unwrap();
        let toma_group = store.character_group_id(toma_id).await.unwrap();
        assert_eq!(
            adventurer_group, mara_group,
            "only the first split's partitioning must stand"
        );
        assert_ne!(adventurer_group, toma_group);
    }

    #[tokio::test]
    async fn opening_turn_speaks_with_the_complete_creation_interface() {
        let (backend, recorded_requests) = ScriptedBackend::recording([response(Content::Text(
            "Welcome. Let us make your Adventurer.".into(),
        ))]);
        let (game, _store, _db, member) = blank_opening_game(backend).await;

        let response = game.enter(member.clone()).await.unwrap().unwrap();
        assert!(
            matches!(response.content, Content::Text(text) if text == "Welcome. Let us make your Adventurer.")
        );
        {
            let requests = recorded_requests.lock().unwrap();
            assert_eq!(requests.len(), 1);
            assert!(matches!(
                requests[0].messages.as_slice(),
                [
                    Message {
                        role: Role::System,
                        ..
                    },
                    Message {
                        role: Role::User,
                        ..
                    },
                ]
            ));
            assert_eq!(
                requests[0]
                    .tools
                    .iter()
                    .map(|tool| tool.name.as_str())
                    .collect::<Vec<_>>(),
                ["roll_hit_protection", "roll_attributes", "ready_to_begin"],
                "the agent must see its real creation interface while deciding whether to roll"
            );
            assert!(requests[0].tools.iter().all(|tool| {
                tool.schema.get("type").and_then(serde_json::Value::as_str) == Some("object")
                    && tool.schema.get("properties") == Some(&serde_json::json!({}))
            }));
        }
        assert!(
            game.enter(member).await.unwrap().is_none(),
            "reconnecting after the opening turn must not start a second character-creation conversation"
        );
        assert_eq!(recorded_requests.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn connecting_then_disconnecting_records_the_zero_one_transition() {
        let (backend, _recorded_requests) = ScriptedBackend::recording([]);
        let (game, store, _db, member) = blank_opening_game(backend).await;

        let connection_id = game.connect(&member).await.unwrap();
        let (last_connected_at, absent_since, generation): (String, Option<String>, i64) =
            sqlx::query_as(
                "SELECT last_connected_at, absent_since, generation FROM character_presence \
                 WHERE character_id = ?",
            )
            .bind(member.character_id)
            .fetch_one(&store.pool())
            .await
            .unwrap();
        assert!(!last_connected_at.is_empty());
        assert_eq!(absent_since, None, "a connected character is not absent");
        assert_eq!(generation, 1, "the first connection is generation 1");

        // A second tab for the same character is already present: it must
        // not bump the generation or otherwise re-announce arrival.
        let second_connection_id = game.connect(&member).await.unwrap();
        let generation_after_second: i64 = sqlx::query_scalar(
            "SELECT generation FROM character_presence WHERE character_id = ?",
        )
        .bind(member.character_id)
        .fetch_one(&store.pool())
        .await
        .unwrap();
        assert_eq!(
            generation_after_second, 1,
            "a second connection for an already-present character is not a new transition"
        );

        game.disconnect(&member, connection_id).await.unwrap();
        let generation_after_one_close: i64 = sqlx::query_scalar(
            "SELECT generation FROM character_presence WHERE character_id = ?",
        )
        .bind(member.character_id)
        .fetch_one(&store.pool())
        .await
        .unwrap();
        assert_eq!(
            generation_after_one_close, 1,
            "closing one of two tabs is not yet a departure"
        );

        game.disconnect(&member, second_connection_id)
            .await
            .unwrap();
        let (absent_since, generation): (Option<String>, i64) = sqlx::query_as(
            "SELECT absent_since, generation FROM character_presence WHERE character_id = ?",
        )
        .bind(member.character_id)
        .fetch_one(&store.pool())
        .await
        .unwrap();
        assert!(
            absent_since.is_some(),
            "closing the last tab marks the character absent"
        );
        assert_eq!(generation, 2, "the departure is a second transition");

        // A disconnect for an id already released is a no-op, not an error.
        game.disconnect(&member, second_connection_id).await.unwrap();
    }

    async fn two_player_bread_thief_game<B>(
        backend: B,
    ) -> (Arc<Game<B>>, Store, crate::store::TestDatabase, PlayerAgent, PlayerAgent)
    where
        B: Backend + Send + Sync + 'static,
    {
        let db = crate::store::TestDatabase::new("presence-test");
        let store = Store::open(db.path()).await.unwrap();
        let owner = store
            .find_or_create_user("warden@example.test", "Warden")
            .await
            .unwrap();
        let scenario = Scenario::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/scenarios/bread_thief.json"
        ))
        .unwrap();
        let installed = store.install_scenario(&owner, &scenario).await.unwrap();
        let companion = store
            .create_player_character(owner.id, installed.world_id)
            .await
            .unwrap();
        // A short real deadline: presence timer tests wait on it in real
        // wall-clock time rather than paused virtual time, which fights the
        // SQLite connection pool's own internal timeout.
        let limits = Limits {
            presence_departure_seconds: 0,
            ..Limits::default()
        };
        let scheduler = InferenceScheduler::new(backend, limits).unwrap();
        let game = Arc::new(Game::new(
            store.clone(),
            scheduler.foreground(),
            limits,
            "scripted".into(),
            Sampling {
                temperature: 0.0,
                ..Default::default()
            },
        ));
        (game, store, db, installed.member, companion)
    }

    /// Yield repeatedly so a spawned handler (process_world's PresenceChanged/
    /// PresenceDeadline dispatch, or a zero-second deadline's sleep) gets to
    /// run before the test observes its effect.
    async fn let_presence_handlers_run() {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }

    #[tokio::test]
    async fn first_arrival_after_an_empty_period_narrates_after_its_deadline() {
        let (backend, recorded_requests) = ScriptedBackend::recording([response(Content::Text(
            "Someone arrives.".into(),
        ))]);
        let (game, _store, _db, first, _second) = two_player_bread_thief_game(backend).await;

        // The group has never been announced online before: this is "first
        // arrival after an empty period," which is delayed behind the
        // departure deadline (here, zero seconds), not immediate.
        let _first_connection = game.connect(&first).await.unwrap();
        let_presence_handlers_run().await;
        assert_eq!(recorded_requests.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn arrival_into_an_already_announced_occupied_group_narrates_immediately() {
        let (backend, recorded_requests) = ScriptedBackend::recording([
            response(Content::Text("Someone arrives.".into())),
            response(Content::Text("Someone else arrives.".into())),
        ]);
        let (game, _store, _db, first, second) = two_player_bread_thief_game(backend).await;

        let _first_connection = game.connect(&first).await.unwrap();
        let_presence_handlers_run().await;
        assert_eq!(
            recorded_requests.lock().unwrap().len(),
            1,
            "the group's first-ever online character settles its own delayed announcement"
        );

        let _second_connection = game.connect(&second).await.unwrap();
        let_presence_handlers_run().await;
        assert_eq!(
            recorded_requests.lock().unwrap().len(),
            2,
            "arrival into a group already announced as occupied narrates immediately, no delay"
        );
    }

    #[tokio::test]
    async fn departure_narrates_after_its_deadline() {
        let (backend, recorded_requests) = ScriptedBackend::recording([
            response(Content::Text("Someone arrives.".into())),
            response(Content::Text("Someone else arrives.".into())),
            response(Content::Text("Someone leaves.".into())),
        ]);
        let (game, _store, _db, first, second) = two_player_bread_thief_game(backend).await;
        let first_connection = game.connect(&first).await.unwrap();
        let_presence_handlers_run().await;
        let _second_connection = game.connect(&second).await.unwrap();
        let_presence_handlers_run().await;
        assert_eq!(
            recorded_requests.lock().unwrap().len(),
            2,
            "the first character's own delayed arrival and the second's immediate arrival already narrated"
        );

        game.disconnect(&first, first_connection).await.unwrap();
        let_presence_handlers_run().await;
        assert_eq!(
            recorded_requests.lock().unwrap().len(),
            3,
            "the departure narrates once its (here, zero-second) deadline elapses"
        );
    }

    #[tokio::test]
    async fn returning_before_the_deadline_cancels_the_departure() {
        let (backend, recorded_requests) = ScriptedBackend::recording([
            response(Content::Text("Someone arrives.".into())),
            response(Content::Text("Someone else arrives.".into())),
        ]);
        let (game, store, _db, first, second) = two_player_bread_thief_game(backend).await;
        // A real, observable window: the zero-second default used by other
        // presence tests here would resolve before this test could ever
        // reconnect within it.
        let group_id = store.character_group_id(first.character_id).await.unwrap();
        let first_connection = game.connect(&first).await.unwrap();
        let_presence_handlers_run().await;
        let _second_connection = game.connect(&second).await.unwrap();
        let_presence_handlers_run().await;
        assert_eq!(recorded_requests.lock().unwrap().len(), 2);

        game.disconnect(&first, first_connection).await.unwrap();
        // The departure's deadline has been scheduled; reconnect immediately,
        // before its (here, zero-second) sleep has had a chance to run.
        let _first_connection_again = game.connect(&first).await.unwrap();
        let_presence_handlers_run().await;
        assert_eq!(
            recorded_requests.lock().unwrap().len(),
            2,
            "returning before the deadline must cancel the departure without ever narrating it"
        );
        let narration = store.presence_narration(group_id).await.unwrap();
        assert!(
            narration.due_at.is_none(),
            "the cancelled departure must not leave a stale pending deadline"
        );
    }

    #[tokio::test]
    async fn an_empty_group_narrates_nothing_on_the_deadline() {
        let (backend, recorded_requests) = ScriptedBackend::recording([
            response(Content::Text("Someone arrives.".into())),
            response(Content::Text("Someone else arrives.".into())),
        ]);
        let (game, _store, _db, first, second) = two_player_bread_thief_game(backend).await;
        let first_connection = game.connect(&first).await.unwrap();
        let_presence_handlers_run().await;
        let second_connection = game.connect(&second).await.unwrap();
        let_presence_handlers_run().await;
        assert_eq!(recorded_requests.lock().unwrap().len(), 2);

        game.disconnect(&first, first_connection).await.unwrap();
        game.disconnect(&second, second_connection).await.unwrap();
        let_presence_handlers_run().await;
        assert_eq!(
            recorded_requests.lock().unwrap().len(),
            2,
            "an empty group retains the difference without running an inference"
        );
    }

    #[tokio::test]
    async fn party_presence_reports_group_members_and_online_state() {
        let (backend, _recorded_requests) = ScriptedBackend::recording([]);
        let (game, store, _db, first, second) = two_player_bread_thief_game(backend).await;
        let first_connection = game.connect(&first).await.unwrap();

        let party = store.party_presence(first.character_id).await.unwrap();
        let names_online: Vec<(bool, &str)> = party
            .iter()
            .map(|member| (member.online, member.name.as_str()))
            .collect();
        // Bread Thief's group also has Mara and Toma (NPCs, never online).
        assert_eq!(party.len(), 4, "the Adventurer, the companion, Mara and Toma");
        assert_eq!(
            names_online
                .iter()
                .filter(|(online, _)| *online)
                .count(),
            1,
            "only the connected Adventurer is online"
        );
        assert!(party.iter().any(|member| member.name.starts_with("Mara")
            || member.name == "Mara"));

        game.disconnect(&first, first_connection).await.unwrap();
        let party_after = store.party_presence(second.character_id).await.unwrap();
        assert!(
            party_after.iter().all(|member| !member.online),
            "nobody remains connected after the sole connection closes"
        );
    }

    #[tokio::test]
    async fn live_narration_does_not_cross_groups_sharing_one_location() {
        let (backend, _recorded_requests) = ScriptedBackend::recording([]);
        let (game, store, _db, first, second) = two_player_bread_thief_game(backend).await;
        // No public API creates a second group yet (that is Developer Mode
        // plan D4's split_group); this directly inserts one sharing the
        // Adventurer's own location, matching the exact shape
        // install_scenario/split_group would produce, to exercise the
        // narration/broadcast isolation this fix adds ahead of D4 existing.
        let location_id = store.player_location_id(&second).await.unwrap();
        let world_gm = store.group_gm_history(store.character_group_id(second.character_id).await.unwrap())
            .await
            .unwrap();
        let other_group_id: i64 = sqlx::query_scalar(
            "INSERT INTO history_branch (agent_id) VALUES (?) RETURNING (SELECT id FROM history_branch WHERE agent_id = ? ORDER BY id DESC LIMIT 1)",
        )
        .bind(world_gm.agent_id)
        .bind(world_gm.agent_id)
        .fetch_one(&store.pool())
        .await
        .unwrap();
        let other_group_id: i64 = sqlx::query_scalar(
            "INSERT INTO \"group\" (world_id, name, location_id, description, branch_id) \
             VALUES (?, 'other', ?, '', ?) RETURNING id",
        )
        .bind(second.world_id)
        .bind(location_id)
        .bind(other_group_id)
        .fetch_one(&store.pool())
        .await
        .unwrap();
        sqlx::query("UPDATE character_group SET group_id = ? WHERE character_id = ?")
            .bind(other_group_id)
            .bind(second.character_id)
            .execute(&store.pool())
            .await
            .unwrap();

        let mut first_receiver = game.subscribe(&first).await.unwrap();
        let mut second_receiver = game.subscribe(&second).await.unwrap();
        let first_group_id = store.character_group_id(first.character_id).await.unwrap();
        assert_ne!(
            first_group_id, other_group_id,
            "the two characters must actually be in different groups for this test"
        );

        let broadcasts = game.world_events(first.world_id).await.broadcasts;
        let _ = group_broadcast(&broadcasts, first_group_id)
            .await
            .send("only the first group should see this".to_string());
        assert_eq!(
            first_receiver.recv().await.unwrap(),
            "only the first group should see this"
        );
        assert!(
            second_receiver.try_recv().is_err(),
            "a broadcast to one group must not reach a live viewer in a different group at the same location"
        );
    }

    #[tokio::test]
    async fn concurrent_viewers_share_one_server_owned_opening() {
        let (backend, started, release, requests) = DelayedBackend::new();
        let (game, store, _db, member) = blank_opening_game(backend).await;
        let first_game = Arc::clone(&game);
        let first_member = member.clone();
        let first = tokio::spawn(async move { first_game.wait_for_opening(first_member).await });
        started.notified().await;

        let second_game = Arc::clone(&game);
        let second_member = member.clone();
        let second = tokio::spawn(async move { second_game.wait_for_opening(second_member).await });
        tokio::task::yield_now().await;
        assert_eq!(
            requests.load(Ordering::SeqCst),
            1,
            "a second viewer must observe the existing opening, not invoke the model"
        );

        release.notify_waiters();
        first.await.unwrap().unwrap();
        second.await.unwrap().unwrap();
        assert_eq!(requests.load(Ordering::SeqCst), 1);
        assert!(
            !store
                .live_segments(member.agent_id, None)
                .await
                .unwrap()
                .is_empty(),
            "the shared opening must finish with stored player-agent history"
        );
    }

    #[tokio::test]
    async fn opening_corrects_a_rejected_tool_call_before_becoming_stored() {
        let (backend, requests) = ScriptedBackend::recording([
            response(Content::ToolCalls(vec![ToolCall {
                id: "bad-1".into(),
                name: "not_a_creation_tool".into(),
                arguments: "{}".into(),
            }])),
            response(Content::Text(
                "Welcome. Let us make your Adventurer.".into(),
            )),
        ]);
        let (game, store, _db, member) = blank_opening_game(backend).await;

        game.wait_for_opening(member.clone()).await.unwrap();
        assert_eq!(requests.lock().unwrap().len(), 2);
        assert!(
            matches!(
                requests.lock().unwrap()[1].messages.last().map(|message| &message.content),
                Some(MessageContent::ToolResult { content, .. })
                    if content.contains("Tool `not_a_creation_tool` is unavailable")
            ),
            "the correction inference must receive the rejected call as a tool result"
        );
        assert!(
            store
                .player_chat(&member)
                .await
                .unwrap()
                .iter()
                .any(|entry| entry.role == Role::Assistant),
            "a later opening must persist the reply that makes reconnects viewers only"
        );
    }

    #[tokio::test]
    #[ignore = "device test: needs a CUDA GPU and the models/ GGUF from default.toml"]
    async fn real_model_opening_reaches_a_stored_player_reply() {
        // This is deliberately the same server-owned opening path the web
        // socket awaits, rather than a direct backend request. A successful
        // return proves the agent settled and its player-visible reply
        // committed to stored history.
        let (backend, model_path, sampling, _limits) = device_test_game().await;
        let (game, store, _db, member) = opening_game(backend, model_path, sampling).await;

        game.wait_for_opening(member.clone())
            .await
            .expect("the real opening must complete");
        assert_eq!(
            store
                .inference_count()
                .await
                .expect("opening inference count should load"),
            1,
            "the real opening must wait for the player instead of calling a creation tool"
        );
        let opening = store
            .reconstruct_inference(1)
            .await
            .expect("opening inference should reconstruct");
        assert_eq!(
            opening
                .request
                .tools
                .iter()
                .map(|tool| tool.name.as_str())
                .collect::<Vec<_>>(),
            ["roll_hit_protection", "roll_attributes", "ready_to_begin"],
            "the real opening must present the model's complete creation interface"
        );
        let prompt = opening
            .model_prompt
            .as_ref()
            .expect("normal game inference retains model prompt evidence");
        assert!(prompt.contains("roll_hit_protection") && prompt.contains("ready_to_begin"));
        let player_chat = store
            .player_chat(&member)
            .await
            .expect("opening history should load");
        assert!(
            player_chat
                .iter()
                .any(|entry| entry.role == Role::Assistant && !entry.text.trim().is_empty()),
            "the completed opening must leave a player-visible assistant reply"
        );
    }

    #[tokio::test]
    #[ignore = "device test: needs a CUDA GPU and the models/ GGUF from default.toml"]
    async fn real_model_calls_ready_to_begin_after_the_final_roll() {
        let (backend, model_path, configured, _limits) = device_test_game().await;
        // Greedy: this asserts the model follows a specific instruction
        // (call ready_to_begin once the rolls are done), so it must not depend
        // on a sampling draw.
        let sampling = Sampling {
            temperature: 0.0,
            ..configured
        };
        let (game, store, _db, member) = opening_game(backend, model_path, sampling).await;
        // A real player is connected before ever chatting (web.rs's
        // play_connection registers it ahead of any opening/message
        // handling); narration recipients are connected PCs (design.md's
        // Membership and presence).
        let _connection = game.connect(&member).await.unwrap();
        let sequence = store
            .begin_sequence(member.world_id, "test completed creation")
            .await
            .unwrap();
        store
            .roll_hit_protection(&member, sequence.id, None)
            .await
            .unwrap();
        store
            .roll_attributes(&member, sequence.id, None)
            .await
            .unwrap();

        // A small model at greedy still occasionally re-rolls or stalls; a
        // real session would prompt again. Three nudges is generous for a
        // model that can follow the instruction and still fails a genuine
        // prompt or template regression.
        for attempt in 1..=3 {
            game.player_message(
                member.clone(),
                "The required rolls are complete. Begin play now.",
            )
            .await
            .unwrap_or_else(|error| panic!("player agent turn {attempt} failed: {error:#}"));
            if store.is_ready_to_begin(&member).await.unwrap() {
                break;
            }
        }
        assert!(
            store.is_ready_to_begin(&member).await.unwrap(),
            "the model never called ready_to_begin across three prompts"
        );
        assert!(
            store
                .player_chat(&member)
                .await
                .unwrap()
                .iter()
                .any(|entry| entry.role == Role::Narration),
            "the GM opening narration must become player-visible before the guide follows up"
        );
    }

    #[tokio::test]
    #[ignore = "device test: needs a CUDA GPU and the models/ GGUF from default.toml"]
    async fn real_model_compacts_a_long_chat_and_keeps_an_early_fact() {
        let (backend, model_path, sampling, base_limits) = device_test_game().await;
        // A threshold the second turn's context clears, retaining enough tail
        // that the summary has real history to work from.
        let limits = Limits {
            compact_before_next_input_tokens: 700,
            keep_tail_messages: 4,
            ..base_limits
        };
        let (game, store, _db, member) =
            opening_game_with_limits(backend, model_path, sampling, limits).await;

        // An early, checkable fact, then enough turns to force a compaction.
        game.player_message(member.clone(), "Remember the passphrase: BRAMBLEFOX.")
            .await
            .expect("first player message");
        for prompt in [
            "Describe the room I'm in.",
            "What can I hear right now?",
            "Tell me about the nearest exit.",
            "What time of day is it?",
        ] {
            game.player_message(member.clone(), prompt)
                .await
                .expect("later player message");
        }

        let branch_id = store
            .branch_id_for_agent(member.agent_id)
            .await
            .expect("branch should resolve");
        let summary = store
            .latest_summary_for_branch(branch_id)
            .await
            .expect("summary should load")
            .expect("a compaction must have run");
        assert!(
            summary.content.to_uppercase().contains("BRAMBLEFOX"),
            "the summary dropped an early fact it should retain: {}",
            summary.content
        );
    }
}
