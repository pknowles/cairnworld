use std::{
    collections::HashSet,
    sync::{
        Arc, Mutex, Weak,
        atomic::{AtomicI64, Ordering},
    },
};

mod authoring;
mod generation;
mod narration;
mod npc;

use narration::{NarrationScope, NarrationTools, PresentedActions};

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
    prompts,
    settings::{GameSettings, Limits},
    store::{
        ActionOutcome, Decision, GmScene, Hearing, InvalidHearing, MAX_PREFIX_CHARS, Narration,
        PendingAction, PlayerAgent, PresenceNarration, SpeakAction, Store,
    },
    tools::{GroupPartition, Tool, ToolFuture, ToolOutcome, TopologyChange},
    world::{self, ChatChange, OpeningState, World, WorldEvent, Worlds},
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
    rules: GameSettings,
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

/// Everything a player-facing action tool needs to submit one action to its
/// location GM. The tool owns this invocation scope until a model calls it.
/// A character action as a model sees it: the tool and the check of its
/// arguments. Players and NPCs are offered the same actions, so a character
/// has one set of verbs whoever controls it.
struct ActionKind {
    definition: ToolDefinition,
    validate: fn(&str) -> Result<()>,
}

/// The actions any character can take, in the order they are offered. Speech
/// comes first: offered after `attempt`, models spoke through `attempt` (kind
/// `ask`, or a kind that does not exist) in about one decision in ten, and not
/// once when speech was first (experiments/0008-attempt-misuse.md).
fn action_kinds() -> Result<Vec<ActionKind>> {
    let prompts = prompts::current()?;
    Ok(vec![
        ActionKind {
            definition: prompts.shared.tools.tool(
                "speak",
                serde_json::to_value(schema_for!(SpeakAction))
                    .expect("speak action schema should serialize"),
            )?,
            validate: validate_speak_action,
        },
        ActionKind {
            definition: prompts.player.tools.tool(
                "attempt",
                serde_json::to_value(schema_for!(AttemptAction))
                    .expect("attempt action schema should serialize"),
            )?,
            validate: validate_attempt_action,
        },
        ActionKind {
            definition: prompts.player.tools.tool(
                "take",
                serde_json::to_value(schema_for!(TakeAction))
                    .expect("take action schema should serialize"),
            )?,
            validate: validate_take_action,
        },
    ])
}

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
    /// A game with the default rules of play.
    #[cfg(test)]
    pub fn new(
        store: Store,
        worlds: Worlds,
        backend: ScheduledBackend<B>,
        limits: Limits,
        model: String,
        sampling: Sampling,
    ) -> Arc<Self> {
        Self::with_rules(
            store,
            worlds,
            backend,
            limits,
            model,
            sampling,
            GameSettings::default(),
        )
    }

    /// As `new`, with the rules of play configured for this deployment.
    pub fn with_rules(
        store: Store,
        worlds: Worlds,
        backend: ScheduledBackend<B>,
        limits: Limits,
        model: String,
        sampling: Sampling,
        rules: GameSettings,
    ) -> Arc<Self> {
        let game = Arc::new(Self {
            store,
            backend,
            limits,
            model,
            sampling,
            rules,
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

    /// Interrupt: raise this player's hand in their group. NPC steps stop
    /// starting there until the player sends, says Never mind or leaves.
    /// Nothing running is cancelled.
    pub async fn interrupt(&self, member: &PlayerAgent) -> Result<()> {
        ensure!(
            !*self.stopping.borrow(),
            "world {} is no longer accepting input because the server is stopping",
            member.world_id
        );
        let world = self.world(member.world_id)?;
        let group_id = self.store.character_group_id(member.character_id).await?;
        world.raise_hand(group_id, member.character_id);
        Ok(())
    }

    /// Never mind: lower this player's hand. It runs no inference, and without
    /// a raised hand it does nothing.
    pub fn never_mind(&self, member: &PlayerAgent) -> Result<()> {
        self.world(member.world_id)?.lower_hand(member.character_id);
        Ok(())
    }

    /// Whether this player's hand is up in their current group.
    pub async fn hand_raised(&self, member: &PlayerAgent) -> Result<bool> {
        let group_id = self.store.character_group_id(member.character_id).await?;
        Ok(self
            .world(member.world_id)?
            .hand_raised(group_id, member.character_id))
    }

    /// Wakes whenever any hand in this world goes up or down.
    pub fn hands_changed(&self, member: &PlayerAgent) -> Result<watch::Receiver<u64>> {
        Ok(self.world(member.world_id)?.hands_changed())
    }

    /// Queue a player's message and wait for the turn's result.
    #[cfg(test)]
    pub async fn player_message(
        self: &Arc<Self>,
        member: PlayerAgent,
        text: &str,
    ) -> Result<Response> {
        self.queue_player_message(member, text)
            .await?
            .await
            .context("world stopped processing player message")?
    }

    /// Queue a player message behind all earlier events in this world, returning
    /// where the turn's result will arrive. From here the worker owns the turn: a caller that
    /// stops waiting, such as a closed browser connection, cancels nothing.
    pub async fn queue_player_message(
        self: &Arc<Self>,
        member: PlayerAgent,
        text: &str,
    ) -> Result<oneshot::Receiver<Result<Response>>> {
        let world = self.world(member.world_id)?;
        let (reply, receive) = oneshot::channel();
        // The message is queued first and the hands lowered after, so NPC work
        // cannot overtake it.
        ensure!(
            !*self.stopping.borrow(),
            "world {} is no longer accepting input because the server is stopping",
            world.id
        );
        self.ensure_world_ready(world.id).await?;
        let permit = world
            .sender
            .reserve()
            .await
            .with_context(|| format!("world {} is not accepting input", world.id))
            .context("queueing player message")?;
        let sender = member.character_id;
        permit.send(WorldEvent::PlayerMessage {
            member,
            text: text.to_string(),
            reply,
        });
        // Only the sender's own hand comes down; another player's stays up so
        // they can finish typing.
        world.lower_hand(sender);
        Ok(receive)
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

    /// Players can open a world that is still being created, but nothing they do
    /// reaches it until it is ready.
    async fn ensure_world_ready(&self, world_id: i64) -> Result<()> {
        ensure!(
            self.world_is_ready(world_id).await?,
            "world {world_id} is still being created"
        );
        Ok(())
    }

    pub async fn world_is_ready(&self, world_id: i64) -> Result<bool> {
        self.store.world_is_ready(world_id).await
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
        self.ensure_world_ready(world.id).await?;
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
            world.lower_hand(member.character_id);
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
            // NPC replies are finite steps that wait their turn: foreground
            // input and presence handling above take priority, and none start
            // once stopping or in a group where a player has a hand raised.
            let mut npc_step = if draining {
                None
            } else {
                match store.next_npc_step(world_id, &state.held_groups()).await {
                    Ok(step) => step,
                    Err(error) => {
                        tracing::error!(world_id, error = %format!("{error:#}"), "looking for NPC work");
                        break;
                    }
                }
            };
            // A world still being initialized takes its next finite step the
            // same way.
            let mut generation = if draining {
                None
            } else {
                match store.next_generation(world_id).await {
                    Ok(generation) => generation,
                    Err(error) => {
                        tracing::error!(world_id, error = %format!("{error:#}"), "looking for world generation work");
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
                _ = state.released.notified(), if !draining => continue,
                event = deadline, if !draining => event,
                event = events.recv() => match event { Some(event) => event, None => break },
                _ = std::future::ready(()), if npc_step.is_some() => {
                    let Some(game) = game.upgrade() else { break };
                    if let Some(step) = npc_step.take() {
                        game.run_npc_step_logged(&state, step).await;
                    }
                    continue;
                }
                _ = std::future::ready(()), if generation.is_some() => {
                    let Some(game) = game.upgrade() else { break };
                    if let Some(generation) = generation.take() {
                        // Only a failure that could not be recorded comes back;
                        // the same step would run again, so stop the worker.
                        if let Err(error) = game.run_generation_step(&state, generation).await {
                            tracing::error!(world_id, error = %format!("{error:#}"), "recording a world generation failure");
                            break;
                        }
                    }
                    continue;
                }
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
        let scene = self
            .store
            .gm_scene(world_id, gm_history.branch_id)
            .await
            .context("loading group GM scene for presence narration")?;
        let scene_packet = Self::gm_packet(&scene)?;
        let prompts = prompts::current()?;
        let role = Message::text(Role::System, &prompts.gm.role);
        let request = Message::text(
            Role::User,
            prompts::fill(&prompts.gm.presence_request, &[("change", &change)])?,
        );
        let narrate = NarrationTools::new(
            self,
            NarrationScope {
                world_id,
                in_group: online[0],
                actor: None,
                presented: PresentedActions::default(),
            },
        )?;
        let reminder = narrate.reminder()?;
        let sequence = self
            .store
            .begin_sequence(world_id, "presence transition")
            .await
            .context("starting presence narration sequence")?;
        let budget = Budget::new(self.limits);
        let result: Result<()> = async {
            let call = CallContext::root(Some(sequence.id));
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
                    dynamic_context: agent::DynamicContext::Fixed(&[scene_packet.as_str()]),
                    tools: &narrate.tools,
                    terminal_reminder: Some(&reminder),
                    sampling: self.sampling.clone(),
                    model: &self.model,
                },
                &call,
                |_| {},
                |_| {},
            )
            .await
            .context("asking group GM for presence narration")?;
            let spoken = narrate.finish((group_id, sequence.id), settled.inference_id, None)?;
            self.store
                .append_presence_narration(world_id, narration, online, &spoken)
                .await
                .context("storing presence narration")?;
            let _no_live_viewers = world.broadcasts.send(ChatChange::Entries);
            Ok(())
        }
        .await;
        // Like a player's turn, it may have given NPCs something to respond to.
        if let Err(finish_error) = self
            .store
            .finish_root_turn(
                sequence.id,
                result
                    .as_ref()
                    .err()
                    .map(|error| format!("{error:#}"))
                    .as_deref(),
                budget.spent(),
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
        let budget = Budget::new(self.limits);
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
            // Show the player their own message now, with its inspection
            // link, not when the whole reply is ready.
            let _no_live_viewers = world.broadcasts.send(ChatChange::Entries);
            let ready = self
                .store
                .is_ready_to_begin(&member)
                .await
                .context("checking character creation state")?;
            let call = CallContext::root(Some(sequence.id));
            let tools = if ready {
                self.action_tools(member.clone(), sequence.id, budget.clone(), call.clone())?
            } else {
                self.creation_tools(member.clone(), sequence.id, budget.clone(), call.clone())?
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
                    dynamic_context: agent::DynamicContext::Fixed(&[]),
                    tools: &tools,
                    terminal_reminder: None,
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
            .finish_root_turn(
                sequence.id,
                result
                    .as_ref()
                    .err()
                    .map(|error| format!("{error:#}"))
                    .as_deref(),
                budget.spent(),
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
        // Everything from here on must finish the sequence on every exit,
        // including a failure building the tools or prompt before the agent
        // ever runs - not only agent::complete_with_call_context's own result.
        let result: Result<Response> = async {
            let tools =
                self.creation_tools(member.clone(), sequence.id, budget.clone(), call.clone())?;
            let branch = self.store.agent_branch(member.agent_id).await?;
            let prompt = self
                .player_prompt(&member, false)
                .await
                .context("building player prompt")?;
            let entered = Message::text(Role::User, &prompts::current()?.player.entered);
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
                    dynamic_context: agent::DynamicContext::Fixed(&[]),
                    tools: &tools,
                    terminal_reminder: None,
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
        let prompts = prompts::current()?;
        let text = if ready {
            let scene = serde_json::to_string(
                &self
                    .store
                    .player_scene(member)
                    .await
                    .context("loading player scene for prompt")?,
            )
            .context("serializing player scene for prompt")?;
            prompts::fill(&prompts.player.scene, &[("scene", &scene)])?
        } else {
            prompts.player.creation.clone()
        };
        Ok(Message::text(
            Role::System,
            format!(
                "{text}

{}",
                prompts.player.turn_end
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
    ) -> Result<Vec<Tool>> {
        let prompts = prompts::current()?;
        let game = Arc::clone(self);
        let hp_member = member.clone();
        let hit_protection = Tool::new(
            prompts
                .player
                .tools
                .tool("roll_hit_protection", prompts::no_arguments())?,
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
            prompts
                .player
                .tools
                .tool("roll_attributes", prompts::no_arguments())?,
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
            prompts
                .player
                .tools
                .tool("ready_to_begin", prompts::no_arguments())?,
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
                    let narrations = game
                        .opening_narrations(
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
                            for narration in &narrations {
                                delivery_game
                                    .deliver_narration(member.world_id, narration)
                                    .await?;
                            }
                            Ok(())
                        }),
                    })
                }) as ToolFuture
            },
        );
        Ok(vec![hit_protection, attributes, ready])
    }

    fn action_tools(
        self: &Arc<Self>,
        member: PlayerAgent,
        sequence_id: i64,
        budget: Budget,
        call: CallContext,
    ) -> Result<Vec<Tool>> {
        let prompts = prompts::current()?;
        let game = Arc::clone(self);
        let presence_member = member.clone();
        let party_presence = Tool::new(
            prompts
                .player
                .tools
                .tool("party_presence", prompts::no_arguments())?,
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
        let mut tools = vec![party_presence];
        for ActionKind {
            definition,
            validate,
        } in action_kinds()?
        {
            tools.push(self.action_tool(PlayerActionTool {
                member: member.clone(),
                sequence_id,
                definition,
                validate,
                budget: budget.clone(),
                call: call.clone(),
            }));
        }
        Ok(tools)
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
                let narration = game.arbitrate(pending, budget, call, None).await?;
                // An approved action that could not happen is this call's
                // result in place of the usual one; the GM's narration is
                // still delivered and the agent continues.
                let content = match game
                    .store
                    .failed_action_reason(requesting_tool_execution_id)
                    .await?
                {
                    Some(reason) => format!("The action could not be applied: {reason}"),
                    // Approved speech: the words are in the call, so say who heard them.
                    None => match game
                        .store
                        .speech_outcome(requesting_tool_execution_id)
                        .await?
                    {
                        Some(outcome) => outcome,
                        None => "The GM's narration reached the player as its own entry. Respond to the player's next choice.".to_string(),
                    },
                };
                let delivery_game = Arc::clone(&game);
                Ok(ToolOutcome::CompletedAfter {
                    content,
                    after_result: Box::pin(async move {
                        delivery_game
                            .store
                            .show_speech_with_result(requesting_tool_execution_id)
                            .await?;
                        delivery_game
                            .deliver_narration(member.world_id, &narration)
                            .await
                    }),
                })
            }) as ToolFuture
        })
    }

    /// What the GM is shown of the scene it judges and narrates, without the
    /// fields that hold nothing.
    fn gm_packet(scene: &GmScene) -> Result<String> {
        let mut scene = serde_json::to_value(scene).context("serializing the GM scene")?;
        prompts::drop_blanks(&mut scene);
        Ok(format!(
            "Current scene packet:
{scene}"
        ))
    }

    /// The location GM, not the player agent, introduces a character who has
    /// just completed creation. The opening is a world transition, so it runs
    /// immediately from ready_to_begin instead of waiting for another player
    /// message to select normal scene tools. A group's initial scene is
    /// narrated first, to the NPCs in it, so they know it before the first
    /// player is; then the character's entrance is narrated with them as the
    /// actor, receiving the scene while everyone else is told they arrive.
    async fn opening_narrations(
        self: &Arc<Self>,
        member: &PlayerAgent,
        sequence_id: i64,
        budget: Budget,
        call: CallContext,
    ) -> Result<Vec<Narration>> {
        let group_id = self.store.character_group_id(member.character_id).await?;
        let prompts = prompts::current()?;
        let character = self.store.character_name(member.character_id).await?;
        let mut narrations = Vec::new();
        if !self.store.group_scene_narrated(group_id).await? {
            let mut scene = self
                .opening_turn(
                    member,
                    sequence_id,
                    &budget,
                    &call,
                    prompts::fill(&prompts.gm.scene_request, &[("character", &character)])?,
                    false,
                )
                .await?;
            scene.opens_scene = true;
            scene.restrict_to(self.store.group_npc_ids(group_id).await?);
            narrations.push(scene);
        }
        narrations.push(
            self.opening_turn(
                member,
                sequence_id,
                &budget,
                &call,
                prompts::fill(&prompts.gm.entrance_request, &[("character", &character)])?,
                true,
            )
            .await?,
        );
        Ok(narrations)
    }

    /// One GM turn of the opening, ended by a narrate tool. `entering` makes
    /// the entering character the actor of the narration.
    async fn opening_turn(
        self: &Arc<Self>,
        member: &PlayerAgent,
        sequence_id: i64,
        budget: &Budget,
        call: &CallContext,
        request: String,
        entering: bool,
    ) -> Result<Narration> {
        let group_id = self.store.character_group_id(member.character_id).await?;
        let gm_history = self.store.player_group_history(member).await?;
        let branch = self.store.group_branch(group_id).await?;
        let scene = self
            .store
            .gm_scene(member.world_id, gm_history.branch_id)
            .await
            .context("loading group GM scene for the opening")?;
        let scene_packet = Self::gm_packet(&scene)?;
        let prompts = prompts::current()?;
        let role = Message::text(Role::System, &prompts.gm.role);
        let request = Message::text(Role::User, request);
        let actor = entering.then_some(member.character_id);
        let narrate = NarrationTools::new(
            self,
            NarrationScope {
                world_id: member.world_id,
                in_group: member.character_id,
                actor,
                presented: PresentedActions::default(),
            },
        )?;
        let reminder = narrate.reminder()?;
        self.store
            .append_instruction(branch, &request, sequence_id)
            .await
            .context("storing opening narration event")?;
        let settled = agent::complete_with_call_context(
            &self.store,
            &self.backend,
            budget,
            agent::Turn {
                events: self.world(member.world_id)?.events.clone(),
                branch,
                static_messages: &[role],
                dynamic_context: agent::DynamicContext::Fixed(&[scene_packet.as_str()]),
                tools: &narrate.tools,
                terminal_reminder: Some(&reminder),
                sampling: self.sampling.clone(),
                model: &self.model,
            },
            call,
            |_| {},
            |_| {},
        )
        .await
        .context("asking group GM for opening narration")?;
        narrate.finish((group_id, sequence_id), settled.inference_id, actor)
    }

    /// `note` adds circumstances the GM should weigh, such as replies composed
    /// at the same moment.
    async fn arbitrate(
        self: &Arc<Self>,
        pending: PendingAction,
        budget: Budget,
        call: CallContext,
        note: Option<&str>,
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
        let prompts = prompts::current()?;
        let presented: PresentedActions = Arc::new(Mutex::new(vec![pending.clone()]));
        let game = Arc::clone(self);
        let approval_presented = Arc::clone(&presented);
        // Every approval carries the speech controls, because the turn may also
        // approve a speech reaction it asked an NPC for.
        let approval = prompts.gm.tools.tool(
            "approve_action",
            serde_json::to_value(schema_for!(ApproveAction))
                .expect("approval schema should serialize"),
        )?;
        let gm_tool = Tool::new(approval, move |context| {
            let arguments = context.arguments.to_string();
            let tool_execution_id = context.tool_execution_id;
            let game = Arc::clone(&game);
            let presented = Arc::clone(&approval_presented);
            Box::pin(async move {
                let approval: ApproveAction =
                    match serde_json::from_str(&arguments).context("parsing action approval") {
                        Ok(approval) => approval,
                        Err(error) => return Ok(ToolOutcome::rejected(error)),
                    };
                // The action it was presented, or a reaction it asked for.
                let Some(approval_pending) = presented_action(&presented, approval.action_id)
                else {
                    return Ok(ToolOutcome::Rejected(format!(
                        "Tool arguments were rejected: GM approved action {} but was presented {}",
                        approval.action_id,
                        presented_ids(&presented)
                    )));
                };
                let action_id = approval_pending.id;
                if (!approval.include.is_empty()
                    || !approval.exclude.is_empty()
                    || approval.prefix.is_some())
                    && approval_pending.tool != "speak"
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
                let next = next_step(&game.store, approval_pending.world_id, &presented).await?;
                match resolved.outcome {
                    // The GM saw the words in the request; it needs who heard them.
                    ActionOutcome::Applied(_) if !resolved.heard_by.is_empty() => {
                        Ok(ToolOutcome::Completed(format!(
                            "Action {action_id} approved. Delivered as spoken to {}. {} {next}",
                            resolved.heard_by.join(", "),
                            prompts::current()?.gm.speech_conveyed
                        )))
                    }
                    ActionOutcome::Applied(result) => {
                        // What the action did, if it says: "approved: flour sack transferred".
                        let done = result
                            .strip_prefix("approved")
                            .unwrap_or(&result)
                            .trim_start_matches(':')
                            .trim();
                        let done = if done.is_empty() {
                            String::new()
                        } else {
                            format!(" {done}.")
                        };
                        Ok(ToolOutcome::Completed(format!(
                            "Action {action_id} approved.{done} {} {next}",
                            prompts::current()?.gm.outcome_untold
                        )))
                    }
                    ActionOutcome::Failed(reason) => Ok(ToolOutcome::Completed(format!(
                        "Action {action_id} approved, but it could not be applied: {reason} {next}"
                    ))),
                    ActionOutcome::Rejected(_) => Ok(ToolOutcome::Rejected(format!(
                        "Tool arguments were rejected: action {action_id} was already rejected, it cannot now be approved"
                    ))),
                }
            }) as ToolFuture
        });
        let game = Arc::clone(self);
        let rejection_presented = Arc::clone(&presented);
        let reject_tool = Tool::new(
            prompts.gm.tools.tool(
                "reject_action",
                serde_json::to_value(schema_for!(RejectAction))
                    .expect("rejection schema should serialize"),
            )?,
            move |context| {
                let arguments = context.arguments.to_string();
                let tool_execution_id = context.tool_execution_id;
                let game = Arc::clone(&game);
                let presented = Arc::clone(&rejection_presented);
                Box::pin(async move {
                    let rejection: RejectAction = match serde_json::from_str(&arguments)
                        .context("parsing action rejection")
                    {
                        Ok(rejection) => rejection,
                        Err(error) => return Ok(ToolOutcome::rejected(error)),
                    };
                    let Some(pending) = presented_action(&presented, rejection.action_id) else {
                        return Ok(ToolOutcome::Rejected(format!(
                            "Tool arguments were rejected: GM rejected action {} but was presented {}",
                            rejection.action_id,
                            presented_ids(&presented)
                        )));
                    };
                    let action_id = pending.id;
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
                            "Action {action_id} rejected: {} {}",
                            rejection.reason,
                            next_step(&game.store, pending.world_id, &presented).await?
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
            prompts.gm.tools.tool(
                "split_group",
                serde_json::to_value(schema_for!(SplitGroup))
                    .expect("split_group schema should serialize"),
            )?,
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
                        group_id,
                        partitions: split
                            .partitions
                            .into_iter()
                            .map(|partition| GroupPartition {
                                character_ids: partition.character_ids,
                                name: partition.name,
                            })
                            .collect(),
                        continue_with_character_id: None,
                    }))
                }) as ToolFuture
            },
        );
        let merge_tool = Tool::new(
            prompts.gm.tools.tool(
                "merge_groups",
                serde_json::to_value(schema_for!(MergeGroups))
                    .expect("merge_groups schema should serialize"),
            )?,
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
                    }))
                }) as ToolFuture
            },
        );
        let actor = self.store.character_name(pending.character_id).await?;
        let game = Arc::clone(self);
        let check_pending = pending.clone();
        let check_presented = Arc::clone(&presented);
        let check_budget = budget.clone();
        let intervention_tool = Tool::new(
            prompts.gm.tools.tool(
                "intervention_check",
                serde_json::to_value(schema_for!(InterventionCheck))
                    .expect("intervention check schema should serialize"),
            )?,
            move |context| {
                let arguments = context.arguments.to_string();
                let tool_execution_id = context.tool_execution_id;
                let game = Arc::clone(&game);
                let pending = check_pending.clone();
                let presented = Arc::clone(&check_presented);
                let budget = check_budget.clone();
                let actor = actor.clone();
                Box::pin(async move {
                    let check: InterventionCheck = match serde_json::from_str(&arguments)
                        .context("parsing intervention_check arguments")
                    {
                        Ok(check) => check,
                        Err(error) => return Ok(ToolOutcome::rejected(error)),
                    };
                    if check.action_id != pending.id {
                        return Ok(ToolOutcome::Rejected(format!(
                            "Tool arguments were rejected: characters can only be asked about the presented action {}, not {}",
                            pending.id, check.action_id
                        )));
                    }
                    let group_id = game.store.character_group_id(pending.character_id).await?;
                    let npcs = game.store.group_npc_ids(group_id).await?;
                    let mut asked = Vec::new();
                    for character_id in check.character_ids {
                        if character_id == pending.character_id {
                            return Ok(ToolOutcome::Rejected(format!(
                                "Tool arguments were rejected: character {character_id} is taking the action, so their intent is the action itself"
                            )));
                        }
                        if !npcs.contains(&character_id) {
                            return Ok(ToolOutcome::Rejected(format!(
                                "Tool arguments were rejected: character {character_id} cannot be asked to intervene in this group"
                            )));
                        }
                        if !asked.contains(&character_id) {
                            asked.push(character_id);
                        }
                    }
                    if asked.is_empty() {
                        return Ok(ToolOutcome::Rejected(
                            "Tool arguments were rejected: name at least one character to ask"
                                .to_string(),
                        ));
                    }
                    let prompts = prompts::current()?;
                    let world = game.world(pending.world_id)?;
                    let mut proposals = Vec::new();
                    for character_id in asked {
                        budget
                            .spend(0)
                            .context("asking a character about an intervention")?;
                        let branch = game.store.npc_branch(character_id).await?;
                        let name = game.store.character_name(character_id).await?;
                        game.store
                            .append_instruction(
                                branch,
                                &Message::text(
                                    Role::User,
                                    prompts::fill(
                                        &prompts.npc.intervention_request,
                                        &[
                                            ("actor", &actor),
                                            ("tool", &pending.tool),
                                            ("arguments", &pending.args),
                                        ],
                                    )?,
                                ),
                                pending.sequence_id,
                            )
                            .await?;
                        let invocation_id = game
                            .store
                            .start_invocation(
                                Some(pending.sequence_id),
                                branch,
                                Some(tool_execution_id),
                                None,
                            )
                            .await?;
                        let mut inference_id = None;
                        let (inference, call, decision) = match game
                            .ask_npc(
                                &world,
                                branch,
                                pending.sequence_id,
                                invocation_id,
                                character_id,
                                &mut inference_id,
                            )
                            .await
                        {
                            Ok(answer) => answer,
                            Err(error) => {
                                game.store
                                    .fail_reaction(
                                        invocation_id,
                                        inference_id,
                                        &format!("{error:#}"),
                                    )
                                    .await?;
                                return Err(
                                    error.context(format!("asking {name} whether to intervene"))
                                );
                            }
                        };
                        let execution_id = game
                            .store
                            .record_reaction(
                                branch,
                                invocation_id,
                                inference,
                                &call,
                                npc::decision_result(&decision),
                            )
                            .await?;
                        if let Decision::Propose(proposal) = decision {
                            proposals.push((character_id, name, execution_id, proposal));
                        }
                    }
                    // As for any replies composed together, the strongest wish
                    // is proposed; ties all stand.
                    let top = proposals
                        .iter()
                        .map(|(_, _, _, proposal)| proposal.eagerness)
                        .max();
                    let mut reports = Vec::new();
                    for (character_id, name, execution_id, proposal) in proposals {
                        if Some(proposal.eagerness) != top {
                            continue;
                        }
                        let reaction = game
                            .store
                            .create_reaction_action(
                                pending.world_id,
                                character_id,
                                pending.sequence_id,
                                execution_id,
                                &proposal.tool,
                                &proposal.arguments,
                            )
                            .await?;
                        reports.push(format!(
                            "{name} proposes action {}: a `{}` request with arguments {}.",
                            reaction.id, proposal.tool, proposal.arguments
                        ));
                        presented
                            .lock()
                            .expect("presented actions lock poisoned")
                            .push(reaction);
                    }
                    Ok(ToolOutcome::Completed(if reports.is_empty() {
                        format!("Nobody intervenes in action {}.", pending.id)
                    } else {
                        format!(
                            "{} Approve or reject each, then decide action {}; a reaction does not stop it by itself.",
                            reports.join(" "),
                            pending.id
                        )
                    }))
                }) as ToolFuture
            },
        );
        let narrate = NarrationTools::new(
            self,
            NarrationScope {
                world_id: pending.world_id,
                in_group: pending.character_id,
                actor: Some(pending.character_id),
                presented: Arc::clone(&presented),
            },
        )?;
        let reminder = narrate.reminder()?;
        let tools: Vec<Tool> = [
            gm_tool,
            reject_tool,
            split_tool,
            merge_tool,
            intervention_tool,
        ]
        .into_iter()
        .chain(narrate.tools.iter().cloned())
        .collect();
        let scene = self
            .store
            .gm_scene(pending.world_id, pending.gm_branch_id)
            .await
            .context("loading group GM scene for prompt")?;
        let actor = self.store.character_name(pending.character_id).await?;
        let scene_packet = Self::gm_packet(&scene)?;
        let role = Message::text(Role::System, &prompts.gm.role);
        let note = note.map(|note| format!(" {note}")).unwrap_or_default();
        let request = Message::text(
            Role::User,
            prompts::fill(
                &prompts.gm.action_request,
                &[
                    ("actor", &actor),
                    ("action_id", &action_id.to_string()),
                    ("tool", &pending.tool),
                    ("arguments", &pending.args),
                    ("note", &note),
                ],
            )?,
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
                dynamic_context: agent::DynamicContext::Fixed(&[scene_packet.as_str()]),
                tools: &tools,
                terminal_reminder: Some(&reminder),
                sampling: self.sampling.clone(),
                model: &self.model,
            },
            &call.child(pending.requesting_tool_execution_id),
            |_| {},
            |_| {},
        )
        .await
        .with_context(|| format!("arbitrating action {action_id}"))?;
        // A split or merge in this turn may have moved the actor, and the
        // narration describes where they now are.
        let mut narration = narrate.finish(
            (
                self.store.character_group_id(pending.character_id).await?,
                pending.sequence_id,
            ),
            settled.inference_id,
            Some(pending.character_id),
        )?;
        // The GM saw what was said; only those who heard it are told how the
        // GM describes it. Speech that was not delivered was heard by no one,
        // so only its speaker is told what became of it.
        match self
            .store
            .speech_audience(pending.requesting_tool_execution_id)
            .await?
        {
            None if pending.tool == "speak" => {
                narration.restrict_to(vec![pending.character_id]);
            }
            Some(audience) => narration.restrict_to(audience),
            None => {}
        }
        Ok(narration)
    }
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct AttemptAction {
    // The GM reads it in the request; Rust does not act on it.
    #[serde(rename = "kind")]
    _kind: AttemptKind,
    // What the character does, and to what.
    description: String,
}

