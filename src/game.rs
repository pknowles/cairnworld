use std::{
    collections::HashSet,
    sync::{
        Arc, Mutex, Weak,
        atomic::{AtomicI64, Ordering},
    },
};

use anyhow::{Context, Result, ensure};
use schemars::{JsonSchema, schema_for};
use serde::Deserialize;
use tokio::{
    sync::{broadcast, oneshot, watch},
    task::JoinSet,
};

use crate::{
    agent::{self, Budget, CallContext},
    inference::ScheduledBackend,
    llm::{Backend, Content, Message, Response, Role, Sampling, ToolDefinition},
    settings::Limits,
    store::{
        ActionOutcome, Hearing, InvalidHearing, MAX_PREFIX_CHARS, PendingAction, PlayerAgent,
        PresenceNarration, SayAction, Store, WhisperAction,
    },
    tools::{GroupPartition, Tool, ToolFuture, ToolOutcome, TopologyChange},
    world::{self, ChatChange, OpeningState, World, WorldEvent, Worlds},
};

const GM_ROLE: &str = "You are the GM for the characters in this group. Ground narration in the scene packet and address the characters as a group. Use the available tools to resolve proposed actions before narrating their outcomes.";

/// The one game service that a browser event will invoke. It derives the player
/// history from a previously authenticated membership; no tool accepts a
/// client-selected agent or character id.
pub struct Game<B> {
    store: Store,
    backend: ScheduledBackend<B>,
    limits: Limits,
    model: String,
    sampling: Sampling,
    worlds: Worlds,
    /// One worker per world, each returning its world id. Dropping the set
    /// aborts them: the last resort for abnormal teardown, since `shutdown`
    /// joins them first.
    tasks: Mutex<JoinSet<i64>>,
    /// Becomes true once and stays true. World workers close their input
    /// queue on seeing it, which is the single admission boundary for
    /// browser input (and future MCP callers): a request is either already
    /// queued, and so drained, or rejected as stopping.
    stopping: watch::Sender<bool>,
    /// Process-local, never restored after restart (design.md's Membership
    /// and presence: "Live connection IDs are process-local").
    next_connection_id: AtomicI64,
}

/// A turn failed and its sequence recorded the error, which player chat
/// delivers from storage. Attached as context so a caller that would otherwise
/// report the error itself can tell it is already delivered.
#[derive(Debug)]
pub struct FailedTurn {
    pub sequence_id: i64,
}

impl std::fmt::Display for FailedTurn {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "turn {} failed", self.sequence_id)
    }
}

struct Narration {
    group_id: i64,
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
    budget: Budget,
    call: CallContext,
}