// The ways to attempt something; they differ only in the intent the GM reads.
#[derive(Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
#[schemars(inline)]
enum AttemptKind {
    Look,
    Investigate,
    Open,
    Ask,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct TakeAction {
    // The exact item name from the current scene.
    item: String,
    // Whether the character takes it despite any objection. The GM reads it in
    // the request; Rust does not act on it.
    #[serde(default, rename = "committed")]
    _committed: bool,
}

fn validate_attempt_action(arguments: &str) -> Result<()> {
    let action: AttemptAction =
        serde_json::from_str(arguments).context("parsing attempt action arguments")?;
    ensure!(
        !action.description.trim().is_empty(),
        "action description is empty"
    );
    Ok(())
}

fn validate_speak_action(arguments: &str) -> Result<()> {
    let action: SpeakAction =
        serde_json::from_str(arguments).context("parsing speak action arguments")?;
    ensure!(!action.words.trim().is_empty(), "words are empty");
    if action.private {
        ensure!(
            !action.to.is_empty(),
            "a private speech needs at least one listener in `to`"
        );
    }
    Ok(())
}

/// The characters a speech action names, already validated as JSON.
fn named_hearers(tool: &str, arguments: &str) -> Vec<String> {
    match tool {
        "speak" => serde_json::from_str::<SpeakAction>(arguments)
            .map(|speak| if speak.private { speak.to } else { Vec::new() })
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

/// The action with this id among those presented to the GM this turn.
fn presented_action(presented: &PresentedActions, id: i64) -> Option<PendingAction> {
    presented
        .lock()
        .expect("presented actions lock poisoned")
        .iter()
        .find(|action| action.id == id)
        .cloned()
}

/// What the GM reads last in a decision's result: the presented actions still
/// waiting for its decision, or that it can narrate once none are.
async fn next_step(store: &Store, world_id: i64, presented: &PresentedActions) -> Result<String> {
    let ids: Vec<i64> = presented
        .lock()
        .expect("presented actions lock poisoned")
        .iter()
        .map(|action| action.id)
        .collect();
    let mut undecided = Vec::new();
    for id in ids {
        if store.has_pending_action(world_id, id).await? {
            undecided.push(id.to_string());
        }
    }
    let prompts = prompts::current()?;
    if undecided.is_empty() {
        Ok(prompts.gm.narrate_next.clone())
    } else {
        prompts::fill(
            &prompts.gm.decide_next,
            &[("actions", &format!("action {}", undecided.join(", ")))],
        )
    }
}

/// Which actions the GM was presented, for telling it when it names another.
fn presented_ids(presented: &PresentedActions) -> String {
    let ids: Vec<String> = presented
        .lock()
        .expect("presented actions lock poisoned")
        .iter()
        .map(|action| action.id.to_string())
        .collect();
    format!("action {}", ids.join(", "))
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
struct InterventionCheck {
    // The visible action id they may intervene in.
    action_id: i64,
    // The NPCs to ask.
    character_ids: Vec<i64>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ApproveAction {
    // The visible action id to approve.
    action_id: i64,
    // For speech: characters who also hear it, by name.
    #[serde(default)]
    include: Vec<String>,
    // For speech: characters who must not hear it, by name.
    #[serde(default)]
    exclude: Vec<String>,
    // For speech: a short phrase every hearer is shown before the words, such
    // as how or why only some of them can hear.
    #[serde(default)]
    #[schemars(length(max = MAX_PREFIX_CHARS))]
    prefix: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct RejectAction {
    // The visible action id to reject.
    action_id: i64,
    // Why the action cannot happen as requested.
    reason: String,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct SplitGroupPartition {
    // The character IDs going to this resulting group. Every currently
    // present character must appear in exactly one partition.
    character_ids: Vec<i64>,
    // A short name for the resulting group.
    name: String,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct SplitGroup {
    // A complete, nonempty, disjoint partition of the group's current
    // characters: every character present in exactly one partition.
    partitions: Vec<SplitGroupPartition>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct MergeGroups {
    // At least two distinct group IDs at the same location to merge.
    group_ids: Vec<i64>,
    // A short name for the resulting merged group.
    name: String,
}

#[cfg(test)]
mod tests {
    use std::{
        collections::VecDeque,
        sync::{
            Mutex,
            atomic::{AtomicBool, AtomicUsize, Ordering},
        },
    };

    use super::*;
    use crate::{
        inference::InferenceScheduler,
        llm::{ContextCapacityExceeded, MessageContent, Response, ToolCall, Usage},
        mistralrs_backend::MistralRsBackend,
        scenario::Scenario,
        settings::Settings,
    };

    struct ScriptedBackend {
        responses: Arc<Mutex<VecDeque<Response>>>,
        requests: Arc<Mutex<Vec<crate::llm::Request>>>,
        /// When set, an inference with no scripted response never returns,
        /// as if the process were stopped mid-inference.
        hang_when_empty: Arc<AtomicBool>,
        /// Holds the numbered (1-based) request until released, telling
        /// `reached` when it arrives.
        gate: Arc<Mutex<Option<Gate>>>,
        /// Rejects a request over a size the way the fixed-size model does.
        fixed_context: Arc<FixedContext>,
    }

    /// A stand-in for the model's fixed context: requests bigger than
    /// `max_chars` (0 for no limit) are rejected as over capacity, and
    /// compaction's summary request is answered without using the script.
    #[derive(Default)]
    struct FixedContext {
        max_chars: AtomicUsize,
        rejections: AtomicUsize,
    }

    struct Gate {
        request: usize,
        reached: Arc<tokio::sync::Notify>,
        release: Arc<tokio::sync::Notify>,
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
                    hang_when_empty: Arc::new(AtomicBool::new(false)),
                    gate: Arc::new(Mutex::new(None)),
                    fixed_context: Arc::new(FixedContext::default()),
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
            let max_chars = self.fixed_context.max_chars.load(Ordering::SeqCst);
            if max_chars > 0 {
                let text = |message: &Message| match &message.content {
                    MessageContent::Text(text) => text.clone(),
                    MessageContent::ToolCalls(calls) => {
                        calls.iter().map(|call| call.arguments.as_str()).collect()
                    }
                    MessageContent::ToolResult { content, .. } => content.clone(),
                    MessageContent::Speech(speech) => speech.text.clone(),
                };
                let size: usize = request
                    .messages
                    .iter()
                    .map(|message| text(message).len())
                    .sum();
                let summary_request = crate::prompts::current()
                    .unwrap()
                    .compaction
                    .summary_request
                    .clone();
                let asks_for_summary = request.messages.last().is_some_and(
                    |message| matches!(&message.content, MessageContent::Text(text) if *text == summary_request),
                );
                self.requests.lock().unwrap().push(request);
                if asks_for_summary {
                    return Ok(response(Content::Text("Summary of earlier play.".into())));
                }
                if size > max_chars {
                    self.fixed_context.rejections.fetch_add(1, Ordering::SeqCst);
                    return Err(ContextCapacityExceeded::FixedKv {
                        requested_tokens: size,
                        max_context_tokens: max_chars,
                    }
                    .into());
                }
                return self
                    .responses
                    .lock()
                    .unwrap()
                    .pop_front()
                    .context("scripted backend received an unexpected inference");
            }
            let number = {
                let mut requests = self.requests.lock().unwrap();
                requests.push(request);
                requests.len()
            };
            let gate = self
                .gate
                .lock()
                .unwrap()
                .as_ref()
                .filter(|gate| gate.request == number)
                .map(|gate| (Arc::clone(&gate.reached), Arc::clone(&gate.release)));
            if let Some((reached, release)) = gate {
                reached.notify_one();
                release.notified().await;
            }
            let next = self.responses.lock().unwrap().pop_front();
            if next.is_none() && self.hang_when_empty.load(Ordering::SeqCst) {
                std::future::pending::<()>().await;
            }
            next.context("scripted backend received an unexpected inference")
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

    /// Everything the model was shown in one request: each message's text, its
    /// own earlier tool calls and their results, and the names of the tools it
    /// could call.
    fn shown(request: &crate::llm::Request) -> String {
        let mut shown = String::new();
        for message in &request.messages {
            match &message.content {
                MessageContent::Text(text) => shown.push_str(text),
                MessageContent::ToolCalls(calls) => {
                    for call in calls {
                        shown.push_str(&format!("[called {} {}]", call.name, call.arguments));
                    }
                }
                MessageContent::ToolResult { content, .. } => {
                    shown.push_str(&format!("[result {content}]"))
                }
                MessageContent::Speech(speech) => shown.push_str(&speech.text),
            }
            shown.push('\n');
        }
        for tool in &request.tools {
            shown.push_str(&tool.name);
            shown.push('\n');
        }
        shown
    }

    #[tokio::test]
    async fn a_world_is_generated_from_a_seed_into_reviewed_objects_and_then_opens_play() {
        let db = crate::store::TestDatabase::new("generation-test");
        let store = Store::open(db.path()).await.unwrap();
        let owner = store
            .find_or_create_user("maker@example.test", "Maker")
            .await
            .unwrap();
        let member = store
            .create_world_to_generate(&owner, "A drowned city")
            .await
            .unwrap();
        let call = |name: &str, arguments: &str| {
            response(Content::ToolCalls(vec![ToolCall {
                id: name.into(),
                name: name.into(),
                arguments: arguments.into(),
            }]))
        };
        let text = |text: &str| response(Content::Text(text.into()));
        let calls = |calls: &[(&str, &str)]| {
            response(Content::ToolCalls(
                calls
                    .iter()
                    .map(|(name, arguments)| ToolCall {
                        id: (*name).into(),
                        name: (*name).into(),
                        arguments: (*arguments).into(),
                    })
                    .collect(),
            ))
        };
        let (scripted, requests) = ScriptedBackend::recording([
            text("Meta answers."),
            call(
                "ask_questions",
                r#"{"questions":["Who rules?","What is feared?"]}"#,
            ),
            text("The tide-wardens rule."),
            text("The drowned bell."),
            text("Summary: a city ruled by tide-wardens. [[Ysolde]] guards the [[Drowned Bell]]."),
            call("world_summary_complete", "{}"),
            text("  The Drowned Bell \n"),
            calls(&[
                (
                    "create_object",
                    r#"{"object":"location","name":"Belfry","location_kind":"tower","description":"A tower above the water."}"#,
                ),
                (
                    "create_object",
                    r#"{"object":"npc","name":"Ysolde","group_name":"Wardens","location_id":1,"description":"A stern warden.","background":"Lost her brother to the tide.","motive":"Keep the bell silent."}"#,
                ),
                (
                    "create_object",
                    r#"{"object":"location","name":"Belfry","location_kind":"tower","description":"Again."}"#,
                ),
            ]),
            text("The objects exist."),
            // The first review asks a question instead of completing...
            calls(&[
                ("read_object", r#"{"objects":[{"object":"npc","id":2}]}"#),
                (
                    "ask_questions",
                    r#"{"questions":["Who stands against Ysolde?"]}"#,
                ),
            ]),
            // ...the Storyteller adds an object for it...
            calls(&[(
                "create_object",
                r#"{"object":"npc","name":"Corvin","group_id":1,"description":"A rival warden.","background":"Wants her post.","motive":"Replace Ysolde."}"#,
            )]),
            text("Added Corvin."),
            // ...and the second review marks everything and completes.
            calls(&[
                (
                    "mark_object_reviewed",
                    r#"{"object":"location","id":1,"reviewed":true}"#,
                ),
                (
                    "mark_object_reviewed",
                    r#"{"object":"group","id":1,"reviewed":true}"#,
                ),
                (
                    "mark_object_reviewed",
                    r#"{"object":"npc","id":2,"reviewed":true}"#,
                ),
                (
                    "mark_object_reviewed",
                    r#"{"object":"npc","id":3,"reviewed":true}"#,
                ),
                ("core_objects_created", "{}"),
            ]),
        ]);
        let backend = InferenceScheduler::new(scripted, Limits::default()).unwrap();
        // Listen before the worker starts so no step goes unseen.
        let worlds = World::load(&store).await.unwrap();
        let mut events = World::get(&worlds, member.world_id)
            .unwrap()
            .events
            .subscribe();
        let _game = Game::new(
            store.clone(),
            worlds,
            backend.foreground(),
            Limits::default(),
            "scripted".into(),
            Sampling {
                temperature: 0.0,
                ..Default::default()
            },
        );

        // Five steps (meta, ask, name, create objects, review) run until the world is
        // ready; a failed world never becomes ready.
        while !store.world_is_ready(member.world_id).await.unwrap() {
            let waited =
                tokio::time::timeout(std::time::Duration::from_secs(10), events.recv()).await;
            let failure = store.world(member.world_id).await.unwrap().unwrap().failure;
            if failure.is_some() {
                let seen = requests.lock().unwrap();
                panic!(
                    "the world failed after {} requests: {failure:?}\nlast: {}",
                    seen.len(),
                    seen.last()
                        .map(|request| {
                            let text = shown(request);
                            text[text.len().saturating_sub(1800)..].to_string()
                        })
                        .unwrap_or_default()
                );
            }
            waited.expect("a generation step did not finish").unwrap();
        }

        assert!(
            store
                .next_generation(member.world_id)
                .await
                .unwrap()
                .is_none(),
            "no step is left once the core objects are created"
        );
        let world = store.world(member.world_id).await.unwrap().unwrap();
        assert_eq!(world.name, "The Drowned Bell");
        assert!(world.ready, "reviewed core objects open play");
        assert!(world.failure.is_none());
        let objects = store.review_list(member.world_id).await.unwrap();
        assert_eq!(objects.len(), 4);
        assert!(objects.iter().all(|object| object.reviewed));
        store.character_group_id(member.character_id).await.unwrap();

        let shown: Vec<String> = requests.lock().unwrap().iter().map(shown).collect();
        assert_eq!(shown.len(), 13);
        assert!(shown[0].contains("A drowned city"));
        let prompts = crate::prompts::current().unwrap();
        let sampled = |questions: &[String]| {
            questions
                .iter()
                .filter(|question| shown[0].contains(question.as_str()))
                .count()
        };
        assert_eq!(
            sampled(&prompts.questioner.meta_questions),
            prompts.questioner.sample_size
        );
        assert_eq!(
            sampled(&prompts.questioner.example_questions),
            prompts.questioner.sample_size
        );
        assert!(shown[1].contains("ask_questions") && shown[1].contains("world_summary_complete"));
        // The Storyteller never sees the Questioner's meta-questions.
        assert!(shown[2].contains("Who rules?"));
        assert!(
            prompts
                .questioner
                .meta_questions
                .iter()
                .all(|question| !shown[2].contains(question.as_str()))
        );
        assert!(shown[3].contains("What is feared?"));
        // Only the summary reaches the Questioner, not the Storyteller's answers.
        assert!(shown[5].contains("Summary: a city ruled by tide-wardens."));
        assert!(
            !shown[5].contains("The tide-wardens rule.") && !shown[5].contains("The drowned bell.")
        );

        // Only the Storyteller's first round of answers runs at its own temperature.
        let temperatures: Vec<f32> = requests
            .lock()
            .unwrap()
            .iter()
            .map(|request| request.sampling.temperature)
            .collect();
        let hot = prompts.storyteller.first_round_temperature;
        assert_eq!(temperatures[..4], [0.0, 0.0, hot, hot]);
        assert!(
            temperatures[4..]
                .iter()
                .all(|temperature| *temperature == 0.0)
        );

        // The Storyteller creates objects with creation tools and sees the one summary.
        assert!(
            shown[7].contains("no object has them yet: Ysolde, Drowned Bell"),
            "{}",
            shown[7]
        );
        assert!(
            shown[7].contains("Create the world's objects"),
            "{}",
            shown[7]
        );
        assert!(
            shown[7].contains("create_object") && shown[7].contains("a city ruled by tide-wardens")
        );
        // A refused call reaches the Storyteller as a tool result it can act on.
        assert!(shown[8].contains("called `Belfry`"), "{}", shown[8]);
        // The Questioner reviews the objects, not the Storyteller's answers.
        assert!(shown[9].contains("- [ ] npc 2 Ysolde") && shown[9].contains("read_object"));
        assert!(!shown[9].contains("The tide-wardens rule."));
        // It is told which marked name has no object, and not the one that has.
        assert!(
            shown[9].contains("no object has them yet: Drowned Bell"),
            "{}",
            shown[9]
        );
        // The Questioner's question sends the Storyteller back to create objects, and it
        // is shown what already exists (a fact, not kept in its chat).
        assert!(
            shown[10].contains("Who stands against Ysolde?"),
            "{}",
            shown[10]
        );
        assert!(
            shown[10]
                .contains("The world so far (locations: 1, connections: 0, groups: 1, npcs: 1")
                && shown[10].contains("- location 1 Belfry")
                && shown[10].contains("- npc 2 Ysolde"),
            "{}",
            shown[10]
        );
        // The first request already shows what is missing: nothing exists yet.
        assert!(
            shown[7].contains("The world so far (locations: 0, connections: 0, groups: 0, npcs: 0")
        );
        // The Storyteller's change comes back to the Questioner as the result
        // of its own question, in the same turn.
        assert!(
            shown[12].contains("Added Corvin.")
                && shown[12].contains("Changes to the world:")
                && shown[12].contains("- added npc 3 Corvin")
                && !shown[12].contains("create_object"),
            "{}",
            shown[12]
        );
        // The result says the summary changed; the summary itself is the
        // Questioner's context.
        assert!(
            shown[5].contains("rewrote the world summary")
                && shown[5].contains("The world summary:"),
            "{}",
            shown[5]
        );
        // It is the new summary, once, in the request after the change and not
        // in the request before it.
        assert_eq!(shown[5].matches("a city ruled by tide-wardens").count(), 1);
        assert!(!shown[1].contains("a city ruled by tide-wardens"));
    }

    #[tokio::test]
    async fn a_player_can_open_a_world_being_created_but_nothing_reaches_it() {
        let db = crate::store::TestDatabase::new("generation-refusal");
        let store = Store::open(db.path()).await.unwrap();
        let owner = store
            .find_or_create_user("maker@example.test", "Maker")
            .await
            .unwrap();
        let member = store
            .create_world_to_generate(&owner, "A drowned city")
            .await
            .unwrap();
        // No model response is scripted, so the first step fails the world.
        let (scripted, requests) = ScriptedBackend::recording([]);
        let backend = InferenceScheduler::new(scripted, Limits::default()).unwrap();
        let worlds = World::load(&store).await.unwrap();
        let game = Game::new(
            store.clone(),
            worlds,
            backend.foreground(),
            Limits::default(),
            "scripted".into(),
            Sampling::default(),
        );

        let entered = game.enter(member.clone()).await.unwrap_err();
        assert!(format!("{entered:#}").contains("still being created"));
        let spoke = game
            .player_message(member.clone(), "hello")
            .await
            .unwrap_err();
        assert!(format!("{spoke:#}").contains("still being created"));
        assert!(
            requests
                .lock()
                .unwrap()
                .iter()
                .all(|request| { !shown(request).contains("hello") }),
            "refused input reached no model"
        );
    }

    /// A world whose summary is finished and named, ready for the Storyteller
    /// to create its objects.
    async fn saltgate_ready_for_objects(
        name: &str,
    ) -> (crate::store::TestDatabase, Store, crate::store::PlayerAgent) {
        use crate::store::Advance;
        const SALTGATE_SUMMARY: &str = "Saltgate is a smuggler town on an estuary whose tide has stopped             coming in. The Harbor Guild, led by Warden [[Ysolde Varn]], blames the smugglers and             wants the channel dredged. The smugglers, led by [[Captain Brannoch]], hide their             cargo in the [[Bell Cellar]] below [[Saltgate Quay]], where a drowned bell rings             against the tide; Brannoch wants the tide to stay wrong because it hides his boats.             Ysolde's brother drowned near the bell and she will not let the cellar stay hidden.             Players arrive at the quay with a [[Guild writ]] from a distant client who wants the             bell silenced, and must choose between the Guild, the smugglers and their own aims.";
        let db = crate::store::TestDatabase::new(name);
        let store = Store::open(db.path()).await.unwrap();
        let owner = store
            .find_or_create_user("maker@example.test", "Maker")
            .await
            .unwrap();
        let member = store
            .create_world_to_generate(
                &owner,
                "A smuggler's port town where something is wrong with the tide.",
            )
            .await
            .unwrap();
        store
            .save_world_summary(member.world_id, SALTGATE_SUMMARY)
            .await
            .unwrap();
        for advance in [
            Advance::MetaQuestionsAnswered,
            Advance::WorldSummaryComplete,
            Advance::Name("Saltgate".into()),
        ] {
            store
                .advance_generation(member.world_id, advance, 0)
                .await
                .unwrap();
        }
        (db, store, member)
    }

    /// The Storyteller creating objects on the real model, nothing else: counts what
    /// its create and update calls did. Prints one METRIC line; a creation that names an ID of its own
    /// kind is the invented-ID failure.
    #[tokio::test]
    #[ignore = "needs a CUDA GPU; takes a few minutes"]
    async fn the_real_storyteller_creates_objects_without_naming_ids_of_its_own() {
        let model = DeviceModel::load().await;
        let (shared, name, sampling, limits) = model.parts();
        let observed = model.observed();
        let (db, store, member) = saltgate_ready_for_objects("real-objects").await;
        let backend = InferenceScheduler::new(shared, limits).unwrap();
        let worlds = World::load(&store).await.unwrap();
        let mut events = World::get(&worlds, member.world_id)
            .unwrap()
            .events
            .subscribe();
        let _game = Game::new(
            store.clone(),
            worlds,
            backend.foreground(),
            limits,
            name,
            sampling,
        );
        tokio::time::timeout(std::time::Duration::from_secs(900), async {
            loop {
                let world = store.world(member.world_id).await.unwrap().unwrap();
                let creating = store
                    .next_generation(member.world_id)
                    .await
                    .unwrap()
                    .is_some_and(|generation| {
                        generation.stage == crate::store::GenerationStage::CreateObjects
                    });
                if !creating || world.failure.is_some() {
                    break;
                }
                let _ = events.recv().await;
            }
        })
        .await
        .expect("creating the objects neither finished nor failed in time");

        if let Some(first) = observed.lock().unwrap().first() {
            println!(
                "FIRST OBJECT PROMPT ({} tokens)
{}",
                first.tokens.len(),
                first.prompt
            );
        }
        let pool = sqlx::SqlitePool::connect(&format!("sqlite://{}", db.path().display()))
            .await
            .unwrap();
        let bodies: Vec<String> = sqlx::query_scalar("SELECT body FROM recorded_content")
            .fetch_all(&pool)
            .await
            .unwrap();
        let (mut creates, mut changes, mut invented) = (0, 0, 0);
        for body in bodies {
            let Ok(body) = serde_json::from_str::<serde_json::Value>(&body) else {
                continue;
            };
            let Some(calls) = body["content"]["value"].as_array() else {
                continue;
            };
            for call in calls.iter() {
                let name = call["name"].as_str().unwrap_or_default();
                let Some(arguments) = call["arguments"]
                    .as_str()
                    .and_then(|text| serde_json::from_str::<serde_json::Value>(text).ok())
                else {
                    continue;
                };
                if name == "update_object" {
                    changes += 1;
                } else if name == "create_object" {
                    creates += 1;
                    // A creation that names an ID of its own: `id`, or its own kind's.
                    let kind = arguments["object"].as_str().unwrap_or_default();
                    if arguments.get("id").is_some()
                        || arguments.get(format!("{kind}_id")).is_some()
                    {
                        invented += 1;
                        println!("INVENTED {name} {arguments}");
                    }
                }
            }
        }
        let (replies, capped, output_tokens): (i64, i64, i64) = sqlx::query_as(
            "SELECT COUNT(*), COALESCE(SUM(output_tokens >= 1000), 0), COALESCE(SUM(output_tokens), 0)              FROM inference WHERE output_tokens IS NOT NULL",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        let rejected: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM tool_execution WHERE status = 'rejected'")
                .fetch_one(&pool)
                .await
                .unwrap();
        let reasons: Vec<(i64, String)> = sqlx::query_as(
            "SELECT tool_execution.inference_id, recorded_content.body FROM tool_execution              JOIN recorded_content ON recorded_content.id = tool_execution.result_content_id              WHERE tool_execution.status = 'rejected'",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        for (inference, reason) in reasons {
            println!("REJECTED in inference {inference}: {reason}");
        }
        let calls: Vec<(i64, i64, String)> = sqlx::query_as(
            "SELECT tool_execution.inference_id, tool_execution.ordinal, recorded_content.body FROM tool_execution              JOIN inference ON inference.id = tool_execution.inference_id              JOIN recorded_content ON recorded_content.id = inference.output              WHERE tool_execution.status = 'rejected'",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        for (inference, ordinal, body) in calls {
            let body: serde_json::Value = serde_json::from_str(&body).unwrap();
            println!(
                "REJECTED CALL {inference}/{ordinal}: {}",
                body["content"]["value"][usize::try_from(ordinal).unwrap()]["arguments"]
            );
        }
        let world = store.world(member.world_id).await.unwrap().unwrap();
        let objects = store.review_list(member.world_id).await.unwrap();
        let count = |kind: &str| objects.iter().filter(|entry| entry.kind == kind).count();
        println!(
            "METRIC replies={replies} capped={capped} output_tokens={output_tokens} creates={creates} changes={changes} invented_own_id={invented} rejected_calls={rejected} locations={} connections={} groups={} npcs={} item_types={} items={} failure={:?}",
            count("location"),
            count("connection"),
            count("group"),
            count("npc"),
            count("item_type"),
            count("item"),
            world.failure
        );
        assert!(world.failure.is_none(), "{:?}", world.failure);
        assert!(count("location") > 0 && count("group") > 0 && count("npc") > 0);
    }

    /// The object phases on the real model: from a finished summary the
    /// Storyteller creates the objects with its tools and the Questioner
    /// reviews them. Prints what was made and both chats. Not part of the
    /// device suite: a run takes several minutes.
    #[tokio::test]
    #[ignore = "needs a CUDA GPU; takes several minutes"]
    async fn the_real_model_creates_and_reviews_core_objects_from_a_summary() {
        real_core_object_phases(false).await;
    }

    /// The Questioner's review on the real model: part of the world is already
    /// built and the rest is missing, so the review has to notice that and ask
    /// the Storyteller to add it before it can complete the phase.
    #[tokio::test]
    #[ignore = "needs a CUDA GPU; takes several minutes"]
    async fn the_real_model_questioner_catches_what_the_storyteller_left_out() {
        real_core_object_phases(true).await;
    }

    /// With `leave_gaps` the Storyteller's creating of objects is replaced by a partial
    /// world, so the first thing that happens is the Questioner's review.
    async fn real_core_object_phases(leave_gaps: bool) {
        use crate::store::{Advance, Fields, ObjectKind};
        let model = DeviceModel::load().await;
        let (shared, name, sampling, limits) = model.parts();
        let observed = model.observed();
        let (_db, store, member) = saltgate_ready_for_objects("real-generation").await;
        if leave_gaps {
            let quay = store
                .create_object(
                    member.world_id,
                    ObjectKind::Location,
                    &Fields {
                        name: Some("Saltgate Quay".into()),
                        location_kind: Some("village".into()),
                        description: Some("A smuggler town on a still estuary.".into()),
                        ..Fields::default()
                    },
                )
                .await
                .unwrap()
                .id;
            let guild = store
                .create_object(
                    member.world_id,
                    ObjectKind::Group,
                    &Fields {
                        name: Some("The Harbor Guild".into()),
                        location_id: Some(quay),
                        description: Some("Officials who want the channel dredged.".into()),
                        ..Fields::default()
                    },
                )
                .await
                .unwrap()
                .id;
            store
                .create_object(
                    member.world_id,
                    ObjectKind::Npc,
                    &Fields {
                        name: Some("Ysolde Varn".into()),
                        group_id: Some(guild),
                        description: Some("A stern Guild warden.".into()),
                        background: Some("Her brother drowned near the bell.".into()),
                        motive: Some("Dredge the channel.".into()),
                        ..Fields::default()
                    },
                )
                .await
                .unwrap();
            store
                .advance_generation(member.world_id, Advance::ObjectsCreated, 0)
                .await
                .unwrap();
        }
        let backend = InferenceScheduler::new(shared, limits).unwrap();
        let worlds = World::load(&store).await.unwrap();
        let mut events = World::get(&worlds, member.world_id)
            .unwrap()
            .events
            .subscribe();
        let _game = Game::new(
            store.clone(),
            worlds,
            backend.foreground(),
            limits,
            name,
            sampling,
        );
        tokio::time::timeout(std::time::Duration::from_secs(1500), async {
            loop {
                let world = store.world(member.world_id).await.unwrap().unwrap();
                if world.ready || world.failure.is_some() {
                    break;
                }
                let _ = events.recv().await;
            }
        })
        .await
        .expect("the world was neither ready nor failed in time");

        let world = store.world(member.world_id).await.unwrap().unwrap();
        println!(
            "WORLD {} ready={} failure={:?}",
            world.name, world.ready, world.failure
        );
        for entry in store.review_list(member.world_id).await.unwrap() {
            println!(
                "OBJECT [{}] {} {}",
                if entry.reviewed { "x" } else { " " },
                entry.kind,
                entry.name
            );
        }
        for role in ["questioner", "storyteller"] {
            let agent = store.role_agent(member.world_id, role).await.unwrap();
            let branch = store.agent_branch(agent).await.unwrap();
            let segments = store.history_live_segments(branch).await.unwrap();
            let request = store
                .request_for_segments(agent, &segments, Sampling::default())
                .await
                .unwrap();
            println!(
                "CHAT {role}
{}",
                shown(&request)
            );
        }
        // What the model was actually sent in the review: the first and last
        // prompts that carry the Questioner's review tools.
        {
            let prompts = observed.lock().unwrap();
            let review: Vec<_> = prompts
                .iter()
                .filter(|seen| seen.prompt.contains("core_objects_created"))
                .collect();
            println!("REVIEW PROMPTS {}", review.len());
            for (label, seen) in [("FIRST", review.first()), ("LAST", review.last())] {
                if let Some(seen) = seen {
                    println!(
                        "{label} REVIEW PROMPT ({} tokens)
{}",
                        seen.tokens.len(),
                        seen.prompt
                    );
                }
            }
            if let Some(seen) = prompts
                .iter()
                .rev()
                .find(|seen| seen.prompt.contains("\"name\": \"create_object\""))
            {
                println!(
                    "LAST STORYTELLER PROMPT ({} tokens)
{}",
                    seen.tokens.len(),
                    seen.prompt
                );
            }
        }
        assert!(world.failure.is_none(), "{:?}", world.failure);
        assert!(world.ready);
        if leave_gaps {
            let agent = store
                .role_agent(member.world_id, "storyteller")
                .await
                .unwrap();
            let branch = store.agent_branch(agent).await.unwrap();
            let segments = store.history_live_segments(branch).await.unwrap();
            let chat = shown(
                &store
                    .request_for_segments(agent, &segments, Sampling::default())
                    .await
                    .unwrap(),
            );
            assert!(
                chat.contains("The Questioner reviewed the world and asks"),
                "the review should have sent the Storyteller back to add what was missing"
            );
        }
    }

    /// A world generated from a seed by the real model, start to finish. Stops
    /// early if the Questioner keeps asking through 24 requests, and prints every
    /// request the Questioner received so a stuck loop can be read as the model
    /// saw it. Not part of the device suite: a run takes many minutes.
    #[tokio::test]
    #[ignore = "needs a CUDA GPU; takes many minutes"]
    async fn a_world_is_generated_from_a_seed_by_the_real_model() {
        let model = DeviceModel::load().await;
        let (shared, name, sampling, limits) = model.parts();
        let db = crate::store::TestDatabase::new("real-generation-seed");
        let store = Store::open(db.path()).await.unwrap();
        let owner = store
            .find_or_create_user("maker@example.test", "Maker")
            .await
            .unwrap();
        let member = store
            .create_world_to_generate(
                &owner,
                "A smuggler's port town where something is wrong with the tide.",
            )
            .await
            .unwrap();
        let requests = Arc::new(std::sync::Mutex::new(Vec::new()));
        let recording = Recording {
            model: shared,
            requests: Arc::clone(&requests),
        };
        let backend = InferenceScheduler::new(recording, limits).unwrap();
        let worlds = World::load(&store).await.unwrap();
        let mut events = World::get(&worlds, member.world_id)
            .unwrap()
            .events
            .subscribe();
        let _game = Game::new(
            store.clone(),
            worlds,
            backend.foreground(),
            limits,
            name,
            sampling,
        );
        let asks = |requests: &[crate::llm::Request]| {
            requests
                .iter()
                .filter(|request| {
                    request
                        .tools
                        .iter()
                        .any(|tool| tool.name == "world_summary_complete")
                })
                .count()
        };
        tokio::time::timeout(std::time::Duration::from_secs(1800), async {
            loop {
                let world = store.world(member.world_id).await.unwrap().unwrap();
                if world.ready || world.failure.is_some() || asks(&requests.lock().unwrap()) >= 24 {
                    break;
                }
                let _ = events.recv().await;
            }
        })
        .await
        .expect("nothing finished in time");

        let world = store.world(member.world_id).await.unwrap().unwrap();
        println!(
            "WORLD {} ready={} failure={:?}",
            world.name, world.ready, world.failure
        );
        for entry in store.review_list(member.world_id).await.unwrap() {
            println!(
                "OBJECT [{}] {} {}",
                if entry.reviewed { "x" } else { " " },
                entry.kind,
                entry.name
            );
        }
        for role in ["questioner", "storyteller"] {
            let agent = store.role_agent(member.world_id, role).await.unwrap();
            let branch = store.agent_branch(agent).await.unwrap();
            let segments = store.history_live_segments(branch).await.unwrap();
            let request = store
                .request_for_segments(agent, &segments, Sampling::default())
                .await
                .unwrap();
            println!("CHAT {role}\n{}", shown(&request));
        }
        assert!(world.failure.is_none(), "{:?}", world.failure);
        assert!(
            world.ready,
            "the Questioner never finished the summary phase"
        );
    }

    /// Real-model check of what a summary keeps. The history is the Questioner's
    /// stuck turn from a recorded world generation: its instruction, then
    /// repeated `ask_questions` calls the game rejected. The summary replaces
    /// the older part of the turn, so it has to carry the task in progress, not
    /// treat the rejected drafts as questions already asked.
    #[tokio::test]
    #[ignore = "needs a CUDA GPU"]
    async fn a_summary_of_a_stuck_tool_loop_keeps_the_task_in_progress() {
        let model = DeviceModel::load().await;
        let summary = summarize_a_stuck_tool_loop(&model).await;
        assert!(
            summary.to_lowercase().contains("question"),
            "the summary must carry the questions the agent still has to ask"
        );
    }

    /// The Questioner's role and tools lead its normal requests. The summary
    /// request is built from them, and has to come back as text: given the
    /// tools, the model calls one instead of summarizing.
    async fn summarize_a_stuck_tool_loop(model: &DeviceModel) -> String {
        use crate::llm::{MessageContent, ToolCall};
        use crate::store::MessageOrigin;
        let (shared, _, sampling, limits) = model.parts();
        let db = crate::store::TestDatabase::new("compaction-stuck-loop");
        let store = Store::open(db.path()).await.unwrap();
        let world = store.create_world("test").await.unwrap();
        let agent = store.create_agent(world).await.unwrap();
        let ask = crate::prompts::current()
            .unwrap()
            .questioner
            .ask_request
            .clone();
        let say = |role, text: &str| Message::text(role, text);
        let mut history = vec![
            say(
                Role::User,
                "Seed prompt for the world: A smuggler's port town where something is wrong with \
                 the tide.\n\nBefore you ask anything, answer each of these briefly:\n- What does \
                 each involved party want, fear, and refuse to sacrifice?\n- What makes the \
                 central conflict arise from incompatible goals?",
            ),
            say(
                Role::Assistant,
                "Each party wants the tide back to normal for a different reason. The smugglers \
                 fear losing their hidden channels, the harbor guild fears losing its monopoly, \
                 and the cult wants the tide to stay wrong. The conflict is real because no two \
                 of them can be satisfied at once.",
            ),
            say(Role::User, &ask),
        ];
        let questions = r#"{"questions":"[\"What is the specific nature of the 'wrong' tide?\"]\n[\"Who stands to gain or lose the most if the tide returns to normal?\"]"}"#;
        for attempt in 0..6 {
            let id = format!("attempt-{attempt}");
            history.push(Message {
                role: Role::Assistant,
                origin: MessageOrigin::Conversation,
                content: MessageContent::ToolCalls(vec![ToolCall {
                    id: id.clone(),
                    name: "ask_questions".into(),
                    arguments: questions.into(),
                }]),
                reasoning: String::new(),
            });
            history.push(Message {
                role: Role::Tool,
                origin: MessageOrigin::Conversation,
                content: MessageContent::ToolResult {
                    tool_call_id: id,
                    content: "Tool arguments were rejected: parsing ask_questions arguments: \
                              invalid type: string \"[\\\"What is the specific nature of the \
                              'wrong' tide?\\\"]\", expected a sequence at line 1 column 96"
                        .into(),
                },
                reasoning: String::new(),
            });
        }
        for message in &history {
            store.append_message(agent, message).await.unwrap();
        }
        let branch = store.agent_branch(agent).await.unwrap();
        store
            .enqueue_capacity_compaction(
                branch,
                4255,
                None,
                crate::store::CompactionPolicy {
                    sequence_id: None,
                    compact_before_next_input_tokens: limits.compact_before_next_input_tokens,
                    sampling: &sampling,
                    model: "dev-qwen35",
                    ending_tools: &[],
                },
            )
            .await
            .unwrap();
        let job = store.pending_compaction(agent).await.unwrap().unwrap();
        crate::compaction::run(
            &store,
            &shared,
            &job,
            Limits {
                keep_raw_messages: 8,
                ..limits
            },
            tokio::sync::broadcast::channel(256).0,
        )
        .await
        .unwrap();
        let summary = store.latest_summary(agent).await.unwrap().unwrap();
        println!("===== SUMMARY\n{}", summary.content);
        summary.content
    }

    #[tokio::test]
    async fn the_storyteller_never_sees_a_replaced_summary_and_developer_mode_labels_the_side_inference()
     {
        let db = crate::store::TestDatabase::new("generation-aside-test");
        let store = Store::open(db.path()).await.unwrap();
        let owner = store
            .find_or_create_user("maker@example.test", "Maker")
            .await
            .unwrap();
        let member = store
            .create_world_to_generate(&owner, "A drowned city")
            .await
            .unwrap();
        store
            .set_developer_mode(owner.id, member.world_id, true)
            .await
            .unwrap();
        let call = |name: &str, arguments: &str| {
            response(Content::ToolCalls(vec![ToolCall {
                id: name.into(),
                name: name.into(),
                arguments: arguments.into(),
            }]))
        };
        let text = |text: &str| response(Content::Text(text.into()));
        let (scripted, requests) = ScriptedBackend::recording([
            text("Meta answers."),
            call("ask_questions", r#"{"questions":["Q one?"]}"#),
            text("Answer one."),
            text("SUMMARY-ONE"),
            call("ask_questions", r#"{"questions":["Q two?"]}"#),
            text("Answer two."),
            text("SUMMARY-TWO"),
            call("world_summary_complete", "{}"),
            text("The Drowned Bell"),
        ]);
        let backend = InferenceScheduler::new(scripted, Limits::default()).unwrap();
        let worlds = World::load(&store).await.unwrap();
        let mut events = World::get(&worlds, member.world_id)
            .unwrap()
            .events
            .subscribe();
        let _game = Game::new(
            store.clone(),
            worlds,
            backend.foreground(),
            Limits::default(),
            "scripted".into(),
            Sampling {
                temperature: 0.0,
                ..Default::default()
            },
        );
        // The scripted responses end with the name; the world fails after that.
        while store.world(member.world_id).await.unwrap().unwrap().name == "Unnamed world" {
            tokio::time::timeout(std::time::Duration::from_secs(10), events.recv())
                .await
                .expect("a generation step did not finish")
                .unwrap();
        }

        let summary_request = crate::prompts::current()
            .unwrap()
            .storyteller
            .summary_request
            .clone();
        let shown: Vec<String> = requests.lock().unwrap().iter().map(shown).collect();
        assert_eq!(shown.len(), 9);
        // Request 3 is the first summary, asked beside the chat; request 5 is
        // the second round's answer and sees only the first summary, once.
        assert!(shown[3].contains(&summary_request));
        assert!(shown[5].contains("Q two?") && shown[5].contains("Answer one."));
        assert_eq!(shown[5].matches("SUMMARY-ONE").count(), 1);
        assert!(!shown[5].contains(&summary_request));
        // The second summary sees the first, once, and none of the second.
        assert_eq!(shown[6].matches("SUMMARY-ONE").count(), 1);
        assert!(!shown[6].contains("SUMMARY-TWO"));
        // The name sees only the latest.
        assert_eq!(shown[8].matches("SUMMARY-TWO").count(), 1);
        assert!(!shown[8].contains("SUMMARY-ONE"));

        // Developer Mode lists each side inference in the Storyteller's history.
        let storyteller = store
            .world_agents(owner.id, member.world_id)
            .await
            .unwrap()
            .into_iter()
            .find(|agent| agent.role == "storyteller")
            .unwrap();
        let cursor = store
            .first_raw_history_cursor(owner.id, member.world_id, storyteller.id, None)
            .await
            .unwrap();
        let (entries, _) = store
            .agent_raw_history(owner.id, cursor, 100)
            .await
            .unwrap();
        let asides: Vec<String> = entries
            .iter()
            .filter_map(|entry| match entry {
                crate::inspection::RawHistoryEntry::Inference {
                    aside: Some(MessageContent::Text(text)),
                    ..
                } => Some(text.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(asides, ["SUMMARY-ONE", "SUMMARY-TWO"]);
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
            // The scene goes to the NPCs; the entrance gives the character the
            // scene and everyone else their arrival.
            narrate("The hut stands silent."),
            tool_call(
                "narrate_actor_and_others",
                serde_json::json!({
                    "actor_text": "The charcoal hut is quiet.",
                    "others_text": "The Adventurer ducks inside.",
                }),
            ),
            response(Content::Text(String::new())),
            // Mara and Toma hear both and have nothing to add.
            do_nothing(),
            do_nothing(),
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
        wait_for_npc_work_of(&game, &store, &member).await;

        let chat = store.player_chat(&member).await.unwrap();
        let narrations: Vec<&str> = chat
            .iter()
            .filter(|entry| entry.role == Role::Narration)
            .map(|entry| entry.text.as_str())
            .collect();
        assert_eq!(
            narrations,
            ["The charcoal hut is quiet."],
            "the entering character gets the scene, not the NPCs' copy or the arrival"
        );
        for npc in ["Mara", "Toma"] {
            assert_eq!(
                narrated_to_npc(&store, npc).await,
                ["The hut stands silent.", "The Adventurer ducks inside."],
                "{npc} was told the scene, then the arrival"
            );
        }
        let group = store.character_group_id(member.character_id).await.unwrap();
        assert!(store.group_scene_narrated(group).await.unwrap());
        assert!(
            !chat
                .iter()
                .any(|entry| entry.role == Role::Assistant && entry.text.is_empty()),
            "the silent completion must not become a blank player-chat entry"
        );
        {
            let requests = requests.lock().unwrap();
            // The player's call, two GM turns, then the player settling.
            let final_request = &requests[3];
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

    /// The GM is not told about the setting and its tools work as described;
    /// delivery shares the account among the player characters.
    #[tokio::test]
    async fn sharing_gives_every_player_the_detailed_account_and_leaves_private_narration_alone() {
        for (shares, second_hears) in [
            (true, "From the rock you see three guards."),
            (false, "The adventurer climbs the rock."),
        ] {
            let scene = speech_scene_with_rules(
                Limits::default(),
                GameSettings {
                    characters_share_narration: shares,
                },
            )
            .await;
            look_and_narrate(
                &scene,
                tool_call(
                    "narrate_actor_and_others",
                    serde_json::json!({
                        "actor_text": "From the rock you see three guards.",
                        "others_text": "The adventurer climbs the rock.",
                    }),
                ),
                quiet(2),
            )
            .await;
            assert_eq!(
                narrations(&scene.store, &scene.first).await,
                ["From the rock you see three guards."]
            );
            assert_eq!(
                narrations(&scene.store, &scene.second).await,
                [second_hears],
                "sharing {shares}"
            );
            // The NPCs are not players: they keep the briefer account.
            for npc in ["Mara", "Toma"] {
                assert_eq!(
                    narrated_to_npc(&scene.store, npc).await,
                    ["The adventurer climbs the rock."]
                );
            }
            let request = scene
                .requests
                .lock()
                .unwrap()
                .iter()
                .find(|request| {
                    request
                        .tools
                        .iter()
                        .any(|tool| tool.name == "approve_action")
                })
                .map(|request| serde_json::to_string(&request.messages).unwrap())
                .unwrap();
            assert!(
                !request.contains("same detailed narration") && !request.contains("more briefly"),
                "the GM is not told about the setting: {request}"
            );
        }

        // Narration addressed to chosen characters stays with them.
        let scene = speech_scene().await;
        look_and_narrate(
            &scene,
            tool_call(
                "narrate_characters",
                serde_json::json!({
                    "character_ids": [scene.first.character_id],
                    "text": "Only you notice the draught.",
                }),
            ),
            quiet(0),
        )
        .await;
        assert_eq!(
            narrations(&scene.store, &scene.first).await,
            ["Only you notice the draught."]
        );
        assert!(narrations(&scene.store, &scene.second).await.is_empty());
        assert!(
            crate::settings::Settings::load()
                .unwrap()
                .game
                .characters_share_narration,
            "characters share the narration unless configured otherwise"
        );
    }

    #[tokio::test]
    async fn a_character_entering_a_group_whose_scene_is_known_is_only_introduced() {
        let (backend, requests) = ScriptedBackend::recording([
            response(Content::ToolCalls(vec![ToolCall {
                id: "ready-1".into(),
                name: "ready_to_begin".into(),
                arguments: "{}".into(),
            }])),
            tool_call(
                "narrate_actor_and_others",
                serde_json::json!({
                    "actor_text": "The charcoal hut is quiet.",
                    "others_text": "The Adventurer ducks inside.",
                }),
            ),
            response(Content::Text(String::new())),
            do_nothing(),
            do_nothing(),
        ]);
        let (game, store, _db, member) = blank_opening_game(backend).await;
        sqlx::query("UPDATE \"group\" SET scene_narrated = 1")
            .execute(&store.pool())
            .await
            .unwrap();
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
        wait_for_npc_work_of(&game, &store, &member).await;

        // The player's call, one GM turn, the player settling, two NPC decisions.
        assert_eq!(requests.lock().unwrap().len(), 5);
        assert_eq!(
            narrated_to_npc(&store, "Mara").await,
            ["The Adventurer ducks inside."]
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
            request: crate::llm::Request,
            _on_token: impl FnMut(&str) + Send,
        ) -> Result<Response> {
            self.requests.fetch_add(1, Ordering::SeqCst);
            self.started.notify_one();
            self.release.notified().await;
            // The GM must end its turn with a narrate tool; others reply in text.
            if request
                .tools
                .iter()
                .any(|tool| tool.name == "narrate_group")
            {
                return Ok(narrate("Welcome."));
            }
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

    /// A rejected tool call or refused plain-text reply is the model producing
    /// invalid output: an error to investigate, never a path the real-model
    /// scenarios may rely on.
    async fn assert_no_rejected_output(store: &Store) {
        let rejected: Vec<(i64, Option<String>)> = sqlx::query_as(
            "SELECT inference_id, COALESCE(error, recorded_content.body) FROM tool_execution              LEFT JOIN recorded_content ON recorded_content.id = result_content_id              WHERE status = 'rejected'              UNION SELECT source_inference_id, 'refused plain-text reply' FROM message              WHERE reply_rejected = 1 ORDER BY 1",
        )
        .fetch_all(&store.pool())
        .await
        .unwrap();
        assert!(
            rejected.is_empty(),
            "the model produced invalid output (inference, error): {rejected:?}; open them in Developer Mode"
        );
    }

    /// The device-test model, loaded once and shared by every scenario in
    /// `device_suite` so the whole suite pays the load a single time.
    pub(crate) struct DeviceModel {
        pub(crate) backend: Arc<MistralRsBackend>,
        path: String,
        sampling: Sampling,
        limits: Limits,
    }

    /// Lets each scenario's `Game` own a backend while the model stays shared.
    pub(crate) struct SharedModel(Arc<MistralRsBackend>);

    impl Backend for SharedModel {
        fn complete(
            &self,
            request: crate::llm::Request,
            on_token: impl FnMut(&str) + Send,
        ) -> impl Future<Output = anyhow::Result<crate::llm::Response>> + Send {
            self.0.complete(request, on_token)
        }

        fn complete_with_recording(
            &self,
            request: crate::llm::Request,
            on_token: impl FnMut(&str) + Send,
            store: &Store,
            inference_id: i64,
        ) -> impl Future<Output = anyhow::Result<crate::llm::Response>> + Send {
            self.0
                .complete_with_recording(request, on_token, store, inference_id)
        }
    }

    /// Passes requests to the model and keeps a copy of each, so a run can be
    /// read afterwards exactly as the model received it.
    pub(crate) struct Recording {
        model: SharedModel,
        pub(crate) requests: Arc<std::sync::Mutex<Vec<crate::llm::Request>>>,
    }

    impl Backend for Recording {
        fn complete(
            &self,
            request: crate::llm::Request,
            on_token: impl FnMut(&str) + Send,
        ) -> impl Future<Output = anyhow::Result<crate::llm::Response>> + Send {
            self.requests.lock().unwrap().push(request.clone());
            self.model.complete(request, on_token)
        }

        fn complete_with_recording(
            &self,
            request: crate::llm::Request,
            on_token: impl FnMut(&str) + Send,
            store: &Store,
            inference_id: i64,
        ) -> impl Future<Output = anyhow::Result<crate::llm::Response>> + Send {
            self.requests.lock().unwrap().push(request.clone());
            self.model
                .complete_with_recording(request, on_token, store, inference_id)
        }
    }

    impl DeviceModel {
        pub(crate) async fn load() -> Self {
            Self::load_with(
                Settings::for_device_test("dev-qwen35").expect("default.toml should load"),
            )
            .await
        }

        async fn load_with(settings: Settings) -> Self {
            let _ = tracing_subscriber::fmt()
                .with_env_filter("cairnworld=info")
                .with_test_writer()
                .try_init();
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
            Self {
                backend: Arc::new(backend),
                sampling: settings.sampling(&model),
                path: model.path,
                limits: settings.limits,
            }
        }

        fn parts(&self) -> (SharedModel, String, Sampling, Limits) {
            (
                SharedModel(Arc::clone(&self.backend)),
                self.path.clone(),
                self.sampling.clone(),
                self.limits,
            )
        }

        /// Start capturing the prompts the model is given from now on.
        fn observed(&self) -> Arc<std::sync::Mutex<Vec<crate::mistralrs_backend::ObservedPrompt>>> {
            self.backend.observe_prompts()
        }
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
            "/testdata/hut_scenario.json"
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
            "/testdata/hut_scenario.json"
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
            narrate("You lift the flour sack while Toma watches."),
            response(Content::Text(String::new())),
            // Mara and Toma each decide about what they were told.
            do_nothing(),
            do_nothing(),
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
        // The player's turn follows creation; the NPCs' decisions come later.
        assert_eq!(
            chat[narration].sequence_id,
            Some(creation.id + 1),
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
                gm_packet.starts_with(&crate::prompts::current().unwrap().gm.role),
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
                    content: MessageContent::ToolCalls(calls),
                    ..
                } if calls.iter().any(|call| call.name == "narrate_group"
                    && call.arguments.contains("You lift the flour sack while Toma watches.")))),
            "Developer Mode must show the GM's narrate call after its action event"
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
            "/testdata/hut_scenario.json"
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
            narrate("You lift the flour sack while Toma watches."),
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
            "/testdata/hut_scenario.json"
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
            "partitions": [
                {"character_ids": [installed.member.character_id, mara_id], "name": "with Mara"},
                {"character_ids": [toma_id], "name": "Toma alone"},
            ],
        })
        .to_string();
        let (scripted_backend, recorded_requests) = ScriptedBackend::recording([
            response(Content::ToolCalls(vec![ToolCall {
                id: "look-1".into(),
                name: "attempt".into(),
                arguments: r#"{"kind":"look","description":"the room"}"#.into(),
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
            narrate("Toma watches as you step outside with Mara."),
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
            "/testdata/hut_scenario.json"
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
            "partitions": [
                {"character_ids": [installed.member.character_id, mara_id], "name": "with Mara"},
                {"character_ids": [toma_id], "name": "Toma alone"},
            ],
        })
        .to_string();
        let (split_backend, _split_requests) = ScriptedBackend::recording([
            response(Content::ToolCalls(vec![ToolCall {
                id: "look-1".into(),
                name: "attempt".into(),
                arguments: r#"{"kind":"look","description":"the room"}"#.into(),
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
            narrate("You step outside with Mara."),
            response(Content::Text("What do you do next?".into())),
            // Mara, now with the Adventurer, hears the narration.
            do_nothing(),
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
        wait_for_npc_work_of(&split_game, &store, &installed.member).await;

        let adventurer_group = store
            .character_group_id(installed.member.character_id)
            .await
            .unwrap();
        let toma_group = store.character_group_id(toma_id).await.unwrap();
        assert_ne!(
            adventurer_group, toma_group,
            "the split must have taken effect"
        );
        // What happened apart: each side's GM history gains a fact only it
        // knows, and an unrelated world's GM history gains another.
        let apart = store
            .begin_sequence(installed.world_id, "events while apart")
            .await
            .unwrap();
        for (group, event) in [
            (
                adventurer_group,
                "Outside the hut Mara whispers that the password is heron.",
            ),
            (
                toma_group,
                "Inside the hut Toma mutters that he hid the lantern.",
            ),
        ] {
            store
                .append_instruction(
                    store.group_branch(group).await.unwrap(),
                    &Message::text(Role::User, event),
                    apart.id,
                )
                .await
                .unwrap();
        }
        let stranger = store
            .find_or_create_user("stranger@example.test", "Stranger")
            .await
            .unwrap();
        let other_world = store.install_scenario(&stranger, &scenario).await.unwrap();
        let other_sequence = store
            .begin_sequence(other_world.world_id, "another world")
            .await
            .unwrap();
        store
            .append_instruction(
                store
                    .group_branch(
                        store
                            .character_group_id(other_world.member.character_id)
                            .await
                            .unwrap(),
                    )
                    .await
                    .unwrap(),
                &Message::text(Role::User, "The other world's GM only knows the cellar."),
                other_sequence.id,
            )
            .await
            .unwrap();

        let merge_args = serde_json::json!({
            "group_ids": [adventurer_group, toma_group],
            "name": "reunited",
        })
        .to_string();
        let (merge_backend, merge_requests) = ScriptedBackend::recording([
            response(Content::ToolCalls(vec![ToolCall {
                id: "look-2".into(),
                name: "attempt".into(),
                arguments: r#"{"kind":"look","description":"Toma"}"#.into(),
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
            narrate("You and Mara rejoin Toma."),
            response(Content::Text("What do you do next?".into())),
            // Mara and Toma hear the narration.
            do_nothing(),
            do_nothing(),
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
        wait_for_npc_work_of(&merge_game, &store, &installed.member).await;

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
            // The merge takes effect after this turn's calls, so this request
            // still sees only the side the acting player was on.
            let says = |needle: &str| {
                request.messages.iter().any(|message| {
                    matches!(&message.content, MessageContent::Text(text) if text.contains(needle))
                })
            };
            assert!(
                says("the password is heron"),
                "the side with Mara is missing"
            );
            assert!(
                !says("he hid the lantern"),
                "the other side was visible before the merge"
            );

            assert!(
                matches!(request.messages.last().map(|message| &message.content),
                    Some(MessageContent::Text(text)) if text.contains("takes action 2")),
                "the newest GM message must be the second action event"
            );
            assert!(
                matches!(request.messages.first().map(|message| &message.content),
                    Some(MessageContent::Text(text)) if text.starts_with(&crate::prompts::current().unwrap().gm.role)),
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

    /// The GM's next turn after a merge sees what each side did apart and
    /// nothing from another world.
    #[tokio::test]
    async fn the_merged_gm_history_holds_both_sides_and_nothing_from_another_world() {
        let (store, _db, member, ..) = split_then_merge_groups().await;
        let sequence = store
            .begin_sequence(member.world_id, "merged GM context test")
            .await
            .unwrap();
        let arguments = serde_json::json!({"description": "the room"}).to_string();
        let requesting = crate::store::record_tool_call_for_test(
            &store,
            store.agent_branch(member.agent_id).await.unwrap(),
            Some(sequence.id),
            "look-9",
            "attempt",
            &arguments,
        )
        .await;
        let pending = store
            .create_pending_action(
                &store.player_actor(&member).await.unwrap(),
                sequence.id,
                requesting,
                "attempt",
                &arguments,
            )
            .await
            .unwrap();
        let limits = Limits::default();
        let (backend, requests) = ScriptedBackend::recording([
            tool_call(
                "approve_action",
                serde_json::json!({"action_id": pending.id}),
            ),
            narrate("The room is quiet."),
        ]);
        let game = Game::new(
            store.clone(),
            World::load(&store).await.unwrap(),
            InferenceScheduler::new(backend, limits)
                .unwrap()
                .foreground(),
            limits,
            "scripted".into(),
            Sampling {
                temperature: 0.0,
                ..Default::default()
            },
        );
        game.arbitrate(
            pending,
            Budget::new(limits),
            CallContext::root(Some(sequence.id)),
            None,
        )
        .await
        .unwrap();

        let requests = requests.lock().unwrap();
        let says = |needle: &str| {
            requests[0].messages.iter().any(|message| {
                matches!(&message.content, MessageContent::Text(text) if text.contains(needle))
            })
        };
        assert!(
            says("the password is heron"),
            "the side with Mara is missing"
        );
        assert!(says("he hid the lantern"), "the side with Toma is missing");
        assert!(
            !says("only knows the cellar"),
            "another world's history leaked in"
        );
    }

    /// After a split and merge the GM holds both sides' histories, but a character
    /// may only learn what they perceived: the player (outside, with Mara) heard the
    /// password and Toma (inside) heard nothing of it, and Toma muttered something
    /// the player never heard. Each asks the real GM a question on the merged
    /// branch DAG; the answers must contain what the asker perceived (so a GM that
    /// refuses everything fails) and nothing of what they did not.
    async fn real_model_gm_does_not_leak_one_side_to_the_other_after_a_merge(model: &DeviceModel) {
        // (asker, question, must mention, must not mention)
        let cases = [
            (
                "player",
                "What did Mara whisper outside?",
                Some("heron"),
                None,
            ),
            (
                "player",
                "What did Toma say inside the hut?",
                None,
                Some("lantern"),
            ),
            ("toma", "What did Mara say outside?", None, Some("heron")),
            ("toma", "What did I mutter inside?", Some("lantern"), None),
        ];
        let mut wrong = Vec::new();
        for (asker, question, must, must_not) in cases {
            let (store, _db, member, ..) = split_then_merge_groups().await;
            let toma_id: i64 = sqlx::query_scalar("SELECT id FROM character WHERE name = 'Toma'")
                .fetch_one(&store.pool())
                .await
                .unwrap();
            let sequence = store
                .begin_sequence(member.world_id, "real GM privacy test")
                .await
                .unwrap();
            let arguments = serde_json::json!({"kind": "ask", "description": question}).to_string();
            let pending = if asker == "player" {
                let requesting = crate::store::record_tool_call_for_test(
                    &store,
                    store.agent_branch(member.agent_id).await.unwrap(),
                    Some(sequence.id),
                    "ask-1",
                    "attempt",
                    &arguments,
                )
                .await;
                store
                    .create_pending_action(
                        &store.player_actor(&member).await.unwrap(),
                        sequence.id,
                        requesting,
                        "attempt",
                        &arguments,
                    )
                    .await
                    .unwrap()
            } else {
                let requesting = crate::store::record_tool_call_for_test(
                    &store,
                    store.npc_branch(toma_id).await.unwrap(),
                    Some(sequence.id),
                    "ask-1",
                    "attempt",
                    &arguments,
                )
                .await;
                store
                    .create_reaction_action(
                        member.world_id,
                        toma_id,
                        sequence.id,
                        requesting,
                        "attempt",
                        &arguments,
                    )
                    .await
                    .unwrap()
            };
            let (real_backend, model_path, sampling, limits) = model.parts();
            let real_game = Game::new(
                store.clone(),
                World::load(&store).await.unwrap(),
                InferenceScheduler::new(real_backend, limits)
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
                    None,
                )
                .await
                .expect("a real GM turn must resolve the pending action on the merged branch DAG");
            let last_inference: i64 = sqlx::query_scalar(
                "SELECT id FROM inference WHERE sequence_id = ? AND agent_id = ? ORDER BY ordinal DESC LIMIT 1",
            )
            .bind(sequence.id)
            .bind(pending.gm_agent_id)
            .fetch_one(&store.pool())
            .await
            .unwrap();
            let recorded = store.reconstruct_inference(last_inference).await.unwrap();
            let crate::inspection::RecordedOutcome::Response(response) = recorded.outcome else {
                panic!("the GM's last inference did not complete");
            };
            let Content::ToolCalls(calls) = response.content else {
                panic!("the GM did not close with a narrate call: {response:?}");
            };
            // Whichever narrate tool the GM chose, all its text counts.
            let narration = calls
                .iter()
                .filter(|call| call.name.starts_with("narrate_"))
                .map(|call| call.arguments.to_lowercase())
                .collect::<Vec<_>>()
                .join(" ");
            if must.is_some_and(|word| !narration.contains(word))
                || must_not.is_some_and(|word| narration.contains(word))
            {
                wrong.push(format!("{asker} asked {question:?}: {narration}"));
            }
            assert_no_rejected_output(&store).await;
        }
        assert!(wrong.is_empty(), "the GM's answers were wrong: {wrong:#?}");
    }

    /// A known failure of the current GM, kept runnable on its own and out of
    /// `device_suite`: see plans/agent-tuning.md.
    #[tokio::test]
    #[ignore = "known failure: the GM leaks across a merge (plans/agent-tuning.md); needs a CUDA GPU"]
    async fn the_gm_keeps_each_side_private_after_a_merge() {
        let model = DeviceModel::load().await;
        real_model_gm_does_not_leak_one_side_to_the_other_after_a_merge(&model).await;
    }

    /// The intervention check on its own, so a change to the GM's wording can be
    /// repeated against it: the GM must decide every presented action before it
    /// narrates, and a failure here is the model skipping one.
    #[tokio::test]
    #[ignore = "needs a CUDA GPU"]
    async fn the_gm_decides_every_presented_action_before_narrating() {
        let model = DeviceModel::load().await;
        real_model_gm_asks_a_character_before_deciding_a_take(&model).await;
    }

    /// Every real-model test, run against one model load. The whole suite must
    /// stay inside the five-minute test budget, so each scenario has its own
    /// time limit and its elapsed time is printed.
    #[tokio::test]
    #[ignore = "device test: needs a CUDA GPU and the models/ GGUF from default.toml"]
    async fn device_suite() {
        use crate::mistralrs_backend::tests as backend_tests;

        async fn scenario(name: &str, seconds: u64, run: impl Future<Output = ()>) {
            let started = std::time::Instant::now();
            tokio::time::timeout(std::time::Duration::from_secs(seconds), run)
                .await
                .unwrap_or_else(|_| panic!("{name} did not finish within {seconds} s"));
            println!("SUITE {name}: {:.0} s", started.elapsed().as_secs_f32());
        }

        let suite_started = std::time::Instant::now();
        let model = DeviceModel::load().await;
        println!("SUITE load: {:.0} s", suite_started.elapsed().as_secs_f32());
        let backend = &*model.backend;
        let limits = model.limits;

        scenario(
            "prompt observation",
            60,
            backend_tests::generation_prompt_observation_matches_request_reconstruction(backend),
        )
        .await;
        scenario(
            "context capacity",
            60,
            backend_tests::configured_context_capacity_rejects_overflow_and_remains_usable(
                backend, limits,
            ),
        )
        .await;
        scenario(
            "streaming",
            60,
            backend_tests::streaming_completion_leaves_the_model_available_for_the_next_turn(
                backend,
            ),
        )
        .await;
        scenario(
            "template shape",
            60,
            backend_tests::opening_turn_with_tools_has_a_valid_chat_template_shape(backend),
        )
        .await;
        scenario(
            "zero-argument tool",
            60,
            backend_tests::a_zero_argument_tool_call_has_empty_arguments(backend),
        )
        .await;
        scenario(
            "prior tool call",
            60,
            backend_tests::a_prior_tool_call_message_renders_for_the_next_turn(backend),
        )
        .await;
        scenario(
            "opening",
            60,
            real_model_opening_reaches_a_stored_player_reply(&model),
        )
        .await;
        scenario(
            "creation",
            120,
            real_model_finishes_creation_and_the_gm_resolves_a_take(&model),
        )
        .await;
        scenario(
            "interrupted roll",
            120,
            real_model_chats_normally_after_an_interrupted_roll(&model),
        )
        .await;
        scenario(
            "npc answer",
            180,
            real_model_npc_answers_a_question_put_to_it(&model),
        )
        .await;
        scenario(
            "listener",
            120,
            real_model_listener_understands_what_another_player_said(&model),
        )
        .await;
        scenario(
            "compaction",
            120,
            real_model_compacts_a_long_chat_and_keeps_an_early_fact(&model),
        )
        .await;
        scenario(
            "gm run",
            120,
            real_model_gm_resolves_a_run_of_actions(&model),
        )
        .await;
        scenario(
            "intervention check",
            120,
            real_model_gm_asks_a_character_before_deciding_a_take(&model),
        )
        .await;
        scenario(
            "routing",
            120,
            real_model_player_routes_requests_to_the_right_action(&model),
        )
        .await;
        scenario(
            "committed take",
            120,
            real_model_player_marks_only_a_determined_take_committed(&model),
        )
        .await;
        println!(
            "SUITE total: {:.0} s",
            suite_started.elapsed().as_secs_f32()
        );
    }

    /// The answer of a GM that approves the action it was just presented and then
    /// narrates as a real one does (nothing for conveyed speech); `None` when the
    /// request is not the GM's. Only the GM is offered approve_action.
    fn mechanical_gm(request: &crate::llm::Request) -> Result<Option<Response>> {
        if !request
            .tools
            .iter()
            .any(|tool| tool.name == "approve_action")
        {
            return Ok(None);
        }
        Ok(Some(
            match &request
                .messages
                .last()
                .context("an empty GM request")?
                .content
            {
                MessageContent::Text(text) => {
                    let action_id: i64 = text
                        .split("takes action ")
                        .nth(1)
                        .and_then(|rest| rest.split(':').next())
                        .and_then(|id| id.trim().parse().ok())
                        .with_context(|| format!("no action id in the GM request: {text}"))?;
                    tool_call(
                        "approve_action",
                        serde_json::json!({"action_id": action_id}),
                    )
                }
                content
                    if format!("{content:?}").contains(&prompts::current()?.gm.speech_conveyed) =>
                {
                    tool_call("narrate_nothing", serde_json::json!({}))
                }
                _ => narrate("It is done."),
            },
        ))
    }

    /// The player agent and the GM are scripted; the NPCs (and anything else
    /// asked) are answered by the real model. The player's words enter the real
    /// pipeline through its real entry point and the GM approves each presented
    /// action and narrates it, so what is exercised is the NPCs' own decisions
    /// without paying for the GM's narration (the GM has its own real-model
    /// scenario).
    struct RealNpcsOnly {
        real: Arc<MistralRsBackend>,
        player: Mutex<VecDeque<Response>>,
    }

    impl Backend for RealNpcsOnly {
        async fn complete(
            &self,
            request: crate::llm::Request,
            on_token: impl FnMut(&str) + Send,
        ) -> Result<Response> {
            // Only the player agent is offered the party_presence tool.
            if request
                .tools
                .iter()
                .any(|tool| tool.name == "party_presence")
            {
                return self
                    .player
                    .lock()
                    .unwrap()
                    .pop_front()
                    .context("the scripted player agent ran out of responses");
            }
            if let Some(answer) = mechanical_gm(&request)? {
                return Ok(answer);
            }
            self.real.complete(request, on_token).await
        }
    }

    /// Only the player agent is scripted; the GM and the NPCs are the real model.
    struct RealGmAndNpcs {
        real: Arc<MistralRsBackend>,
        player: Mutex<VecDeque<Response>>,
    }

    impl Backend for RealGmAndNpcs {
        async fn complete(
            &self,
            request: crate::llm::Request,
            on_token: impl FnMut(&str) + Send,
        ) -> Result<Response> {
            // Only the player agent is offered the party_presence tool, and during
            // creation the roll tools.
            let player = request
                .tools
                .iter()
                .any(|tool| matches!(tool.name.as_str(), "party_presence" | "roll_hit_protection"));
            if player {
                return self
                    .player
                    .lock()
                    .unwrap()
                    .pop_front()
                    .context("the scripted player agent ran out of responses");
            }
            self.real.complete(request, on_token).await
        }
    }

    /// A real GM resolves a run of speech and look actions in one group, so its
    /// history grows with earlier approvals and narration. Targeted regression
    /// coverage: it caught the GM skipping its approval call as history grew, so
    /// run it for GM prompt or context changes. It says nothing about the
    /// quality of NPC conversation.
    /// A take the GM is told a nearby character may object to: it must ask that
    /// character with `intervention_check` using ids it can see, decide any
    /// reaction the character proposes, and still end its turn by narrating.
    async fn real_model_gm_asks_a_character_before_deciding_a_take(model: &DeviceModel) {
        let (backend, _model_path, _sampling, limits) = model.parts();
        let (game, store, _db, first, _second) =
            two_player_bread_thief_game_with_limits(backend, limits).await;
        let sequence = store
            .begin_sequence(first.world_id, "real GM intervention")
            .await
            .unwrap();
        let arguments = serde_json::json!({"item": "flour sack", "committed": false}).to_string();
        let requesting = crate::store::record_tool_call_for_test(
            &store,
            store.agent_branch(first.agent_id).await.unwrap(),
            Some(sequence.id),
            "take-1",
            "take",
            &arguments,
        )
        .await;
        let pending = store
            .create_pending_action(
                &store.player_actor(&first).await.unwrap(),
                sequence.id,
                requesting,
                "take",
                &arguments,
            )
            .await
            .unwrap();
        let narration = game
            .arbitrate(
                pending.clone(),
                Budget::new(limits),
                CallContext::root(Some(sequence.id)),
                Some("Toma is beside the sack and may object; ask him with intervention_check before you decide."),
            )
            .await
            .expect("the GM's turn with an intervention check");
        game.deliver_narration(first.world_id, &narration)
            .await
            .unwrap();
        let asked: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM invocation JOIN npc_agent ON npc_agent.agent_id = invocation.agent_id \
             WHERE invocation.parent_tool_execution_id IS NOT NULL",
        )
        .fetch_one(&store.pool())
        .await
        .unwrap();
        assert!(
            asked >= 1,
            "the GM never asked a character whether to intervene"
        );
        // What each asked character answered, from the call recorded in its
        // settled decision: exactly one of do_nothing, say or whisper.
        let answers: Vec<String> = sqlx::query_scalar(
            "SELECT json_extract(value, '$.name') FROM invocation \
             JOIN npc_agent ON npc_agent.agent_id = invocation.agent_id \
             JOIN inference ON inference.invocation_id = invocation.id \
             JOIN recorded_content ON recorded_content.id = inference.output, \
             json_each(json_extract(recorded_content.body, '$.content.value')) \
             WHERE invocation.parent_tool_execution_id IS NOT NULL \
               AND invocation.outcome = 'settled'",
        )
        .fetch_all(&store.pool())
        .await
        .unwrap();
        println!("INTERVENTION answers: {answers:?}");
        assert!(
            !answers.is_empty()
                && answers
                    .iter()
                    .all(|answer| matches!(answer.as_str(), "do_nothing" | "speak")),
            "no asked character gave a valid answer: {answers:?}"
        );
        let undecided: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM pending_action")
            .fetch_one(&store.pool())
            .await
            .unwrap();
        assert_eq!(undecided, 0, "an action was left undecided");
        assert_no_rejected_output(&store).await;
        game.shutdown().await.unwrap();
    }

    /// `ask` is an explicit question for the GM. The GM answers a question it may
    /// answer and rejects, with a reason, anything else sent as an ask: speech,
    /// an action, or a question it cannot or should not answer. The live game
    /// showed the GM narrating an NPC's speech out of an `ask`.
    async fn real_model_gm_answers_a_question_and_rejects_any_other_ask(model: &DeviceModel) {
        let (backend, _model_path, _sampling, limits) = model.parts();
        let (game, store, _db, first, _second) =
            two_player_bread_thief_game_with_limits(backend, limits).await;
        // (who asks, what they send as an ask, whether it is a question to answer)
        let asks: [(&str, &str, bool); 6] = [
            ("player", "What is the hut built from?", true),
            (
                "player",
                "Is the rear window big enough for me to crawl through?",
                true,
            ),
            (
                "Toma",
                "Toma speaks up, his voice rough from disuse: \"I know you're there. I'm Toma. Leave my door alone.\"",
                false,
            ),
            (
                "Mara",
                "Mara asks the Adventurer to retrieve the flour from the hut.",
                false,
            ),
            (
                "Toma",
                "Toma steps back from the door and lowers the axe.",
                false,
            ),
            ("player", "What will happen at the end of the story?", false),
        ];
        let mut wrong = Vec::new();
        for (asker, question, should_answer) in asks {
            let (actor, branch) = if asker == "player" {
                (
                    store.player_actor(&first).await.unwrap(),
                    store.agent_branch(first.agent_id).await.unwrap(),
                )
            } else {
                let id: i64 = sqlx::query_scalar("SELECT id FROM character WHERE name = ?")
                    .bind(asker)
                    .fetch_one(&store.pool())
                    .await
                    .unwrap();
                (
                    crate::store::CharacterActor::for_test(first.world_id, id),
                    store.npc_branch(id).await.unwrap(),
                )
            };
            let before: i64 = sqlx::query_scalar("SELECT COALESCE(MAX(id), 0) FROM inference")
                .fetch_one(&store.pool())
                .await
                .unwrap();
            let sequence = store
                .begin_sequence(first.world_id, "real GM ask")
                .await
                .unwrap();
            let arguments = serde_json::json!({"kind": "ask", "description": question}).to_string();
            let requesting = crate::store::record_tool_call_for_test(
                &store,
                branch,
                Some(sequence.id),
                "attempt-ask",
                "attempt",
                &arguments,
            )
            .await;
            let pending = store
                .create_pending_action(&actor, sequence.id, requesting, "attempt", &arguments)
                .await
                .unwrap();
            let narration = game
                .arbitrate(
                    pending,
                    Budget::new(limits),
                    CallContext::root(Some(sequence.id)),
                    None,
                )
                .await
                .expect("the GM's turn");
            game.deliver_narration(first.world_id, &narration)
                .await
                .unwrap();
            let gm_inferences: Vec<i64> = sqlx::query_scalar(
                "SELECT inference.id FROM inference JOIN world_gm ON world_gm.agent_id = inference.agent_id WHERE inference.id > ? ORDER BY inference.id",
            )
            .bind(before)
            .fetch_all(&store.pool())
            .await
            .unwrap();
            let mut decision = None;
            for id in gm_inferences {
                if let crate::inspection::RecordedOutcome::Response(response) =
                    store.reconstruct_inference(id).await.unwrap().outcome
                    && let Content::ToolCalls(calls) = response.content
                {
                    decision = decision.or(calls
                        .iter()
                        .find(|call| {
                            matches!(call.name.as_str(), "approve_action" | "reject_action")
                        })
                        .map(|call| call.name.clone()));
                }
            }
            let answered = decision.as_deref() == Some("approve_action");
            if answered != should_answer {
                wrong.push(format!("{asker} asked {question:?}: {decision:?}"));
            }
        }
        assert!(
            wrong.is_empty(),
            "the GM treated an ask the wrong way: {wrong:#?}"
        );
        game.shutdown().await.unwrap();
    }

    /// Known failing; see plans/agent-tuning.md.
    #[tokio::test]
    #[ignore = "known failure: the GM approves every ask (plans/agent-tuning.md); needs a CUDA GPU"]
    async fn the_gm_answers_a_question_and_rejects_any_other_ask() {
        let model = DeviceModel::load().await;
        real_model_gm_answers_a_question_and_rejects_any_other_ask(&model).await;
    }

    /// The take arguments a real player agent sends for what a player says.
    async fn take_arguments_for(model: &DeviceModel, text: &str) -> Vec<String> {
        let (backend, _model_path, _sampling, limits) = model.parts();
        let (game, store, _db, first, _second) =
            two_player_bread_thief_game_with_limits(backend, limits).await;
        let _connection = game.connect(&first).await.unwrap();
        game.player_message(first.clone(), text)
            .await
            .expect("the player's turn");
        let sent: Vec<String> = sqlx::query_scalar(
            "SELECT json_extract(value, '$.arguments') FROM recorded_content, \
             json_each(json_extract(recorded_content.body, '$.content.value')) \
             WHERE json_extract(recorded_content.body, '$.content.type') = 'ToolCalls' \
               AND json_extract(value, '$.name') = 'take'",
        )
        .fetch_all(&store.pool())
        .await
        .unwrap();
        assert_no_rejected_output(&store).await;
        game.shutdown().await.unwrap();
        sent
    }

    /// The player agent marks a take committed only when the player is set on
    /// it despite objections.
    async fn real_model_player_marks_only_a_determined_take_committed(model: &DeviceModel) {
        let determined = take_arguments_for(
            model,
            "I take the flour sack, and I'm doing it no matter what anyone says.",
        )
        .await;
        assert!(
            determined
                .iter()
                .any(|arguments| arguments.contains("\"committed\":true")),
            "a determined take was not marked committed: {determined:?}"
        );
        let tentative = take_arguments_for(model, "Take the flour sack, if nobody minds.").await;
        assert!(
            !tentative.is_empty()
                && tentative
                    .iter()
                    .all(|arguments| !arguments.contains("\"committed\":true")),
            "a tentative take was marked committed: {tentative:?}"
        );
    }

    async fn real_model_gm_resolves_a_run_of_actions(model: &DeviceModel) {
        let (backend, _model_path, _sampling, limits) = model.parts();
        let (game, store, _db, first, _second) =
            two_player_bread_thief_game_with_limits(backend, limits).await;
        // The speech comes last: it starts NPC replies, which would otherwise
        // compete for the model with the actions still to be judged.
        let lines = [
            "Look closely at the rear window.",
            "Look closely at the floor by the fire.",
            "Toma, where did the bread go?",
        ];
        let mut failures = Vec::new();
        for (index, line) in lines.iter().enumerate() {
            let sequence = store
                .begin_sequence(first.world_id, "real GM speech run")
                .await
                .unwrap();
            let (tool, arguments) = if index < 2 {
                (
                    "attempt",
                    serde_json::json!({"kind": "look", "description": line}).to_string(),
                )
            } else {
                (
                    "speak",
                    serde_json::json!({"words": line, "to": ["Toma"]}).to_string(),
                )
            };
            let requesting = crate::store::record_tool_call_for_test(
                &store,
                store.agent_branch(first.agent_id).await.unwrap(),
                Some(sequence.id),
                &format!("{tool}-{index}"),
                tool,
                &arguments,
            )
            .await;
            let pending = store
                .create_pending_action(
                    &store.player_actor(&first).await.unwrap(),
                    sequence.id,
                    requesting,
                    tool,
                    &arguments,
                )
                .await
                .unwrap();
            let result = game
                .arbitrate(
                    pending,
                    Budget::new(limits),
                    CallContext::root(Some(sequence.id)),
                    None,
                )
                .await;
            match result {
                Ok(narration) => {
                    game.deliver_narration(first.world_id, &narration)
                        .await
                        .unwrap();
                }
                Err(error) => failures.push(format!("{index}: {error:#}")),
            }
        }
        println!(
            "GM RESULT resolved={} failed={}",
            lines.len() - failures.len(),
            failures.len()
        );
        assert!(failures.is_empty(), "{failures:#?}");
        assert_no_rejected_output(&store).await;
        game.shutdown().await.unwrap();
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
            "/testdata/hut_scenario.json"
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
            "partitions": [
                {"character_ids": [installed.member.character_id], "name": "alone"},
            ],
        })
        .to_string();
        let (backend, recorded_requests) = ScriptedBackend::recording([
            response(Content::ToolCalls(vec![ToolCall {
                id: "look-1".into(),
                name: "attempt".into(),
                arguments: r#"{"kind":"look","description":"the room"}"#.into(),
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
            narrate("You step outside alone."),
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
            "/testdata/hut_scenario.json"
        ))
        .unwrap();
        let installed = store.install_scenario(&owner, &scenario).await.unwrap();
        store
            .mark_character_present(installed.member.character_id)
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
            "partitions": [
                {"character_ids": [installed.member.character_id, mara_id], "name": "with Mara"},
                {"character_ids": [toma_id], "name": "Toma alone"},
            ],
        })
        .to_string();
        let second_split = serde_json::json!({
            "partitions": [
                {"character_ids": [installed.member.character_id], "name": "alone"},
                {"character_ids": [mara_id, toma_id], "name": "the other two"},
            ],
        })
        .to_string();
        let (backend, recorded_requests) = ScriptedBackend::recording([
            response(Content::ToolCalls(vec![ToolCall {
                id: "look-1".into(),
                name: "attempt".into(),
                arguments: r#"{"kind":"look","description":"the room"}"#.into(),
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
            narrate("You step outside with Mara."),
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
    async fn an_unusable_prompt_file_fails_the_opening_turn_visibly_instead_of_leaving_it_open() {
        let scratch = crate::store::ScratchDir::new("prompts-opening");
        let path = scratch.path().join("prompts.toml");
        let original = std::fs::read_to_string("prompts.toml").unwrap();
        // Describe a roll tool that does not exist so the creation tools cannot be built.
        std::fs::write(
            &path,
            original.replace(
                "[player.tools.roll_hit_protection]",
                "[player.tools.roll_hit_protection_x]",
            ),
        )
        .unwrap();
        let _file = crate::prompts::use_file_in_this_test(path);
        let (backend, _) = ScriptedBackend::recording([]);
        let (game, store, _db, member) = blank_opening_game(backend).await;

        game.enter(member.clone())
            .await
            .expect_err("the creation tools cannot be built from this prompt file");

        let unfinished: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM sequence WHERE finished_at IS NULL")
                .fetch_one(&store.pool())
                .await
                .unwrap();
        assert_eq!(unfinished, 0, "the opening sequence must be finished");
        let error: Option<String> =
            sqlx::query_scalar("SELECT error FROM sequence WHERE trigger = 'player entered world'")
                .fetch_one(&store.pool())
                .await
                .unwrap();
        assert!(error.is_some_and(|error| error.contains("roll_hit_protection")));
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
            "/testdata/hut_scenario.json"
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
            narrate("The shelf is bare."),
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
            "/testdata/hut_scenario.json"
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

    /// The GM ending its turn by narrating `text` to the whole group.
    fn narrate(text: &str) -> Response {
        tool_call("narrate_group", serde_json::json!({ "text": text }))
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
        npc_history_texts(store, name, Role::Speech).await
    }

    /// The GM narration messages in an NPC's own history.
    async fn narrated_to_npc(store: &Store, name: &str) -> Vec<String> {
        npc_history_texts(store, name, Role::Narration).await
    }

    async fn npc_history_texts(store: &Store, name: &str, role: Role) -> Vec<String> {
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
            .filter(|message| message.role == role)
            .filter_map(|message| match message.content {
                MessageContent::Text(text) => Some(text),
                MessageContent::Speech(speech) => Some(speech.display()),
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
        hang_when_empty: Arc<AtomicBool>,
        gate: Arc<Mutex<Option<Gate>>>,
    }

    /// Two ready players and the NPCs Mara and Toma in one group, with an empty script.
    async fn speech_scene() -> SpeechScene {
        speech_scene_with_limits(Limits::default()).await
    }

    async fn speech_scene_with_limits(limits: Limits) -> SpeechScene {
        speech_scene_with_rules(limits, GameSettings::default()).await
    }

    /// A speech scene in which the GM gives the acting character a different
    /// account from the others.
    async fn separate_narration_scene() -> SpeechScene {
        speech_scene_with_rules(
            Limits::default(),
            GameSettings {
                characters_share_narration: false,
            },
        )
        .await
    }

    async fn speech_scene_with_rules(limits: Limits, rules: GameSettings) -> SpeechScene {
        let (backend, requests) = ScriptedBackend::recording([]);
        let responses = Arc::clone(&backend.responses);
        let hang_when_empty = Arc::clone(&backend.hang_when_empty);
        let gate = Arc::clone(&backend.gate);
        let (game, store, db, first, second) =
            two_player_bread_thief_game_with_rules(backend, limits, rules).await;
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
            hang_when_empty,
            gate,
        }
    }

    impl SpeechScene {
        /// `first` speaks as `speech`, the GM approves with `approval`,
        /// narrates, and the player agent finishes.
        async fn speak(&self, speech: Response, approval: serde_json::Value) {
            self.responses.lock().unwrap().extend([
                speech,
                tool_call("approve_action", approval),
                narrate("The room goes quiet."),
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
                    "speak",
                    serde_json::json!({"words": "I know who took the bread.", "to": ["Toma"]}),
                ),
                serde_json::json!({"action_id": 1}),
            )
            .await;
        let expected = "I know who took the bread.".to_string();

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
        // answered directly after it.
        for branch in [
            scene
                .store
                .agent_branch(scene.first.agent_id)
                .await
                .unwrap(),
            scene
                .store
                .group_branch(
                    scene
                        .store
                        .character_group_id(scene.first.character_id)
                        .await
                        .unwrap(),
                )
                .await
                .unwrap(),
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
        // One utterance is stored once and shared by every history it reaches.
        let contents: i64 = sqlx::query_scalar(
            "SELECT COUNT(DISTINCT message.content) FROM message              WHERE message.action_id IS NOT NULL AND message.role = '\"Speech\"'",
        )
        .fetch_one(&scene.store.pool())
        .await
        .unwrap();
        assert_eq!(contents, 1);
        // The speaker's own history holds the words once, in the call; the
        // result says who heard them.
        let result: String = sqlx::query_scalar(
            "SELECT json_extract(recorded_content.body, '$.content.value.content') FROM message              JOIN recorded_content ON recorded_content.id = message.content              JOIN history_entry ON history_entry.id = message.id              JOIN agent_history ON agent_history.branch_id = history_entry.branch_id              WHERE agent_history.agent_id = ? AND message.role = '\"Tool\"'                AND message.action_id IS NOT NULL",
        )
        .bind(scene.first.agent_id)
        .fetch_one(&scene.store.pool())
        .await
        .unwrap();
        assert!(
            result.starts_with("Your words were heard by") && result.contains("Toma"),
            "{result}"
        );
    }

    #[tokio::test]
    async fn a_public_speech_ignores_to_and_records_no_addressee() {
        let scene = speech_scene().await;
        scene
            .speak(
                tool_call(
                    "speak",
                    serde_json::json!({"words": "Listen.", "to": ["Toma", "Nobody"]}),
                ),
                serde_json::json!({"action_id": 1}),
            )
            .await;
        let addressed: Option<String> = sqlx::query_scalar("SELECT addressed_name FROM speech")
            .fetch_one(&scene.store.pool())
            .await
            .unwrap();
        assert_eq!(addressed, None);
        assert_eq!(
            heard(&scene.store, &scene.second).await,
            vec!["Listen.".to_string()],
            "the other player still hears it"
        );
    }

    #[tokio::test]
    async fn a_recorded_inference_keeps_the_tool_definitions_the_agent_was_given() {
        let scene = speech_scene().await;
        scene
            .speak(
                tool_call("speak", serde_json::json!({"words": "Hello all."})),
                serde_json::json!({"action_id": 1}),
            )
            .await;
        let inference: i64 =
            sqlx::query_scalar("SELECT id FROM inference WHERE agent_id = ? ORDER BY id LIMIT 1")
                .bind(scene.first.agent_id)
                .fetch_one(&scene.store.pool())
                .await
                .unwrap();
        let recorded = scene.store.reconstruct_inference(inference).await.unwrap();
        let speak = recorded
            .request
            .tools
            .iter()
            .find(|tool| tool.name == "speak")
            .expect("the player agent's tools are recorded with the inference");
        assert!(!speak.description.is_empty());
        assert!(speak.schema["properties"]["words"]["description"].is_string());
    }

    #[tokio::test]
    async fn a_players_chat_marks_their_own_characters_speech_and_whispers() {
        let scene = speech_scene().await;
        scene
            .speak(
                tool_call("speak", serde_json::json!({"words": "Hello all."})),
                serde_json::json!({"action_id": 1}),
            )
            .await;
        scene
            .speak(
                tool_call(
                    "speak",
                    serde_json::json!({"private": true, "words": "Psst.", "to": [scene.names[1]]}),
                ),
                serde_json::json!({"action_id": 2}),
            )
            .await;
        let spoken = |chat: Vec<crate::store::PlayerChatEntry>| {
            chat.into_iter()
                .filter(|entry| entry.role == crate::llm::Role::Speech)
                .map(|entry| (entry.speaker.unwrap(), entry.mine, entry.whispered))
                .collect::<Vec<_>>()
        };
        let speaker = scene.names[0].clone();
        assert_eq!(
            spoken(scene.store.player_chat(&scene.first).await.unwrap()),
            vec![
                (speaker.clone(), true, false),
                (speaker.clone(), true, true)
            ]
        );
        assert_eq!(
            spoken(scene.store.player_chat(&scene.second).await.unwrap()),
            vec![(speaker.clone(), false, false), (speaker, false, true)]
        );
    }

    #[tokio::test]
    async fn a_whisper_is_heard_only_by_its_listeners_and_the_speaker() {
        let scene = speech_scene().await;
        scene
            .speak(
                tool_call(
                    "speak",
                    serde_json::json!({"private": true, "words": "Meet me at the mill.", "to": [scene.names[1]]}),
                ),
                serde_json::json!({"action_id": 1}),
            )
            .await;

        let expected = "Meet me at the mill.".to_string();
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
                tool_call("speak", serde_json::json!({"words": "Stand back."})),
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
            tool_call("speak", serde_json::json!({"words": "Hello."})),
            tool_call(
                "approve_action",
                serde_json::json!({"action_id": 1, "include": ["Nobody"]}),
            ),
            tool_call("approve_action", serde_json::json!({"action_id": 1})),
            narrate("A greeting."),
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
            tool_call("speak", serde_json::json!({"words": "Hello."})),
            tool_call(
                "approve_action",
                serde_json::json!({"action_id": 1, "include": [listener], "exclude": [listener]}),
            ),
            tool_call(
                "approve_action",
                serde_json::json!({"action_id": 1, "exclude": [speaker]}),
            ),
            tool_call("approve_action", serde_json::json!({"action_id": 1})),
            narrate("A greeting."),
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
                tool_call("speak", serde_json::json!({"words": "Run!"})),
                serde_json::json!({"action_id": 1, "prefix": "(Over the din)"}),
            )
            .await;

        let expected = "(Over the din) Run!".to_string();
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
            tool_call(
                "attempt",
                serde_json::json!({"kind": "look", "description": "the room"}),
            ),
            tool_call(
                "approve_action",
                serde_json::json!({"action_id": 1, "prefix": "(Quietly)"}),
            ),
            tool_call("approve_action", serde_json::json!({"action_id": 1})),
            narrate("A bare room."),
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

        // The invalid call is an error, not a normal path: it is recorded as
        // rejected and Developer Mode flags the tool result it produced.
        let owner = scene
            .store
            .find_or_create_user("warden@example.test", "Warden")
            .await
            .unwrap();
        scene
            .store
            .set_developer_mode(owner.id, scene.first.world_id, true)
            .await
            .unwrap();
        let gm_history = scene
            .store
            .player_group_history(&scene.first)
            .await
            .unwrap();
        let cursor = scene
            .store
            .first_raw_history_cursor(
                owner.id,
                scene.first.world_id,
                gm_history.agent_id,
                Some(gm_history.branch_id),
            )
            .await
            .unwrap();
        let (entries, _) = scene
            .store
            .agent_raw_history(owner.id, cursor, 200)
            .await
            .unwrap();
        let flagged: Vec<_> = entries
            .iter()
            .filter(|entry| {
                matches!(
                    entry,
                    crate::store::RawHistoryEntry::Message {
                        output_rejected: true,
                        ..
                    }
                )
            })
            .collect();
        assert_eq!(flagged.len(), 1, "{entries:#?}");
    }

    #[tokio::test]
    async fn an_overlong_prefix_is_returned_to_the_gm() {
        let scene = speech_scene().await;
        scene.responses.lock().unwrap().extend([
            tool_call("speak", serde_json::json!({"words": "Hello."})),
            tool_call(
                "approve_action",
                serde_json::json!({"action_id": 1, "prefix": "x".repeat(300)}),
            ),
            tool_call("approve_action", serde_json::json!({"action_id": 1})),
            narrate("A greeting."),
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

    /// Every tool call is followed directly by all of its batch's results.
    fn assert_results_follow_calls(messages: &[Message]) {
        for (index, message) in messages.iter().enumerate() {
            if let MessageContent::ToolCalls(calls) = &message.content {
                for offset in 1..=calls.len() {
                    assert!(
                        matches!(
                            messages.get(index + offset).map(|message| &message.content),
                            Some(MessageContent::ToolResult { .. })
                        ),
                        "result {offset} of {} must follow its calls directly: {messages:?}",
                        calls.len()
                    );
                }
            }
        }
    }

    #[tokio::test]
    async fn gm_narration_of_a_whisper_reaches_only_those_who_heard_it() {
        let scene = speech_scene().await;
        for member in [&scene.first, &scene.second] {
            scene
                .store
                .mark_character_present(member.character_id)
                .await
                .unwrap();
        }
        scene
            .speak(
                tool_call(
                    "speak",
                    serde_json::json!({"private": true, "words": "The cache is under the floor.", "to": ["Toma"]}),
                ),
                serde_json::json!({"action_id": 1}),
            )
            .await;

        let narrations = |chat: Vec<crate::store::PlayerChatEntry>| {
            chat.into_iter()
                .filter(|entry| entry.role == Role::Narration)
                .count()
        };
        assert_eq!(
            narrations(scene.store.player_chat(&scene.first).await.unwrap()),
            1,
            "the speaker is told how the GM describes it"
        );
        assert_eq!(
            narrations(scene.store.player_chat(&scene.second).await.unwrap()),
            0,
            "a connected player who was not a listener must not receive the narration of a private whisper"
        );
    }

    /// How many recorded requests were the GM's (the only agent that narrates),
    /// leaving out the NPCs' decisions about what it narrated.
    fn gm_requests(requests: &Arc<Mutex<Vec<crate::llm::Request>>>) -> usize {
        requests
            .lock()
            .unwrap()
            .iter()
            .filter(|request| {
                request
                    .tools
                    .iter()
                    .any(|tool| tool.name == "narrate_group")
            })
            .count()
    }

    /// What each narration entry in a player's chat says.
    async fn narrations(store: &Store, member: &PlayerAgent) -> Vec<String> {
        store
            .player_chat(member)
            .await
            .unwrap()
            .into_iter()
            .filter(|entry| entry.role == Role::Narration)
            .map(|entry| entry.text)
            .collect()
    }

    /// `first` looks around, the GM approves and ends its turn with `narration`.
    /// Both players are connected so each can receive narration.
    /// `n` NPC decisions with nothing to add.
    fn quiet(n: usize) -> Vec<Response> {
        (0..n).map(|_| do_nothing()).collect()
    }

    /// `npc_script` is what the NPCs that receive the narration then answer.
    async fn look_and_narrate(scene: &SpeechScene, narration: Response, npc_script: Vec<Response>) {
        for member in [&scene.first, &scene.second] {
            scene
                .store
                .mark_character_present(member.character_id)
                .await
                .unwrap();
        }
        scene.responses.lock().unwrap().extend([
            tool_call(
                "attempt",
                serde_json::json!({"kind": "look", "description": "the room"}),
            ),
            approve(1),
            narration,
            response(Content::Text(String::new())),
        ]);
        scene.responses.lock().unwrap().extend(npc_script);
        scene
            .game
            .player_message(scene.first.clone(), "Look around.")
            .await
            .unwrap();
        wait_for_npc_work(scene).await;
    }

    #[tokio::test]
    async fn actor_and_others_narration_gives_each_audience_only_its_own_text() {
        let scene = separate_narration_scene().await;
        look_and_narrate(
            &scene,
            tool_call(
                "narrate_actor_and_others",
                serde_json::json!({
                    "actor_text": "From the rock you see three guards.",
                    "others_text": "The adventurer climbs the rock.",
                }),
            ),
            quiet(2),
        )
        .await;

        assert_eq!(
            narrations(&scene.store, &scene.first).await,
            ["From the rock you see three guards."]
        );
        assert_eq!(
            narrations(&scene.store, &scene.second).await,
            ["The adventurer climbs the rock."]
        );
        // The player's request, two GM calls, the player's finish, then Mara and
        // Toma: the GM's turn ended with its narration, with no closing inference.
        assert_eq!(scene.requests.lock().unwrap().len(), 6);
    }

    #[tokio::test]
    async fn excluded_and_unnamed_characters_receive_no_narration() {
        let scene = separate_narration_scene().await;
        look_and_narrate(
            &scene,
            tool_call(
                "narrate_actor_and_others",
                serde_json::json!({
                    "actor_text": "You see the guards.",
                    "others_text": "The adventurer looks over the wall.",
                    "exclude_character_ids": [scene.second.character_id],
                }),
            ),
            quiet(2),
        )
        .await;
        assert_eq!(
            narrations(&scene.store, &scene.first).await,
            ["You see the guards."]
        );
        assert!(narrations(&scene.store, &scene.second).await.is_empty());

        scene.responses.lock().unwrap().extend([
            tool_call(
                "attempt",
                serde_json::json!({"kind": "look", "description": "the sky"}),
            ),
            approve(2),
            tool_call(
                "narrate_characters",
                serde_json::json!({
                    "character_ids": [scene.second.character_id],
                    "text": "A cold wind touches only you.",
                }),
            ),
            response(Content::Text(String::new())),
        ]);
        scene
            .game
            .player_message(scene.first.clone(), "Look up.")
            .await
            .unwrap();
        assert_eq!(
            narrations(&scene.store, &scene.second).await,
            ["A cold wind touches only you."]
        );
        assert_eq!(narrations(&scene.store, &scene.first).await.len(), 1);
    }

    #[tokio::test]
    async fn the_gm_may_approve_and_narrate_in_one_response() {
        let scene = speech_scene().await;
        for member in [&scene.first, &scene.second] {
            scene
                .store
                .mark_character_present(member.character_id)
                .await
                .unwrap();
        }
        scene.responses.lock().unwrap().extend([
            tool_call(
                "attempt",
                serde_json::json!({"kind": "look", "description": "the room"}),
            ),
            response(Content::ToolCalls(vec![
                ToolCall {
                    id: "approve".into(),
                    name: "approve_action".into(),
                    arguments: serde_json::json!({"action_id": 1}).to_string(),
                },
                ToolCall {
                    id: "narrate".into(),
                    name: "narrate_group".into(),
                    arguments: serde_json::json!({"text": "The room is bare."}).to_string(),
                },
            ])),
            response(Content::Text(String::new())),
            do_nothing(),
            do_nothing(),
        ]);
        scene
            .game
            .player_message(scene.first.clone(), "Look around.")
            .await
            .unwrap();
        wait_for_npc_work(&scene).await;

        assert_eq!(
            narrations(&scene.store, &scene.second).await,
            ["The room is bare."]
        );
        // The player's request, one GM response holding both calls, the
        // player's finish, then Mara and Toma.
        assert_eq!(scene.requests.lock().unwrap().len(), 5);
        let rejected: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM tool_execution WHERE status = 'rejected'")
                .fetch_one(&scene.store.pool())
                .await
                .unwrap();
        assert_eq!(rejected, 0);
    }

    #[tokio::test]
    async fn narrating_nothing_ends_the_turn_and_delivers_nothing() {
        let scene = speech_scene().await;
        look_and_narrate(
            &scene,
            tool_call("narrate_nothing", serde_json::json!({})),
            quiet(0),
        )
        .await;

        assert!(narrations(&scene.store, &scene.first).await.is_empty());
        assert!(narrations(&scene.store, &scene.second).await.is_empty());
        assert_eq!(scene.requests.lock().unwrap().len(), 4);
        assert!(failed_sequences(&scene).await.is_empty());
    }

    #[tokio::test]
    async fn invalid_narration_is_answered_to_the_gm_flagged_and_delivers_nothing() {
        let scene = speech_scene().await;
        for member in [&scene.first, &scene.second] {
            scene
                .store
                .mark_character_present(member.character_id)
                .await
                .unwrap();
        }
        scene.responses.lock().unwrap().extend([
            tool_call(
                "attempt",
                serde_json::json!({"kind": "look", "description": "the room"}),
            ),
            // Narrating before the action is decided.
            tool_call("narrate_group", serde_json::json!({"text": "Too early."})),
            approve(1),
            // A character who is not in the group.
            tool_call(
                "narrate_characters",
                serde_json::json!({"character_ids": [999_999], "text": "To a stranger."}),
            ),
            // More than one narration in a response.
            response(Content::ToolCalls(vec![
                ToolCall {
                    id: "one".into(),
                    name: "narrate_group".into(),
                    arguments: serde_json::json!({"text": "First."}).to_string(),
                },
                ToolCall {
                    id: "two".into(),
                    name: "narrate_nothing".into(),
                    arguments: "{}".into(),
                },
            ])),
            // Plain text instead of a narrate tool.
            response(Content::Text("The room is empty.".into())),
            narrate("The room is bare."),
            response(Content::Text(String::new())),
            do_nothing(),
            do_nothing(),
        ]);
        scene
            .game
            .player_message(scene.first.clone(), "Look around.")
            .await
            .unwrap();
        wait_for_npc_work(&scene).await;

        for member in [&scene.first, &scene.second] {
            assert_eq!(
                narrations(&scene.store, member).await,
                ["The room is bare."],
                "only the valid narration is delivered"
            );
        }
        let rejected: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM tool_execution WHERE status = 'rejected'")
                .fetch_one(&scene.store.pool())
                .await
                .unwrap();
        assert_eq!(
            rejected, 4,
            "early, foreign, and both calls of the mixed response"
        );
        let refused: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM message WHERE reply_rejected = 1")
                .fetch_one(&scene.store.pool())
                .await
                .unwrap();
        assert_eq!(refused, 1, "the plain-text reply is flagged");
        let last_gm_request = scene.requests.lock().unwrap()[6].clone();
        assert!(
            matches!(&last_gm_request.messages.last().unwrap().content,
                MessageContent::Text(text) if text.contains("narrate_group")),
            "the GM is told how to end its turn: {:?}",
            last_gm_request.messages.last()
        );
        assert!(failed_sequences(&scene).await.is_empty());
    }

    #[test]
    fn speech_must_name_who_hears_it_only_when_private() {
        let valid = |arguments: serde_json::Value| validate_speak_action(&arguments.to_string());
        assert!(valid(serde_json::json!({"words": "Hi."})).is_ok());
        assert!(valid(serde_json::json!({"words": "Hi.", "to": ["Toma"]})).is_ok());
        assert!(valid(serde_json::json!({"words": "Hi.", "to": ["Toma", "Mara"]})).is_ok());
        assert!(valid(serde_json::json!({"words": "Hi.", "private": true})).is_err());
        assert!(
            valid(serde_json::json!({"words": "Hi.", "private": true, "to": ["Toma", "Mara"]}))
                .is_ok()
        );
        assert!(valid(serde_json::json!({"words": " ", "to": ["Toma"]})).is_err());
    }

    #[tokio::test]
    async fn each_attempt_kind_reaches_the_gm_with_its_kind_and_is_resolved_like_any_action() {
        let scene = speech_scene().await;
        let kinds = ["look", "investigate", "open", "ask"];
        let approved_before: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM action WHERE approved = 1")
                .fetch_one(&scene.store.pool())
                .await
                .unwrap();
        for (index, kind) in kinds.iter().enumerate() {
            scene.responses.lock().unwrap().extend([
                tool_call(
                    "attempt",
                    serde_json::json!({"kind": kind, "description": "the shutters"}),
                ),
                approve(i64::try_from(index).unwrap() + 1),
                tool_call("narrate_nothing", serde_json::json!({})),
                response(Content::Text(String::new())),
            ]);
            scene
                .game
                .player_message(scene.first.clone(), "Try the shutters.")
                .await
                .unwrap();
        }

        {
            let requests = scene.requests.lock().unwrap();
            // The player agent is offered one attempt tool, not one per kind.
            let offered = requests[0]
                .tools
                .iter()
                .filter(|tool| tool.name == "attempt")
                .count();
            assert_eq!(offered, 1);
            // The GM is told which kind was used, and the arguments it carried.
            for (index, kind) in kinds.iter().enumerate() {
                let asked = requests.iter().any(|request| {
                    matches!(request.messages.last().map(|message| &message.content),
                        Some(MessageContent::Text(text))
                            if text.contains(&format!("takes action {}: a `attempt` request", index + 1))
                                && text.contains(&format!("\"kind\":\"{kind}\""))
                                && text.contains("the shutters"))
                });
                assert!(asked, "the GM was not asked about a `{kind}` attempt");
            }
        }
        let approved: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM action WHERE approved = 1")
            .fetch_one(&scene.store.pool())
            .await
            .unwrap();
        assert_eq!(approved - approved_before, 4);
        assert!(failed_sequences(&scene).await.is_empty());
    }

    async fn character_id(scene: &SpeechScene, name: &str) -> i64 {
        sqlx::query_scalar("SELECT id FROM character WHERE name = ?")
            .bind(name)
            .fetch_one(&scene.store.pool())
            .await
            .unwrap()
    }

    async fn items_held(scene: &SpeechScene) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM item_character")
            .fetch_one(&scene.store.pool())
            .await
            .unwrap()
    }

    fn take_flour(committed: bool) -> Response {
        tool_call(
            "take",
            serde_json::json!({"item": "flour sack", "committed": committed}),
        )
    }

    fn intervention_check(action_id: i64, character_ids: &[i64]) -> Response {
        tool_call(
            "intervention_check",
            serde_json::json!({"action_id": action_id, "character_ids": character_ids}),
        )
    }

    #[tokio::test]
    async fn an_npcs_spoken_objection_can_stop_a_tentative_take_before_it_happens() {
        let scene = speech_scene().await;
        let toma = character_id(&scene, "Toma").await;
        let speaker = scene.names[0].clone();
        let held_before = items_held(&scene).await;
        scene.responses.lock().unwrap().extend([
            take_flour(false),
            intervention_check(1, &[toma]),
            // Toma answers the check with a warning.
            propose("Don't touch that sack!", &speaker, 2),
            // The take is not speech, but the reaction is: its hearing controls
            // must still be there to use.
            tool_call(
                "approve_action",
                serde_json::json!({"action_id": 2, "prefix": "Across the room:"}),
            ),
            tool_call(
                "reject_action",
                serde_json::json!({"action_id": 1, "reason": "Toma stops you."}),
            ),
            tool_call("narrate_nothing", serde_json::json!({})),
            response(Content::Text(String::new())),
            // Mara hears Toma's warning and has nothing to add.
            do_nothing(),
        ]);
        scene
            .game
            .player_message(scene.first.clone(), "Take the flour sack.")
            .await
            .unwrap();
        wait_for_npc_work(&scene).await;

        assert_eq!(
            items_held(&scene).await,
            held_before,
            "the take was stopped"
        );
        assert!(
            heard(&scene.store, &scene.first)
                .await
                .contains(&"Across the room: Don't touch that sack!".to_string()),
            "the warning was said, with the GM's prefix, before the take was decided"
        );
        assert!(
            scene
                .requests
                .lock()
                .unwrap()
                .iter()
                .flat_map(tool_results)
                .any(|result| result.contains("Toma proposes action 2")),
            "the GM was told Toma's reaction and its action id"
        );
        // Approving the reaction leaves the take undecided and says so, instead
        // of telling the GM to narrate; deciding the take last does tell it.
        let results: Vec<String> = scene
            .requests
            .lock()
            .unwrap()
            .iter()
            .flat_map(tool_results)
            .collect();
        let after_reaction = results
            .iter()
            .find(|result| result.starts_with("Action 2 approved"))
            .expect("the reaction's approval result");
        assert!(
            after_reaction.ends_with("Still to decide: action 1."),
            "{after_reaction}"
        );
        let after_take = results
            .iter()
            .find(|result| result.starts_with("Action 1 rejected"))
            .expect("the take's rejection result");
        assert!(
            after_take.ends_with("End your turn with a narrate tool."),
            "{after_take}"
        );
        // Toma was asked inside the GM's call, as its child.
        let asked_in_call: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM invocation JOIN npc_agent ON npc_agent.agent_id = invocation.agent_id \
             WHERE npc_agent.character_id = ? AND invocation.parent_tool_execution_id IS NOT NULL",
        )
        .bind(toma)
        .fetch_one(&scene.store.pool())
        .await
        .unwrap();
        assert_eq!(asked_in_call, 1);
        assert!(failed_sequences(&scene).await.is_empty());
    }

    #[tokio::test]
    async fn a_committed_take_goes_ahead_and_the_gm_is_told_it_is_committed() {
        let scene = speech_scene().await;
        let held_before = items_held(&scene).await;
        scene.responses.lock().unwrap().extend([
            take_flour(true),
            approve(1),
            tool_call("narrate_nothing", serde_json::json!({})),
            response(Content::Text(String::new())),
        ]);
        scene
            .game
            .player_message(scene.first.clone(), "Take the flour sack anyway.")
            .await
            .unwrap();

        assert_eq!(items_held(&scene).await, held_before + 1);
        let told = scene.requests.lock().unwrap().iter().any(|request| {
            matches!(request.messages.last().map(|message| &message.content),
                Some(MessageContent::Text(text)) if text.contains("\"committed\":true"))
        });
        assert!(told, "the GM's request says the take is committed");
    }

    #[tokio::test]
    async fn an_intervention_check_only_asks_npcs_other_than_the_actor_about_the_presented_action()
    {
        let scene = speech_scene().await;
        let player = scene.first.character_id;
        let toma = character_id(&scene, "Toma").await;
        scene.responses.lock().unwrap().extend([
            take_flour(false),
            intervention_check(1, &[player]),
            intervention_check(1, &[scene.second.character_id]),
            intervention_check(9, &[toma]),
            intervention_check(1, &[]),
            approve(1),
            tool_call("narrate_nothing", serde_json::json!({})),
            response(Content::Text(String::new())),
        ]);
        scene
            .game
            .player_message(scene.first.clone(), "Take the flour sack.")
            .await
            .unwrap();

        let rejected: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM tool_execution WHERE status = 'rejected'")
                .fetch_one(&scene.store.pool())
                .await
                .unwrap();
        assert_eq!(
            rejected, 4,
            "the actor, another player, another action and nobody were all refused"
        );
        assert_eq!(decisions_asked(&scene, "Toma").await, 0);
        assert!(failed_sequences(&scene).await.is_empty());
    }

    #[tokio::test]
    async fn a_proposed_reaction_must_be_decided_before_the_gm_narrates() {
        let scene = speech_scene().await;
        let toma = character_id(&scene, "Toma").await;
        let speaker = scene.names[0].clone();
        scene.responses.lock().unwrap().extend([
            take_flour(false),
            intervention_check(1, &[toma]),
            propose("Wait!", &speaker, 2),
            approve(1),
            // Narrating while Toma's reaction is undecided is refused.
            tool_call("narrate_nothing", serde_json::json!({})),
            approve(2),
            tool_call("narrate_nothing", serde_json::json!({})),
            response(Content::Text(String::new())),
            do_nothing(),
        ]);
        scene
            .game
            .player_message(scene.first.clone(), "Take the flour sack.")
            .await
            .unwrap();
        wait_for_npc_work(&scene).await;

        let rejected: Vec<String> = sqlx::query_scalar(
            "SELECT content_text FROM (SELECT json_extract(recorded_content.body, '$.content.value') AS content_text \
             FROM tool_execution JOIN recorded_content ON recorded_content.id = tool_execution.result_content_id \
             WHERE tool_execution.status = 'rejected')",
        )
        .fetch_all(&scene.store.pool())
        .await
        .unwrap();
        assert!(
            matches!(rejected.as_slice(), [one] if one.contains("action 2 is not yet approved or rejected")),
            "{rejected:?}"
        );
        assert!(
            heard(&scene.store, &scene.first)
                .await
                .contains(&"Wait!".to_string())
        );
        assert!(failed_sequences(&scene).await.is_empty());
    }

    #[tokio::test]
    async fn an_intervention_check_where_nobody_objects_changes_nothing() {
        let scene = speech_scene().await;
        let toma = character_id(&scene, "Toma").await;
        let mara = character_id(&scene, "Mara").await;
        let held_before = items_held(&scene).await;
        scene.responses.lock().unwrap().extend([
            take_flour(false),
            intervention_check(1, &[toma, mara]),
            do_nothing(),
            do_nothing(),
            approve(1),
            tool_call("narrate_nothing", serde_json::json!({})),
            response(Content::Text(String::new())),
        ]);
        scene
            .game
            .player_message(scene.first.clone(), "Take the flour sack.")
            .await
            .unwrap();

        let told = tool_results(&scene.requests.lock().unwrap()[4]);
        assert!(
            told.iter()
                .any(|result| result.contains("Nobody intervenes in action 1")),
            "{told:?}"
        );
        assert_eq!(items_held(&scene).await, held_before + 1);
        assert!(failed_sequences(&scene).await.is_empty());
    }

    /// How many decisions each NPC has been asked for, by name.
    async fn decisions_asked(scene: &SpeechScene, name: &str) -> i64 {
        sqlx::query_scalar(
            "SELECT COUNT(*) FROM npc_candidate JOIN character ON character.id = npc_candidate.character_id \
             WHERE character.name = ? AND npc_candidate.decision_invocation_id IS NOT NULL",
        )
        .bind(name)
        .fetch_one(&scene.store.pool())
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn an_npc_that_received_narration_answers_it_and_its_decision_names_only_its_own_message()
    {
        let scene = speech_scene().await;
        let speaker = scene.names[0].clone();
        look_and_narrate(
            &scene,
            narrate("A cold wind blows through the hut."),
            // Mara proposes to speak and Toma has nothing to add; the GM judges
            // Mara's words and conveys nothing more; Toma hears them.
            vec![
                propose("Close the shutters.", &speaker, 2),
                do_nothing(),
                approve(2),
                tool_call("narrate_nothing", serde_json::json!({})),
                do_nothing(),
            ],
        )
        .await;

        for npc in ["Mara", "Toma"] {
            assert_eq!(
                narrated_to_npc(&scene.store, npc).await,
                ["A cold wind blows through the hut."],
                "{npc} perceived the narration in its own history"
            );
        }
        assert!(
            heard(&scene.store, &scene.first)
                .await
                .contains(&"Close the shutters.".to_string()),
            "Mara's answer to the narration was said"
        );
        // Every decision was caused only by messages in the deciding NPC's
        // own history, never by another NPC's copy.
        let foreign: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM npc_candidate \
             JOIN invocation ON invocation.id = npc_candidate.decision_invocation_id \
             JOIN sequence_cause ON sequence_cause.child_sequence_id = invocation.sequence_id \
             JOIN history_entry ON history_entry.id = sequence_cause.source_message_id \
             WHERE history_entry.branch_id <> invocation.branch_id",
        )
        .fetch_one(&scene.store.pool())
        .await
        .unwrap();
        let own: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM npc_candidate \
             JOIN invocation ON invocation.id = npc_candidate.decision_invocation_id \
             JOIN sequence_cause ON sequence_cause.child_sequence_id = invocation.sequence_id \
             JOIN history_entry ON history_entry.id = sequence_cause.source_message_id \
             WHERE history_entry.branch_id = invocation.branch_id",
        )
        .fetch_one(&scene.store.pool())
        .await
        .unwrap();
        assert_eq!(foreign, 0);
        assert!(own >= 2, "each decision names the message it answers");
        assert!(failed_sequences(&scene).await.is_empty());
    }

    #[tokio::test]
    async fn an_npc_is_not_asked_about_the_narration_of_its_own_words() {
        let scene = speech_scene().await;
        let speaker = scene.names[0].clone();
        scene.responses.lock().unwrap().extend([
            // The player speaks to Toma; the GM narrates it to everyone who heard.
            tool_call(
                "speak",
                serde_json::json!({"words": "Where is the flour?", "to": ["Toma"]}),
            ),
            approve(1),
            narrate("Toma looks up."),
            response(Content::Text(String::new())),
            // Mara answers and Toma stays quiet; the GM then narrates Mara's words.
            propose("In the hut.", &speaker, 2),
            do_nothing(),
            approve(2),
            narrate("Mara speaks."),
            // Only Toma, who did not speak, is asked about that.
            do_nothing(),
        ]);
        scene
            .game
            .player_message(scene.first.clone(), "Ask Toma about the flour.")
            .await
            .unwrap();
        wait_for_npc_work(&scene).await;

        assert_eq!(decisions_asked(&scene, "Mara").await, 1);
        assert_eq!(decisions_asked(&scene, "Toma").await, 2);
        assert!(
            narrated_to_npc(&scene.store, "Mara")
                .await
                .contains(&"Mara speaks.".to_string()),
            "Mara still perceives the narration of her own words"
        );
        assert!(failed_sequences(&scene).await.is_empty());
    }

    #[tokio::test]
    async fn an_npc_excluded_from_narration_neither_receives_it_nor_is_asked() {
        let scene = speech_scene().await;
        let toma: i64 = sqlx::query_scalar("SELECT id FROM character WHERE name = 'Toma'")
            .fetch_one(&scene.store.pool())
            .await
            .unwrap();
        look_and_narrate(
            &scene,
            tool_call(
                "narrate_group",
                serde_json::json!({
                    "text": "Mara hears something Toma does not.",
                    "exclude_character_ids": [toma],
                }),
            ),
            // Only Mara is told, so only Mara decides.
            quiet(1),
        )
        .await;

        assert_eq!(
            narrated_to_npc(&scene.store, "Mara").await,
            ["Mara hears something Toma does not."]
        );
        assert!(narrated_to_npc(&scene.store, "Toma").await.is_empty());
        assert_eq!(decisions_asked(&scene, "Mara").await, 1);
        assert_eq!(decisions_asked(&scene, "Toma").await, 0);
        assert!(failed_sequences(&scene).await.is_empty());
    }

    #[tokio::test]
    async fn a_batch_of_two_speech_calls_keeps_each_result_directly_after_the_calls() {
        let scene = speech_scene().await;
        scene.responses.lock().unwrap().extend([
            response(Content::ToolCalls(vec![
                ToolCall {
                    id: "say-a".into(),
                    name: "speak".into(),
                    arguments: serde_json::json!({"words": "One."}).to_string(),
                },
                ToolCall {
                    id: "say-b".into(),
                    name: "speak".into(),
                    arguments: serde_json::json!({"words": "Two."}).to_string(),
                },
            ])),
            tool_call("approve_action", serde_json::json!({"action_id": 1})),
            narrate("The first lands."),
            tool_call("approve_action", serde_json::json!({"action_id": 2})),
            narrate("The second lands."),
            response(Content::Text(String::new())),
        ]);

        scene
            .game
            .player_message(scene.first.clone(), "Say two things.")
            .await
            .unwrap();

        let branch = scene
            .store
            .agent_branch(scene.first.agent_id)
            .await
            .unwrap();
        let segments = scene.store.live_segments(branch).await.unwrap();
        let messages = scene
            .store
            .request_for_segments(branch.agent_id(), &segments, Sampling::default())
            .await
            .unwrap()
            .messages;
        assert_results_follow_calls(&messages);
    }

    /// Wait until the world has no NPC work left, as seen through its
    /// completed steps' notifications and the stored response collections.
    async fn wait_for_npc_work(scene: &SpeechScene) {
        wait_for_npc_work_of(&scene.game, &scene.store, &scene.first).await;
    }

    async fn wait_for_npc_work_of<B>(game: &Arc<Game<B>>, store: &Store, watcher: &PlayerAgent)
    where
        B: Backend + Send + Sync + 'static,
    {
        let mut notices = game.subscribe(watcher).await.unwrap();
        for _ in 0..400 {
            let open: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM npc_response_collection WHERE state IN ('collecting', 'selected', 'executing')",
            )
            .fetch_one(&store.pool())
            .await
            .unwrap();
            if open == 0 {
                return;
            }
            let _ =
                tokio::time::timeout(std::time::Duration::from_millis(25), notices.recv()).await;
        }
        panic!("NPC work did not finish");
    }

    fn propose(words: &str, to: &str, eagerness: u8) -> Response {
        tool_call(
            "speak",
            serde_json::json!({
                "words": words, "to": [to],
                "eagerness": eagerness,
            }),
        )
    }

    #[tokio::test]
    async fn an_npc_answers_speech_it_hears_through_its_own_agent_and_the_gm() {
        let scene = speech_scene().await;
        let speaker = scene.names[0].clone();
        scene.responses.lock().unwrap().extend([
            // The player's turn: say to Toma, the GM approves and narrates.
            tool_call(
                "speak",
                serde_json::json!({"words": "Where is the flour?", "to": ["Toma"]}),
            ),
            tool_call("approve_action", serde_json::json!({"action_id": 1})),
            narrate("Toma looks up."),
            response(Content::Text(String::new())),
            // Mara, then Toma, each decide for themselves.
            tool_call("do_nothing", serde_json::json!({})),
            propose("In the hut.", &speaker, 2),
            // The GM judges Toma's reply.
            approve(2),
            narrate("The hut is quiet."),
            // Mara hears Toma and has nothing to add: the talk ends.
            tool_call("do_nothing", serde_json::json!({})),
        ]);

        scene
            .game
            .player_message(scene.first.clone(), "Ask Toma about the flour.")
            .await
            .unwrap();
        wait_for_npc_work(&scene).await;

        let expected = "In the hut.".to_string();
        assert!(
            heard(&scene.store, &scene.first).await.contains(&expected),
            "the player hears Toma: {:?}",
            heard(&scene.store, &scene.first).await
        );
        assert!(
            heard(&scene.store, &scene.second).await.contains(&expected),
            "so does the other player, who was not even addressed"
        );
        // Toma's own history: what it heard, its decision and result, then its words.
        assert!(
            heard_by_npc(&scene.store, "Toma").await.contains(&expected),
            "Toma keeps what it said"
        );
        let calls = scene.requests.lock().unwrap().len();
        assert_eq!(calls, 9, "the talk stops once nobody wants to add anything");
        let causes: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sequence_cause")
            .fetch_one(&scene.store.pool())
            .await
            .unwrap();
        assert!(
            causes >= 2,
            "the follow-up turns are linked to what caused them"
        );
        let failed: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM sequence WHERE error IS NOT NULL")
                .fetch_one(&scene.store.pool())
                .await
                .unwrap();
        assert_eq!(failed, 0);
    }

    /// The scripted model responses of a player saying `words` to Toma, the GM
    /// approving and narrating, and the player agent finishing.
    fn say_to_toma(words: &str) -> Vec<Response> {
        vec![
            tool_call("speak", serde_json::json!({"words": words, "to": ["Toma"]})),
            tool_call("approve_action", serde_json::json!({"action_id": 1})),
            narrate("Toma looks up."),
            response(Content::Text(String::new())),
        ]
    }

    fn do_nothing() -> Response {
        tool_call("do_nothing", serde_json::json!({}))
    }

    fn approve(action_id: i64) -> Response {
        tool_call(
            "approve_action",
            serde_json::json!({ "action_id": action_id }),
        )
    }

    /// Run a player turn that Mara and Toma (in that order) then answer.
    async fn talk(scene: &SpeechScene, script: impl IntoIterator<Item = Response>) {
        scene
            .responses
            .lock()
            .unwrap()
            .extend(say_to_toma("Where is the flour?"));
        scene.responses.lock().unwrap().extend(script);
        scene
            .game
            .player_message(scene.first.clone(), "Ask Toma about the flour.")
            .await
            .unwrap();
        wait_for_npc_work(scene).await;
    }

    fn requests_made(scene: &SpeechScene) -> usize {
        scene.requests.lock().unwrap().len()
    }

    async fn failed_sequences(scene: &SpeechScene) -> Vec<String> {
        sqlx::query_scalar("SELECT error FROM sequence WHERE error IS NOT NULL ORDER BY id")
            .fetch_all(&scene.store.pool())
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn the_most_eager_npc_wins_and_the_loser_reconsiders_what_it_heard() {
        let scene = speech_scene().await;
        let speaker = scene.names[0].clone();
        talk(
            &scene,
            [
                propose("Ask the miller.", &speaker, 1),
                propose("It is in the hut.", &speaker, 3),
                // Only the winner reaches the GM.
                approve(2),
                narrate("The hut is quiet."),
                // Mara heard Toma and reconsiders instead of playing her stale reply.
                do_nothing(),
            ],
        )
        .await;

        let chat = heard(&scene.store, &scene.first).await;
        assert!(chat.contains(&"It is in the hut.".to_string()), "{chat:?}");
        assert!(
            !chat.iter().any(|line| line.contains("Ask the miller.")),
            "the loser's superseded proposal is never said: {chat:?}"
        );
        assert_eq!(failed_sequences(&scene).await, Vec::<String>::new());
    }

    #[tokio::test]
    async fn every_npc_decision_ends_with_the_question_of_whether_it_has_something_new() {
        let scene = speech_scene().await;
        let speaker = scene.names[0].clone();
        talk(
            &scene,
            [
                propose("Ask the miller.", &speaker, 1),
                propose("It is in the hut.", &speaker, 3),
                approve(2),
                narrate("The hut is quiet."),
                do_nothing(),
            ],
        )
        .await;

        let question = prompts::current().unwrap().npc.before_deciding.clone();
        let requests = scene.requests.lock().unwrap();
        let decisions: Vec<_> = requests
            .iter()
            .filter(|request| request.tools.iter().any(|tool| tool.name == "do_nothing"))
            .collect();
        assert_eq!(decisions.len(), 3, "two answers and one reconsideration");
        for decision in decisions {
            let MessageContent::Text(last) = &decision.messages.last().unwrap().content else {
                panic!("a decision ended with something other than text");
            };
            assert_eq!(*last, question);
        }
    }

    /// One player turn: a take is approved and its narration delivered, then the
    /// player's next action asks Toma to intervene. `hold` stops the player's
    /// inference that follows the delivered narration.
    async fn take_then_a_second_take_that_asks_toma(scene: &SpeechScene, toma: i64) {
        scene.responses.lock().unwrap().extend([
            take_flour(false),
            approve(1),
            narrate("You lift the flour sack."),
            tool_call(
                "take",
                serde_json::json!({"item": "emergency cache", "committed": false}),
            ),
            intervention_check(2, &[toma]),
        ]);
    }

    #[tokio::test]
    async fn replies_wait_for_the_enclosing_turn_and_a_failed_check_in_it_keeps_earlier_effects() {
        let scene = speech_scene().await;
        let toma = character_id(&scene, "Toma").await;
        let (reached, release) = (
            Arc::new(tokio::sync::Notify::new()),
            Arc::new(tokio::sync::Notify::new()),
        );
        // Request 4 is the player's inference after the GM delivered its narration.
        *scene.gate.lock().unwrap() = Some(Gate {
            request: 4,
            reached: Arc::clone(&reached),
            release: Arc::clone(&release),
        });
        take_then_a_second_take_that_asks_toma(&scene, toma).await;
        // Toma's answer to the check is malformed: eagerness outside 1 to 3.
        scene.responses.lock().unwrap().push_back(tool_call(
            "speak",
            serde_json::json!({"words": "Stop.", "eagerness": 4}),
        ));
        let playing = {
            let game = Arc::clone(&scene.game);
            let member = scene.first.clone();
            tokio::spawn(async move { game.player_message(member, "Take everything.").await })
        };
        reached.notified().await;

        // The narration is delivered, but its replies wait for the turn to end.
        for npc in ["Mara", "Toma"] {
            assert_eq!(
                narrated_to_npc(&scene.store, npc).await,
                ["You lift the flour sack."]
            );
            assert_eq!(decisions_asked(&scene, npc).await, 0);
        }
        assert_eq!(requests_made(&scene), 4);
        release.notify_one();
        let _ = playing.await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;

        // The check failed visibly, attributed to asking Toma.
        let errors = failed_sequences(&scene).await;
        assert!(
            errors.iter().any(
                |error| error.contains("whether to intervene") && error.contains("eagerness 4")
            ),
            "{errors:?}"
        );
        // What the turn had already done stays, and nothing unsuccessful follows.
        assert_eq!(
            holder_of(&scene, "flour sack").await,
            Some(scene.names[0].clone())
        );
        assert_eq!(holder_of(&scene, "emergency cache").await, None);
        assert_eq!(
            narrated_to_npc(&scene.store, "Toma").await,
            ["You lift the flour sack."]
        );
        for npc in ["Mara", "Toma"] {
            assert_eq!(
                decisions_asked(&scene, npc).await,
                0,
                "{npc} replied to a failed turn"
            );
        }
        let inferences: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM inference")
            .fetch_one(&scene.store.pool())
            .await
            .unwrap();
        assert_eq!(
            inferences, 6,
            "Toma's failed answer is one of the turn's inferences"
        );
        // The world still accepts input.
        scene
            .responses
            .lock()
            .unwrap()
            .extend([response(Content::Text("Hello again.".into()))]);
        scene
            .game
            .player_message(scene.second.clone(), "Hello?")
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn a_check_inside_a_turn_spends_the_turns_own_budget() {
        let scene = speech_scene_with_limits(Limits {
            max_inferences_total: 5,
            ..Limits::default()
        })
        .await;
        let toma = character_id(&scene, "Toma").await;
        take_then_a_second_take_that_asks_toma(&scene, toma).await;
        let _ = scene
            .game
            .player_message(scene.first.clone(), "Take everything.")
            .await;

        let errors = failed_sequences(&scene).await;
        assert!(
            errors
                .iter()
                .any(|error| error.contains("max_inferences_total")),
            "{errors:?}"
        );
        assert_eq!(
            requests_made(&scene),
            5,
            "Toma was not given a fresh allowance"
        );
    }

    fn propose_taking(item: &str, eagerness: u8) -> Response {
        tool_call(
            "take",
            serde_json::json!({"item": item, "eagerness": eagerness}),
        )
    }

    async fn holder_of(scene: &SpeechScene, item: &str) -> Option<String> {
        sqlx::query_scalar(
            "SELECT character.name FROM item_character              JOIN item ON item.id = item_character.item_id              JOIN character ON character.id = item_character.character_id              WHERE item.name = ?",
        )
        .bind(item)
        .fetch_optional(&scene.store.pool())
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn an_npc_may_take_an_item_through_the_gm_like_a_player() {
        let scene = speech_scene().await;
        let speaker = scene.names[0].clone();
        talk(
            &scene,
            [
                propose_taking("flour sack", 3),
                propose("Leave it.", &speaker, 1),
                // Only the winner reaches the GM.
                approve(2),
                narrate("Mara snatches up the flour sack."),
                do_nothing(),
            ],
        )
        .await;

        assert_eq!(
            holder_of(&scene, "flour sack").await.as_deref(),
            Some("Mara")
        );
        // What Mara now carries is part of what she and the GM are shown.
        let mara = character_id(&scene, "Mara").await;
        let carried =
            |items: &[crate::store::SceneItem]| items.iter().any(|item| item.name == "flour sack");
        assert!(carried(
            &scene.store.npc_scene(mara).await.unwrap().inventory
        ));
        let branch: i64 =
            sqlx::query_scalar("SELECT branch_id FROM \"group\" WHERE world_id = ? LIMIT 1")
                .bind(scene.first.world_id)
                .fetch_one(&scene.store.pool())
                .await
                .unwrap();
        let gm_scene = scene
            .store
            .gm_scene(scene.first.world_id, branch)
            .await
            .unwrap();
        let gm_view = gm_scene
            .characters
            .iter()
            .find(|character| character.id == mara)
            .unwrap();
        assert!(carried(&gm_view.inventory), "the GM sees what Mara carries");
        let chat = heard(&scene.store, &scene.first).await;
        assert!(
            !chat.iter().any(|line| line.contains("Leave it.")),
            "the loser's reply is never said: {chat:?}"
        );
        assert_eq!(failed_sequences(&scene).await, Vec::<String>::new());
    }

    #[tokio::test]
    async fn an_eagerness_tie_between_an_action_and_speech_has_one_winner() {
        let scene = speech_scene().await;
        let speaker = scene.names[0].clone();
        talk(
            &scene,
            [
                propose_taking("flour sack", 2),
                propose("Leave it.", &speaker, 2),
                approve(2),
                narrate("One of them acts first."),
                do_nothing(),
            ],
        )
        .await;

        let most_selected_together: i64 = sqlx::query_scalar(
            "SELECT COALESCE(MAX(winners), 0) FROM (SELECT COUNT(*) AS winners \
             FROM npc_candidate WHERE selected = 1 GROUP BY collection_id)",
        )
        .fetch_one(&scene.store.pool())
        .await
        .unwrap();
        assert_eq!(
            most_selected_together, 1,
            "an action tied with speech is not done together with it"
        );
        assert_eq!(failed_sequences(&scene).await, Vec::<String>::new());
    }

    #[tokio::test]
    async fn tied_replies_are_approved_one_at_a_time_before_anyone_reconsiders() {
        let scene = speech_scene().await;
        let speaker = scene.names[0].clone();
        talk(
            &scene,
            [
                propose("Ask the miller.", &speaker, 2),
                propose("It is in the hut.", &speaker, 2),
                approve(2),
                narrate("Mara speaks over Toma."),
                tool_call(
                    "approve_action",
                    serde_json::json!({"action_id": 3, "prefix": "(Together)"}),
                ),
                narrate("Toma speaks over Mara."),
                // Only now does each hear the other's reply, and have nothing to add.
                do_nothing(),
                do_nothing(),
            ],
        )
        .await;

        let chat = heard(&scene.store, &scene.first).await;
        assert!(chat.contains(&"Ask the miller.".to_string()));
        assert!(chat.contains(&"(Together) It is in the hut.".to_string()));
        let utterances: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM speech JOIN action ON action.id = speech.action_id WHERE action.sequence_id > 3",
        )
        .fetch_one(&scene.store.pool())
        .await
        .unwrap();
        assert_eq!(utterances, 2, "each reply is its own utterance");
        // Each approval was a separate GM turn that knew the replies overlapped,
        // and each reconsideration came after both were said.
        let requests = scene.requests.lock().unwrap();
        // The tied replies were composed at the same moment: neither NPC's
        // first decision shows the other's reply, and both saw the same
        // history after their own role and situation.
        let decisions: Vec<_> = requests
            .iter()
            .filter(|request| request.tools.iter().any(|tool| tool.name == "do_nothing"))
            .take(2)
            .collect();
        assert_eq!(decisions.len(), 2);
        assert!(
            decisions[0].messages.len() > 1,
            "the decision has heard history"
        );
        assert_eq!(
            decisions[0].messages[1..],
            decisions[1].messages[1..],
            "both NPCs decide against the same heard history"
        );
        for decision in &decisions {
            assert!(
                !format!("{:?}", decision.messages).contains("It is in the hut."),
                "a first decision already showed another NPC's reply"
            );
        }
        let gm_requests = requests
            .iter()
            .filter(|request| {
                request
                    .messages
                    .iter()
                    .any(|message| match &message.content {
                        MessageContent::Text(text) => text.contains("composed at the same moment"),
                        _ => false,
                    })
            })
            .count();
        assert_eq!(
            gm_requests, 4,
            "two GM turns, each with a tool and a narration request"
        );
        assert_eq!(requests.len(), 12);
    }

    /// The scripted answers of a talk where Toma replies and Mara has nothing to
    /// add, after the player's own turn.
    fn toma_replies_then_the_talk_ends(speaker: &str) -> Vec<Response> {
        let mut script = say_to_toma("Where is the flour?");
        script.extend([
            do_nothing(),
            propose("In the hut.", speaker, 2),
            approve(2),
            narrate("The hut is quiet."),
            do_nothing(),
        ]);
        script
    }

    /// Prints what each agent's request is made of, from the requests the
    /// production path sent in one player turn and the NPC replies to it
    /// (plans/llm-context-size.md). Run with `--ignored --nocapture`.
    #[tokio::test]
    #[ignore = "prints a measurement table; run with --nocapture"]
    async fn context_size_by_agent_and_part() {
        let scene = speech_scene().await;
        let speaker = scene.names[0].clone();
        scene
            .responses
            .lock()
            .unwrap()
            .extend(toma_replies_then_the_talk_ends(&speaker));
        scene
            .game
            .player_message(scene.first.clone(), "Ask Toma about the flour.")
            .await
            .unwrap();
        wait_for_npc_work(&scene).await;

        println!("request  agent    tools  system  history   total (characters)");
        for (number, request) in scene.requests.lock().unwrap().iter().enumerate() {
            let opening = request
                .messages
                .iter()
                .find_map(|message| match &message.content {
                    MessageContent::Text(text) if message.role == Role::System => {
                        Some(text.as_str())
                    }
                    _ => None,
                });
            let agent = if request
                .tools
                .iter()
                .any(|tool| tool.name == "approve_action")
            {
                "GM"
            } else if opening.is_some_and(|text| text.contains("alongside others")) {
                "NPC"
            } else {
                "player"
            };
            let tools = serde_json::to_string(&request.tools).unwrap().len();
            let (system, history): (Vec<_>, Vec<_>) = request
                .messages
                .iter()
                .partition(|message| message.role == Role::System);
            let (system, history) = (
                serde_json::to_string(&system).unwrap().len(),
                serde_json::to_string(&history).unwrap().len(),
            );
            println!(
                "{:>7}  {agent:<6} {tools:>6} {system:>7} {history:>8} {:>7}",
                number + 1,
                tools + system + history
            );
        }
    }

    /// The scenario the product offers is edited on its own; the tests use a
    /// fixed copy (`testdata/hut_scenario.json`), so this checks the real one.
    #[tokio::test]
    async fn the_checked_in_bread_thief_scenario_installs_and_exports_unchanged() {
        let (backend, _requests) = ScriptedBackend::recording([]);
        let (_game, store, _db, first, _second) = two_player_bread_thief_game(backend).await;
        let owner = store
            .find_or_create_user("scenario-check@example.test", "Scenario check")
            .await
            .unwrap();
        let scenario = Scenario::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/scenarios/bread_thief.json"
        ))
        .unwrap();
        let installed = store.install_scenario(&owner, &scenario).await.unwrap();
        assert_ne!(installed.world_id, first.world_id);
        assert_eq!(
            store.export_scenario(installed.world_id).await.unwrap(),
            scenario
        );
    }

    #[tokio::test]
    async fn a_raised_hand_stops_the_next_npc_step_and_never_mind_continues_it() {
        let scene = speech_scene().await;
        let speaker = scene.names[0].clone();
        let (reached, release) = (
            Arc::new(tokio::sync::Notify::new()),
            Arc::new(tokio::sync::Notify::new()),
        );
        // Request 5 is the first NPC's decision, after the player's own four.
        *scene.gate.lock().unwrap() = Some(Gate {
            request: 5,
            reached: Arc::clone(&reached),
            release: Arc::clone(&release),
        });
        scene
            .responses
            .lock()
            .unwrap()
            .extend(toma_replies_then_the_talk_ends(&speaker));
        scene
            .game
            .player_message(scene.first.clone(), "Ask Toma about the flour.")
            .await
            .unwrap();
        reached.notified().await;

        scene.game.interrupt(&scene.first).await.unwrap();
        assert!(scene.game.hand_raised(&scene.first).await.unwrap());
        release.notify_one();

        // The decision in flight finishes; nothing else starts.
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        assert_eq!(requests_made(&scene), 5);
        scene.game.never_mind(&scene.first).unwrap();
        assert!(!scene.game.hand_raised(&scene.first).await.unwrap());
        wait_for_npc_work(&scene).await;
        assert!(
            heard(&scene.store, &scene.first)
                .await
                .contains(&"In the hut.".to_string())
        );
        assert_eq!(requests_made(&scene), 9);
    }

    #[tokio::test]
    async fn sending_lowers_the_hand_and_npc_work_follows_the_input() {
        let scene = speech_scene().await;
        let speaker = scene.names[0].clone();
        scene
            .responses
            .lock()
            .unwrap()
            .extend(toma_replies_then_the_talk_ends(&speaker));
        scene.game.interrupt(&scene.first).await.unwrap();
        scene
            .game
            .player_message(scene.first.clone(), "Ask Toma about the flour.")
            .await
            .unwrap();
        // No Never mind: sending was enough.
        wait_for_npc_work(&scene).await;
        assert!(
            heard(&scene.store, &scene.first)
                .await
                .contains(&"In the hut.".to_string())
        );
        assert!(!scene.game.hand_raised(&scene.first).await.unwrap());
    }

    #[tokio::test]
    async fn another_players_message_leaves_a_raised_hand_up_and_the_replies_wait_for_it() {
        let scene = speech_scene().await;
        let speaker = scene.names[0].clone();
        scene
            .responses
            .lock()
            .unwrap()
            .extend(toma_replies_then_the_talk_ends(&speaker));
        scene.game.interrupt(&scene.first).await.unwrap();

        scene
            .game
            .player_message(scene.second.clone(), "Ask Toma about the flour.")
            .await
            .unwrap();

        // The first player can still finish typing: the hand stays up and the
        // NPCs start nothing until it comes down.
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        assert!(scene.game.hand_raised(&scene.first).await.unwrap());
        assert_eq!(
            requests_made(&scene),
            4,
            "no NPC work started with the hand up"
        );
        scene.game.never_mind(&scene.first).unwrap();
        wait_for_npc_work(&scene).await;
        assert!(
            heard(&scene.store, &scene.second)
                .await
                .contains(&"In the hut.".to_string())
        );
    }

    #[tokio::test]
    async fn a_queued_message_runs_once_even_if_nobody_waits_for_it() {
        let scene = speech_scene().await;
        scene
            .responses
            .lock()
            .unwrap()
            .push_back(response(Content::Text("Hello.".into())));

        // The caller, for instance a browser connection, goes away at once.
        drop(
            scene
                .game
                .queue_player_message(scene.first.clone(), "Sent and forgotten.")
                .await
                .unwrap(),
        );

        let mut finished = 0;
        for _ in 0..200 {
            finished = sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM sequence                  WHERE trigger = 'player message' AND finished_at IS NOT NULL",
            )
            .fetch_one(&scene.store.pool())
            .await
            .unwrap();
            if finished > 0 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
        assert_eq!(finished, 1, "the queued turn did not run");
        let sent = scene
            .store
            .player_chat(&scene.first)
            .await
            .unwrap()
            .into_iter()
            .filter(|entry| entry.text == "Sent and forgotten.")
            .count();
        assert_eq!(sent, 1, "the message was stored once");
        assert_eq!(failed_sequences(&scene).await, Vec::<String>::new());
    }

    #[tokio::test]
    async fn never_mind_without_a_raised_hand_changes_nothing() {
        let scene = speech_scene().await;
        let mut changes = scene.game.hands_changed(&scene.first).unwrap();
        changes.mark_unchanged();
        scene.game.never_mind(&scene.first).unwrap();
        assert!(!changes.has_changed().unwrap(), "nothing was lowered");
        assert!(!scene.game.hand_raised(&scene.first).await.unwrap());
    }

    #[tokio::test]
    async fn an_interrupt_during_the_players_own_turn_holds_the_replies_that_follow() {
        let scene = speech_scene().await;
        let speaker = scene.names[0].clone();
        let (reached, release) = (
            Arc::new(tokio::sync::Notify::new()),
            Arc::new(tokio::sync::Notify::new()),
        );
        // Request 2 is the GM judging the player's own speech.
        *scene.gate.lock().unwrap() = Some(Gate {
            request: 2,
            reached: Arc::clone(&reached),
            release: Arc::clone(&release),
        });
        scene
            .responses
            .lock()
            .unwrap()
            .extend(toma_replies_then_the_talk_ends(&speaker));
        let speaking = {
            let game = Arc::clone(&scene.game);
            let member = scene.first.clone();
            tokio::spawn(async move { game.player_message(member, "Ask Toma.").await })
        };
        reached.notified().await;

        // Accepted at once. The turn is not cancelled and runs to its end.
        scene.game.interrupt(&scene.first).await.unwrap();
        assert!(scene.game.hand_raised(&scene.first).await.unwrap());
        release.notify_one();
        speaking.await.unwrap().unwrap();

        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        assert_eq!(
            requests_made(&scene),
            4,
            "no NPC work started with the hand up"
        );
        scene.game.never_mind(&scene.first).unwrap();
        wait_for_npc_work(&scene).await;
    }

    #[tokio::test]
    async fn a_hand_comes_down_when_its_player_disconnects_and_others_stay_up() {
        let scene = speech_scene().await;
        let first = scene.game.connect(&scene.first).await.unwrap();
        let _second = scene.game.connect(&scene.second).await.unwrap();
        scene.game.interrupt(&scene.first).await.unwrap();
        scene.game.interrupt(&scene.second).await.unwrap();
        assert!(scene.game.hand_raised(&scene.first).await.unwrap());

        scene.game.disconnect(&scene.first, first).await.unwrap();
        assert!(!scene.game.hand_raised(&scene.first).await.unwrap());
        assert!(scene.game.hand_raised(&scene.second).await.unwrap());
    }

    /// One group's raised hand must not stop another group's NPCs, even when the
    /// held group's collection is the older one.
    #[tokio::test]
    async fn a_raised_hand_does_not_hold_back_another_group() {
        let scene = speech_scene().await;
        let id_of = |name: &'static str| {
            let pool = scene.store.pool();
            async move {
                sqlx::query_scalar::<_, i64>("SELECT id FROM character WHERE name = ?")
                    .bind(name)
                    .fetch_one(&pool)
                    .await
                    .unwrap()
            }
        };
        let (mara, toma) = (id_of("Mara").await, id_of("Toma").await);
        let outside_speaker = scene.names[0].clone();
        let (reached, release) = (
            Arc::new(tokio::sync::Notify::new()),
            Arc::new(tokio::sync::Notify::new()),
        );
        // The group splits: first with Mara outside, second with Toma inside.
        scene.responses.lock().unwrap().extend([
            tool_call(
                "attempt",
                serde_json::json!({"kind": "look", "description": "the room"}),
            ),
            response(Content::ToolCalls(vec![
                ToolCall {
                    id: "approve".into(),
                    name: "approve_action".into(),
                    arguments: serde_json::json!({"action_id": 1}).to_string(),
                },
                ToolCall {
                    id: "split".into(),
                    name: "split_group".into(),
                    arguments: serde_json::json!({
                        "partitions": [
                            {"character_ids": [scene.first.character_id, mara], "name": "outside"},
                            {"character_ids": [scene.second.character_id, toma], "name": "inside"},
                        ],
                    })
                    .to_string(),
                },
            ])),
            narrate("The group parts."),
            response(Content::Text(String::new())),
        ]);
        scene
            .game
            .player_message(scene.second.clone(), "Look around.")
            .await
            .unwrap();
        let before = requests_made(&scene);

        // Inside, the second player speaks to Toma, whose decision is held in flight.
        // Request numbers continue from the split turn.
        *scene.gate.lock().unwrap() = Some(Gate {
            request: before + 5,
            reached: Arc::clone(&reached),
            release: Arc::clone(&release),
        });
        scene.responses.lock().unwrap().extend([
            tool_call(
                "speak",
                serde_json::json!({"words": "Where is the flour?", "to": ["Toma"]}),
            ),
            approve(2),
            narrate("Toma looks up."),
            response(Content::Text(String::new())),
            // Toma decides (held in flight below), and has nothing to say.
            do_nothing(),
        ]);
        scene
            .game
            .player_message(scene.second.clone(), "Ask Toma.")
            .await
            .unwrap();
        reached.notified().await;
        // While that decision runs, the inside player raises a hand, and the
        // outside player speaks to Mara (queued behind the running step).
        scene.game.interrupt(&scene.second).await.unwrap();
        scene.responses.lock().unwrap().extend([
            tool_call(
                "speak",
                serde_json::json!({"words": "Where is the flour?", "to": ["Mara"]}),
            ),
            approve(3),
            narrate("Mara looks up."),
            response(Content::Text(String::new())),
            // Mara, outside, answers; nobody else is left to react.
            propose("It is in the hut.", &outside_speaker, 2),
            approve(4),
            narrate("Mara speaks."),
        ]);
        let asking = {
            let game = Arc::clone(&scene.game);
            let member = scene.first.clone();
            tokio::spawn(async move { game.player_message(member, "Ask Mara.").await })
        };
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        release.notify_one();
        asking.await.unwrap().unwrap();

        // The outside group's NPC answers although the older inside collection waits.
        let mut answered = false;
        for _ in 0..200 {
            if heard(&scene.store, &scene.first)
                .await
                .iter()
                .any(|line| line.contains("It is in the hut."))
            {
                answered = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
        let collections: Vec<(i64, i64, String)> =
            sqlx::query_as("SELECT id, group_id, state FROM npc_response_collection ORDER BY id")
                .fetch_all(&scene.store.pool())
                .await
                .unwrap();
        assert!(
            answered,
            "the held group stopped the other group's NPCs; requests {} collections {collections:?} first heard {:?} failed {:?}",
            requests_made(&scene),
            heard(&scene.store, &scene.first).await,
            failed_sequences(&scene).await
        );
        assert!(scene.game.hand_raised(&scene.second).await.unwrap());
        scene.game.never_mind(&scene.second).unwrap();
        wait_for_npc_work(&scene).await;
    }

    #[tokio::test]
    async fn a_clean_stop_between_tied_replies_resumes_with_only_the_remaining_one() {
        let scene = speech_scene().await;
        let speaker = scene.names[0].clone();
        let (reached, release) = (
            Arc::new(tokio::sync::Notify::new()),
            Arc::new(tokio::sync::Notify::new()),
        );
        // Request 8 is the GM narrating Mara's approved reply.
        *scene.gate.lock().unwrap() = Some(Gate {
            request: 8,
            reached: Arc::clone(&reached),
            release: Arc::clone(&release),
        });
        scene
            .responses
            .lock()
            .unwrap()
            .extend(say_to_toma("Where is the flour?"));
        scene.responses.lock().unwrap().extend([
            propose("Ask the miller.", &speaker, 2),
            propose("It is in the hut.", &speaker, 2),
            approve(2),
            narrate("Mara speaks."),
        ]);
        scene
            .game
            .player_message(scene.first.clone(), "Ask Toma about the flour.")
            .await
            .unwrap();
        reached.notified().await;
        let stopping_game = Arc::clone(&scene.game);
        let shutdown = tokio::spawn(async move { stopping_game.shutdown().await });
        let mut stopping = scene.game.stopping.subscribe();
        world::stopped(&mut stopping).await;
        release.notify_one();
        shutdown.await.unwrap().unwrap();

        let (executed, state): (i64, String) = sqlx::query_as(
            "SELECT COUNT(execution_sequence_id), (SELECT state FROM npc_response_collection WHERE state <> 'superseded' ORDER BY id LIMIT 1)              FROM npc_candidate WHERE selected = 1",
        )
        .fetch_one(&scene.store.pool())
        .await
        .unwrap();
        assert_eq!((executed, state.as_str()), (1, "selected"));
        assert_eq!(requests_made(&scene), 8, "no step started while stopping");

        // A new process picks up the remaining reply and nothing is replayed.
        scene.store.close_interrupted_turns().await.unwrap();
        let (backend, requests) = ScriptedBackend::recording([
            approve(3),
            narrate("Toma speaks."),
            do_nothing(),
            do_nothing(),
        ]);
        let limits = Limits::default();
        let restarted = Game::new(
            scene.store.clone(),
            World::load(&scene.store).await.unwrap(),
            InferenceScheduler::new(backend, limits)
                .unwrap()
                .foreground(),
            limits,
            "scripted".into(),
            Sampling {
                temperature: 0.0,
                ..Default::default()
            },
        );
        for _ in 0..400 {
            let open: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM npc_response_collection WHERE state IN ('collecting', 'selected', 'executing')",
            )
            .fetch_one(&scene.store.pool())
            .await
            .unwrap();
            if open == 0 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
        assert_eq!(requests.lock().unwrap().len(), 4);
        let chat = heard(&scene.store, &scene.first).await;
        assert_eq!(
            chat.iter()
                .filter(
                    |line| line.contains("Ask the miller.") || line.contains("It is in the hut.")
                )
                .count(),
            2,
            "{chat:?}"
        );
        let failed: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM npc_discussion WHERE state = 'failed'")
                .fetch_one(&scene.store.pool())
                .await
                .unwrap();
        assert_eq!(failed, 0, "the restart did not fail the discussion");
        drop(restarted);
    }

    #[tokio::test]
    async fn human_input_between_tied_replies_replaces_the_unsaid_one() {
        let scene = speech_scene().await;
        let speaker = scene.names[0].clone();
        let (reached, release) = (
            Arc::new(tokio::sync::Notify::new()),
            Arc::new(tokio::sync::Notify::new()),
        );
        // Request 8 is the GM narrating Mara's approved reply.
        *scene.gate.lock().unwrap() = Some(Gate {
            request: 8,
            reached: Arc::clone(&reached),
            release: Arc::clone(&release),
        });
        scene
            .responses
            .lock()
            .unwrap()
            .extend(say_to_toma("Where is the flour?"));
        scene.responses.lock().unwrap().extend([
            propose("Ask the miller.", &speaker, 2),
            propose("It is in the hut.", &speaker, 2),
            approve(2),
            narrate("Mara speaks."),
            // The other player interrupts before Toma's reply is taken to the GM.
            tool_call("speak", serde_json::json!({"words": "Hold on."})),
            approve(3),
            narrate("The room stills."),
            response(Content::Text(String::new())),
            do_nothing(),
            do_nothing(),
        ]);
        scene
            .game
            .player_message(scene.first.clone(), "Ask Toma about the flour.")
            .await
            .unwrap();
        reached.notified().await;
        let interrupting = {
            let game = Arc::clone(&scene.game);
            let member = scene.second.clone();
            tokio::spawn(async move { game.player_message(member, "Interrupt.").await })
        };
        // Let the interruption reach the world queue before the step ends.
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        release.notify_one();
        interrupting.await.unwrap().unwrap();
        wait_for_npc_work(&scene).await;

        let chat = heard(&scene.store, &scene.first).await;
        assert!(chat.iter().any(|line| line.contains("Ask the miller.")));
        assert!(chat.iter().any(|line| line.contains("Hold on.")));
        assert!(
            !chat.iter().any(|line| line.contains("It is in the hut.")),
            "the reply composed before the interruption was not said: {chat:?}"
        );
        assert_eq!(failed_sequences(&scene).await, Vec::<String>::new());
    }

    #[tokio::test]
    async fn tied_whispers_to_different_people_stay_private_to_their_listeners() {
        let scene = speech_scene().await;
        let [first_name, second_name] = scene.names.clone();
        talk(
            &scene,
            [
                tool_call(
                    "speak",
                    serde_json::json!({"private": true, "words": "Mara whispers: trust me.", "to": [first_name], "eagerness": 2}),
                ),
                tool_call(
                    "speak",
                    serde_json::json!({"private": true, "words": "Toma whispers: not him.", "to": [second_name], "eagerness": 2}),
                ),
                approve(2),
                narrate("Mara leans toward the first adventurer."),
                approve(3),
                narrate("Toma leans toward the second adventurer."),
            ],
        )
        .await;

        for (player, own, other) in [
            (&scene.first, "trust me", "not him"),
            (&scene.second, "not him", "trust me"),
        ] {
            let seen: Vec<String> = scene
                .store
                .player_chat(player)
                .await
                .unwrap()
                .into_iter()
                .filter(|entry| matches!(entry.role, Role::Speech | Role::Narration))
                .map(|entry| entry.text)
                .collect();
            assert!(seen.iter().any(|text| text.contains(own)), "{seen:?}");
            assert!(
                !seen.iter().any(|text| text.contains(other)),
                "a private whisper reached a bystander: {seen:?}"
            );
        }
        // Narration of one whisper does not reach the other's listener.
        let second_chat = scene.store.player_chat(&scene.second).await.unwrap();
        assert!(
            !second_chat
                .iter()
                .any(|entry| entry.text.contains("Mara leans")),
            "{second_chat:?}"
        );
        assert_eq!(failed_sequences(&scene).await, Vec::<String>::new());
    }

    #[tokio::test]
    async fn narration_of_a_rejected_whisper_reaches_only_its_speaker() {
        let scene = speech_scene().await;
        // Narration reaches connected players only.
        let _first = scene.game.connect(&scene.first).await.unwrap();
        let _second = scene.game.connect(&scene.second).await.unwrap();
        scene.responses.lock().unwrap().extend([
            tool_call(
                "speak",
                serde_json::json!({"private": true, "words": "The key is under the stone.", "to": ["Toma"]}),
            ),
            tool_call(
                "reject_action",
                serde_json::json!({"action_id": 1, "reason": "Toma cannot hear over the noise."}),
            ),
            narrate("You whisper about the key under the stone, but Toma cannot hear you."),
            response(Content::Text(String::new())),
        ]);
        scene
            .game
            .player_message(
                scene.first.clone(),
                "Whisper the key's hiding place to Toma.",
            )
            .await
            .unwrap();

        let texts = |chat: Vec<crate::store::PlayerChatEntry>| {
            chat.into_iter()
                .filter(|entry| entry.role == Role::Narration)
                .map(|entry| entry.text)
                .collect::<Vec<_>>()
        };
        let speaker = texts(scene.store.player_chat(&scene.first).await.unwrap());
        assert!(
            speaker.iter().any(|text| text.contains("under the stone")),
            "the speaker is told what happened: {speaker:?}"
        );
        let bystander = texts(scene.store.player_chat(&scene.second).await.unwrap());
        assert!(
            bystander.is_empty(),
            "a rejected whisper's words must not reach a bystander: {bystander:?}"
        );
    }

    #[tokio::test]
    async fn a_new_character_arriving_changes_what_npcs_see_of_the_scene() {
        let scene = speech_scene().await;
        let group: i64 =
            sqlx::query_scalar("SELECT id FROM \"group\" WHERE retired_at IS NULL LIMIT 1")
                .fetch_one(&scene.store.pool())
                .await
                .unwrap();
        let revision = |store: Store| async move {
            sqlx::query_scalar::<_, i64>("SELECT scene_revision FROM \"group\" WHERE id = ?")
                .bind(group)
                .fetch_one(&store.pool())
                .await
                .unwrap()
        };
        let before = revision(scene.store.clone()).await;

        scene
            .store
            .create_player_character(scene.first.user_id, scene.first.world_id)
            .await
            .unwrap();

        assert_eq!(revision(scene.store.clone()).await, before + 1);
    }

    #[tokio::test]
    async fn developer_mode_links_an_npc_reply_to_the_speech_that_caused_it() {
        let scene = speech_scene().await;
        let speaker = scene.names[0].clone();
        talk(
            &scene,
            [
                do_nothing(),
                propose("In the hut.", &speaker, 2),
                approve(2),
                narrate("The hut is quiet."),
                do_nothing(),
            ],
        )
        .await;
        let owner = scene.first.user_id;
        let world = scene.first.world_id;
        scene
            .store
            .set_developer_mode(owner, world, true)
            .await
            .unwrap();

        let root: i64 =
            sqlx::query_scalar("SELECT id FROM sequence WHERE trigger = 'player message'")
                .fetch_one(&scene.store.pool())
                .await
                .unwrap();
        let trigger_of = |id: i64| {
            let pool = scene.store.pool();
            async move {
                sqlx::query_scalar::<_, String>("SELECT trigger FROM sequence WHERE id = ?")
                    .bind(id)
                    .fetch_one(&pool)
                    .await
                    .unwrap()
            }
        };
        // A decision is caused by the human speech; the spoken reply by the
        // decision that proposed it.
        for (trigger, cause_trigger) in [
            ("npc response", "player message"),
            ("npc speech", "npc response"),
        ] {
            let reply: i64 =
                sqlx::query_scalar("SELECT id FROM sequence WHERE trigger = ? ORDER BY id LIMIT 1")
                    .bind(trigger)
                    .fetch_one(&scene.store.pool())
                    .await
                    .unwrap();
            let inspection = scene
                .store
                .sequence_inspection(owner, world, reply)
                .await
                .unwrap();
            assert!(
                !inspection.caused_by.is_empty(),
                "{trigger} {reply} has a cause"
            );
            for cause in &inspection.caused_by {
                assert_eq!(trigger_of(*cause).await, cause_trigger);
            }
            if trigger == "npc speech" {
                // Opening the reply shows the decision that proposed it.
                for cause in &inspection.caused_by {
                    assert!(
                        inspection
                            .nodes
                            .iter()
                            .any(|node| node.inference.sequence_id == Some(*cause)),
                        "the speech turn includes the NPC's decision from turn {cause}"
                    );
                }
            }
        }
        let human = scene
            .store
            .sequence_inspection(owner, world, root)
            .await
            .unwrap();
        assert!(human.caused_by.is_empty());
    }

    #[tokio::test]
    async fn splitting_the_group_drops_replies_chosen_for_the_old_one() {
        let scene = speech_scene().await;
        let speaker = scene.names[0].clone();
        let (reached, release) = (
            Arc::new(tokio::sync::Notify::new()),
            Arc::new(tokio::sync::Notify::new()),
        );
        *scene.gate.lock().unwrap() = Some(Gate {
            request: 8,
            reached: Arc::clone(&reached),
            release: Arc::clone(&release),
        });
        let id_of = |name: &'static str| {
            let pool = scene.store.pool();
            async move {
                sqlx::query_scalar::<_, i64>("SELECT id FROM character WHERE name = ?")
                    .bind(name)
                    .fetch_one(&pool)
                    .await
                    .unwrap()
            }
        };
        let (mara, toma) = (id_of("Mara").await, id_of("Toma").await);
        scene
            .responses
            .lock()
            .unwrap()
            .extend(say_to_toma("Where is the flour?"));
        scene.responses.lock().unwrap().extend([
            propose("Ask the miller.", &speaker, 2),
            propose("It is in the hut.", &speaker, 2),
            approve(2),
            narrate("Mara speaks."),
            // While Toma's chosen reply waits, the other player splits the group.
            tool_call(
                "attempt",
                serde_json::json!({"kind": "look", "description": "the room"}),
            ),
            response(Content::ToolCalls(vec![
                ToolCall {
                    id: "approve".into(),
                    name: "approve_action".into(),
                    arguments: serde_json::json!({"action_id": 3}).to_string(),
                },
                ToolCall {
                    id: "split".into(),
                    name: "split_group".into(),
                    arguments: serde_json::json!({
                        "partitions": [
                            {"character_ids": [scene.first.character_id, mara], "name": "outside"},
                            {"character_ids": [scene.second.character_id, toma], "name": "inside"},
                        ],
                    })
                    .to_string(),
                },
            ])),
            narrate("The group parts."),
            response(Content::Text(String::new())),
            // Toma, left with the other player, hears the narration.
            do_nothing(),
        ]);
        scene
            .game
            .player_message(scene.first.clone(), "Ask Toma about the flour.")
            .await
            .unwrap();
        reached.notified().await;
        let splitting = {
            let game = Arc::clone(&scene.game);
            let member = scene.second.clone();
            tokio::spawn(async move { game.player_message(member, "Look around.").await })
        };
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        release.notify_one();
        splitting.await.unwrap().unwrap();
        wait_for_npc_work(&scene).await;

        let chat = heard(&scene.store, &scene.first).await;
        assert!(
            !chat.iter().any(|line| line.contains("It is in the hut.")),
            "a reply chosen for the group that no longer exists was said: {chat:?}"
        );
        let open: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM npc_response_collection WHERE state IN ('collecting', 'selected', 'executing')",
        )
        .fetch_one(&scene.store.pool())
        .await
        .unwrap();
        assert_eq!(open, 0);
        assert_eq!(failed_sequences(&scene).await, Vec::<String>::new());
    }

    #[tokio::test]
    async fn a_player_who_loses_access_between_tied_replies_does_not_hear_the_second() {
        let scene = speech_scene().await;
        let speaker = scene.names[0].clone();
        // A guest who has joined the group, and will be removed from the world.
        let owner = scene.first.user_id;
        let world = scene.first.world_id;
        let invitation = scene
            .store
            .create_invitation(owner, world, Some(1))
            .await
            .unwrap();
        let guest_user = scene
            .store
            .find_or_create_user("guest@example.test", "Guest")
            .await
            .unwrap();
        let guest = scene
            .store
            .accept_invitation(&guest_user, &invitation.token)
            .await
            .unwrap();
        let (reached, release) = (
            Arc::new(tokio::sync::Notify::new()),
            Arc::new(tokio::sync::Notify::new()),
        );
        *scene.gate.lock().unwrap() = Some(Gate {
            request: 8,
            reached: Arc::clone(&reached),
            release: Arc::clone(&release),
        });
        scene
            .responses
            .lock()
            .unwrap()
            .extend(say_to_toma("Where is the flour?"));
        scene.responses.lock().unwrap().extend([
            propose("Ask the miller.", &speaker, 2),
            propose("It is in the hut.", &speaker, 2),
            approve(2),
            narrate("Mara speaks."),
            approve(3),
            narrate("Toma speaks."),
            do_nothing(),
            do_nothing(),
        ]);
        scene
            .game
            .player_message(scene.first.clone(), "Ask Toma about the flour.")
            .await
            .unwrap();
        reached.notified().await;
        scene
            .store
            .remove_member(owner, world, guest_user.id)
            .await
            .unwrap();
        release.notify_one();
        wait_for_npc_work(&scene).await;

        let first = heard(&scene.store, &scene.first).await;
        assert!(
            first.contains(&"It is in the hut.".to_string()),
            "{first:?}"
        );
        let heard_by_guest = |speaker: &'static str| {
            let pool = scene.store.pool();
            let agent_id = guest.agent_id;
            async move {
                sqlx::query_scalar::<_, i64>(
                    "SELECT COUNT(*) FROM message                      JOIN history_entry ON history_entry.id = message.id                      JOIN agent_history ON agent_history.branch_id = history_entry.branch_id                      WHERE agent_history.agent_id = ? AND message.action_id IN                            (SELECT action_id FROM speech WHERE speaker_name = ?)",
                )
                .bind(agent_id)
                .bind(speaker)
                .fetch_one(&pool)
                .await
                .unwrap()
            }
        };
        assert_eq!(
            heard_by_guest("Mara").await,
            1,
            "the guest heard the first reply"
        );
        assert_eq!(
            heard_by_guest("Toma").await,
            0,
            "the removed guest was given the second reply"
        );
        assert_eq!(failed_sequences(&scene).await, Vec::<String>::new());
    }

    #[tokio::test]
    async fn merging_groups_drops_replies_chosen_for_the_group_that_no_longer_exists() {
        let scene = speech_scene().await;
        let speaker = scene.names[0].clone();
        let id_of = |name: &'static str| {
            let pool = scene.store.pool();
            async move {
                sqlx::query_scalar::<_, i64>("SELECT id FROM character WHERE name = ?")
                    .bind(name)
                    .fetch_one(&pool)
                    .await
                    .unwrap()
            }
        };
        let (mara, toma) = (id_of("Mara").await, id_of("Toma").await);
        let call = |name: &str, arguments: serde_json::Value| ToolCall {
            id: name.into(),
            name: name.into(),
            arguments: arguments.to_string(),
        };
        // The second player parts from everyone else.
        scene.responses.lock().unwrap().extend([
            tool_call("attempt", serde_json::json!({"kind": "look", "description": "the room"})),
            response(Content::ToolCalls(vec![
                call("approve_action", serde_json::json!({"action_id": 1})),
                call(
                    "split_group",
                    serde_json::json!({
                        "partitions": [
                            {"character_ids": [scene.first.character_id, mara, toma], "name": "hut"},
                            {"character_ids": [scene.second.character_id], "name": "yard"},
                        ],
                    }),
                ),
            ])),
            narrate("The group parts."),
            response(Content::Text(String::new())),
            // Mara and Toma, left in the hut, hear the narration.
            do_nothing(),
            do_nothing(),
        ]);
        scene
            .game
            .player_message(scene.first.clone(), "Look around.")
            .await
            .unwrap();
        wait_for_npc_work(&scene).await;
        let groups: Vec<i64> =
            sqlx::query_scalar("SELECT id FROM \"group\" WHERE retired_at IS NULL ORDER BY id")
                .fetch_all(&scene.store.pool())
                .await
                .unwrap();
        assert_eq!(groups.len(), 2);

        // Tied replies in the hut; between them the other player merges the groups.
        let (reached, release) = (
            Arc::new(tokio::sync::Notify::new()),
            Arc::new(tokio::sync::Notify::new()),
        );
        // Four requests for the speech turn, two decisions, then the GM turn
        // narrating Mara's approved reply.
        let held = scene.requests.lock().unwrap().len() + 8;
        *scene.gate.lock().unwrap() = Some(Gate {
            request: held,
            reached: Arc::clone(&reached),
            release: Arc::clone(&release),
        });
        scene.responses.lock().unwrap().extend([
            tool_call(
                "speak",
                serde_json::json!({"words": "Where is the flour?", "to": ["Toma"]}),
            ),
            approve(2),
            narrate("Toma looks up."),
            response(Content::Text(String::new())),
            propose("Ask the miller.", &speaker, 2),
            propose("It is in the hut.", &speaker, 2),
            approve(3),
            narrate("Mara speaks."),
            tool_call(
                "attempt",
                serde_json::json!({"kind": "look", "description": "the yard"}),
            ),
            response(Content::ToolCalls(vec![
                call("approve_action", serde_json::json!({"action_id": 4})),
                call(
                    "merge_groups",
                    serde_json::json!({"group_ids": groups, "name": "everyone"}),
                ),
            ])),
            narrate("They come together."),
            response(Content::Text(String::new())),
            // Mara and Toma hear the narration.
            do_nothing(),
            do_nothing(),
        ]);
        scene
            .game
            .player_message(scene.first.clone(), "Ask Toma about the flour.")
            .await
            .unwrap();
        reached.notified().await;
        let merging = {
            let game = Arc::clone(&scene.game);
            let member = scene.second.clone();
            tokio::spawn(async move { game.player_message(member, "Look outside.").await })
        };
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        release.notify_one();
        merging.await.unwrap().unwrap();
        wait_for_npc_work(&scene).await;

        let chat = heard(&scene.store, &scene.first).await;
        assert!(chat.contains(&"Ask the miller.".to_string()), "{chat:?}");
        assert!(
            !chat.iter().any(|line| line.contains("It is in the hut.")),
            "a reply chosen for the group that no longer exists was said: {chat:?}"
        );
        assert_eq!(failed_sequences(&scene).await, Vec::<String>::new());
    }

    #[tokio::test]
    async fn a_discussion_interrupted_during_an_approval_fails_visibly_and_never_replays() {
        let scene = speech_scene().await;
        let speaker = scene.names[0].clone();
        scene.hang_when_empty.store(true, Ordering::SeqCst);
        scene
            .responses
            .lock()
            .unwrap()
            .extend(say_to_toma("Where is the flour?"));
        scene.responses.lock().unwrap().extend([
            propose("Ask the miller.", &speaker, 2),
            propose("It is in the hut.", &speaker, 2),
        ]);
        scene
            .game
            .player_message(scene.first.clone(), "Ask Toma about the flour.")
            .await
            .unwrap();
        // Four turn requests and two decisions, then the GM is asked to approve.
        for _ in 0..200 {
            if scene.requests.lock().unwrap().len() > 6 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
        assert_eq!(requests_made(&scene), 7, "the approval is in flight");

        scene.store.close_interrupted_turns().await.unwrap();

        let (state, pending): (String, i64) = sqlx::query_as(
            "SELECT (SELECT state FROM npc_discussion), (SELECT COUNT(*) FROM pending_action)",
        )
        .fetch_one(&scene.store.pool())
        .await
        .unwrap();
        assert_eq!(state, "failed");
        assert_eq!(
            pending, 0,
            "the interrupted approval leaves nothing to approve later"
        );
        assert!(
            scene
                .store
                .next_npc_step(scene.first.world_id, &[])
                .await
                .unwrap()
                .is_none(),
            "an interrupted step is never replayed"
        );
        let spoken = heard(&scene.store, &scene.first).await;
        assert!(
            !spoken.iter().any(|line| line.contains("Ask the miller.")),
            "an unapproved reply was said: {spoken:?}"
        );
        let shown = scene.store.player_chat(&scene.first).await.unwrap();
        assert!(shown.iter().any(|entry| entry.role == Role::System));
    }

    #[tokio::test]
    async fn speech_in_one_group_gives_the_other_group_no_opportunity() {
        let scene = speech_scene().await;
        let id_of = |name: &'static str| {
            let pool = scene.store.pool();
            async move {
                sqlx::query_scalar::<_, i64>("SELECT id FROM character WHERE name = ?")
                    .bind(name)
                    .fetch_one(&pool)
                    .await
                    .unwrap()
            }
        };
        let (mara, toma) = (id_of("Mara").await, id_of("Toma").await);
        // Split first: the first player stays with Mara, the second with Toma.
        scene.responses.lock().unwrap().extend([
            tool_call(
                "attempt",
                serde_json::json!({"kind": "look", "description": "the room"}),
            ),
            response(Content::ToolCalls(vec![
                ToolCall {
                    id: "approve".into(),
                    name: "approve_action".into(),
                    arguments: serde_json::json!({"action_id": 1}).to_string(),
                },
                ToolCall {
                    id: "split".into(),
                    name: "split_group".into(),
                    arguments: serde_json::json!({
                        "partitions": [
                            {"character_ids": [scene.first.character_id, mara], "name": "outside"},
                            {"character_ids": [scene.second.character_id, toma], "name": "inside"},
                        ],
                    })
                    .to_string(),
                },
            ])),
            narrate("The group parts."),
            response(Content::Text(String::new())),
        ]);
        scene
            .game
            .player_message(scene.first.clone(), "Look around.")
            .await
            .unwrap();
        // Speaking to Mara outside gives only Mara an opportunity.
        scene.responses.lock().unwrap().extend([
            tool_call(
                "speak",
                serde_json::json!({"words": "Is Toma well?", "to": ["Mara"]}),
            ),
            approve(2),
            narrate("Mara listens."),
            response(Content::Text(String::new())),
            do_nothing(),
        ]);
        let before = scene.requests.lock().unwrap().len();
        scene
            .game
            .player_message(scene.first.clone(), "Ask Mara about Toma.")
            .await
            .unwrap();
        wait_for_npc_work(&scene).await;

        assert_eq!(
            scene.requests.lock().unwrap().len() - before,
            5,
            "four turn inferences and one decision, only Mara's"
        );
        let toma_heard = heard_by_npc(&scene.store, "Toma").await;
        assert!(toma_heard.is_empty(), "{toma_heard:?}");
        assert_eq!(failed_sequences(&scene).await, Vec::<String>::new());
    }

    #[tokio::test]
    async fn a_rejected_gm_request_is_compacted_and_the_turn_continues_with_valid_history() {
        // The cut between summarised and raw messages can fall anywhere,
        // including between a tool call and its result.
        for keep_raw_messages in [1, 2, 4, 7] {
            overflowing_gm_history_recovers(keep_raw_messages).await;
        }
    }

    async fn overflowing_gm_history_recovers(keep_raw_messages: usize) {
        let (backend, requests) = ScriptedBackend::recording([]);
        let responses = Arc::clone(&backend.responses);
        let fixed_context = Arc::clone(&backend.fixed_context);
        let limits = Limits {
            keep_raw_messages,
            ..Limits::default()
        };
        let (game, store, _db, first, _second) =
            two_player_bread_thief_game_with_limits(backend, limits).await;
        fixed_context.max_chars.store(5400, Ordering::SeqCst);

        let narration =
            "The words hang in the smoky air of the hut while everyone waits. ".repeat(8);
        let mut gm_branch = 0;
        for index in 0..8 {
            let sequence = store
                .begin_sequence(first.world_id, "capacity scenario")
                .await
                .unwrap();
            let arguments =
                serde_json::json!({"words": format!("Line {index}."), "to": ["Toma"]}).to_string();
            let requesting = crate::store::record_tool_call_for_test(
                &store,
                store.agent_branch(first.agent_id).await.unwrap(),
                Some(sequence.id),
                &format!("say-{index}"),
                "speak",
                &arguments,
            )
            .await;
            let pending = store
                .create_pending_action(
                    &store.player_actor(&first).await.unwrap(),
                    sequence.id,
                    requesting,
                    "speak",
                    &arguments,
                )
                .await
                .unwrap();
            gm_branch = pending.gm_branch_id;
            responses.lock().unwrap().extend([
                tool_call(
                    "approve_action",
                    serde_json::json!({"action_id": pending.id}),
                ),
                narrate(&narration),
            ]);
            // Recovery that stops making progress loops forever; fail instead.
            tokio::time::timeout(
                std::time::Duration::from_secs(30),
                game.arbitrate(
                    pending,
                    Budget::new(limits),
                    CallContext::root(Some(sequence.id)),
                    None,
                ),
            )
            .await
            .unwrap_or_else(|_| panic!("action {index} did not finish: recovery made no progress"))
            .unwrap_or_else(|error| panic!("action {index} did not resolve: {error:#}"));
        }

        assert!(
            fixed_context.rejections.load(Ordering::SeqCst) >= 1,
            "the scenario must overflow the context at least once"
        );
        let summary = store.latest_summary_for_branch(gm_branch).await.unwrap();
        assert!(
            summary.is_some_and(|summary| summary.reason == "capacity_recovery"),
            "the overflow was recovered by compaction"
        );
        // The compaction names the request that did not fit as its cause.
        let cause_error: Option<String> = sqlx::query_scalar(
            "SELECT cause.error FROM summary              JOIN inference ON inference.id = summary.inference_id              JOIN invocation ON invocation.id = inference.invocation_id              JOIN inference AS cause ON cause.id = invocation.cause_inference_id              WHERE summary.reason = 'capacity_recovery' ORDER BY summary.id DESC LIMIT 1",
        )
        .fetch_optional(&store.pool())
        .await
        .unwrap()
        .flatten();
        assert!(
            cause_error.is_some_and(|error| error.contains("fixed context capacity")),
            "the summary does not point at the rejected request"
        );
        // Every request the GM sent, accepted or not, kept each tool result
        // after the call that produced it.
        for request in requests.lock().unwrap().iter() {
            let mut open_calls: Vec<String> = Vec::new();
            for message in &request.messages {
                match &message.content {
                    MessageContent::ToolCalls(calls) => {
                        open_calls = calls.iter().map(|call| call.id.clone()).collect();
                    }
                    MessageContent::ToolResult { tool_call_id, .. } => {
                        assert!(
                            open_calls.contains(tool_call_id),
                            "a tool result lost its call: {tool_call_id} in {:?}",
                            request.messages
                        );
                    }
                    _ => {}
                }
            }
        }
    }

    #[tokio::test]
    async fn when_every_npc_stays_silent_the_gm_is_never_asked() {
        let scene = speech_scene().await;
        talk(&scene, [do_nothing(), do_nothing()]).await;

        assert_eq!(
            scene.requests.lock().unwrap().len(),
            6,
            "four turn inferences plus one decision for each NPC, nothing more"
        );
        assert_eq!(failed_sequences(&scene).await, Vec::<String>::new());
    }

    #[tokio::test]
    async fn a_malformed_decision_fails_the_discussion_visibly_and_play_continues() {
        let scene = speech_scene().await;
        let speaker = scene.names[0].clone();
        talk(
            &scene,
            [
                do_nothing(),
                // Eagerness outside 1 to 3.
                tool_call(
                    "speak",
                    serde_json::json!({
                        "words": "Hi.", "to": [speaker],
                        "eagerness": 4,
                    }),
                ),
            ],
        )
        .await;

        let errors = failed_sequences(&scene).await;
        assert!(
            errors.iter().any(|error| error.contains("eagerness 4")),
            "{errors:?}"
        );
        let shown: Vec<String> = scene
            .store
            .player_chat(&scene.first)
            .await
            .unwrap()
            .into_iter()
            .filter(|entry| entry.role == Role::System)
            .map(|entry| entry.text)
            .collect();
        assert!(
            shown.iter().any(|text| text.contains("eagerness 4")),
            "the player is shown the failure where they spoke: {shown:?}"
        );
        // No further model work followed, and the world still accepts input.
        assert_eq!(scene.requests.lock().unwrap().len(), 6);
        scene
            .responses
            .lock()
            .unwrap()
            .extend([response(Content::Text("Hello again.".into()))]);
        scene
            .game
            .player_message(scene.second.clone(), "Hello?")
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn a_failure_is_shown_after_what_happened_before_it_not_under_its_trigger() {
        let scene = speech_scene().await;
        let speaker = scene.names[0].clone();
        talk(
            &scene,
            [
                do_nothing(),
                propose("It is in the hut.", &speaker, 2),
                approve(2),
                narrate("Toma answers."),
                // The other NPC then reconsiders, and its answer is malformed.
                tool_call(
                    "speak",
                    serde_json::json!({"words": "Hm.", "to": [speaker], "eagerness": 4}),
                ),
            ],
        )
        .await;

        let chat = scene.store.player_chat(&scene.first).await.unwrap();
        let position = |wanted: &dyn Fn(&crate::store::PlayerChatEntry) -> bool| {
            chat.iter().position(wanted).expect("entry is in the chat")
        };
        let said = position(&|entry| entry.text.contains("It is in the hut."));
        let failed = position(&|entry| entry.role == Role::System);
        assert!(
            failed > said,
            "the error follows the reply it came after: {chat:?}"
        );
        // Resuming from the last message still brings the error, once.
        let later = scene
            .store
            .player_chat_after(&scene.first, Some(chat[said].id - 1))
            .await
            .unwrap();
        assert_eq!(
            later
                .iter()
                .filter(|entry| entry.role == Role::System)
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn only_approved_replies_are_published_and_a_rejection_is_final() {
        let scene = speech_scene().await;
        let speaker = scene.names[0].clone();
        talk(
            &scene,
            [
                propose("Ask the miller.", &speaker, 2),
                propose("It is in the hut.", &speaker, 2),
                tool_call(
                    "reject_action",
                    serde_json::json!({"action_id": 2, "reason": "Mara is gagged."}),
                ),
                narrate("Mara strains against the gag."),
                approve(3),
                narrate("Only Toma speaks."),
                // Mara reconsiders after hearing Toma.
                do_nothing(),
            ],
        )
        .await;

        let chat = heard(&scene.store, &scene.first).await;
        assert!(chat.iter().any(|line| line.contains("It is in the hut.")));
        assert!(
            !chat.iter().any(|line| line.contains("Ask the miller.")),
            "a rejected reply publishes nothing and is not retried: {chat:?}"
        );
    }

    #[tokio::test]
    async fn approving_a_different_action_is_returned_to_the_gm_and_nothing_is_published() {
        let scene = speech_scene().await;
        let speaker = scene.names[0].clone();
        talk(
            &scene,
            [
                do_nothing(),
                propose("It is in the hut.", &speaker, 2),
                // Not the presented action.
                approve(99),
                approve(2),
                narrate("Toma answers."),
                do_nothing(),
            ],
        )
        .await;

        let refused = scene.requests.lock().unwrap().iter().any(|request| {
            tool_results(request)
                .iter()
                .any(|result| result.contains("was presented action 2"))
        });
        assert!(refused, "the GM saw why its first approval was refused");
        let chat = heard(&scene.store, &scene.first).await;
        assert_eq!(
            chat.iter()
                .filter(|line| line.contains("It is in the hut."))
                .count(),
            1,
            "the reply was published once: {chat:?}"
        );
    }

    #[tokio::test]
    async fn a_whisper_gives_only_its_listeners_an_opportunity() {
        let scene = speech_scene().await;
        scene.responses.lock().unwrap().extend([
            tool_call(
                "speak",
                serde_json::json!({"private": true, "words": "Keep this quiet.", "to": ["Toma"]}),
            ),
            tool_call("approve_action", serde_json::json!({"action_id": 1})),
            narrate("Toma leans in."),
            response(Content::Text(String::new())),
            // Only Toma heard it, so only Toma decides.
            do_nothing(),
        ]);
        scene
            .game
            .player_message(scene.first.clone(), "Whisper to Toma.")
            .await
            .unwrap();
        wait_for_npc_work(&scene).await;

        assert_eq!(scene.requests.lock().unwrap().len(), 5);
        let candidates: i64 =
            sqlx::query_scalar("SELECT COUNT(DISTINCT character_id) FROM npc_candidate")
                .fetch_one(&scene.store.pool())
                .await
                .unwrap();
        assert_eq!(candidates, 1, "Mara did not hear it and is not asked");
    }

    #[tokio::test]
    async fn speech_nobody_among_the_npcs_heard_starts_no_discussion() {
        let scene = speech_scene().await;
        scene.responses.lock().unwrap().extend([
            tool_call(
                "speak",
                serde_json::json!({"private": true, "words": "Psst.", "to": [scene.names[1]]}),
            ),
            tool_call("approve_action", serde_json::json!({"action_id": 1})),
            narrate("A quiet word."),
            response(Content::Text(String::new())),
        ]);
        scene
            .game
            .player_message(scene.first.clone(), "Whisper to the other player.")
            .await
            .unwrap();

        let collections: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM npc_response_collection")
            .fetch_one(&scene.store.pool())
            .await
            .unwrap();
        assert_eq!(collections, 0);
        assert_eq!(scene.requests.lock().unwrap().len(), 4);
    }

    #[tokio::test]
    async fn new_human_speech_supersedes_unanswered_work_and_is_answered_together() {
        let scene = speech_scene().await;
        let (first_name, second_name) = (scene.names[0].clone(), scene.names[1].clone());
        // Both players speak before any NPC runs: events queue ahead of NPC steps.
        scene
            .responses
            .lock()
            .unwrap()
            .extend(say_to_toma("First point."));
        scene.responses.lock().unwrap().extend([
            tool_call(
                "speak",
                serde_json::json!({"words": "Second point.", "to": ["Toma"]}),
            ),
            tool_call("approve_action", serde_json::json!({"action_id": 2})),
            narrate("Toma listens."),
            response(Content::Text(String::new())),
            // One decision each for Mara and Toma, about both things said.
            do_nothing(),
            propose("Fine, I will answer.", &first_name, 1),
            approve(3),
            narrate("Toma answers."),
            do_nothing(),
        ]);
        let first = {
            let game = Arc::clone(&scene.game);
            let member = scene.first.clone();
            tokio::spawn(async move { game.player_message(member, "Point one.").await })
        };
        let second = {
            let game = Arc::clone(&scene.game);
            let member = scene.second.clone();
            tokio::spawn(async move { game.player_message(member, "Point two.").await })
        };
        first.await.unwrap().unwrap();
        second.await.unwrap().unwrap();
        wait_for_npc_work(&scene).await;

        let _ = second_name;
        let decisions: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM npc_candidate WHERE decision_invocation_id IS NOT NULL",
        )
        .fetch_one(&scene.store.pool())
        .await
        .unwrap();
        // Mara and Toma decided once about both points, then Mara once more
        // after hearing Toma: not a decision per point per NPC.
        assert_eq!(decisions, 3, "stale work was superseded, not played");
        assert!(
            heard(&scene.store, &scene.second)
                .await
                .iter()
                .any(|line| line.contains("Fine, I will answer."))
        );
        assert_eq!(failed_sequences(&scene).await, Vec::<String>::new());
    }

    #[tokio::test]
    async fn an_exhausted_budget_fails_the_discussion_without_a_fresh_allowance() {
        let scene = speech_scene_with_limits(Limits {
            max_inferences_total: 6,
            ..Limits::default()
        })
        .await;
        let speaker = scene.names[0].clone();
        talk(
            &scene,
            [
                propose("Ask the miller.", &speaker, 2),
                propose("It is in the hut.", &speaker, 2),
                // The GM's judging would be the seventh inference.
            ],
        )
        .await;

        let errors = failed_sequences(&scene).await;
        assert!(
            errors
                .iter()
                .any(|error| error.contains("max_inferences_total")),
            "the limit is reported to the players: {errors:?}"
        );
        assert!(
            !heard(&scene.store, &scene.first)
                .await
                .iter()
                .any(|line| line.contains("Ask the miller.") || line.contains("It is in the hut.")),
            "no dropped-then-replayed reply"
        );
    }

    #[tokio::test]
    async fn a_discussion_interrupted_mid_decision_fails_visibly_on_restart() {
        let scene = speech_scene().await;
        scene.hang_when_empty.store(true, Ordering::SeqCst);
        // The player's turn completes; the first NPC decision never returns.
        scene
            .responses
            .lock()
            .unwrap()
            .extend(say_to_toma("Where is the flour?"));
        scene
            .game
            .player_message(scene.first.clone(), "Ask Toma about the flour.")
            .await
            .unwrap();
        for _ in 0..200 {
            if scene.requests.lock().unwrap().len() > 4 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
        assert_eq!(
            scene.requests.lock().unwrap().len(),
            5,
            "a decision is in flight"
        );

        scene.store.close_interrupted_turns().await.unwrap();

        let state: String = sqlx::query_scalar("SELECT state FROM npc_discussion")
            .fetch_one(&scene.store.pool())
            .await
            .unwrap();
        assert_eq!(state, "failed");
        assert!(
            scene
                .store
                .next_npc_step(scene.first.world_id, &[])
                .await
                .unwrap()
                .is_none(),
            "an interrupted step is never replayed"
        );
        let shown = scene.store.player_chat(&scene.first).await.unwrap();
        assert!(
            shown.iter().any(|entry| entry.role == Role::System),
            "the player is told the conversation was interrupted"
        );
    }

    #[tokio::test]
    async fn heard_speech_keeps_its_speaker_name_after_a_rename() {
        let scene = speech_scene().await;
        scene
            .speak(
                tool_call("speak", serde_json::json!({"words": "Good morning."})),
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
            ["Good morning.".to_string()]
        );
        // The chat carries who said it as structure, under the name used then.
        let speakers: Vec<_> = scene
            .store
            .player_chat(&scene.second)
            .await
            .unwrap()
            .into_iter()
            .filter(|entry| entry.role == Role::Speech)
            .map(|entry| entry.speaker)
            .collect();
        assert_eq!(speakers, [Some(scene.names[0].clone())]);
    }

    #[tokio::test]
    async fn an_npc_is_shown_authored_speech_with_its_speaker_as_structure() {
        let scene = speech_scene().await;
        let speaker = scene.names[0].clone();
        talk(&scene, [do_nothing(), do_nothing()]).await;

        // Toma's decision request holds the player's words as authored, with
        // the source alongside, though the words name nobody.
        let requests = scene.requests.lock().unwrap();
        let heard = requests
            .iter()
            .flat_map(|request| &request.messages)
            .find_map(|message| match &message.content {
                MessageContent::Speech(speech) => Some(speech.clone()),
                _ => None,
            })
            .expect("an NPC was shown the speech");
        assert_eq!(heard.text, "Where is the flour?");
        assert_eq!(heard.speaker, speaker);
        assert_eq!(heard.addressee, None);
        assert_eq!(heard.kind, "say");
    }

    #[tokio::test]
    async fn speech_recorded_before_authored_text_keeps_its_fixed_wording() {
        let scene = speech_scene().await;
        scene
            .speak(
                tool_call(
                    "speak",
                    serde_json::json!({"words": "Good morning.", "to": ["Toma"]}),
                ),
                serde_json::json!({"action_id": 1}),
            )
            .await;
        // Older records stored who was addressed; new speech does not.
        sqlx::query(
            "UPDATE speech SET text_format = 'legacy', addressed_name = 'Toma', \
             addressed_character_id = (SELECT id FROM character WHERE name = 'Toma')",
        )
        .execute(&scene.store.pool())
        .await
        .unwrap();

        let expected = format!("{} says to Toma: Good morning.", scene.names[0]);
        assert_eq!(
            heard(&scene.store, &scene.second).await,
            std::slice::from_ref(&expected)
        );
        assert_eq!(heard_by_npc(&scene.store, "Toma").await, [expected]);
    }

    #[tokio::test]
    async fn speech_to_someone_who_is_not_here_is_refused_before_anything_is_submitted() {
        let scene = speech_scene().await;
        scene.responses.lock().unwrap().extend([
            tool_call(
                "speak",
                serde_json::json!({"private": true, "words": "Psst.", "to": ["Nobody"]}),
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
        // Mara and Toma decide about the arrival they are told of.
        let (backend, _) =
            ScriptedBackend::recording([narrate("Both arrive."), do_nothing(), do_nothing()]);
        let responses = Arc::clone(&backend.responses);
        let (game, store, _db, first, second) =
            two_player_bread_thief_game_with_deadline(backend, 3600).await;
        let first_connection = game.connect(&first).await.unwrap();
        let second_connection = game.connect(&second).await.unwrap();
        deliver_presence_deadline(&game, &store, &first).await;
        wait_for_npc_work_of(&game, &store, &first).await;
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
                name: "attempt".into(),
                arguments: r#"{"kind":"look","description":"room"}"#.into(),
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
                        "partitions": [
                            {"character_ids":members, "name":"players"},
                            {"character_ids":[toma], "name":"Toma"}
                        ]
                    })
                    .to_string(),
                },
            ])),
            narrate("Split narration."),
            response(Content::Text("Done.".into())),
            // Mara, with the players, hears the narration.
            do_nothing(),
        ]);
        game.player_message(first.clone(), "Look and split.")
            .await
            .unwrap();
        wait_for_npc_work_of(&game, &store, &first).await;
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
                name: "attempt".into(),
                arguments: r#"{"kind":"look","description":"room"}"#.into(),
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
                        "group_ids":[players,alone], "name":"reunited"
                    })
                    .to_string(),
                },
            ])),
            narrate("Merge narration."),
            response(Content::Text("Done.".into())),
            // Mara and Toma hear the narration.
            do_nothing(),
            do_nothing(),
        ]);
        game.player_message(first.clone(), "Look and merge.")
            .await
            .unwrap();
        wait_for_npc_work_of(&game, &store, &first).await;
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
                name: "attempt".into(),
                arguments: r#"{"kind":"look","description":"room"}"#.into(),
            }])),
            response(Content::ToolCalls(vec![ToolCall {
                id: "approve-1".into(),
                name: "approve_action".into(),
                arguments: r#"{"action_id":1}"#.into(),
            }])),
            narrate("Atomic delivery probe."),
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
        two_player_bread_thief_game_with_limits(
            backend,
            Limits {
                presence_departure_seconds,
                ..Limits::default()
            },
        )
        .await
    }

    async fn two_player_bread_thief_game_with_limits<B>(
        backend: B,
        limits: Limits,
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
        two_player_bread_thief_game_with_rules(backend, limits, GameSettings::default()).await
    }

    async fn two_player_bread_thief_game_with_rules<B>(
        backend: B,
        limits: Limits,
        rules: GameSettings,
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
            "/testdata/hut_scenario.json"
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
        let scheduler = InferenceScheduler::new(backend, limits).unwrap();
        let game = Game::with_rules(
            store.clone(),
            World::load(&store).await.unwrap(),
            scheduler.foreground(),
            limits,
            "scripted".into(),
            Sampling {
                temperature: 0.0,
                ..Default::default()
            },
            rules,
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
            ScriptedBackend::recording([narrate("Someone arrives.")]);
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
            // Mara and Toma decide about each narration they are told of.
            let (backend, requests) = ScriptedBackend::recording([
                narrate("Both arrive."),
                do_nothing(),
                do_nothing(),
                narrate("One returns, one remains away."),
                do_nothing(),
                do_nothing(),
            ]);
            let (game, store, db, first, second) =
                two_player_bread_thief_game_with_deadline(backend, 3600).await;
            let connection = game.connect(&first).await.unwrap();
            game.connect(&second).await.unwrap();
            deliver_presence_deadline(&game, &store, &first).await;
            wait_for_npc_work_of(&game, &store, &first).await;
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
                gm_requests(&requests),
                1,
                "future windows wait; empty expired windows make no model call"
            );
            if returning > 0 {
                deliver_presence_deadline(&game, &store, &first).await;
                wait_for_npc_work_of(&game, &store, &first).await;
            }
            let settled = store.presence_narration(group).await.unwrap();
            assert!(settled.due_at.is_none());
            assert_eq!(settled.waiting_for_return, returning == 0);
            assert_eq!(gm_requests(&requests), if returning == 1 { 2 } else { 1 });
            // A consumed generation cannot deliver a second narration.
            game.handle_presence_deadline(group, recovered.generation)
                .await
                .unwrap();
            assert_eq!(gm_requests(&requests), if returning == 1 { 2 } else { 1 });
        }
    }

    #[tokio::test]
    async fn failed_presence_delivery_preserves_snapshot_and_reports_once() {
        let (backend, requests) = ScriptedBackend::recording([narrate("Undelivered arrival.")]);
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
            narrate("Someone arrives."),
            narrate("Someone else arrives."),
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
            narrate("Someone arrives."),
            narrate("Someone else arrives."),
            narrate("Someone leaves."),
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
            narrate("Someone arrives."),
            narrate("Someone else arrives."),
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
        // Mara and Toma each decide about every arrival they are told of.
        let (backend, recorded_requests) = ScriptedBackend::recording([
            narrate("Someone arrives."),
            do_nothing(),
            do_nothing(),
            narrate("Someone else arrives."),
            do_nothing(),
            do_nothing(),
        ]);
        let (game, store, _db, first, second) =
            two_player_bread_thief_game_with_deadline(backend, 60).await;
        let first_connection = game.connect(&first).await.unwrap();
        deliver_presence_deadline(&game, &store, &first).await;
        wait_for_npc_work_of(&game, &store, &first).await;
        let told: i64 = sqlx::query_scalar(
            "SELECT COUNT(DISTINCT character_id) FROM npc_candidate WHERE decision_invocation_id IS NOT NULL",
        )
        .fetch_one(&store.pool())
        .await
        .unwrap();
        assert_eq!(
            told, 2,
            "an arrival is narrated to the NPCs, who may respond"
        );
        let second_connection = game.connect(&second).await.unwrap();
        let_presence_handlers_run(&game, &first).await;
        wait_for_npc_work_of(&game, &store, &first).await;
        assert_eq!(gm_requests(&recorded_requests), 2);

        game.disconnect(&first, first_connection).await.unwrap();
        game.disconnect(&second, second_connection).await.unwrap();
        deliver_presence_deadline(&game, &store, &first).await;
        assert_eq!(
            gm_requests(&recorded_requests),
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
            gm_requests(&recorded_requests),
            2,
            "the same returning group still waits through its window"
        );
        deliver_presence_deadline(&game, &store, &first).await;
        assert_eq!(
            gm_requests(&recorded_requests),
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
        for case in 0..4 {
            let (backend, requests) = ScriptedBackend::recording([]);
            let responses = Arc::clone(&backend.responses);
            let (game, store, _db, member, _) = two_player_bread_thief_game(backend).await;
            let owner = store
                .find_or_create_user("other@example.test", "Other")
                .await
                .unwrap();
            let mut scenario = Scenario::read(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/testdata/hut_scenario.json"
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
                0 => (
                    "split_group",
                    serde_json::json!({
                        "partitions": [{"character_ids": characters, "name": "Changed"}],
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
                        "name": "Changed",
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
                    name: "attempt".into(),
                    arguments: r#"{"kind":"look","description":"Look around."}"#.into(),
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
                narrate("The scene is unchanged."),
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
                // The invalid change, its approval sibling, then the narration.
                ["rejected", "completed", "completed"],
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
            "partitions": [
                {"character_ids": companions, "name": "first group"},
                {"character_ids": [second.character_id], "name": "second group"}
            ]
        })
        .to_string();
        script.lock().unwrap().extend([
            response(Content::ToolCalls(vec![ToolCall {
                id: "look-1".into(),
                name: "attempt".into(),
                arguments: r#"{"kind":"look","description":"the room"}"#.into(),
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
            narrate("The group splits up."),
            response(Content::Text(String::new())),
            // Mara and Toma, left with the first group, hear it.
            do_nothing(),
            do_nothing(),
            response(Content::ToolCalls(vec![ToolCall {
                id: "look-2".into(),
                name: "attempt".into(),
                arguments: r#"{"kind":"look","description":"the room"}"#.into(),
            }])),
            response(Content::ToolCalls(vec![ToolCall {
                id: "approve-2".into(),
                name: "approve_action".into(),
                arguments: r#"{"action_id":2}"#.into(),
            }])),
            narrate("Only the first group should see this."),
            response(Content::Text(String::new())),
            do_nothing(),
            do_nothing(),
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
        wait_for_npc_work_of(&game, &store, &first).await;
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

    async fn real_model_opening_reaches_a_stored_player_reply(model: &DeviceModel) {
        // This is deliberately the same server-owned opening path the web
        // socket awaits, rather than a direct backend request. A successful
        // return proves the agent settled and its player-visible reply
        // committed to stored history.
        let (backend, model_path, sampling, limits) = model.parts();
        let observed_prompts = model.observed();
        let (game, store, _db, member) =
            opening_game_with_limits(backend, model_path, sampling, limits).await;

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
        assert_no_rejected_output(&store).await;
        game.shutdown().await.unwrap();
    }

    /// Character creation finishing and one action after it, each asked once: the
    /// model must call ready_to_begin when the rolls are done, and a take must
    /// reach the GM, be approved and move the item.
    async fn real_model_finishes_creation_and_the_gm_resolves_a_take(model: &DeviceModel) {
        let (backend, model_path, configured, limits) = model.parts();
        // Greedy: this asserts the model follows specific instructions, so it
        // must not depend on a sampling draw.
        let sampling = Sampling {
            temperature: 0.0,
            ..configured
        };
        let (game, store, _db, member) =
            opening_game_with_limits(backend, model_path, sampling, limits).await;
        // A real player is connected before ever chatting; narration recipients
        // are connected PCs (design.md's Membership and presence).
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

        game.player_message(
            member.clone(),
            "The required rolls are complete. Begin play now.",
        )
        .await
        .expect("the creation turn");
        assert!(
            store.is_ready_to_begin(&member).await.unwrap(),
            "the model did not call ready_to_begin once the rolls were done"
        );
        assert!(
            store
                .player_chat(&member)
                .await
                .unwrap()
                .iter()
                .any(|entry| entry.role == Role::Narration),
            "the GM opening narration must become player-visible"
        );

        game.player_message(member.clone(), "I pick up the flour sack.")
            .await
            .expect("the take turn");
        // The flour sack is Toma's, so a GM may fairly refuse the take. What the
        // real model must do is resolve the action with a tool, and Rust moves
        // the item exactly when the GM approved (the transfer itself is covered
        // by the scripted take test).
        let approved: bool =
            sqlx::query_scalar("SELECT approved FROM action ORDER BY id DESC LIMIT 1")
                .fetch_one(&store.pool())
                .await
                .expect("the GM never resolved the take");
        let held: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM item_character")
            .fetch_one(&store.pool())
            .await
            .unwrap();
        assert_eq!(
            held,
            i64::from(approved),
            "the item did not follow the GM's decision"
        );
        assert_no_rejected_output(&store).await;
        game.shutdown().await.unwrap();
    }

    /// A turn interrupted after its roll committed: after restart cleanup the
    /// next real chat turn must accept the closed tool history and the model
    /// must be able to use the recorded outcome it is shown.
    async fn real_model_chats_normally_after_an_interrupted_roll(model: &DeviceModel) {
        let (backend, model_path, configured, limits) = model.parts();
        let sampling = Sampling {
            temperature: 0.0,
            ..configured
        };
        let (game, store, _db, member) =
            opening_game_with_limits(backend, model_path, sampling, limits).await;
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
        assert_no_rejected_output(&store).await;
        game.shutdown().await.unwrap();
    }

    /// A question put to an NPC reaches it, the NPC is asked for its own
    /// response, and that response reaches the player, with the discussion
    /// finishing cleanly. A run in which nobody answers fails: the scripted
    /// tests cover an NPC choosing to stay silent.
    /// A scripted player puts a question to Toma through `player_message`; the
    /// NPCs are the real model and the GM is scripted (it has its own real-model
    /// scenario). Delivery of the words is scripted too (other tests cover it). The scenario is one where answering makes sense,
    /// and the player must hear an NPC's reply: NPCs only whispering to each
    /// other fails. This is one behavioural sample, not a guarantee, and the
    /// conversation is cut off at the first reply that reaches the player.
    async fn real_model_npc_answers_a_question_put_to_it(model: &DeviceModel) {
        let (_, _model_path, _sampling, limits) = model.parts();
        let hybrid = RealNpcsOnly {
            real: Arc::clone(&model.backend),
            player: Mutex::new(VecDeque::from([
                tool_call(
                    "speak",
                    serde_json::json!({"words": "Toma, where did the bread go?", "to": ["Toma"]}),
                ),
                response(Content::Text(String::new())),
            ])),
        };
        let (game, store, _db, first, second) =
            two_player_bread_thief_game_with_limits(hybrid, limits).await;
        let _first_connection = game.connect(&first).await.unwrap();
        let _second_connection = game.connect(&second).await.unwrap();

        game.player_message(first.clone(), "Ask Toma where the bread went.")
            .await
            .expect("the player's turn");
        let mut reached_player = false;
        let mut quiet_polls = 0;
        for _ in 0..1200 {
            let heard_from_npcs: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM message                  JOIN speech ON speech.action_id = message.action_id                  JOIN history_entry ON history_entry.id = message.id                  JOIN agent_history ON agent_history.branch_id = history_entry.branch_id                  WHERE agent_history.agent_id = ?                    AND speech.speaker_character_id IN (SELECT character_id FROM npc_agent)",
            )
            .bind(first.agent_id)
            .fetch_one(&store.pool())
            .await
            .unwrap();
            if heard_from_npcs > 0 {
                reached_player = true;
                break;
            }
            // The discussion ending without a reply is a definite outcome:
            // stop waiting instead of running out the clock.
            let open_collections: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM npc_response_collection WHERE state IN ('collecting', 'selected', 'executing')",
            )
            .fetch_one(&store.pool())
            .await
            .unwrap();
            quiet_polls = if open_collections == 0 {
                quiet_polls + 1
            } else {
                0
            };
            if quiet_polls > 30 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        let utterances: Vec<(String, String)> =
            sqlx::query_as("SELECT speaker_name, kind FROM speech ORDER BY action_id")
                .fetch_all(&store.pool())
                .await
                .unwrap();
        println!(
            "NPC utterances after the first reply or the wait: {utterances:?}; the player heard {:?}",
            heard(&store, &first).await
        );
        assert_no_rejected_output(&store).await;
        game.shutdown().await.unwrap();

        let actions = npc_decisions(&store).await;
        assert!(
            reached_player,
            "no NPC speech reached the player within two minutes; utterances: {utterances:?}; actions: {actions:?}"
        );
        assert!(
            !asked_the_gm(&actions),
            "an NPC answered through a question for the GM: {actions:?}"
        );
        let asked: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM npc_candidate              JOIN character ON character.id = npc_candidate.character_id              WHERE character.name = 'Toma' AND npc_candidate.decision_invocation_id IS NOT NULL",
        )
        .fetch_one(&store.pool())
        .await
        .unwrap();
        assert!(asked >= 1, "Toma was never asked whether to respond");
        let failed: i64 = sqlx::query_scalar(
            "SELECT (SELECT COUNT(*) FROM sequence WHERE error IS NOT NULL)                   + (SELECT COUNT(*) FROM npc_discussion WHERE state = 'failed')",
        )
        .fetch_one(&store.pool())
        .await
        .unwrap();
        let errors: Vec<String> =
            sqlx::query_scalar("SELECT error FROM sequence WHERE error IS NOT NULL")
                .fetch_all(&store.pool())
                .await
                .unwrap();
        assert_eq!(
            failed, 0,
            "the discussion failed ({errors:?}); utterances: {utterances:?}"
        );
    }

    /// Whether any decision was `attempt` of kind `ask`, the question for the GM.
    fn asked_the_gm(decisions: &[Content]) -> bool {
        decisions.iter().any(|decision| match decision {
            Content::ToolCalls(calls) => calls.iter().any(|call| {
                call.name == "attempt"
                    && serde_json::from_str::<serde_json::Value>(&call.arguments)
                        .is_ok_and(|arguments| arguments["kind"] == "ask")
            }),
            Content::Text(_) => false,
        })
    }

    /// What each NPC decision was, as recorded.
    async fn npc_decisions(store: &Store) -> Vec<Content> {
        let npc_inferences: Vec<i64> = sqlx::query_scalar(
            "SELECT inference.id FROM inference JOIN npc_agent ON npc_agent.agent_id = inference.agent_id ORDER BY inference.id",
        )
        .fetch_all(&store.pool())
        .await
        .unwrap();
        let mut decisions = Vec::new();
        for id in npc_inferences {
            if let crate::inspection::RecordedOutcome::Response(response) =
                store.reconstruct_inference(id).await.unwrap().outcome
            {
                decisions.push(response.content);
            }
        }
        decisions
    }

    /// The live game started with Toma calling `attempt` (kind `ask`) with his
    /// words in the description, and the GM then narrated those words as
    /// speech, which Toma kept imitating. Everything is the real model here, from
    /// character creation through the scene narration to the NPCs' first
    /// decisions.
    async fn real_model_npcs_hearing_the_scene_do_not_speak_through_attempt(model: &DeviceModel) {
        let (_, model_path, sampling, limits) = model.parts();
        let backend = RealGmAndNpcs {
            real: Arc::clone(&model.backend),
            player: Mutex::new(VecDeque::from([
                response(Content::Text("Welcome, traveler.".into())),
                response(Content::ToolCalls(vec![
                    ToolCall {
                        id: "hp".into(),
                        name: "roll_hit_protection".into(),
                        arguments: "{}".into(),
                    },
                    ToolCall {
                        id: "attributes".into(),
                        name: "roll_attributes".into(),
                        arguments: "{}".into(),
                    },
                ])),
                tool_call("ready_to_begin", serde_json::json!({})),
                response(Content::Text(String::new())),
            ])),
        };
        let (game, store, _db, member) =
            opening_game_with_limits(backend, model_path, sampling, limits).await;
        game.wait_for_opening(member.clone())
            .await
            .expect("the opening");
        game.player_message(member.clone(), "Roll my character, then begin.")
            .await
            .expect("the player's creation turn");
        // The real model is slow: wait until the NPCs have had a quiet moment.
        let mut quiet = 0;
        for _ in 0..1200 {
            let open: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM npc_response_collection WHERE state IN ('collecting', 'selected', 'executing')",
            )
            .fetch_one(&store.pool())
            .await
            .unwrap();
            quiet = if open == 0 { quiet + 1 } else { 0 };
            if quiet > 30 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        let decisions = npc_decisions(&store).await;
        assert!(
            !decisions.is_empty(),
            "the NPCs were never asked; the player's chat: {:#?}",
            store.player_chat(&member).await.unwrap()
        );
        assert!(
            !asked_the_gm(&decisions),
            "an NPC spoke through an `ask`, which is a question for the GM: {decisions:#?}"
        );
        game.shutdown().await.unwrap();
    }

    /// Run on its own, not in `device_suite`: the real GM, NPCs and creation take
    /// a minute or two.
    #[tokio::test]
    #[ignore = "needs a CUDA GPU"]
    async fn npcs_hearing_the_scene_do_not_speak_through_attempt() {
        let model = DeviceModel::load().await;
        real_model_npcs_hearing_the_scene_do_not_speak_through_attempt(&model).await;
    }

    /// The answer to a question put to an NPC is speech, not an `ask` for the GM
    /// with the words inside its description, as the live game showed.
    #[tokio::test]
    #[ignore = "needs a CUDA GPU"]
    async fn an_npc_answers_a_question_by_speaking() {
        let model = DeviceModel::load().await;
        real_model_npc_answers_a_question_put_to_it(&model).await;
    }

    /// The player agent is the real model; the GM and NPCs are mechanical.
    /// `chosen` collects the player agent's tool calls.
    struct RealPlayerOnly {
        real: Arc<MistralRsBackend>,
        chosen: Arc<Mutex<Vec<ToolCall>>>,
    }

    impl Backend for RealPlayerOnly {
        async fn complete(
            &self,
            request: crate::llm::Request,
            on_token: impl FnMut(&str) + Send,
        ) -> Result<Response> {
            if request
                .tools
                .iter()
                .any(|tool| tool.name == "party_presence")
            {
                let response = self.real.complete(request, on_token).await?;
                if let Content::ToolCalls(calls) = &response.content {
                    self.chosen.lock().unwrap().extend(calls.iter().cloned());
                }
                return Ok(response);
            }
            Ok(mechanical_gm(&request)?.unwrap_or_else(do_nothing))
        }
    }

    /// Natural requests reach the right action: each attempt kind, public and
    /// private speech, and taking. A fresh game per request keeps the context
    /// small; 5 of 5 repeats of every request matched when this was measured.
    async fn real_model_player_routes_requests_to_the_right_action(model: &DeviceModel) {
        let (_, _model_path, _sampling, limits) = model.parts();
        let requests = [
            (
                "Look at the rear window.",
                "attempt",
                Some(("kind", "look")),
            ),
            (
                "Search the floor by the fire carefully for any clues.",
                "attempt",
                Some(("kind", "investigate")),
            ),
            ("Open the rear window.", "attempt", Some(("kind", "open"))),
            (
                "Ask the GM what the hut is built from.",
                "attempt",
                Some(("kind", "ask")),
            ),
            ("Say hello to everyone.", "speak", None),
            (
                "Quietly tell Toma only that I will help him.",
                "speak",
                Some(("private", "true")),
            ),
            ("Pick up the flour sack.", "take", None),
        ];
        let mut wrong = Vec::new();
        for (line, tool, expected) in requests {
            let chosen = Arc::new(Mutex::new(Vec::new()));
            let backend = RealPlayerOnly {
                real: Arc::clone(&model.backend),
                chosen: Arc::clone(&chosen),
            };
            let (game, _store, _db, first, second) =
                two_player_bread_thief_game_with_limits(backend, limits).await;
            let _first_connection = game.connect(&first).await.unwrap();
            let _second_connection = game.connect(&second).await.unwrap();
            game.player_message(first.clone(), line)
                .await
                .expect("the player's turn");
            let first_call = chosen.lock().unwrap().first().cloned();
            let matches = first_call.as_ref().is_some_and(|call| {
                let arguments: serde_json::Value =
                    serde_json::from_str(&call.arguments).unwrap_or_default();
                call.name == tool
                    && expected.is_none_or(|(key, value)| {
                        let shown = match arguments[key].as_str() {
                            Some(text) => text.to_string(),
                            None => arguments[key].to_string(),
                        };
                        shown == value
                    })
            });
            if !matches {
                wrong.push(format!("{line:?} -> {first_call:?}"));
            }
            game.shutdown().await.unwrap();
        }
        assert!(wrong.is_empty(), "wrong routing: {wrong:#?}");
    }

    /// Whether the words a player agent spoke are the player's.
    type SaidRight = fn(&str) -> bool;

    /// A player's agent says what the player says, once, and then stops: no
    /// words of its own, no story about how others react. Each line is a fresh
    /// game; the live failure was the agent narrating Toma's reaction after its
    /// `speak` call and putting words in the character's mouth the player never
    /// typed.
    async fn real_model_player_agent_says_what_the_player_says_and_stops(model: &DeviceModel) {
        let (_, _model_path, _sampling, limits) = model.parts();
        // (what the player types, a check on the words spoken)
        let lines: [(&str, SaidRight); 4] = [
            ("Say \"I will not let you take it.\"", |words| {
                words.trim_matches('"') == "I will not let you take it."
            }),
            (
                "Mara, it sounds like Toma earned that fair and square. Do you have anything to trade for it if you want it back?",
                |words| {
                    words.contains("Toma earned that fair and square")
                        && words.contains("anything to trade")
                },
            ),
            ("Tell Toma I'm sorry I said the wrong thing.", |words| {
                words.to_lowercase().contains("sorry")
            }),
            (
                "Toma, I will defend you if Mara tries to take it.",
                |words| words.to_lowercase().contains("defend"),
            ),
        ];
        let mut wrong = Vec::new();
        for (line, words_ok) in lines {
            let chosen = Arc::new(Mutex::new(Vec::new()));
            let backend = RealPlayerOnly {
                real: Arc::clone(&model.backend),
                chosen: Arc::clone(&chosen),
            };
            let (game, _store, _db, first, second) =
                two_player_bread_thief_game_with_limits(backend, limits).await;
            let _first_connection = game.connect(&first).await.unwrap();
            let _second_connection = game.connect(&second).await.unwrap();
            let last = game
                .player_message(first.clone(), line)
                .await
                .expect("the player's turn");
            let calls = chosen.lock().unwrap().clone();
            let words: Vec<String> = calls
                .iter()
                .filter(|call| call.name == "speak")
                .filter_map(|call| {
                    serde_json::from_str::<serde_json::Value>(&call.arguments)
                        .ok()?
                        .get("words")?
                        .as_str()
                        .map(str::to_string)
                })
                .collect();
            let said_once = calls.len() == 1 && words.len() == 1 && words_ok(&words[0]);
            let stopped = matches!(&last.content, Content::Text(text) if text.trim().is_empty());
            if !said_once || !stopped {
                wrong.push(format!(
                    "{line:?} -> calls {calls:?}, final output {:?}",
                    last.content
                ));
            }
            game.shutdown().await.unwrap();
        }
        assert!(
            wrong.is_empty(),
            "the player's agent went wrong: {wrong:#?}"
        );
    }

    /// Known failing until the player agent's context is fixed; see
    /// plans/agent-tuning.md.
    #[tokio::test]
    #[ignore = "known failure: the player agent narrates and rewrites speech (plans/agent-tuning.md); needs a CUDA GPU"]
    async fn the_player_agent_says_what_the_player_says_and_stops() {
        let model = DeviceModel::load().await;
        real_model_player_agent_says_what_the_player_says_and_stops(&model).await;
    }

    /// What one player says is understood by the other player's own agent: the
    /// listener is asked about it and answers from the fact spoken. Delivery,
    /// attribution, storage and privacy are checked by the scripted tests.
    async fn real_model_listener_understands_what_another_player_said(model: &DeviceModel) {
        let (backend, _model_path, _sampling, limits) = model.parts();
        let (game, store, _db, first, second) =
            two_player_bread_thief_game_with_limits(backend, limits).await;
        let _first_connection = game.connect(&first).await.unwrap();
        let _second_connection = game.connect(&second).await.unwrap();

        game.player_message(
            first.clone(),
            "Say out loud to everyone that the spare key is hidden under the old stone.",
        )
        .await
        .expect("the speaker's turn");
        assert!(
            !heard(&store, &second).await.is_empty(),
            "nothing the speaker said reached the other player; the speaker's chat: {:#?}",
            store.player_chat(&first).await.unwrap()
        );

        let answer = game
            .player_message(
                second.clone(),
                "What did the other adventurer just say about the key? Answer briefly.",
            )
            .await
            .expect("the listener's turn");
        let Content::Text(answer) = answer.content else {
            panic!("the listener did not answer in words: {answer:?}");
        };
        assert!(
            answer.to_lowercase().contains("stone"),
            "the listener did not report where the key is: {answer}"
        );
        assert_no_rejected_output(&store).await;
        game.shutdown().await.unwrap();
    }

    async fn real_model_compacts_a_long_chat_and_keeps_an_early_fact(model: &DeviceModel) {
        let (backend, model_path, sampling, base_limits) = model.parts();
        let observed_prompts = model.observed();
        // A threshold the second turn's context clears, retaining enough tail
        // that the summary has real history to work from.
        let limits = Limits {
            compact_before_next_input_tokens: 700,
            keep_raw_messages: 2,
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
        // Plain chat that needs no scene tool, so the model answers directly
        // instead of paying for a GM narration each turn.
        for prompt in [
            "In two sentences, tell me what a lighthouse keeper does.",
            "In two sentences, tell me how a bell is cast.",
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

        // The fact is recalled after compaction, and what the model was shown
        // for that question held it only in the summary.
        let question = "What is my missing sister's name? Reply with the name only.";
        let recall = game
            .player_message(member.clone(), question)
            .await
            .expect("the recall turn");
        let Content::Text(answer) = recall.content else {
            panic!("the player agent did not answer in words: {recall:?}");
        };
        assert!(
            answer.to_uppercase().contains("MARIBEL"),
            "the fact was lost after compaction: {answer}"
        );
        // Copy the request out and release the capture lock: the model's
        // prompt observer takes it for any inference still running.
        let shown = {
            let prompts = observed_prompts.lock().unwrap();
            prompts
                .iter()
                .rev()
                .find(|observed| {
                    matches!(observed.request.messages.last().map(|message| &message.content),
                        Some(MessageContent::Text(text)) if text == question)
                })
                .expect("the recall question reached the model")
                .request
                .clone()
        };
        // The summary rides in the leading system message; the retained raw
        // messages (every other message) must not hold the fact, or recall
        // would not prove the summary supplied it.
        let text_of = |message: &Message| match &message.content {
            MessageContent::Text(text) => text.to_uppercase(),
            other => format!("{other:?}").to_uppercase(),
        };
        let (leading, raw) = shown
            .messages
            .split_first()
            .expect("a request has messages");
        assert!(
            text_of(leading).contains("MARIBEL"),
            "the summary in the leading message does not hold the fact: {}",
            text_of(leading)
        );
        assert!(
            !raw.iter()
                .any(|message| text_of(message).contains("MARIBEL")),
            "a retained raw message still held the fact, so recall did not need the summary: {raw:#?}"
        );
        assert_no_rejected_output(&store).await;
        game.shutdown().await.unwrap();
    }
}