impl<B> Game<B>
where
    B: Backend + Send + Sync + 'static,
{
    pub fn new(
        store: Store,
        worlds: Worlds,
        backend: ScheduledBackend<B>,
        limits: Limits,
        model: String,
        sampling: Sampling,
    ) -> Arc<Self> {
        let game = Arc::new(Self {
            store,
            backend,
            limits,
            model,
            sampling,
            worlds,
            tasks: Mutex::new(JoinSet::new()),
            stopping: watch::channel(false).0,
            next_connection_id: AtomicI64::new(1),
        });
        for world in game.worlds.read().expect("worlds lock poisoned").values() {
            game.start_world(Arc::clone(world));
        }
        game
    }

    pub fn start_world(self: &Arc<Self>, world: Arc<World>) {
        self.tasks
            .lock()
            .expect("world tasks lock poisoned")
            .spawn(Self::process_world(
                Arc::downgrade(self),
                world,
                self.stopping.subscribe(),
            ));
    }

    /// Routine shutdown (design.md's Server shutdown and crashes): close
    /// admission, then wait for every accepted turn - queued, active, nested
    /// calls and delivery - and every admitted deferred job. Independent
    /// automatic work (presence narration, new compactions) is not started;
    /// its durable obligations stay pending for the next startup. There is no
    /// timeout: forcing the process to stop is a crash, handled at startup.
    pub async fn shutdown(&self) -> Result<()> {
        self.stopping.send_replace(true);
        self.backend.stop_independent_work();
        let mut workers =
            std::mem::take(&mut *self.tasks.lock().expect("world tasks lock poisoned"));
        let mut waiting = Vec::new();
        for world in self.worlds.read().expect("worlds lock poisoned").values() {
            waiting.push(world.id);
        }
        waiting.sort_unstable();
        tracing::info!(worlds = ?waiting, "shutting down: finishing accepted turns");
        // One world's failure must not abort the others' accepted turns, so
        // keep draining and report every failure afterwards.
        let mut failures = Vec::new();
        while let Some(stopped) = workers.join_next().await {
            match stopped {
                Ok(world_id) => {
                    waiting.retain(|id| *id != world_id);
                    tracing::info!(world_id, remaining = ?waiting, "world finished its accepted turns");
                }
                Err(error) => failures.push(
                    anyhow::Error::new(error).context("world worker failed while shutting down"),
                ),
            }
        }
        tracing::info!("shutting down: finishing admitted deferred jobs");
        failures.extend(self.backend.finish_deferred_jobs().await.err());
        match failures.len() {
            0 => Ok(()),
            1 => Err(failures.remove(0)),
            count => Err(anyhow::anyhow!(
                "{count} failures while shutting down:
{}",
                failures
                    .iter()
                    .map(|failure| format!("{failure:#}"))
                    .collect::<Vec<_>>()
                    .join(
                        "
"
                    )
            )),
        }
    }

    fn world(&self, id: i64) -> Result<Arc<World>> {
        World::get(&self.worlds, id)
    }

    /// Queue a player message behind all earlier events in this world.
    pub async fn player_message(
        self: &Arc<Self>,
        member: PlayerAgent,
        text: &str,
    ) -> Result<Response> {
        let world = self.world(member.world_id)?;
        let (reply, receive) = oneshot::channel();
        self.admit(
            &world,
            WorldEvent::PlayerMessage {
                member,
                text: text.to_string(),
                reply,
            },
        )
        .await
        .context("queueing player message")?;
        receive
            .await
            .context("world stopped processing player message")?
    }

    /// Queue opening a world behind all earlier events in that world.
    pub async fn enter(self: &Arc<Self>, member: PlayerAgent) -> Result<Option<Response>> {
        let world = self.world(member.world_id)?;
        let (reply, receive) = oneshot::channel();
        self.admit(&world, WorldEvent::Enter { member, reply })
            .await
            .context("queueing world entry")?;
        receive
            .await
            .context("world stopped processing world entry")?
    }

    /// The one admission point for external game input. Shutdown refuses new
    /// input at once; the world's queue then closes, so input that raced the
    /// refusal is either queued ahead of the close and finished, or refused.
    async fn admit(&self, world: &World, event: WorldEvent) -> Result<()> {
        ensure!(
            !*self.stopping.borrow(),
            "world {} is no longer accepting input because the server is stopping",
            world.id
        );
        world
            .sender
            .send(event)
            .await
            .with_context(|| format!("world {} is not accepting input", world.id))
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
        let world = self.world(member.world_id)?;
        let mut receiver = {
            let mut openings = world.openings.lock().await;
            if let Some(receiver) = openings.get(&character_id) {
                receiver.clone()
            } else {
                let (sender, receiver) = watch::channel(OpeningState::Pending);
                openings.insert(character_id, receiver.clone());
                let game = Arc::clone(self);
                let world = Arc::clone(&world);
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
                    world.openings.lock().await.remove(&character_id);
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

    /// A stable subscription follows the character through group changes.
    /// Notifications carry no private chat content; readers query their own history.
    pub async fn subscribe(
        self: &Arc<Self>,
        member: &PlayerAgent,
    ) -> Result<broadcast::Receiver<ChatChange>> {
        Ok(self.world(member.world_id)?.broadcasts.subscribe())
    }

    pub async fn player_chat_error(
        &self,
        member: &PlayerAgent,
        sequence_id: i64,
    ) -> Result<Option<crate::store::PlayerChatEntry>> {
        self.store.player_chat_error(member, sequence_id).await
    }

    /// Register one live connection for a character, returning its id for the
    /// matching `disconnect` call. A durable zero/one transition (design.md's
    /// Membership and presence) persists `character_presence` and wakes the
    /// world's coalesced changed-character set; this never waits for an
    /// ongoing model turn or space in the action queue.
    pub async fn connect(self: &Arc<Self>, member: &PlayerAgent) -> Result<i64> {
        let connection_id = self.next_connection_id.fetch_add(1, Ordering::Relaxed);
        let world = self.world(member.world_id)?;
        let mut presence = world.presence.lock().await;
        if !presence.connections.contains_key(&member.character_id) {
            self.store
                .mark_character_present(member.character_id)
                .await
                .context("recording character presence")?;
            presence.changed.insert(member.character_id);
            world.changed.notify_one();
        }
        presence
            .connections
            .entry(member.character_id)
            .or_default()
            .insert(connection_id);
        Ok(connection_id)
    }

    /// Release one live connection, reversing `connect`. A connection's owner
    /// releases it exactly once; disconnecting an id already removed (e.g. a
    /// double-release race) is a no-op rather than an error, matching a
    /// one/zero set membership check rather than a counter.
    pub async fn disconnect(
        self: &Arc<Self>,
        member: &PlayerAgent,
        connection_id: i64,
    ) -> Result<()> {
        let world = self.world(member.world_id)?;
        let mut presence = world.presence.lock().await;
        let Some(characters) = presence.connections.get(&member.character_id) else {
            return Ok(());
        };
        if !characters.contains(&connection_id) {
            return Ok(());
        }
        if characters.len() == 1 {
            self.store
                .mark_character_absent(member.character_id)
                .await
                .context("recording character absence")?;
            presence.connections.remove(&member.character_id);
            presence.changed.insert(member.character_id);
            world.changed.notify_one();
        } else {
            presence
                .connections
                .get_mut(&member.character_id)
                .expect("connection exists")
                .remove(&connection_id);
        }
        Ok(())
    }

    async fn process_world(
        game: Weak<Self>,
        state: Arc<World>,
        mut stopping: watch::Receiver<bool>,
    ) -> i64 {
        let world_id = state.id;
        let mut events = state.queued.lock().await;
        let Some(active) = game.upgrade() else {
            return world_id;
        };
        let store = active.store.clone();
        drop(active);
        // Once stopping, only already-queued input is handled: presence
        // narration is independent automatic work, and its durable schedule
        // stays pending for the next startup.
        let mut draining = false;
        loop {
            let next = if draining {
                None
            } else {
                match store.next_presence_deadline(world_id).await {
                    Ok(next) => next,
                    Err(error) => {
                        tracing::error!(world_id, error = %format!("{error:#}"), "reading presence schedule");
                        break;
                    }
                }
            };
            let deadline = async {
                match next {
                    Some((group_id, generation, seconds)) => {
                        if seconds > 0 {
                            tokio::time::sleep(std::time::Duration::from_secs(seconds as u64))
                                .await;
                        }
                        WorldEvent::PresenceDeadline {
                            group_id,
                            generation,
                        }
                    }
                    None => std::future::pending().await,
                }
            };
            let event = tokio::select! {
                biased;
                _ = world::stopped(&mut stopping), if !draining => {
                    events.close();
                    draining = true;
                    continue;
                }
                _ = state.changed.notified(), if !draining => {
                    let Some(game) = game.upgrade() else { break };
                    let changed = std::mem::take(&mut state.presence.lock().await.changed);
                    let mut groups = HashSet::new();
                    for character_id in changed {
                        match store.character_group_id(character_id).await {
                            Ok(group_id) => { groups.insert(group_id); }
                            Err(error) => tracing::error!(world_id, character_id, error = %format!("{error:#}"), "resolving changed presence group"),
                        }
                    }
                    for group_id in groups {
                        match game.handle_presence_changed(group_id).await {
                            Ok(()) => {}
                            Err(error) => tracing::error!(world_id, group_id, error = %format!("{error:#}"), "handling presence change"),
                        }
                    }
                    continue;
                }
                event = deadline, if !draining => event,
                event = events.recv() => match event { Some(event) => event, None => break },
            };
            let Some(game) = game.upgrade() else { break };
            match event {
                WorldEvent::PlayerMessage {
                    member,
                    text,
                    reply,
                } => {
                    let character_id = member.character_id;
                    let result = game.resolve_player_message(member, &text).await;
                    if let Err(error) = &result {
                        tracing::error!(world_id, character_id, error = %format!("{error:#}"), "processing player message");
                    }
                    let _requester_disconnected = reply.send(result);
                }
                WorldEvent::Enter { member, reply } => {
                    let character_id = member.character_id;
                    let result = game.resolve_enter(member).await;
                    if let Err(error) = &result {
                        tracing::error!(world_id, character_id, error = %format!("{error:#}"), "processing world entry");
                    }
                    let _requester_disconnected = reply.send(result);
                }
                WorldEvent::PresenceDeadline {
                    group_id,
                    generation,
                } => {
                    if let Err(error) = game.handle_presence_deadline(group_id, generation).await {
                        tracing::error!(world_id, group_id, generation, error = %format!("{error:#}"), "handling presence deadline");
                    }
                }
                #[cfg(test)]
                WorldEvent::Settled(reply) => {
                    let _test_finished = reply.send(());
                }
                #[cfg(test)]
                WorldEvent::Panic => panic!("injected world worker panic"),
            }
        }
        // World retains the receiver storage. Explicitly close and drain it
        // when its worker stops so queued/future callers fail instead of hanging.
        events.close();
        while events.try_recv().is_ok() {}
        world_id
    }

    /// React to a character's connect/disconnect: on first arrival into an
    /// occupied group, narrate immediately; on a departure, or first arrival
    /// after an empty period, schedule (or leave already-scheduled) a
    /// 60-second deadline. Reads current presence/group state rather than
    /// trusting the event's own payload, since further changes may have
    /// queued behind it (design.md's Membership and presence).
    async fn handle_presence_changed(self: &Arc<Self>, group_id: i64) -> Result<()> {
        let online = self
            .store
            .group_ready_online_character_ids(group_id)
            .await?;
        let narration = self.store.presence_narration(group_id).await?;
        if narration.due_at.is_some() && (!narration.waiting_for_return || online.is_empty()) {
            return Ok(());
        }
        if !narration.waiting_for_return
            && online
                .iter()
                .any(|id| !narration.last_announced_character_ids.contains(id))
        {
            return self
                .narrate_presence_transition(group_id, &narration, &online)
                .await;
        }
        let needs_window = if narration.waiting_for_return {
            !online.is_empty()
        } else {
            online != narration.last_announced_character_ids
        };
        if needs_window {
            self.store
                .schedule_presence_deadline(
                    group_id,
                    i64::try_from(self.limits.presence_departure_seconds)?,
                    narration.generation,
                )
                .await?;
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
    ) -> Result<()> {
        let Some(narration) = self
            .store
            .consume_presence_deadline(group_id, generation)
            .await?
        else {
            return Ok(());
        };
        let online = self
            .store
            .group_ready_online_character_ids(group_id)
            .await?;
        self.narrate_presence_transition(group_id, &narration, &online)
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
    ) -> Result<()> {
        if online.is_empty() || online == narration.last_announced_character_ids {
            self.store
                .announce_presence(group_id, online, narration.generation)
                .await?;
            return Ok(());
        }
        let world_id = self.store.world_id_for_group(group_id).await?;
        let world = self.world(world_id)?;
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
        let scene_packet = format!("Current scene packet:\n{scene}");
        let role = Message::text(Role::System, GM_ROLE);
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
            let branch = self.store.group_branch(group_id).await?;
            self.store
                .append_instruction(branch, &request, sequence.id)
                .await
                .context("storing presence transition event")?;
            let settled = agent::complete_with_call_context(
                &self.store,
                &self.backend,
                &budget,
                agent::Turn {
                    events: world.events.clone(),
                    branch,
                    static_messages: &[role],
                    dynamic_context: &[scene_packet.as_str()],
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
            ensure!(
                matches!(settled.response.content, Content::Text(_)),
                "group GM did not settle while narrating presence"
            );
            self.store
                .append_presence_narration(world_id, narration, online, settled.output_content_id)
                .await
                .context("storing presence narration")?;
            let _no_live_viewers = world.broadcasts.send(ChatChange::Entries);
            Ok(())
        }
        .await;
        if let Err(finish_error) = self
            .store
            .finish_sequence(
                sequence.id,
                result
                    .as_ref()
                    .err()
                    .map(|error| format!("{error:#}"))
                    .as_deref(),
            )
            .await
        {
            tracing::error!(
                sequence_id = sequence.id,
                error = %format!("{finish_error:#}"),
                "finishing presence narration sequence"
            );
        }
        if result.is_err() {
            let _no_live_viewers = world.broadcasts.send(ChatChange::Failed {
                sequence_id: sequence.id,
            });
        }
        result
    }

    /// Resolve one player message through its membership-owned player agent.
    async fn resolve_player_message(
        self: &Arc<Self>,
        member: PlayerAgent,
        text: &str,
    ) -> Result<Response> {
        let world = self.world(member.world_id)?;
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
                self.action_tools(member.clone(), sequence.id, budget.clone(), call.clone())
            } else {
                self.creation_tools(member.clone(), sequence.id, budget.clone(), call.clone())
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
                    events: world.events.clone(),
                    branch: self.store.agent_branch(member.agent_id).await?,
                    static_messages: &[prompt],
                    dynamic_context: &[],
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
            self.warn_unexpected_silent_player_completion(&member, sequence.id, &settled);
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
                result
                    .as_ref()
                    .err()
                    .map(|error| format!("{error:#}"))
                    .as_deref(),
            )
            .await
        {
            tracing::error!(
                sequence_id = sequence.id,
                error = %format!("{finish_error:#}"),
                "failed to finish player message sequence"
            );
        }
        let _no_live_viewers = world
            .broadcasts
            .send(Self::turn_change(&result, sequence.id));
        let response = result.map_err(|error| {
            error.context(FailedTurn {
                sequence_id: sequence.id,
            })
        })?;
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
    async fn resolve_enter(self: &Arc<Self>, member: PlayerAgent) -> Result<Option<Response>> {
        let world = self.world(member.world_id)?;
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
        let tools = self.creation_tools(member.clone(), sequence.id, budget.clone(), call.clone());
        // Everything from here on must finish the sequence on every exit,
        // including a failure building the prompt before the agent ever
        // runs - not only agent::complete_with_call_context's own result.
        let result: Result<Response> = async {
            let branch = self.store.agent_branch(member.agent_id).await?;
            let prompt = self
                .player_prompt(&member, false)
                .await
                .context("building player prompt")?;
            let entered = Message::text(
                Role::User,
                "The player has entered the world and has not chosen to roll yet. Speak directly to them.",
            );
            // This starts the conversation: later player turns and a
            // compaction summary need the same ordinary history event, while
            // inspection must distinguish it from human input.
            self.store
                .append_instruction(branch, &entered, sequence.id)
                .await
                .context("storing opening player instruction")?;
            let settled = agent::complete_with_call_context(
                &self.store,
                &self.backend,
                &budget,
                agent::Turn {
                    events: world.events.clone(),
                    branch,
                    static_messages: &[prompt],
                    dynamic_context: &[],
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
            self.warn_unexpected_silent_player_completion(&member, sequence.id, &settled);
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
                result
                    .as_ref()
                    .err()
                    .map(|error| format!("{error:#}"))
                    .as_deref(),
            )
            .await
        {
            tracing::error!(
                sequence_id = sequence.id,
                error = %format!("{finish_error:#}"),
                "failed to finish player entered sequence"
            );
        }
        let _no_live_viewers = world
            .broadcasts
            .send(Self::turn_change(&result, sequence.id));
        let response = result?;
        ensure!(
            matches!(response.content, Content::Text(_)),
            "player agent did not settle while opening the conversation"
        );
        Ok(Some(response))
    }

    /// What every connected tab is told when a turn ends: new entries, or - so a
    /// tab whose cursor is already past the turn's trigger still learns of it -
    /// the failure, which also carries the entries.
    fn turn_change<T>(result: &Result<T>, sequence_id: i64) -> ChatChange {
        if result.is_ok() {
            ChatChange::Entries
        } else {
            ChatChange::Failed { sequence_id }
        }
    }

    /// Whether the player agent has already produced its stored opening reply.
    pub async fn opening_complete(&self, member: &PlayerAgent) -> Result<bool> {
        self.store
            .player_agent_has_assistant_reply(member.agent_id)
            .await
            .context("checking stored player-agent opening reply")
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
            "Speak first and guide the player through their Adventurer's Cairn character creation. Character creation requires Hit Protection first, then attributes. Introduce the next required step in short thematic language. Use a roll tool only when the player explicitly asks to roll that exact step; asking to begin play does not substitute for an unfinished roll. The roll tools make the real, recorded results. Immediately after attributes are rolled, call ready_to_begin; do not invent, repeat, or paraphrase the opening scene, which the location GM narrates.".to_string()
        };
        Ok(Message::text(
            Role::System,
            format!(
                "{text}\n\nAn empty response ends the turn. Use it when a tool result or GM narration has already given the player the complete appropriate result; otherwise continue with another tool call or player-facing text."
            ),
        ))
    }

    fn warn_unexpected_silent_player_completion(
        &self,
        member: &PlayerAgent,
        sequence_id: i64,
        settled: &agent::Settled,
    ) {
        if !settled.used_tools
            && matches!(&settled.response.content, Content::Text(text) if text.is_empty())
        {
            tracing::warn!(
                world_id = member.world_id,
                character_id = member.character_id,
                sequence_id,
                inference_id = settled.inference_id,
                "player agent ended without text before making a tool call"
            );
        }
    }

    fn creation_tools(
        self: &Arc<Self>,
        member: PlayerAgent,
        sequence_id: i64,
        budget: Budget,
        call: CallContext,
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
                Box::pin(async move {
                    if let Err(error) = parse_no_arguments(&arguments) {
                        return Ok(ToolOutcome::rejected(error));
                    }
                    game.store
                        .ready_to_begin(&member, sequence_id, Some(tool_execution_id))
                        .await?;
                    let narration = game
                        .opening_narration(
                            &member,
                            sequence_id,
                            budget,
                            call.child(tool_execution_id),
                        )
                        .await?;
                    let delivery_game = Arc::clone(&game);
                    Ok(ToolOutcome::CompletedAfter {
                        content: "Character creation complete.".to_string(),
                        after_result: Box::pin(async move {
                            delivery_game.deliver_narration(&member, &narration).await
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
        budget: Budget,
        call: CallContext,
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
                budget: budget.clone(),
                call: call.clone(),
            }),
            self.action_tool(PlayerActionTool {
                member: member.clone(),
                sequence_id,
                definition: ToolDefinition {
                    name: "say".to_string(),
                    description: "Say words aloud. Everyone present hears them, even when you address one character.".to_string(),
                    schema: serde_json::to_value(schema_for!(SayAction))
                        .expect("say action schema should serialize"),
                },
                validate: validate_say_action,
                budget: budget.clone(),
                call: call.clone(),
            }),
            self.action_tool(PlayerActionTool {
                member: member.clone(),
                sequence_id,
                definition: ToolDefinition {
                    name: "whisper".to_string(),
                    description: "Whisper words that only the named characters hear.".to_string(),
                    schema: serde_json::to_value(schema_for!(WhisperAction))
                        .expect("whisper action schema should serialize"),
                },
                validate: validate_whisper_action,
                budget: budget.clone(),
                call: call.clone(),
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
                budget,
                call,
            }),
        ]
    }

    fn action_tool(self: &Arc<Self>, action: PlayerActionTool) -> Tool {
        let PlayerActionTool {
            member,
            sequence_id,
            definition,
            validate,
            budget,
            call,
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
            let tool_name = tool_name.clone();
            Box::pin(async move {
                if let Err(error) = validate(&arguments) {
                    return Ok(ToolOutcome::rejected(error));
                }
                // The membership is checked now, so a removed player cannot act.
                let actor = game.store.player_actor(&member).await?;
                let named = named_hearers(&tool_name, &arguments);
                if !named.is_empty()
                    && let Err(reason) = game
                        .store
                        .check_speech_targets(
                            actor.character_id(),
                            &named.iter().map(String::as_str).collect::<Vec<_>>(),
                        )
                        .await?
                {
                    return Ok(ToolOutcome::Rejected(format!(
                        "Tool arguments were rejected: {reason}"
                    )));
                }
                let pending = game
                    .store
                    .create_pending_action(
                        &actor,
                        sequence_id,
                        tool_execution_id,
                        &tool_name,
                        &arguments,
                    )
                    .await
                    .with_context(|| format!("submitting {tool_name} to location GM"))?;
                let requesting_tool_execution_id = pending.requesting_tool_execution_id;
                let narration = game.arbitrate(pending, budget, call).await?;
                // An approved action that could not happen is this call's
                // result in place of the usual one; the GM's narration is
                // still delivered and the agent continues.
                let content = match game
                    .store
                    .failed_action_reason(requesting_tool_execution_id)
                    .await?
                {
                    Some(reason) => format!("The action could not be applied: {reason}"),
                    None => "The GM's narration reached the player as its own entry. Respond to the player's next choice.".to_string(),
                };
                let delivery_game = Arc::clone(&game);
                Ok(ToolOutcome::CompletedAfter {
                    content,
                    after_result: Box::pin(async move {
                        delivery_game
                            .store
                            .deliver_speech_to_speaker(requesting_tool_execution_id)
                            .await?;
                        delivery_game.deliver_narration(&member, &narration).await
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
        sequence_id: i64,
        budget: Budget,
        call: CallContext,
    ) -> Result<Narration> {
        let group_id = self.store.character_group_id(member.character_id).await?;
        let gm_history = self.store.player_group_history(member).await?;
        let branch = self.store.group_branch(group_id).await?;
        let scene = serde_json::to_string(
            &self
                .store
                .gm_scene(member.world_id, gm_history.branch_id)
                .await
                .context("loading group GM scene for the opening")?,
        )
        .context("serializing group GM opening scene")?;
        let scene_packet = format!("Current scene packet:\n{scene}");
        let role = Message::text(Role::System, GM_ROLE);
        let request = Message::text(
            Role::User,
            "A player character has just begun play here. Narrate the scene, surroundings, and plot-relevant details the characters present can perceive.",
        );
        self.store
            .append_instruction(branch, &request, sequence_id)
            .await
            .context("storing opening narration event")?;
        let settled = agent::complete_with_call_context(
            &self.store,
            &self.backend,
            &budget,
            agent::Turn {
                events: self.world(member.world_id)?.events.clone(),
                branch,
                static_messages: &[role],
                dynamic_context: &[scene_packet.as_str()],
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
        let Content::Text(_) = settled.response.content else {
            anyhow::bail!("group GM did not settle while opening play");
        };
        Ok(Narration {
            group_id,
            content_id: settled.output_content_id,
        })
    }

    async fn arbitrate(
        self: &Arc<Self>,
        pending: PendingAction,
        budget: Budget,
        call: CallContext,
    ) -> Result<Narration> {
        let group_id = self
            .store
            .character_group_id(pending.character_id)
            .await
            .context("resolving acting character's group for arbitration")?;
        let action_id = pending.id;
        let branch = self.store.group_branch(group_id).await?;
        ensure!(
            branch.branch_id() == pending.gm_branch_id && branch.agent_id() == pending.gm_agent_id,
            "pending action {action_id} names GM branch {} for agent {}, but its group now uses branch {} for agent {}",
            pending.gm_branch_id,
            pending.gm_agent_id,
            branch.branch_id(),
            branch.agent_id(),
        );
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
                let approval_pending = approval_pending.clone();
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
                    if (!approval.include.is_empty()
                        || !approval.exclude.is_empty()
                        || approval.prefix.is_some())
                        && !matches!(approval_pending.tool.as_str(), "say" | "whisper")
                    {
                        return Ok(ToolOutcome::Rejected(format!(
                            "Tool arguments were rejected: include, exclude and prefix shape how speech is heard, and action {action_id} is not speech"
                        )));
                    }
                    let resolved = match game
                        .store
                        .resolve_pending_action(
                            approval_pending.gm_agent_id,
                            approval_pending.world_id,
                            action_id,
                            tool_execution_id,
                            "approved",
                            &Hearing {
                                include: approval.include,
                                exclude: approval.exclude,
                                prefix: approval.prefix,
                            },
                        )
                        .await
                    {
                        Ok(resolved) => resolved,
                        // Nothing was resolved, so the GM may try again.
                        Err(error) => match error.downcast_ref::<InvalidHearing>() {
                            Some(invalid) => {
                                return Ok(ToolOutcome::Rejected(format!(
                                    "Tool arguments were rejected: {invalid}"
                                )));
                            }
                            None => {
                                return Err(error.context(format!("approving action {action_id}")));
                            }
                        },
                    };
                    if !resolved.heard_by.is_empty() {
                        let _no_live_viewers = game
                            .world(approval_pending.world_id)?
                            .broadcasts
                            .send(ChatChange::Entries);
                    }
                    game.world(approval_pending.world_id)?.publish(
                        crate::inspection::DevEvent::Changed {
                            sequence_id: Some(approval_pending.sequence_id),
                            inference_id: None,
                        },
                    );
                    // The result is a report for the GM's later judgements; the
                    // acting player's own call receives its own result.
                    match resolved.outcome {
                        // The GM saw the words in the request; it needs who heard them.
                        ActionOutcome::Applied(_) if !resolved.heard_by.is_empty() => {
                            Ok(ToolOutcome::Completed(format!(
                                "Action {action_id} approved. Delivered as spoken to {}.",
                                resolved.heard_by.join(", ")
                            )))
                        }
                        ActionOutcome::Applied(result) => Ok(ToolOutcome::Completed(format!(
                            "Action {action_id} approved. Outcome: {result}"
                        ))),
                        ActionOutcome::Failed(reason) => Ok(ToolOutcome::Completed(format!(
                            "Action {action_id} approved, but it could not be applied: {reason}"
                        ))),
                        ActionOutcome::Rejected(_) => Ok(ToolOutcome::Rejected(format!(
                            "Tool arguments were rejected: action {action_id} was already rejected, it cannot now be approved"
                        ))),
                    }
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
                            &Hearing::default(),
                        )
                        .await
                        .with_context(|| format!("rejecting action {action_id}"))?;
                    game.world(pending.world_id)?
                        .publish(crate::inspection::DevEvent::Changed {
                            sequence_id: Some(pending.sequence_id),
                            inference_id: None,
                        });
                    match resolved.outcome {
                        ActionOutcome::Rejected(_) => Ok(ToolOutcome::Completed(format!(
                            "Action {action_id} rejected: {}",
                            rejection.reason
                        ))),
                        ActionOutcome::Applied(_) | ActionOutcome::Failed(_) => {
                            Ok(ToolOutcome::Rejected(format!(
                                "Tool arguments were rejected: action {action_id} was already approved, it cannot now be rejected"
                            )))
                        }
                    }
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
        let scene_packet = format!("Current scene packet:\n{scene}");
        let role = Message::text(Role::System, GM_ROLE);
        let request = Message::text(
            Role::User,
            format!(
                "{actor} takes action {action_id}: a `{}` request with arguments {}. Judge it from the current scene. Call approve_action with action_id {action_id} if it can happen, or reject_action with action_id {action_id} and a concise reason. Then narrate the outcome to the characters present.",
                pending.tool, pending.args,
            ),
        );
        self.store
            .append_instruction(branch, &request, pending.sequence_id)
            .await
            .with_context(|| format!("storing arbitration event for action {action_id}"))?;
        let settled = agent::complete_with_call_context(
            &self.store,
            &self.backend,
            &budget,
            agent::Turn {
                events: self.world(pending.world_id)?.events.clone(),
                branch,
                static_messages: &[role],
                dynamic_context: &[scene_packet.as_str()],
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
        let Content::Text(_) = settled.response.content else {
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
            group_id: self.store.character_group_id(pending.character_id).await?,
            content_id: settled.output_content_id,
        })
    }

    async fn deliver_narration(&self, member: &PlayerAgent, narration: &Narration) -> Result<()> {
        let world = self.world(member.world_id)?;
        self.store
            .append_group_narration(member.world_id, narration.group_id, narration.content_id)
            .await
            .context("storing GM narration before the player agent follows up")?;
        // A player may disconnect while the GM resolves. The narration remains
        // stored for reload; active viewers receive this exact same text.
        let _no_live_viewers = world.broadcasts.send(ChatChange::Entries);
        Ok(())
    }
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

fn validate_say_action(arguments: &str) -> Result<()> {
    let action: SayAction =
        serde_json::from_str(arguments).context("parsing say action arguments")?;
    ensure!(!action.words.trim().is_empty(), "words are empty");
    Ok(())
}

fn validate_whisper_action(arguments: &str) -> Result<()> {
    let action: WhisperAction =
        serde_json::from_str(arguments).context("parsing whisper action arguments")?;
    ensure!(!action.words.trim().is_empty(), "words are empty");
    ensure!(
        !action.to.is_empty(),
        "a whisper needs at least one listener"
    );
    Ok(())
}

/// The characters a speech action names, already validated as JSON.
fn named_hearers(tool: &str, arguments: &str) -> Vec<String> {
    match tool {
        "say" => serde_json::from_str::<SayAction>(arguments)
            .ok()
            .and_then(|say| say.to)
            .into_iter()
            .collect(),
        "whisper" => serde_json::from_str::<WhisperAction>(arguments)
            .map(|whisper| whisper.to)
            .unwrap_or_default(),
        _ => Vec::new(),
    }
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
    /// For speech: characters who also hear it, by name.
    #[serde(default)]
    include: Vec<String>,
    /// For speech: characters who must not hear it, by name.
    #[serde(default)]
    exclude: Vec<String>,
    /// For speech: a short phrase every hearer is shown before the words, such
    /// as how or why only some of them can hear.
    #[serde(default)]
    #[schemars(length(max = MAX_PREFIX_CHARS))]
    prefix: Option<String>,
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
        responses: Arc<Mutex<VecDeque<Response>>>,
        requests: Arc<Mutex<Vec<crate::llm::Request>>>,
    }

    impl ScriptedBackend {
        fn recording(
            responses: impl IntoIterator<Item = Response>,
        ) -> (Self, Arc<Mutex<Vec<crate::llm::Request>>>) {
            let requests = Arc::new(Mutex::new(vec![]));
            (
                Self {
                    responses: Arc::new(Mutex::new(responses.into_iter().collect())),
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

    #[tokio::test]
    async fn player_prompt_allows_a_silent_finish_after_a_player_visible_result() {
        let (backend, _) = ScriptedBackend::recording([]);
        let (game, _store, _db, member) = blank_opening_game(backend).await;
        let prompt = game.player_prompt(&member, false).await.unwrap();
        let MessageContent::Text(text) = prompt.content else {
            panic!("player prompt must be text")
        };
        assert!(
            text.contains("An empty response ends the turn"),
            "the player agent needs the general silent-finish rule: {text}"
        );
        assert!(
            !text.contains("Ask what the player wants to do next"),
            "the prompt must not force filler after a player-visible result: {text}"
        );
        assert!(
            text.contains("explicitly asks to roll that exact step"),
            "beginning play must not be interpreted as an omitted character-creation roll: {text}"
        );
    }

    #[tokio::test]
    async fn ready_to_begin_delivers_narration_then_allows_a_silent_player_finish() {
        let (backend, requests) = ScriptedBackend::recording([
            response(Content::ToolCalls(vec![ToolCall {
                id: "ready-1".into(),
                name: "ready_to_begin".into(),
                arguments: "{}".into(),
            }])),
            response(Content::Text("The charcoal hut is quiet.".into())),
            response(Content::Text(String::new())),
        ]);
        let (game, store, _db, member) = blank_opening_game(backend).await;
        let creation = store
            .begin_sequence(member.world_id, "complete creation")
            .await
            .unwrap();
        store
            .roll_hit_protection(&member, creation.id, None)
            .await
            .unwrap();
        store
            .roll_attributes(&member, creation.id, None)
            .await
            .unwrap();
        let _connection = game.connect(&member).await.unwrap();

        game.player_message(member.clone(), "Begin play.")
            .await
            .unwrap();

        let chat = store.player_chat(&member).await.unwrap();
        assert!(chat.iter().any(|entry| {
            entry.role == Role::Narration && entry.text == "The charcoal hut is quiet."
        }));
        assert!(
            !chat
                .iter()
                .any(|entry| entry.role == Role::Assistant && entry.text.is_empty()),
            "the silent completion must not become a blank player-chat entry"
        );
        {
            let requests = requests.lock().unwrap();
            let final_request = requests
                .last()
                .expect("player agent must settle after the tool");
            assert!(matches!(
                final_request.messages.as_slice(),
                messages if messages.windows(2).any(|pair| matches!(
                    pair,
                    [
                        Message { content: MessageContent::ToolResult { content, .. }, .. },
                        Message { role: Role::Narration, content: MessageContent::Text(narration), .. },
                    ] if content == "Character creation complete."
                        && narration == "The charcoal hut is quiet."
                ))
            ));
        }
        assert!(
            game.opening_complete(&member).await.unwrap(),
            "a durable silent completion must prevent reopening character creation"
        );
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
        device_test_game_for("dev-qwen35").await
    }

    async fn device_test_game_for(name: &str) -> (MistralRsBackend, String, Sampling, Limits) {
        let _ = tracing_subscriber::fmt()
            .with_env_filter("cairnworld=info")
            .with_test_writer()
            .try_init();
        let settings = Settings::for_device_test(name).expect("default.toml should load");
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
        let game = Game::new(
            store.clone(),
            World::load(&store).await.unwrap(),
            scheduler.foreground(),
            limits,
            model,
            sampling,
        );
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
            response(Content::Text(String::new())),
        ]);
        let backend = InferenceScheduler::new(scripted_backend, Limits::default()).unwrap();
        let game = Game::new(
            store.clone(),
            World::load(&store).await.unwrap(),
            backend.foreground(),
            Limits::default(),
            "scripted".into(),
            Sampling {
                temperature: 0.0,
                ..Default::default()
            },
        );
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
            ChatChange::Entries
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
        let sequence: i64 = sqlx::query_scalar(
            "SELECT id FROM sequence WHERE world_id = ? ORDER BY id DESC LIMIT 1",
        )
        .bind(installed.world_id)
        .fetch_one(&store.pool())
        .await
        .unwrap();
        assert_eq!(
            chat[narration].sequence_id,
            Some(sequence),
            "a delivered GM narration must retain its producing turn so its chat entry opens Developer Mode directly"
        );
        assert!(
            chat.get(narration + 1).is_none(),
            "an empty post-tool completion leaves the GM narration as the final player-visible event"
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
                    .live_segments(store.agent_branch(installed.member.agent_id).await.unwrap())
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
        assert!(
            gm_inference
                .segments
                .iter()
                .any(|segment| matches!(segment, crate::store::Segment::Context { .. })),
            "the recorded GM request must keep its current scene as a distinct context segment"
        );
        // The player message remains inspectable, while the silent completion
        // stays in durable history rather than becoming a blank chat entry.
        let player_line = chat
            .iter()
            .find(|entry| entry.text == "I take the flour sack.")
            .expect("the player's own message is stored");
        assert_eq!(player_line.sequence_id, player_inference.sequence_id);
        assert!(
            !chat
                .iter()
                .any(|entry| entry.role == Role::Assistant && entry.text.is_empty()),
            "player chat must omit a silent completion"
        );
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
        assert!(
            !live_entries
                .iter()
                .any(|entry| entry.role == Role::Assistant && entry.text.is_empty()),
            "the live cursor must omit a silent completion too"
        );
        {
            let requests = recorded_requests.lock().unwrap();
            let gm_request = requests
                .iter()
                .find(|request| {
                    request.messages.last().is_some_and(|message| {
                        matches!(&message.content, MessageContent::Text(text)
                            if text.starts_with("Adventurer") && text.contains("takes action"))
                    })
                })
                .expect("the GM arbitration must end with the action that started it");
            let MessageContent::Text(gm_packet) = &gm_request.messages[0].content else {
                panic!("the GM request must begin with its role and scene packet");
            };
            assert!(
                gm_packet.starts_with(GM_ROLE),
                "the GM request must use the single GM role prompt: {gm_packet}"
            );
            for name in &player_names {
                assert!(
                    gm_packet.contains(&format!("\"name\":\"{name}\"")),
                    "the GM packet must name every player character in its location"
                );
            }
            let acting = &player_names[0];
            assert!(
                matches!(&gm_request.messages.last().unwrap().content, MessageContent::Text(text)
                    if text.starts_with(&format!("{acting} takes action"))),
                "the GM's newest message must name the character who took the action"
            );
        }

        store
            .set_developer_mode(owner.id, installed.world_id, true)
            .await
            .unwrap();
        let gm_history = store.player_group_history(&installed.member).await.unwrap();
        let gm_cursor = store
            .first_raw_history_cursor(
                owner.id,
                installed.world_id,
                gm_history.agent_id,
                Some(gm_history.branch_id),
            )
            .await
            .unwrap();
        let (gm_history, _) = store
            .agent_raw_history(owner.id, gm_cursor, 100)
            .await
            .unwrap();
        assert!(
            gm_history.iter().any(|entry| matches!(entry,
                crate::store::RawHistoryEntry::Message {
                    role: Role::User,
                    content: MessageContent::Text(text),
                    ..
                } if text.contains("takes action 1"))),
            "Developer Mode must show the action that started the GM turn"
        );
        assert!(
            gm_history.iter().any(|entry| matches!(entry,
                crate::store::RawHistoryEntry::Message {
                    role: Role::Assistant,
                    content: MessageContent::Text(text),
                    ..
                } if text == "You lift the flour sack while Toma watches.")),
            "Developer Mode must show the GM's response after its action event"
        );

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
        let game = Game::new(
            store.clone(),
            World::load(&store).await.unwrap(),
            backend.foreground(),
            Limits::default(),
            "scripted".into(),
            Sampling {
                temperature: 0.0,
                ..Default::default()
            },
        );
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
                MessageContent::ToolResult {
                    tool_call_id,
                    content,
                } if tool_call_id == "reject-1" => Some(content.clone()),
                _ => None,
            })
            .expect("the GM's own next turn must see reject-1's tool result");
        assert!(
            reject_result.contains("already approved")
                && reject_result.contains("cannot now be rejected"),
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
            response(Content::Text(
                "Toma watches as you step outside with Mara.".into(),
            )),
            response(Content::Text("What do you do next?".into())),
        ]);
        let backend = InferenceScheduler::new(scripted_backend, Limits::default()).unwrap();
        let game = Game::new(
            store.clone(),
            World::load(&store).await.unwrap(),
            backend.foreground(),
            Limits::default(),
            "scripted".into(),
            Sampling {
                temperature: 0.0,
                ..Default::default()
            },
        );

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
                    MessageContent::ToolResult {
                        tool_call_id,
                        content,
                    } if tool_call_id == "split-1" => Some(content.clone()),
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
        let parent_branch_id: i64 =
            sqlx::query_scalar("SELECT branch_id FROM \"group\" WHERE id = ?")
                .bind(original_group_id)
                .fetch_one(&store.pool())
                .await
                .unwrap();
        let new_branch_id: i64 = sqlx::query_scalar("SELECT branch_id FROM \"group\" WHERE id = ?")
            .bind(adventurer_group)
            .fetch_one(&store.pool())
            .await
            .unwrap();
        let linked_parent: i64 =
            sqlx::query_scalar("SELECT parent_branch_id FROM history_parent WHERE branch_id = ?")
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
        let split_game = Game::new(
            store.clone(),
            World::load(&store).await.unwrap(),
            split_backend.foreground(),
            Limits::default(),
            "scripted".into(),
            Sampling {
                temperature: 0.0,
                ..Default::default()
            },
        );
        split_game
            .player_message(installed.member.clone(), "I look, then we split up.")
            .await
            .unwrap();

        let adventurer_group = store
            .character_group_id(installed.member.character_id)
            .await
            .unwrap();
        let toma_group = store.character_group_id(toma_id).await.unwrap();
        assert_ne!(
            adventurer_group, toma_group,
            "the split must have taken effect"
        );

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
        let merge_game = Game::new(
            store.clone(),
            World::load(&store).await.unwrap(),
            merge_backend.foreground(),
            Limits::default(),
            "scripted".into(),
            Sampling {
                temperature: 0.0,
                ..Default::default()
            },
        );
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
                    MessageContent::ToolResult {
                        tool_call_id,
                        content,
                    } if tool_call_id == "merge-1" => Some(content.clone()),
                    _ => None,
                })
                .expect("the GM's own next turn must see merge-1's tool result")
        };
        assert!(merge_result.contains("merged into group"), "{merge_result}");
        {
            let requests = merge_requests.lock().unwrap();
            let request = requests
                .iter()
                .find(|request| {
                    matches!(request.messages.last().map(|message| &message.content),
                        Some(MessageContent::Text(text)) if text.contains("takes action 2"))
                })
                .expect("the second GM turn must end with its action event");
            let prior_event = request
                .messages
                .iter()
                .position(|message| {
                    matches!(&message.content, MessageContent::Text(text) if text.contains("takes action 1"))
                })
                .expect("the merged GM history must retain the first action event");
            let prior_tool_call = request
                .messages
                .iter()
                .position(|message| {
                    matches!(&message.content, MessageContent::ToolCalls(calls)
                        if calls.iter().any(|call| call.id == "approve-1"))
                })
                .expect("the merged GM history must retain the first action's tool call");
            assert!(
                prior_event < prior_tool_call,
                "the first action event must precede the tool call that resolved it"
            );
            assert!(
                matches!(request.messages.last().map(|message| &message.content),
                    Some(MessageContent::Text(text)) if text.contains("takes action 2")),
                "the newest GM message must be the second action event"
            );
            assert!(
                matches!(request.messages.first().map(|message| &message.content),
                    Some(MessageContent::Text(text)) if text.starts_with(GM_ROLE)),
                "all GM turns must begin with the one GM role prompt"
            );
        }

        let merged_adventurer = store
            .character_group_id(installed.member.character_id)
            .await
            .unwrap();
        let merged_toma = store.character_group_id(toma_id).await.unwrap();
        let merged_mara = store.character_group_id(mara_id).await.unwrap();
        assert_eq!(
            merged_adventurer, merged_toma,
            "the merge must reunite all characters"
        );
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
        let (
            store,
            _db,
            _member,
            adventurer_group,
            toma_group,
            merged_adventurer,
            _original_group_id,
        ) = split_then_merge_groups().await;
        assert_ne!(
            merged_adventurer, adventurer_group,
            "merge retires the sources, not reuses one"
        );
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
        assert_eq!(
            parents.len(),
            2,
            "the merged branch must bound both source branches"
        );
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

    /// Run a real GM against the merged branch DAG and a real pending `say`
    /// action. The player-side tool call is recorded directly because this
    /// test verifies GM context and action resolution, not a small model's
    /// unrelated choice of player tools.
    #[tokio::test]
    #[ignore = "device test: needs a CUDA GPU and the models/ GGUF from default.toml"]
    async fn real_model_assembles_context_after_a_split_and_merge() {
        let (store, _db, member, ..) = split_then_merge_groups().await;
        let sequence = store
            .begin_sequence(member.world_id, "real GM context test")
            .await
            .unwrap();
        let requesting_tool_execution_id = crate::store::record_tool_call_for_test(
            &store,
            store.agent_branch(member.agent_id).await.unwrap(),
            Some(sequence.id),
            "say-1",
            "say",
            r#"{"text":"Good to have us all back together."}"#,
        )
        .await;
        let pending = store
            .create_pending_action(
                &store.player_actor(&member).await.unwrap(),
                sequence.id,
                requesting_tool_execution_id,
                "say",
                r#"{"text":"Good to have us all back together."}"#,
            )
            .await
            .unwrap();
        let (real_backend, model_path, sampling, _limits) = device_test_game().await;
        let limits = Limits::default();
        let real_game = Game::new(
            store.clone(),
            World::load(&store).await.unwrap(),
            InferenceScheduler::new(real_backend, Limits::default())
                .unwrap()
                .foreground(),
            limits,
            model_path,
            sampling,
        );
        real_game
            .arbitrate(
                pending.clone(),
                Budget::new(limits),
                CallContext::root(Some(sequence.id)),
            )
            .await
            .expect("a real GM turn must resolve the pending action on the merged branch DAG");
        assert!(
            !store
                .has_pending_action(member.world_id, pending.id)
                .await
                .unwrap(),
            "the real GM must approve or reject the presented action"
        );
        let mut gm_request = None;
        let gm_inference_ids: Vec<i64> = sqlx::query_scalar(
            "SELECT id FROM inference WHERE sequence_id = ? AND agent_id = ? ORDER BY ordinal",
        )
        .bind(sequence.id)
        .bind(pending.gm_agent_id)
        .fetch_all(&store.pool())
        .await
        .unwrap();
        for inference_id in gm_inference_ids {
            let request = store
                .reconstruct_inference(inference_id)
                .await
                .expect("the real GM request should be reconstructable");
            if matches!(request.request.messages.last().map(|message| &message.content),
                Some(MessageContent::Text(text)) if text.contains("takes action"))
            {
                gm_request = Some(request);
                break;
            }
        }
        let gm_request = gm_request.expect("the real GM must receive the action event");
        assert!(
            matches!(gm_request.request.messages.last().map(|message| &message.content),
                Some(MessageContent::Text(text)) if text.contains("takes action")),
            "the real GM request must end with the action event"
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
        let game = Game::new(
            store.clone(),
            World::load(&store).await.unwrap(),
            backend.foreground(),
            Limits::default(),
            "scripted".into(),
            Sampling {
                temperature: 0.0,
                ..Default::default()
            },
        );
        game.player_message(installed.member.clone(), "I look around, then leave alone.")
            .await
            .unwrap();

        let split_result = {
            let requests = recorded_requests.lock().unwrap();
            requests
                .iter()
                .flat_map(|request| request.messages.iter())
                .find_map(|message| match &message.content {
                    MessageContent::ToolResult {
                        tool_call_id,
                        content,
                    } if tool_call_id == "split-1" => Some(content.clone()),
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
        assert!(
            retired_at.is_none(),
            "a rejected split must not retire the group"
        );
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
        let game = Game::new(
            store.clone(),
            World::load(&store).await.unwrap(),
            backend.foreground(),
            Limits::default(),
            "scripted".into(),
            Sampling {
                temperature: 0.0,
                ..Default::default()
            },
        );
        game.player_message(
            installed.member.clone(),
            "I look, then someone tries to split us twice.",
        )
        .await
        .unwrap();

        let (first_result, second_result) = {
            let requests = recorded_requests.lock().unwrap();
            let find = |call_id: &str| {
                requests
                    .iter()
                    .flat_map(|request| request.messages.iter())
                    .find_map(|message| match &message.content {
                        MessageContent::ToolResult {
                            tool_call_id,
                            content,
                        } if tool_call_id == call_id => Some(content.clone()),
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
        let (game, store, _db, member) = blank_opening_game(backend).await;

        let response = game.enter(member.clone()).await.unwrap().unwrap();
        assert!(
            matches!(response.content, Content::Text(text) if text == "Welcome. Let us make your Adventurer.")
        );
        let chat = store.player_chat(&member).await.unwrap();
        assert_eq!(chat.len(), 1, "reload must show only the spoken welcome");
        assert_eq!(chat[0].text, "Welcome. Let us make your Adventurer.");
        assert_eq!(
            store
                .player_chat_after(&member, Some(chat[0].id))
                .await
                .unwrap(),
            vec![]
        );
        store
            .set_developer_mode(member.user_id, member.world_id, true)
            .await
            .unwrap();
        let cursor = store
            .first_raw_history_cursor(member.user_id, member.world_id, member.agent_id, None)
            .await
            .unwrap();
        let (history, _) = store
            .agent_raw_history(member.user_id, cursor, 100)
            .await
            .unwrap();
        assert!(
            history.iter().any(|entry| matches!(entry,
                crate::store::RawHistoryEntry::Message {
                    role: Role::User,
                    origin: crate::store::MessageOrigin::Instruction,
                    content: MessageContent::Text(text),
                    ..
                } if text == "The player has entered the world and has not chosen to roll yet. Speak directly to them."
            )),
            "the opening event must appear as an instruction in agent history: {history:?}"
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
                        origin: crate::store::MessageOrigin::Instruction,
                        content: MessageContent::Text(text),
                        ..
                    },
                ] if text == "The player has entered the world and has not chosen to roll yet. Speak directly to them."
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
        let generation_after_second: i64 =
            sqlx::query_scalar("SELECT generation FROM character_presence WHERE character_id = ?")
                .bind(member.character_id)
                .fetch_one(&store.pool())
                .await
                .unwrap();
        assert_eq!(
            generation_after_second, 1,
            "a second connection for an already-present character is not a new transition"
        );

        game.disconnect(&member, connection_id).await.unwrap();
        let generation_after_one_close: i64 =
            sqlx::query_scalar("SELECT generation FROM character_presence WHERE character_id = ?")
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
        game.disconnect(&member, second_connection_id)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn presence_never_narrates_an_unready_adventurers_arrival() {
        let (backend, requests) = ScriptedBackend::recording([]);
        let (game, _store, _db, member) = blank_opening_game(backend).await;
        game.connect(&member).await.unwrap();
        let_presence_handlers_run(&game, &member).await;

        assert!(
            requests.lock().unwrap().is_empty(),
            "character creation must finish through ready_to_begin before any presence narration can call the GM"
        );
    }

    #[tokio::test]
    async fn failed_presence_write_does_not_register_a_connection() {
        let (backend, _) = ScriptedBackend::recording([]);
        let (game, _store, _db, mut member) = blank_opening_game(backend).await;
        member.character_id = i64::MAX;
        for _ in 0..2 {
            assert!(
                game.connect(&member).await.is_err(),
                "a failed durable transition must not make a later connection appear registered"
            );
        }
    }

    #[tokio::test]
    async fn worlds_are_loaded_at_construction_and_released_with_the_game() {
        let (backend, _) = ScriptedBackend::recording([]);
        let (game, _store, _db, member) = blank_opening_game(backend).await;
        let (settled, processed) = oneshot::channel();
        game.world(member.world_id)
            .unwrap()
            .sender
            .send(WorldEvent::Settled(settled))
            .await
            .unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(10), processed)
            .await
            .expect("a world must process its queue after construction")
            .unwrap();
        let world = Arc::downgrade(&game.world(member.world_id).unwrap());
        assert_eq!(game.worlds.read().unwrap().len(), 1);
        let closed = game.world(member.world_id).unwrap().sender.clone();
        drop(game);
        closed.closed().await;
        assert!(
            world.upgrade().is_none(),
            "idle world tasks must not retain their owner"
        );
    }

    #[tokio::test]
    async fn presence_survives_a_full_action_queue_during_inference() {
        let (backend, started, release, requests) = DelayedBackend::new();
        let (game, store, _db, first, second) = two_player_bread_thief_game(backend).await;
        let pending_game = Arc::clone(&game);
        let member = first.clone();
        let turn = tokio::spawn(async move {
            pending_game
                .player_message(member, "Describe the scene.")
                .await
        });
        started.notified().await;

        let world = game.world(first.world_id).unwrap();
        let mut completions = Vec::new();
        while world.sender.capacity() > 0 {
            let (reply, receive) = oneshot::channel();
            world.sender.try_send(WorldEvent::Settled(reply)).unwrap();
            completions.push(receive);
        }
        // More transitions than the action queue can hold, while its consumer
        // is held inside a real game inference. Transport changes must finish.
        for _ in 0..70 {
            let connection = game.connect(&first).await.unwrap();
            game.disconnect(&first, connection).await.unwrap();
        }
        let _first_connection = game.connect(&first).await.unwrap();
        let _second_connection = game.connect(&second).await.unwrap();
        assert_eq!(requests.load(Ordering::SeqCst), 1);

        release.notify_one();
        turn.await.unwrap().unwrap();
        started.notified().await;
        release.notify_one();
        for completed in completions {
            completed.await.unwrap();
        }
        let_presence_handlers_run(&game, &first).await;
        let group_id = store.character_group_id(first.character_id).await.unwrap();
        let announced = store.presence_narration(group_id).await.unwrap();
        assert_eq!(
            announced.last_announced_character_ids,
            [first.character_id, second.character_id]
        );
        assert_eq!(
            requests.load(Ordering::SeqCst),
            2,
            "the blocked turn and one consolidated arrival narration"
        );
    }

    #[tokio::test]
    async fn shutdown_finishes_active_and_queued_turns_and_refuses_later_input() {
        let (backend, started, release, requests) = DelayedBackend::new();
        let (game, store, _db, first, second) = two_player_bread_thief_game(backend).await;
        let active_game = Arc::clone(&game);
        let active_member = first.clone();
        let active = tokio::spawn(async move {
            active_game
                .player_message(active_member, "Describe the scene.")
                .await
        });
        started.notified().await;
        let queued_game = Arc::clone(&game);
        let queued_member = second.clone();
        let queued = tokio::spawn(async move {
            queued_game
                .player_message(queued_member, "Look around.")
                .await
        });
        let world = game.world(first.world_id).unwrap();
        while world.sender.capacity() == world.sender.max_capacity() {
            tokio::task::yield_now().await;
        }

        let stopping_game = Arc::clone(&game);
        let shutdown = tokio::spawn(async move { stopping_game.shutdown().await });
        let mut stopping = game.stopping.subscribe();
        world::stopped(&mut stopping).await;
        let refused = game
            .player_message(first.clone(), "Too late.")
            .await
            .expect_err("input after shutdown began must be refused");
        assert!(
            format!("{refused:#}").contains("server is stopping"),
            "the refusal must say why: {refused:#}"
        );
        assert!(
            !shutdown.is_finished(),
            "shutdown must wait for the accepted turns"
        );

        release.notify_one();
        started.notified().await;
        release.notify_one();
        active.await.unwrap().unwrap();
        queued.await.unwrap().unwrap();
        shutdown.await.unwrap().unwrap();
        assert_eq!(
            requests.load(Ordering::SeqCst),
            2,
            "each accepted turn ran once"
        );
        let chat = store.player_chat(&first).await.unwrap();
        assert!(
            !chat.iter().any(|entry| entry.text == "Too late."),
            "a refused message must not enter the chat"
        );
    }

    #[tokio::test]
    async fn a_failed_world_does_not_abort_other_worlds_draining_during_shutdown() {
        let (backend, started, release, _requests) = DelayedBackend::new();
        let (game, store, _db, failing, _) = two_player_bread_thief_game(backend).await;
        let owner = store
            .find_or_create_user("other@example.test", "Other")
            .await
            .unwrap();
        let scenario = Scenario::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/scenarios/bread_thief.json"
        ))
        .unwrap();
        let other = store.install_scenario(&owner, &scenario).await.unwrap();
        game.start_world(World::install(&game.worlds, other.world_id));

        let draining_game = Arc::clone(&game);
        let draining_member = other.member.clone();
        let draining = tokio::spawn(async move {
            draining_game
                .player_message(draining_member, "Hello.")
                .await
        });
        started.notified().await;
        game.world(failing.world_id)
            .unwrap()
            .sender
            .send(WorldEvent::Panic)
            .await
            .unwrap();

        let stopping_game = Arc::clone(&game);
        let shutdown = tokio::spawn(async move { stopping_game.shutdown().await });
        let mut stopping = game.stopping.subscribe();
        world::stopped(&mut stopping).await;
        release.notify_one();
        draining
            .await
            .unwrap()
            .expect("the healthy world must finish its accepted turn");
        let error = shutdown
            .await
            .unwrap()
            .expect_err("a worker failure must be reported");
        assert!(
            format!("{error:#}").contains("world worker failed"),
            "{error:#}"
        );
    }

    #[tokio::test]
    async fn a_failed_turn_shows_its_system_error_to_that_player_only() {
        let (backend, _) = ScriptedBackend::recording([]);
        let (game, store, _db, first, second) = two_player_bread_thief_game(backend).await;

        game.player_message(first.clone(), "Describe the scene.")
            .await
            .expect_err("the backend has no response, so the turn fails");

        let chat = store.player_chat(&first).await.unwrap();
        let positions: Vec<_> = chat
            .iter()
            .map(|entry| (entry.role.clone(), entry.text.as_str()))
            .collect();
        let [
            ..,
            (Role::User, "Describe the scene."),
            (Role::System, error),
        ] = positions.as_slice()
        else {
            panic!("the error must follow the message that started the turn: {positions:?}");
        };
        assert!(error.contains("unexpected inference"), "{error}");
        let error_entry = chat.last().unwrap();
        assert!(
            store
                .player_chat_after(&first, Some(error_entry.id))
                .await
                .unwrap()
                .is_empty(),
            "a reader past the trigger message does not see the error again"
        );
        assert!(
            !store
                .player_chat(&second)
                .await
                .unwrap()
                .iter()
                .any(|entry| entry.role == Role::System),
            "another player in the group must not see this player's failed turn"
        );
    }

    /// The GM's approval result reports what happened to the approved action,
    /// including a failure to apply it; the acting player's own call receives
    /// that failure in place of its usual result, and the turn carries on.
    #[tokio::test]
    async fn an_approved_take_that_cannot_happen_is_reported_to_the_gm_and_the_player() {
        let (backend, requests) = ScriptedBackend::recording([
            response(Content::ToolCalls(vec![ToolCall {
                id: "take-1".into(),
                name: "take".into(),
                arguments: r#"{"item":"moon cheese"}"#.into(),
            }])),
            response(Content::ToolCalls(vec![ToolCall {
                id: "approve-1".into(),
                name: "approve_action".into(),
                arguments: r#"{"action_id":1}"#.into(),
            }])),
            response(Content::Text("The shelf is bare.".into())),
            response(Content::Text(String::new())),
        ]);
        let (game, store, _db, member) = blank_opening_game(backend).await;
        let _connection = game.connect(&member).await.unwrap();
        let creation = store
            .begin_sequence(member.world_id, "creation test")
            .await
            .unwrap();
        store
            .roll_hit_protection(&member, creation.id, None)
            .await
            .unwrap();
        store
            .roll_attributes(&member, creation.id, None)
            .await
            .unwrap();
        store
            .ready_to_begin(&member, creation.id, None)
            .await
            .unwrap();

        game.player_message(member.clone(), "I take the moon cheese.")
            .await
            .expect("an action that cannot happen is not a system failure");

        let tool_results = |request: &crate::llm::Request| -> Vec<String> {
            request
                .messages
                .iter()
                .filter_map(|message| match &message.content {
                    MessageContent::ToolResult { content, .. } => Some(content.clone()),
                    _ => None,
                })
                .collect()
        };
        let (gm_report, player_result) = {
            let requests = requests.lock().unwrap();
            (tool_results(&requests[2]), tool_results(&requests[3]))
        };
        assert!(
            matches!(gm_report.as_slice(), [report]
                if report.contains("approved, but it could not be applied")
                    && report.contains("moon cheese")),
            "the GM must be told what happened to the action it approved: {gm_report:?}"
        );
        assert!(
            matches!(player_result.as_slice(), [result]
                if result.contains("The action could not be applied")
                    && result.contains("moon cheese")),
            "the caller receives the failure in place of its usual result: {player_result:?}"
        );
        let sequence_error: Option<String> =
            sqlx::query_scalar("SELECT error FROM sequence ORDER BY id DESC LIMIT 1")
                .fetch_one(&store.pool())
                .await
                .unwrap();
        assert_eq!(sequence_error, None);
        let (held,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM item_character")
            .fetch_one(&store.pool())
            .await
            .unwrap();
        assert_eq!(held, 0, "nothing moves for an action that cannot happen");
    }

    #[tokio::test]
    async fn shutdown_reports_every_failure_not_only_the_first() {
        let (backend, _) = ScriptedBackend::recording([]);
        let (game, store, _db, first_world_member, _) = two_player_bread_thief_game(backend).await;
        let owner = store
            .find_or_create_user("other@example.test", "Other")
            .await
            .unwrap();
        let scenario = Scenario::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/scenarios/bread_thief.json"
        ))
        .unwrap();
        let other = store.install_scenario(&owner, &scenario).await.unwrap();
        game.start_world(World::install(&game.worlds, other.world_id));
        for world_id in [first_world_member.world_id, other.world_id] {
            game.world(world_id)
                .unwrap()
                .sender
                .send(WorldEvent::Panic)
                .await
                .unwrap();
        }

        let error = game.shutdown().await.expect_err("both workers failed");

        let report = format!("{error:#}");
        assert_eq!(
            report.matches("world worker failed").count(),
            2,
            "every failure must be reported: {report}"
        );
    }

    /// Every connected tab of the character must be told when a turn fails:
    /// a tab whose cursor already passed the turn's trigger message learns of
    /// the error only from this signal.
    #[tokio::test]
    async fn a_failed_player_turn_notifies_every_subscriber() {
        let (backend, _) = ScriptedBackend::recording([]);
        let (game, _store, _db, first, _) = two_player_bread_thief_game(backend).await;
        let mut other_tab = game.subscribe(&first).await.unwrap();

        game.player_message(first.clone(), "Describe the scene.")
            .await
            .expect_err("the backend has no response, so the turn fails");

        let mut changes = Vec::new();
        while let Ok(change) = other_tab.try_recv() {
            changes.push(change);
        }
        assert!(
            changes
                .iter()
                .any(|change| matches!(change, ChatChange::Failed { .. })),
            "the failure must be announced, not only new entries: {changes:?}"
        );
    }

    fn tool_call(name: &str, arguments: serde_json::Value) -> Response {
        response(Content::ToolCalls(vec![ToolCall {
            id: format!("{name}-call"),
            name: name.into(),
            arguments: arguments.to_string(),
        }]))
    }

    /// The text of each speech entry in a player's chat.
    async fn heard(store: &Store, member: &PlayerAgent) -> Vec<String> {
        store
            .player_chat(member)
            .await
            .unwrap()
            .into_iter()
            .filter(|entry| entry.role == Role::Speech)
            .map(|entry| entry.text)
            .collect()
    }

    /// The speech messages in an NPC's own history, as its model would see them.
    async fn heard_by_npc(store: &Store, name: &str) -> Vec<String> {
        let agent_id: i64 = sqlx::query_scalar(
            "SELECT npc_agent.agent_id FROM npc_agent \
             JOIN character ON character.id = npc_agent.character_id WHERE character.name = ?",
        )
        .bind(name)
        .fetch_one(&store.pool())
        .await
        .unwrap();
        let branch = store.agent_branch(agent_id).await.unwrap();
        let messages: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM history_entry WHERE branch_id = ? AND kind = 'message'",
        )
        .bind(branch.branch_id())
        .fetch_one(&store.pool())
        .await
        .unwrap();
        if messages == 0 {
            // A model request needs something to answer, so there is none to read.
            return Vec::new();
        }
        let segments = store.live_segments(branch).await.unwrap();
        store
            .request_for_segments(agent_id, &segments, Sampling::default())
            .await
            .unwrap()
            .messages
            .into_iter()
            .filter(|message| message.role == Role::Speech)
            .filter_map(|message| match message.content {
                MessageContent::Text(text) => Some(text),
                _ => None,
            })
            .collect()
    }

    /// The tool results a recorded request showed its model.
    fn tool_results(request: &crate::llm::Request) -> Vec<String> {
        request
            .messages
            .iter()
            .filter_map(|message| match &message.content {
                MessageContent::ToolResult { content, .. } => Some(content.clone()),
                _ => None,
            })
            .collect()
    }

    struct SpeechScene {
        game: Arc<Game<ScriptedBackend>>,
        store: Store,
        _db: crate::store::TestDatabase,
        first: PlayerAgent,
        second: PlayerAgent,
        /// The character names of `first` and `second`.
        names: [String; 2],
        responses: Arc<Mutex<VecDeque<Response>>>,
        requests: Arc<Mutex<Vec<crate::llm::Request>>>,
    }

    /// Two ready players and the NPC Toma in one group, with an empty script.
    async fn speech_scene() -> SpeechScene {
        let (backend, requests) = ScriptedBackend::recording([]);
        let responses = Arc::clone(&backend.responses);
        let (game, store, db, first, second) = two_player_bread_thief_game(backend).await;
        let names = [
            store.player_scene(&first).await.unwrap().character_name,
            store.player_scene(&second).await.unwrap().character_name,
        ];
        SpeechScene {
            game,
            store,
            _db: db,
            first,
            second,
            names,
            responses,
            requests,
        }
    }

    impl SpeechScene {
        /// `first` speaks as `speech`, the GM approves with `approval`,
        /// narrates, and the player agent finishes.
        async fn speak(&self, speech: Response, approval: serde_json::Value) {
            self.responses.lock().unwrap().extend([
                speech,
                tool_call("approve_action", approval),
                response(Content::Text("The room goes quiet.".into())),
                response(Content::Text(String::new())),
            ]);
            self.game
                .player_message(self.first.clone(), "Say something.")
                .await
                .unwrap();
        }
    }

    #[tokio::test]
    async fn spoken_words_reach_every_hearer_attributed_and_the_speaker_once() {
        let scene = speech_scene().await;
        scene
            .speak(
                tool_call(
                    "say",
                    serde_json::json!({"words": "I know who took the bread.", "to": "Toma"}),
                ),
                serde_json::json!({"action_id": 1}),
            )
            .await;
        let expected = format!(
            "{} says to Toma: I know who took the bread.",
            scene.names[0]
        );

        // Another player who has no browser open still hears it, exactly once.
        assert_eq!(
            heard(&scene.store, &scene.second).await,
            std::slice::from_ref(&expected)
        );
        // The speaker sees their own words once, not an echo beside another copy.
        assert_eq!(
            heard(&scene.store, &scene.first).await,
            std::slice::from_ref(&expected)
        );
        // An NPC in the group has it in its own history, with the same words.
        assert_eq!(heard_by_npc(&scene.store, "Toma").await, [expected]);
        // The GM learns of it through its approval result.
        let gm_report = tool_results(&scene.requests.lock().unwrap()[2]);
        assert!(
            matches!(gm_report.as_slice(), [report]
                if report.contains("Delivered as spoken to")
                    && report.contains("Toma")),
            "{gm_report:?}"
        );
        // The speaker's and the GM's histories stay valid: every tool call is
        // answered directly after it, with the speaker's own copy of the words
        // arriving after its result.
        let group = scene
            .store
            .character_group_id(scene.first.character_id)
            .await
            .unwrap();
        for branch in [
            scene
                .store
                .agent_branch(scene.first.agent_id)
                .await
                .unwrap(),
            scene.store.group_branch(group).await.unwrap(),
        ] {
            let segments = scene.store.live_segments(branch).await.unwrap();
            let messages = scene
                .store
                .request_for_segments(branch.agent_id(), &segments, Sampling::default())
                .await
                .unwrap()
                .messages;
            for (index, message) in messages.iter().enumerate() {
                if matches!(message.content, MessageContent::ToolCalls(_)) {
                    assert!(
                        matches!(
                            messages[index + 1].content,
                            MessageContent::ToolResult { .. }
                        ),
                        "a tool call must be followed directly by its result: {messages:?}"
                    );
                }
            }
        }
        // Who was addressed is kept as a character, not only as a name.
        let (addressed_id, addressed_name): (i64, String) =
            sqlx::query_as("SELECT addressed_character_id, addressed_name FROM speech")
                .fetch_one(&scene.store.pool())
                .await
                .unwrap();
        let toma: i64 = sqlx::query_scalar("SELECT id FROM character WHERE name = 'Toma'")
            .fetch_one(&scene.store.pool())
            .await
            .unwrap();
        assert_eq!((addressed_id, addressed_name), (toma, "Toma".to_string()));
        // One utterance is stored once and shared by every history.
        let contents: i64 = sqlx::query_scalar(
            "SELECT COUNT(DISTINCT message.content) FROM speech_message \
             JOIN message ON message.id = speech_message.message_id",
        )
        .fetch_one(&scene.store.pool())
        .await
        .unwrap();
        assert_eq!(contents, 1);
    }

    #[tokio::test]
    async fn a_whisper_is_heard_only_by_its_listeners_and_the_speaker() {
        let scene = speech_scene().await;
        scene
            .speak(
                tool_call(
                    "whisper",
                    serde_json::json!({"words": "Meet me at the mill.", "to": [scene.names[1]]}),
                ),
                serde_json::json!({"action_id": 1}),
            )
            .await;

        let expected = format!("{} whispers: Meet me at the mill.", scene.names[0]);
        assert_eq!(
            heard(&scene.store, &scene.second).await,
            std::slice::from_ref(&expected)
        );
        assert_eq!(heard(&scene.store, &scene.first).await, [expected]);
        assert!(
            heard_by_npc(&scene.store, "Toma").await.is_empty(),
            "an NPC that was not a listener must not receive the whisper"
        );
    }

    #[tokio::test]
    async fn the_gm_can_exclude_a_hearer_from_ordinary_speech() {
        let scene = speech_scene().await;
        scene
            .speak(
                tool_call("say", serde_json::json!({"words": "Stand back."})),
                serde_json::json!({"action_id": 1, "exclude": [scene.names[1]]}),
            )
            .await;

        assert!(heard(&scene.store, &scene.second).await.is_empty());
        assert_eq!(heard_by_npc(&scene.store, "Toma").await.len(), 1);
    }

    #[tokio::test]
    async fn an_invalid_audience_is_returned_to_the_gm_and_nothing_is_resolved() {
        let scene = speech_scene().await;
        scene.responses.lock().unwrap().extend([
            tool_call("say", serde_json::json!({"words": "Hello."})),
            tool_call(
                "approve_action",
                serde_json::json!({"action_id": 1, "include": ["Nobody"]}),
            ),
            tool_call("approve_action", serde_json::json!({"action_id": 1})),
            response(Content::Text("A greeting.".into())),
            response(Content::Text(String::new())),
        ]);

        scene
            .game
            .player_message(scene.first.clone(), "Greet them.")
            .await
            .unwrap();

        let refusal = tool_results(&scene.requests.lock().unwrap()[2])
            .pop()
            .unwrap();
        assert!(
            refusal.contains("rejected") && refusal.contains("Nobody"),
            "the GM must see why the audience was refused: {refusal}"
        );
        assert_eq!(
            heard(&scene.store, &scene.second).await.len(),
            1,
            "the retry resolved it"
        );
    }

    #[tokio::test]
    async fn contradictory_or_impossible_audience_overrides_are_refused() {
        let scene = speech_scene().await;
        let listener = scene.names[1].clone();
        let speaker = scene.names[0].clone();
        scene.responses.lock().unwrap().extend([
            tool_call("say", serde_json::json!({"words": "Hello."})),
            tool_call(
                "approve_action",
                serde_json::json!({"action_id": 1, "include": [listener], "exclude": [listener]}),
            ),
            tool_call(
                "approve_action",
                serde_json::json!({"action_id": 1, "exclude": [speaker]}),
            ),
            tool_call("approve_action", serde_json::json!({"action_id": 1})),
            response(Content::Text("A greeting.".into())),
            response(Content::Text(String::new())),
        ]);

        scene
            .game
            .player_message(scene.first.clone(), "Greet them.")
            .await
            .unwrap();

        let requests = scene.requests.lock().unwrap();
        let both = tool_results(&requests[2]).pop().unwrap();
        let speaker_excluded = tool_results(&requests[3]).pop().unwrap();
        assert!(both.contains("both included and excluded"), "{both}");
        assert!(
            speaker_excluded.contains("hears their own words"),
            "{speaker_excluded}"
        );
    }

    #[tokio::test]
    async fn the_gms_prefix_is_shown_to_every_hearer_before_the_words() {
        let scene = speech_scene().await;
        scene
            .speak(
                tool_call("say", serde_json::json!({"words": "Run!"})),
                serde_json::json!({"action_id": 1, "prefix": "(Over the din)"}),
            )
            .await;

        let expected = format!("(Over the din) {} says: Run!", scene.names[0]);
        assert_eq!(
            heard(&scene.store, &scene.second).await,
            std::slice::from_ref(&expected)
        );
        assert_eq!(
            heard(&scene.store, &scene.first).await,
            std::slice::from_ref(&expected)
        );
        assert_eq!(heard_by_npc(&scene.store, "Toma").await, [expected]);
    }

    #[tokio::test]
    async fn a_prefix_on_something_that_is_not_speech_is_refused() {
        let scene = speech_scene().await;
        scene.responses.lock().unwrap().extend([
            tool_call("look", serde_json::json!({"description": "the room"})),
            tool_call(
                "approve_action",
                serde_json::json!({"action_id": 1, "prefix": "(Quietly)"}),
            ),
            tool_call("approve_action", serde_json::json!({"action_id": 1})),
            response(Content::Text("A bare room.".into())),
            response(Content::Text(String::new())),
        ]);

        scene
            .game
            .player_message(scene.first.clone(), "Look around.")
            .await
            .unwrap();

        let refusal = tool_results(&scene.requests.lock().unwrap()[2])
            .pop()
            .unwrap();
        assert!(refusal.contains("is not speech"), "{refusal}");
    }

    #[tokio::test]
    async fn an_overlong_prefix_is_returned_to_the_gm() {
        let scene = speech_scene().await;
        scene.responses.lock().unwrap().extend([
            tool_call("say", serde_json::json!({"words": "Hello."})),
            tool_call(
                "approve_action",
                serde_json::json!({"action_id": 1, "prefix": "x".repeat(300)}),
            ),
            tool_call("approve_action", serde_json::json!({"action_id": 1})),
            response(Content::Text("A greeting.".into())),
            response(Content::Text(String::new())),
        ]);

        scene
            .game
            .player_message(scene.first.clone(), "Greet them.")
            .await
            .unwrap();

        let refusal = tool_results(&scene.requests.lock().unwrap()[2])
            .pop()
            .unwrap();
        assert!(refusal.contains("rejected"), "{refusal}");
        assert_eq!(
            heard(&scene.store, &scene.second).await.len(),
            1,
            "the retry without a prefix resolved it"
        );
    }

    #[tokio::test]
    async fn heard_speech_keeps_its_speaker_name_after_a_rename() {
        let scene = speech_scene().await;
        scene
            .speak(
                tool_call("say", serde_json::json!({"words": "Good morning."})),
                serde_json::json!({"action_id": 1}),
            )
            .await;

        sqlx::query("UPDATE character SET name = 'Someone Else' WHERE id = ?")
            .bind(scene.first.character_id)
            .execute(&scene.store.pool())
            .await
            .unwrap();

        assert_eq!(
            heard(&scene.store, &scene.second).await,
            [format!("{} says: Good morning.", scene.names[0])]
        );
    }

    #[tokio::test]
    async fn speech_to_someone_who_is_not_here_is_refused_before_anything_is_submitted() {
        let scene = speech_scene().await;
        scene.responses.lock().unwrap().extend([
            tool_call(
                "whisper",
                serde_json::json!({"words": "Psst.", "to": ["Nobody"]}),
            ),
            response(Content::Text("Nobody answers.".into())),
        ]);

        let actions_before = scene.store.action_counts().await.unwrap();

        scene
            .game
            .player_message(scene.first.clone(), "Whisper to nobody.")
            .await
            .unwrap();

        let refusal = tool_results(&scene.requests.lock().unwrap()[1])
            .pop()
            .unwrap();
        assert!(
            refusal.contains("Nobody") && refusal.contains("is not in this group"),
            "{refusal}"
        );
        assert!(heard(&scene.store, &scene.second).await.is_empty());
        assert_eq!(
            scene.store.action_counts().await.unwrap(),
            actions_before,
            "nothing was submitted for the GM"
        );
    }

    #[tokio::test]
    async fn shutdown_does_not_start_presence_narration() {
        let (backend, started, release, requests) = DelayedBackend::new();
        let (game, _store, _db, first, second) = two_player_bread_thief_game(backend).await;
        let active_game = Arc::clone(&game);
        let active_member = first.clone();
        let active = tokio::spawn(async move {
            active_game
                .player_message(active_member, "Describe the scene.")
                .await
        });
        started.notified().await;
        // Arrival narration is due, but the worker is busy with the active turn.
        let _first_connection = game.connect(&first).await.unwrap();
        let _second_connection = game.connect(&second).await.unwrap();

        let stopping_game = Arc::clone(&game);
        let shutdown = tokio::spawn(async move { stopping_game.shutdown().await });
        release.notify_one();
        active.await.unwrap().unwrap();
        shutdown.await.unwrap().unwrap();
        assert_eq!(
            requests.load(Ordering::SeqCst),
            1,
            "only the accepted turn may use the model while stopping"
        );
    }

    #[tokio::test]
    async fn group_changes_preserve_presence_and_wake_existing_viewers() {
        let (backend, _) =
            ScriptedBackend::recording([response(Content::Text("Both arrive.".into()))]);
        let responses = Arc::clone(&backend.responses);
        let (game, store, _db, first, second) =
            two_player_bread_thief_game_with_deadline(backend, 3600).await;
        let first_connection = game.connect(&first).await.unwrap();
        let second_connection = game.connect(&second).await.unwrap();
        deliver_presence_deadline(&game, &store, &first).await;
        game.disconnect(&second, second_connection).await.unwrap();
        let_presence_handlers_run(&game, &first).await;
        game.connect(&second).await.unwrap();
        let_presence_handlers_run(&game, &first).await;
        let original = store.character_group_id(first.character_id).await.unwrap();
        let before = store.presence_narration(original).await.unwrap();
        assert_eq!(
            before.last_announced_character_ids,
            [first.character_id, second.character_id]
        );
        assert!(before.due_at.is_some());
        let mut old_subscription = game.subscribe(&second).await.unwrap();
        let toma: i64 = sqlx::query_scalar("SELECT id FROM character WHERE name = 'Toma'")
            .fetch_one(&store.pool())
            .await
            .unwrap();
        let members: Vec<i64> = sqlx::query_scalar(
            "SELECT character_id FROM character_group WHERE group_id = ? AND character_id != ?",
        )
        .bind(original)
        .bind(toma)
        .fetch_all(&store.pool())
        .await
        .unwrap();
        responses.lock().unwrap().extend([
            response(Content::ToolCalls(vec![ToolCall {
                id: "look-1".into(),
                name: "look".into(),
                arguments: r#"{"description":"room"}"#.into(),
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
                    arguments: serde_json::json!({
                        "group_id": original,
                        "partitions": [
                            {"character_ids":members, "name":"players", "description":"outside"},
                            {"character_ids":[toma], "name":"Toma", "description":"inside"}
                        ]
                    })
                    .to_string(),
                },
            ])),
            response(Content::Text("Split narration.".into())),
            response(Content::Text("Done.".into())),
        ]);
        game.player_message(first.clone(), "Look and split.")
            .await
            .unwrap();
        let players = store.character_group_id(second.character_id).await.unwrap();
        let alone = store.character_group_id(toma).await.unwrap();
        let split_state = store.presence_narration(players).await.unwrap();
        let split_preserved =
            split_state.last_announced_character_ids == before.last_announced_character_ids;
        let split_deadline = split_state.due_at.is_some();
        let split_stored = store
            .player_chat(&second)
            .await
            .unwrap()
            .iter()
            .any(|entry| entry.text == "Split narration.");
        let split_live = old_subscription.try_recv().is_ok();
        let mut split_subscription = game.subscribe(&second).await.unwrap();
        assert_eq!(split_state.due_at, before.due_at);
        assert!(
            store
                .consume_presence_deadline(original, before.generation)
                .await
                .unwrap()
                .is_none()
        );
        let alone_state = store.presence_narration(alone).await.unwrap();
        assert!(alone_state.waiting_for_return);
        store
            .schedule_presence_deadline(alone, 1800, alone_state.generation)
            .await
            .unwrap();
        let earliest = store.presence_narration(alone).await.unwrap().due_at;
        responses.lock().unwrap().extend([
            response(Content::ToolCalls(vec![ToolCall {
                id: "look-2".into(),
                name: "look".into(),
                arguments: r#"{"description":"room"}"#.into(),
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
                    arguments: serde_json::json!({
                        "group_ids":[players,alone], "name":"reunited", "description":"together"
                    })
                    .to_string(),
                },
            ])),
            response(Content::Text("Merge narration.".into())),
            response(Content::Text("Done.".into())),
        ]);
        game.player_message(first.clone(), "Look and merge.")
            .await
            .unwrap();
        let merged = store.character_group_id(second.character_id).await.unwrap();
        let merge_state = store.presence_narration(merged).await.unwrap();
        let merge_preserved =
            merge_state.last_announced_character_ids == before.last_announced_character_ids;
        let merge_deadline = merge_state.due_at.is_some();
        assert_eq!(merge_state.due_at, earliest);
        let merge_stored = store
            .player_chat(&second)
            .await
            .unwrap()
            .iter()
            .any(|entry| entry.text == "Merge narration.");
        let merge_live = split_subscription.try_recv().is_ok();
        game.disconnect(&first, first_connection).await.unwrap();
        assert!(
            split_stored && merge_stored,
            "real narration must have reached the passive player's durable chat"
        );
        assert!(
            split_preserved
                && split_deadline
                && merge_preserved
                && merge_deadline
                && split_live
                && merge_live,
            "topology changes must preserve presence and wake existing passive viewers"
        );
    }

    #[tokio::test]
    async fn narration_failure_rolls_back_every_recipient() {
        let (backend, _) = ScriptedBackend::recording([
            response(Content::ToolCalls(vec![ToolCall {
                id: "look-1".into(),
                name: "look".into(),
                arguments: r#"{"description":"room"}"#.into(),
            }])),
            response(Content::ToolCalls(vec![ToolCall {
                id: "approve-1".into(),
                name: "approve_action".into(),
                arguments: r#"{"action_id":1}"#.into(),
            }])),
            response(Content::Text("Atomic delivery probe.".into())),
        ]);
        let (game, store, _db, first, second) =
            two_player_bread_thief_game_with_deadline(backend, 3600).await;
        game.connect(&first).await.unwrap();
        game.connect(&second).await.unwrap();
        let_presence_handlers_run(&game, &first).await;
        let branch = store.agent_branch(second.agent_id).await.unwrap();
        // Deliberate write-fault injection, not fixture seeding or a product workaround.
        let trigger = format!(
            "CREATE TRIGGER review_fail_recipient BEFORE INSERT ON message WHEN NEW.role = '\"Narration\"' AND (SELECT branch_id FROM history_entry WHERE id = NEW.id) = {} BEGIN SELECT RAISE(ABORT, 'review recipient failure'); END",
            branch.branch_id()
        );
        sqlx::query(&trigger).execute(&store.pool()).await.unwrap();
        let (actions_before, pending_before) = store.action_counts().await.unwrap();
        let result = game.player_message(first.clone(), "Look.").await;
        let error = result.expect_err("fault injection must reach the real delivery path");
        assert!(format!("{error:#}").contains("review recipient failure"));
        let first_has = store
            .player_chat(&first)
            .await
            .unwrap()
            .iter()
            .any(|entry| entry.text == "Atomic delivery probe.");
        let second_has = store
            .player_chat(&second)
            .await
            .unwrap()
            .iter()
            .any(|entry| entry.text == "Atomic delivery probe.");
        assert!(
            !first_has && !second_has,
            "failed delivery must roll back all recipient references"
        );
        assert_eq!(
            store.action_counts().await.unwrap(),
            (actions_before + 1, pending_before)
        );
        let recorded: i64 = sqlx::query_scalar("SELECT count(*) FROM inference JOIN recorded_content ON recorded_content.id = inference.output WHERE recorded_content.body LIKE '%Atomic delivery probe.%'")
            .fetch_one(&store.pool()).await.unwrap();
        assert_eq!(
            recorded, 1,
            "delivery rollback must retain the generated model result"
        );
    }

    async fn two_player_bread_thief_game<B>(
        backend: B,
    ) -> (
        Arc<Game<B>>,
        Store,
        crate::store::TestDatabase,
        PlayerAgent,
        PlayerAgent,
    )
    where
        B: Backend + Send + Sync + 'static,
    {
        two_player_bread_thief_game_with_deadline(backend, 0).await
    }

    async fn two_player_bread_thief_game_with_deadline<B>(
        backend: B,
        presence_departure_seconds: u64,
    ) -> (
        Arc<Game<B>>,
        Store,
        crate::store::TestDatabase,
        PlayerAgent,
        PlayerAgent,
    )
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
        // Presence narration only applies after character creation. Prepare
        // both players through the real creation records so these tests model
        // playable characters rather than silently exercising the unready
        // guard.
        for player in [&installed.member, &companion] {
            let creation = store
                .begin_sequence(installed.world_id, "presence test character creation")
                .await
                .unwrap();
            store
                .roll_hit_protection(player, creation.id, None)
                .await
                .unwrap();
            store
                .roll_attributes(player, creation.id, None)
                .await
                .unwrap();
            store
                .ready_to_begin(player, creation.id, None)
                .await
                .unwrap();
        }
        let limits = Limits {
            presence_departure_seconds,
            ..Limits::default()
        };
        let scheduler = InferenceScheduler::new(backend, limits).unwrap();
        let game = Game::new(
            store.clone(),
            World::load(&store).await.unwrap(),
            scheduler.foreground(),
            limits,
            "scripted".into(),
            Sampling {
                temperature: 0.0,
                ..Default::default()
            },
        );
        (game, store, db, installed.member, companion)
    }

    async fn let_presence_handlers_run<B: Backend + Send + Sync + 'static>(
        game: &Game<B>,
        member: &PlayerAgent,
    ) {
        let (reply, completed) = oneshot::channel();
        game.world(member.world_id)
            .unwrap()
            .sender
            .send(WorldEvent::Settled(reply))
            .await
            .unwrap();
        completed.await.unwrap();
    }

    /// Deliver a scheduled deadline deterministically through the same world
    /// queue as production, without sleeping through a departure window.
    async fn deliver_presence_deadline<B: Backend + Send + Sync + 'static>(
        game: &Game<B>,
        store: &Store,
        member: &PlayerAgent,
    ) {
        let_presence_handlers_run(game, member).await;
        let group_id = store.character_group_id(member.character_id).await.unwrap();
        let narration = store.presence_narration(group_id).await.unwrap();
        assert!(
            narration.due_at.is_some(),
            "the transition must schedule a deadline"
        );
        game.world(member.world_id)
            .unwrap()
            .sender
            .send(WorldEvent::PresenceDeadline {
                group_id,
                generation: narration.generation,
            })
            .await
            .unwrap();
        let_presence_handlers_run(game, member).await;
    }

    #[tokio::test]
    async fn first_arrival_after_an_empty_period_narrates_after_its_deadline() {
        let (backend, recorded_requests) =
            ScriptedBackend::recording([response(Content::Text("Someone arrives.".into()))]);
        let (game, _store, _db, first, _second) = two_player_bread_thief_game(backend).await;

        // The group has never been announced online before: this is "first
        // arrival after an empty period," which is delayed behind the
        // departure deadline (here, zero seconds), not immediate.
        let _first_connection = game.connect(&first).await.unwrap();
        let_presence_handlers_run(&game, &first).await;
        assert_eq!(recorded_requests.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn restart_recovers_connections_and_durable_windows() {
        for returning in 0..=2 {
            let (backend, requests) = ScriptedBackend::recording([
                response(Content::Text("Both arrive.".into())),
                response(Content::Text("One returns, one remains away.".into())),
            ]);
            let (game, store, db, first, second) =
                two_player_bread_thief_game_with_deadline(backend, 3600).await;
            let connection = game.connect(&first).await.unwrap();
            game.connect(&second).await.unwrap();
            deliver_presence_deadline(&game, &store, &first).await;
            game.disconnect(&first, connection).await.unwrap();
            let_presence_handlers_run(&game, &first).await;
            let group = store.character_group_id(first.character_id).await.unwrap();
            let departing = store.presence_narration(group).await.unwrap();
            store
                .schedule_presence_deadline(group, 30, departing.generation)
                .await
                .unwrap();
            let before = store.presence_narration(group).await.unwrap();
            let backend = game.backend.clone();
            let closed = game.world(first.world_id).unwrap().sender.clone();
            drop(game);
            closed.closed().await;
            drop(store);
            let store = Store::open(db.path()).await.unwrap();
            store.recover_presence().await.unwrap();
            assert!(
                store
                    .party_presence(first.character_id)
                    .await
                    .unwrap()
                    .iter()
                    .all(|member| !member.online)
            );
            let recovered = store.presence_narration(group).await.unwrap();
            assert_eq!(
                recovered.last_announced_character_ids,
                before.last_announced_character_ids
            );
            assert_eq!(recovered.due_at, before.due_at);
            assert!(recovered.waiting_for_return);
            assert!(
                store
                    .next_presence_deadline(first.world_id)
                    .await
                    .unwrap()
                    .unwrap()
                    .2
                    > 0
            );
            if returning == 0 {
                store
                    .schedule_presence_deadline(group, -1, recovered.generation)
                    .await
                    .unwrap();
            }
            let game = Game::new(
                store.clone(),
                World::load(&store).await.unwrap(),
                backend,
                Limits {
                    presence_departure_seconds: 3600,
                    ..Default::default()
                },
                "scripted".into(),
                Sampling::default(),
            );
            if returning > 0 {
                game.connect(&first).await.unwrap();
            }
            if returning == 2 {
                game.connect(&second).await.unwrap();
            }
            let_presence_handlers_run(&game, &first).await;
            if returning > 0 {
                assert!(
                    store
                        .next_presence_deadline(first.world_id)
                        .await
                        .unwrap()
                        .unwrap()
                        .2
                        > 30,
                    "the first real return starts a full new window"
                );
            }
            assert_eq!(
                requests.lock().unwrap().len(),
                1,
                "future windows wait; empty expired windows make no model call"
            );
            if returning > 0 {
                deliver_presence_deadline(&game, &store, &first).await;
            }
            let settled = store.presence_narration(group).await.unwrap();
            assert!(settled.due_at.is_none());
            assert_eq!(settled.waiting_for_return, returning == 0);
            assert_eq!(
                requests.lock().unwrap().len(),
                if returning == 1 { 2 } else { 1 }
            );
            // A consumed generation cannot deliver a second narration.
            game.handle_presence_deadline(group, recovered.generation)
                .await
                .unwrap();
            assert_eq!(
                requests.lock().unwrap().len(),
                if returning == 1 { 2 } else { 1 }
            );
        }
    }

    #[tokio::test]
    async fn failed_presence_delivery_preserves_snapshot_and_reports_once() {
        let (backend, requests) =
            ScriptedBackend::recording([response(Content::Text("Undelivered arrival.".into()))]);
        let (game, store, _db, first, second) =
            two_player_bread_thief_game_with_deadline(backend, 3600).await;
        game.connect(&first).await.unwrap();
        game.connect(&second).await.unwrap();
        let_presence_handlers_run(&game, &first).await;
        let group = store.character_group_id(first.character_id).await.unwrap();
        let before = store.presence_narration(group).await.unwrap();
        let branch = store.agent_branch(second.agent_id).await.unwrap();
        let trigger = format!(
            "CREATE TRIGGER fail_presence_delivery BEFORE INSERT ON message WHEN NEW.role = '\"Narration\"' AND (SELECT branch_id FROM history_entry WHERE id = NEW.id) = {} BEGIN SELECT RAISE(ABORT, 'presence recipient failure'); END",
            branch.branch_id()
        );
        sqlx::query(&trigger).execute(&store.pool()).await.unwrap();
        let mut notices = game.subscribe(&first).await.unwrap();
        deliver_presence_deadline(&game, &store, &first).await;
        let after = store.presence_narration(group).await.unwrap();
        assert_eq!(
            after.last_announced_character_ids,
            before.last_announced_character_ids
        );
        assert!(after.due_at.is_none());
        for member in [&first, &second] {
            assert!(
                !store
                    .player_chat(member)
                    .await
                    .unwrap()
                    .iter()
                    .any(|entry| entry.text == "Undelivered arrival.")
            );
        }
        let ChatChange::Failed { sequence_id } = notices.try_recv().unwrap() else {
            panic!("failed delivery must not announce successful entries")
        };
        assert!(
            game.player_chat_error(&first, sequence_id)
                .await
                .unwrap()
                .unwrap()
                .text
                .contains("presence recipient failure")
        );
        let mut foreign = first.clone();
        foreign.world_id += 1;
        assert!(
            game.player_chat_error(&foreign, sequence_id)
                .await
                .unwrap()
                .is_none()
        );
        let viewer = store
            .find_or_create_user("presence-viewer@example.test", "Viewer")
            .await
            .unwrap();
        let invitation = store
            .create_invitation(first.user_id, first.world_id, None)
            .await
            .unwrap();
        let viewer = store
            .accept_invitation(&viewer, &invitation.token)
            .await
            .unwrap();
        assert!(
            game.player_chat_error(&viewer, sequence_id)
                .await
                .unwrap()
                .is_some()
        );
        store
            .remove_member(first.user_id, first.world_id, viewer.user_id)
            .await
            .unwrap();
        assert!(
            game.player_chat_error(&viewer, sequence_id)
                .await
                .unwrap()
                .is_none(),
            "a live ban revokes recorded failure access"
        );
        let_presence_handlers_run(&game, &first).await;
        assert_eq!(
            requests.lock().unwrap().len(),
            1,
            "delivery failure must not rerun inference"
        );
        assert!(matches!(
            notices.try_recv(),
            Err(broadcast::error::TryRecvError::Empty)
        ));
    }

    #[tokio::test]
    async fn arrival_into_an_already_announced_occupied_group_narrates_immediately() {
        let (backend, recorded_requests) = ScriptedBackend::recording([
            response(Content::Text("Someone arrives.".into())),
            response(Content::Text("Someone else arrives.".into())),
        ]);
        let (game, _store, _db, first, second) = two_player_bread_thief_game(backend).await;

        let _first_connection = game.connect(&first).await.unwrap();
        let_presence_handlers_run(&game, &first).await;
        assert_eq!(
            recorded_requests.lock().unwrap().len(),
            1,
            "the group's first-ever online character settles its own delayed announcement"
        );

        let _second_connection = game.connect(&second).await.unwrap();
        let_presence_handlers_run(&game, &first).await;
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
        let_presence_handlers_run(&game, &first).await;
        let _second_connection = game.connect(&second).await.unwrap();
        let_presence_handlers_run(&game, &first).await;
        assert_eq!(
            recorded_requests.lock().unwrap().len(),
            2,
            "the first character's own delayed arrival and the second's immediate arrival already narrated"
        );

        game.disconnect(&first, first_connection).await.unwrap();
        let_presence_handlers_run(&game, &first).await;
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
        let (game, store, _db, first, second) =
            two_player_bread_thief_game_with_deadline(backend, 60).await;
        // Keep the departure window open until both transport changes have completed.
        let group_id = store.character_group_id(first.character_id).await.unwrap();
        let first_connection = game.connect(&first).await.unwrap();
        deliver_presence_deadline(&game, &store, &first).await;
        let _second_connection = game.connect(&second).await.unwrap();
        let_presence_handlers_run(&game, &first).await;
        assert_eq!(recorded_requests.lock().unwrap().len(), 2);

        game.disconnect(&first, first_connection).await.unwrap();
        let_presence_handlers_run(&game, &first).await;
        // Reconnect inside the scheduled departure window, then deliver its deadline.
        let _first_connection_again = game.connect(&first).await.unwrap();
        deliver_presence_deadline(&game, &store, &first).await;
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
        let (game, store, _db, first, second) =
            two_player_bread_thief_game_with_deadline(backend, 60).await;
        let first_connection = game.connect(&first).await.unwrap();
        deliver_presence_deadline(&game, &store, &first).await;
        let second_connection = game.connect(&second).await.unwrap();
        let_presence_handlers_run(&game, &first).await;
        assert_eq!(recorded_requests.lock().unwrap().len(), 2);

        game.disconnect(&first, first_connection).await.unwrap();
        game.disconnect(&second, second_connection).await.unwrap();
        deliver_presence_deadline(&game, &store, &first).await;
        assert_eq!(
            recorded_requests.lock().unwrap().len(),
            2,
            "an empty group retains the difference without running an inference"
        );
        let group = store.character_group_id(first.character_id).await.unwrap();
        let empty = store.presence_narration(group).await.unwrap();
        assert_eq!(
            empty.last_announced_character_ids,
            [first.character_id, second.character_id]
        );
        assert!(empty.waiting_for_return);
        game.connect(&first).await.unwrap();
        game.connect(&second).await.unwrap();
        let_presence_handlers_run(&game, &first).await;
        assert_eq!(
            recorded_requests.lock().unwrap().len(),
            2,
            "the same returning group still waits through its window"
        );
        deliver_presence_deadline(&game, &store, &first).await;
        assert_eq!(
            recorded_requests.lock().unwrap().len(),
            2,
            "an unchanged returned set needs no repeated narration"
        );
        assert!(
            !store
                .presence_narration(group)
                .await
                .unwrap()
                .waiting_for_return
        );
    }

    #[tokio::test]
    async fn group_tools_reject_foreign_worlds_before_compaction_or_mutation() {
        for case in 0..5 {
            let (backend, requests) = ScriptedBackend::recording([]);
            let responses = Arc::clone(&backend.responses);
            let (game, store, _db, member, _) = two_player_bread_thief_game(backend).await;
            let owner = store
                .find_or_create_user("other@example.test", "Other")
                .await
                .unwrap();
            let mut scenario = Scenario::read(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/scenarios/bread_thief.json"
            ))
            .unwrap();
            let mut second_group = scenario.groups[0].clone();
            second_group.name = "other group".into();
            scenario.npcs[0].group = second_group.name.clone();
            scenario.groups.push(second_group);
            let other = store.install_scenario(&owner, &scenario).await.unwrap();
            game.start_world(World::install(&game.worlds, other.world_id));
            let foreign_groups: Vec<i64> =
                sqlx::query_scalar("SELECT id FROM \"group\" WHERE world_id = ? ORDER BY id")
                    .bind(other.world_id)
                    .fetch_all(&store.pool())
                    .await
                    .unwrap();
            let local_group = store.character_group_id(member.character_id).await.unwrap();
            // Every potential merge source exceeds the real preparation threshold.
            for group in [local_group, foreign_groups[0], foreign_groups[1]] {
                let branch = store.group_branch(group).await.unwrap();
                let sequence = store
                    .begin_sequence(branch.world_id(), "source history")
                    .await
                    .unwrap();
                for _ in 0..=Limits::default().merge_branch_messages {
                    store
                        .append_instruction(
                            branch,
                            &Message::text(Role::User, "Earlier scene."),
                            sequence.id,
                        )
                        .await
                        .unwrap();
                }
                store.finish_sequence(sequence.id, None).await.unwrap();
            }
            let characters: Vec<i64> = sqlx::query_scalar(
                "SELECT character_id FROM character_group WHERE group_id = ? ORDER BY character_id",
            )
            .bind(foreign_groups[0])
            .fetch_all(&store.pool())
            .await
            .unwrap();
            let (name, arguments) = match case {
                0 | 4 => (
                    "split_group",
                    serde_json::json!({
                        "group_id": if case == 0 { foreign_groups[0] } else { i64::MAX },
                        "partitions": [{"character_ids": characters, "name": "Changed", "description": "Changed"}],
                        "continue_with_character_id": other.member.character_id,
                    }),
                ),
                _ => (
                    "merge_groups",
                    serde_json::json!({
                        "group_ids": match case {
                            1 => foreign_groups.clone(),
                            2 => vec![local_group, foreign_groups[0]],
                            _ => vec![foreign_groups[0], local_group],
                        },
                        "name": "Changed", "description": "Changed",
                    }),
                ),
            };
            let before_groups: Vec<(i64, i64, Option<String>)> =
                sqlx::query_as("SELECT id, branch_id, retired_at FROM \"group\" ORDER BY id")
                    .fetch_all(&store.pool())
                    .await
                    .unwrap();
            let before_members: Vec<(i64, i64)> = sqlx::query_as(
                "SELECT character_id, group_id FROM character_group ORDER BY character_id",
            )
            .fetch_all(&store.pool())
            .await
            .unwrap();
            responses.lock().unwrap().extend([
                response(Content::ToolCalls(vec![ToolCall {
                    id: "look-1".into(),
                    name: "look".into(),
                    arguments: r#"{"description":"Look around."}"#.into(),
                }])),
                response(Content::ToolCalls(vec![
                    ToolCall {
                        id: "invalid-change".into(),
                        name: name.into(),
                        arguments: arguments.to_string(),
                    },
                    ToolCall {
                        id: "approve-1".into(),
                        name: "approve_action".into(),
                        arguments: r#"{"action_id":1}"#.into(),
                    },
                ])),
                response(Content::Text("The scene is unchanged.".into())),
                response(Content::Text("Done.".into())),
            ]);
            game.player_message(member.clone(), "Look around.")
                .await
                .unwrap();
            let after_groups: Vec<(i64, i64, Option<String>)> =
                sqlx::query_as("SELECT id, branch_id, retired_at FROM \"group\" ORDER BY id")
                    .fetch_all(&store.pool())
                    .await
                    .unwrap();
            let after_members: Vec<(i64, i64)> = sqlx::query_as(
                "SELECT character_id, group_id FROM character_group ORDER BY character_id",
            )
            .fetch_all(&store.pool())
            .await
            .unwrap();
            assert_eq!(
                after_groups, before_groups,
                "group structure changed in case {case}"
            );
            assert_eq!(
                after_members, before_members,
                "membership changed in case {case}"
            );
            assert!(
                store.pending_compactions().await.unwrap().is_empty(),
                "case {case} queued compaction"
            );
            let summaries: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM summary")
                .fetch_one(&store.pool())
                .await
                .unwrap();
            assert_eq!(summaries, 0, "case {case} ran compaction");
            assert_eq!(
                requests.lock().unwrap().len(),
                4,
                "case {case} ran extra model work"
            );
            let statuses: Vec<String> = sqlx::query_scalar("SELECT tool_execution.status FROM tool_execution JOIN inference ON inference.id = tool_execution.inference_id WHERE inference.agent_id = ? ORDER BY tool_execution.ordinal")
                .bind(store.group_branch(local_group).await.unwrap().agent_id())
                .fetch_all(&store.pool()).await.unwrap();
            assert_eq!(
                statuses,
                ["rejected", "completed"],
                "case {case} lost the ordinary sibling result"
            );
        }
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
        assert_eq!(
            party.len(),
            4,
            "the Adventurer, the companion, Mara and Toma"
        );
        assert_eq!(
            names_online.iter().filter(|(online, _)| *online).count(),
            1,
            "only the connected Adventurer is online"
        );
        assert!(
            party
                .iter()
                .any(|member| member.name.starts_with("Mara") || member.name == "Mara")
        );

        game.disconnect(&first, first_connection).await.unwrap();
        let party_after = store.party_presence(second.character_id).await.unwrap();
        assert!(
            party_after.iter().all(|member| !member.online),
            "nobody remains connected after the sole connection closes"
        );
    }

    #[tokio::test]
    async fn live_narration_does_not_cross_groups_sharing_one_location() {
        let (backend, _) = ScriptedBackend::recording([]);
        let script = Arc::clone(&backend.responses);
        let (game, store, _db, first, second) = two_player_bread_thief_game(backend).await;
        let group_id = store.character_group_id(first.character_id).await.unwrap();
        let companions: Vec<i64> = store
            .party_presence(first.character_id)
            .await
            .unwrap()
            .into_iter()
            .map(|member| member.character_id)
            .filter(|id| *id != second.character_id)
            .collect();
        let split = serde_json::json!({
            "group_id": group_id,
            "partitions": [
                {"character_ids": companions, "name": "first group", "description": "outside"},
                {"character_ids": [second.character_id], "name": "second group", "description": "inside"}
            ]
        }).to_string();
        script.lock().unwrap().extend([
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
                    arguments: split,
                },
            ])),
            response(Content::Text("The group splits up.".into())),
            response(Content::Text(String::new())),
            response(Content::ToolCalls(vec![ToolCall {
                id: "look-2".into(),
                name: "look".into(),
                arguments: r#"{"description":"the room"}"#.into(),
            }])),
            response(Content::ToolCalls(vec![ToolCall {
                id: "approve-2".into(),
                name: "approve_action".into(),
                arguments: r#"{"action_id":2}"#.into(),
            }])),
            response(Content::Text(
                "Only the first group should see this.".into(),
            )),
            response(Content::Text(String::new())),
        ]);
        store
            .mark_character_present(first.character_id)
            .await
            .unwrap();
        store
            .mark_character_present(second.character_id)
            .await
            .unwrap();
        let presence = store.presence_narration(group_id).await.unwrap();
        store
            .announce_presence(
                group_id,
                &[first.character_id, second.character_id],
                presence.generation,
            )
            .await
            .unwrap();
        game.player_message(first.clone(), "Look around and split up.")
            .await
            .unwrap();
        assert!(
            store
                .player_chat(&first)
                .await
                .unwrap()
                .iter()
                .any(|entry| entry.text == "The group splits up."),
            "the GM's settled reply belongs to the triggering character's resulting group"
        );
        assert!(
            !store
                .player_chat(&second)
                .await
                .unwrap()
                .iter()
                .any(|entry| entry.text == "The group splits up."),
            "the final reply must not leak into the other resulting group"
        );
        assert_ne!(
            store.character_group_id(first.character_id).await.unwrap(),
            store.character_group_id(second.character_id).await.unwrap()
        );
        for member in [&first, &second] {
            let group = store.character_group_id(member.character_id).await.unwrap();
            let presence = store.presence_narration(group).await.unwrap();
            assert_eq!(presence.last_announced_character_ids, [member.character_id]);
            assert!(!presence.waiting_for_return);
        }
        assert_eq!(
            store.player_location_id(&first).await.unwrap(),
            store.player_location_id(&second).await.unwrap()
        );
        let mut first_receiver = game.subscribe(&first).await.unwrap();
        let mut second_receiver = game.subscribe(&second).await.unwrap();
        game.player_message(first.clone(), "Look around again.")
            .await
            .unwrap();
        assert_eq!(first_receiver.recv().await.unwrap(), ChatChange::Entries);
        assert_eq!(second_receiver.try_recv().unwrap(), ChatChange::Entries);
        assert!(
            store
                .player_chat(&first)
                .await
                .unwrap()
                .iter()
                .any(|entry| entry.text == "Only the first group should see this.")
        );
        assert!(
            !store
                .player_chat(&second)
                .await
                .unwrap()
                .iter()
                .any(|entry| entry.text == "Only the first group should see this."),
            "reloaded history must preserve the same isolation as live narration"
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
                .live_segments(store.agent_branch(member.agent_id).await.unwrap())
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
        let (mut backend, model_path, sampling, limits) = device_test_game_for("dev-qwen25").await;
        let observed_prompts = backend.observe_prompts();
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
        {
            let observed_prompts = observed_prompts.lock().unwrap();
            assert_eq!(
                observed_prompts.len(),
                1,
                "the opening must format and tokenize exactly one model request"
            );
            let observed = &observed_prompts[0];
            assert_eq!(
                opening.request, observed.request,
                "inspection must preserve the exact request Game sent to the model"
            );
            assert_eq!(
                prompt, &observed.prompt,
                "inspection must reconstruct the exact prompt the model received"
            );
            let evidence = opening
                .prompt_evidence
                .as_ref()
                .expect("normal game inference retains prompt checksums");
            assert_eq!(
                evidence.tokens_hash,
                crate::store::PromptEvidence::tokens_hash(&observed.tokens),
                "inspection must retain the token checksum observed at the model boundary"
            );
            assert_eq!(evidence.max_output_tokens, limits.max_output_tokens);
        }
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

    /// A turn interrupted after its roll committed: after restart cleanup the
    /// next real chat turn must accept the closed tool history and the model
    /// must be able to use the recorded outcome it is shown.
    #[tokio::test]
    #[ignore = "device test: needs a CUDA GPU and the models/ GGUF from default.toml"]
    async fn real_model_chats_normally_after_an_interrupted_roll() {
        let (backend, model_path, configured, _limits) = device_test_game().await;
        let sampling = Sampling {
            temperature: 0.0,
            ..configured
        };
        let (game, store, _db, member) = opening_game(backend, model_path, sampling).await;
        let sequence = store
            .begin_sequence(member.world_id, "interrupted turn")
            .await
            .unwrap();
        store
            .append_message_with_sequence(
                member.agent_id,
                &Message::text(Role::User, "Roll my Hit Protection."),
                sequence.id,
            )
            .await
            .unwrap();
        let branch = store.agent_branch(member.agent_id).await.unwrap();
        let (_, executions) = crate::store::record_tool_batch_for_test(
            &store,
            branch,
            sequence.id,
            &["roll_hit_protection"],
        )
        .await;
        store.begin_tool_execution(executions[0]).await.unwrap();
        let hp = store
            .roll_hit_protection(&member, sequence.id, Some(executions[0]))
            .await
            .unwrap();
        store.close_interrupted_turns().await.unwrap();

        let response = game
            .player_message(
                member.clone(),
                "What did my Hit Protection roll come to? Reply with the number only.",
            )
            .await
            .expect("the closed tool history must be accepted by the real model");
        let Content::Text(reply) = response.content else {
            panic!("the player agent did not settle: {response:?}");
        };
        assert!(
            reply.contains(&hp.to_string()),
            "the model should report the recorded roll {hp}: {reply}"
        );
    }

    /// A real GM judging a take, with the approval result now reporting the
    /// action's outcome: the turn completes and the item moves.
    #[tokio::test]
    #[ignore = "device test: needs a CUDA GPU and the models/ GGUF from default.toml"]
    async fn real_model_gm_approves_a_take_and_the_item_moves() {
        let (backend, model_path, configured, _limits) = device_test_game().await;
        let sampling = Sampling {
            temperature: 0.0,
            ..configured
        };
        let (game, store, _db, member) = opening_game(backend, model_path, sampling).await;
        let _connection = game.connect(&member).await.unwrap();
        let creation = store
            .begin_sequence(member.world_id, "test completed creation")
            .await
            .unwrap();
        store
            .roll_hit_protection(&member, creation.id, None)
            .await
            .unwrap();
        store
            .roll_attributes(&member, creation.id, None)
            .await
            .unwrap();
        store
            .ready_to_begin(&member, creation.id, None)
            .await
            .unwrap();

        for attempt in 1..=3 {
            game.player_message(member.clone(), "I pick up the flour sack.")
                .await
                .unwrap_or_else(|error| panic!("turn {attempt} failed: {error:#}"));
            let (held,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM item_character")
                .fetch_one(&store.pool())
                .await
                .unwrap();
            if held == 1 {
                return;
            }
        }
        panic!("the flour sack never moved across three prompts");
    }

    /// A real player agent speaks, a real GM approves, and the other player's
    /// real agent then sees what was said as attributed speech in its context.
    #[tokio::test]
    #[ignore = "device test: needs a CUDA GPU and the models/ GGUF from default.toml"]
    async fn real_model_speech_reaches_another_player_in_their_context() {
        let (mut backend, _model_path, _sampling, _limits) = device_test_game().await;
        let observed_prompts = backend.observe_prompts();
        let (game, store, _db, first, second) = two_player_bread_thief_game(backend).await;
        let _first_connection = game.connect(&first).await.unwrap();
        let _second_connection = game.connect(&second).await.unwrap();

        for attempt in 1..=3 {
            game.player_message(first.clone(), "Say hello to Toma out loud.")
                .await
                .unwrap_or_else(|error| panic!("speaker turn {attempt} failed: {error:#}"));
            if !heard(&store, &second).await.is_empty() {
                break;
            }
        }
        let overheard = heard(&store, &second).await;
        assert!(
            !overheard.is_empty(),
            "no speech reached the other player across three prompts"
        );

        game.player_message(second.clone(), "What did you just hear? Answer briefly.")
            .await
            .expect("the listener's turn must accept speech in its history");
        let listener_prompt = observed_prompts
            .lock()
            .unwrap()
            .last()
            .expect("the listener ran")
            .prompt
            .clone();
        assert!(
            listener_prompt.contains(&overheard[0]),
            "the listener's model must be shown the attributed speech:
{listener_prompt}"
        );

        // A private word: only the named listener hears it, marked as a whisper.
        let listener = store.player_scene(&second).await.unwrap().character_name;
        let before = heard(&store, &second).await.len();
        for attempt in 1..=3 {
            game.player_message(
                first.clone(),
                &format!("Whisper to {listener} that we should leave quietly."),
            )
            .await
            .unwrap_or_else(|error| panic!("whisper turn {attempt} failed: {error:#}"));
            if heard(&store, &second).await.len() > before {
                break;
            }
        }
        let after = heard(&store, &second).await;
        assert!(
            after.len() > before,
            "the whisper never reached its listener"
        );
        assert!(
            after.last().unwrap().contains("whispers"),
            "the listener should be told it was a whisper: {after:?}"
        );
        assert!(
            heard_by_npc(&store, "Toma")
                .await
                .iter()
                .all(|text| !text.contains("leave quietly")),
            "a bystander must not hear the whisper"
        );
    }

    #[tokio::test]
    #[ignore = "device test: needs a CUDA GPU and the models/ GGUF from default.toml"]
    async fn real_model_compacts_a_long_chat_and_keeps_an_early_fact() {
        let (mut backend, model_path, sampling, base_limits) = device_test_game().await;
        let observed_prompts = backend.observe_prompts();
        // A threshold the second turn's context clears, retaining enough tail
        // that the summary has real history to work from.
        let limits = Limits {
            compact_before_next_input_tokens: 700,
            keep_tail_messages: 4,
            ..base_limits
        };
        let (game, store, _db, member) =
            opening_game_with_limits(backend, model_path, sampling, limits).await;

        // An early character goal, then enough turns to force a compaction.
        game.player_message(
            member.clone(),
            "My character's missing sister is named Maribel. Finding her is my reason for adventuring.",
        )
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
            .agent_branch(member.agent_id)
            .await
            .expect("branch should resolve")
            .branch_id();
        let summary = store
            .latest_summary_for_branch(branch_id)
            .await
            .expect("summary should load")
            .expect("a compaction must have run");
        assert!(
            summary.content.to_uppercase().contains("MARIBEL"),
            "the summary dropped an early fact it should retain: {}; actual model prompts: {:#?}",
            summary.content,
            observed_prompts
                .lock()
                .unwrap()
                .iter()
                .map(|observed| &observed.prompt)
                .collect::<Vec<_>>()
        );
    }
}
