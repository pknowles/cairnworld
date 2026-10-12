use std::{collections::HashMap, sync::Arc};

use anyhow::{Context, Result, ensure};
use schemars::{JsonSchema, schema_for};
use serde::Deserialize;
use tokio::sync::{Mutex, broadcast, mpsc, oneshot, watch};

use crate::{
    agent::{self, Budget, CallContext},
    inference::ScheduledBackend,
    llm::{Backend, Content, Message, Response, Role, Sampling, ToolDefinition},
    settings::Limits,
    store::{PendingAction, PlayerAgent, Store},
    tools::{Tool, ToolFuture, ToolOutcome},
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
}

type LocationBroadcasts = Arc<Mutex<HashMap<i64, broadcast::Sender<String>>>>;

struct Narration {
    location_id: i64,
    text: String,
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
    broadcasts: LocationBroadcasts,
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
    /// current location. World processing remains serialized across locations.
    pub async fn subscribe(
        self: &Arc<Self>,
        member: &PlayerAgent,
    ) -> Result<broadcast::Receiver<String>> {
        let location_id = self.store.player_location_id(member).await?;
        Ok(location_broadcast(
            &self.world_events(member.world_id).await.broadcasts,
            location_id,
        )
        .await
        .subscribe())
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
        };
        worlds.insert(world_id, world.clone());
        drop(worlds);
        let game = Arc::clone(self);
        let broadcasts = Arc::clone(&world.broadcasts);
        tokio::spawn(async move { game.process_world(receiver, broadcasts).await });
        world
    }

    async fn process_world(
        self: Arc<Self>,
        mut events: mpsc::Receiver<WorldEvent>,
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
            }
        }
    }

    /// Resolve one player message through its membership-owned player agent.
    async fn resolve_player_message(
        self: &Arc<Self>,
        member: PlayerAgent,
        text: &str,
        broadcasts: &LocationBroadcasts,
    ) -> Result<Response> {
        let sequence = self
            .store
            .begin_sequence(member.world_id, "player message")
            .await
            .context("starting player message sequence")?;
        self.store
            .append_message(member.agent_id, &Message::text(Role::User, text))
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
            let location_id = self.store.player_location_id(&member).await?;
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
        let prompt = self.player_prompt(&member, ready).await?;
        let response = agent::complete_with_call_context(
            &self.store,
            &self.backend,
            &budget,
            agent::Turn {
                agent_id: member.agent_id,
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
        broadcasts: &LocationBroadcasts,
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
        let prompt = self.player_prompt(&member, false).await?;
        // This is the game event that starts the agent's first turn. It is
        // deliberately static context, not a player-visible or stored chat
        // message: the agent must speak first, while tool-capable templates
        // require a user message before their declarations.
        let entered = Message::text(
            Role::User,
            "The player has entered the world and has not chosen to roll yet. Speak directly to them.",
        );
        let response = agent::complete_with_call_context(
            &self.store,
            &self.backend,
            &budget,
            agent::Turn {
                agent_id: member.agent_id,
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
        broadcasts: LocationBroadcasts,
    ) -> Vec<Tool> {
        let game = Arc::clone(self);
        let hp_member = member.clone();
        let hit_protection = Tool::new(
            ToolDefinition {
                name: "roll_hit_protection".to_string(),
                description: "Roll the Adventurer's starting 1d6 Hit Protection.".to_string(),
                schema: no_argument_schema(),
            },
            move |arguments, inference_id| {
                let game = Arc::clone(&game);
                let member = hp_member.clone();
                let arguments = arguments.to_string();
                Box::pin(async move {
                    if let Err(error) = parse_no_arguments(&arguments) {
                        return Ok(ToolOutcome::rejected(error));
                    }
                    Ok(ToolOutcome::Completed(format!(
                        "Hit Protection: {}",
                        game.store
                            .roll_hit_protection(&member, sequence_id, Some(inference_id))
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
            move |arguments, inference_id| {
                let game = Arc::clone(&game);
                let member = attributes_member.clone();
                let arguments = arguments.to_string();
                Box::pin(async move {
                    if let Err(error) = parse_no_arguments(&arguments) {
                        return Ok(ToolOutcome::rejected(error));
                    }
                    let (str, dex, wil) = game
                        .store
                        .roll_attributes(&member, sequence_id, Some(inference_id))
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
            move |arguments, inference_id| {
                let game = Arc::clone(&game);
                let member = ready_member.clone();
                let arguments = arguments.to_string();
                let budget = ready_budget.clone();
                let call = ready_call.clone();
                let broadcasts = Arc::clone(&ready_broadcasts);
                Box::pin(async move {
                    if let Err(error) = parse_no_arguments(&arguments) {
                        return Ok(ToolOutcome::rejected(error));
                    }
                    game.store
                        .ready_to_begin(&member, sequence_id, Some(inference_id))
                        .await?;
                    let narration = game
                        .opening_narration(&member, budget, call.child(inference_id))
                        .await?;
                    let delivery_game = Arc::clone(&game);
                    Ok(ToolOutcome::CompletedAfter {
                        content: "The GM's narration reached the player as its own entry. Ask what they want to do next.".to_string(),
                        after_result: Box::pin(async move {
                            delivery_game
                                .deliver_narration(&member, narration.location_id, &narration.text, &broadcasts)
                                .await
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
        broadcasts: LocationBroadcasts,
    ) -> Vec<Tool> {
        vec![
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
        Tool::new(definition, move |arguments, inference_id| {
            let game = Arc::clone(&game);
            let member = member.clone();
            let arguments = arguments.to_string();
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
                        inference_id,
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
                        delivery_game
                            .deliver_narration(&member, narration.location_id, &narration.text, &broadcasts)
                            .await
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
        let gm_agent_id = self.store.player_location_gm_agent_id(member).await?;
        let scene = serde_json::to_string(
            &self
                .store
                .gm_scene(member.world_id, gm_agent_id)
                .await
                .context("loading location GM scene for the opening")?,
        )
        .context("serializing location GM opening scene")?;
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
        let response = agent::complete_with_call_context(
            &self.store,
            &self.backend,
            &budget,
            agent::Turn {
                agent_id: gm_agent_id,
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
        .context("asking location GM for opening narration")?;
        let Content::Text(narration) = response.content else {
            anyhow::bail!("location GM did not settle while opening play");
        };
        Ok(Narration {
            location_id,
            text: narration,
        })
    }

    async fn arbitrate(
        self: &Arc<Self>,
        pending: PendingAction,
        location_id: i64,
        budget: Budget,
        call: CallContext,
    ) -> Result<Narration> {
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
            move |arguments, inference_id| {
                let game = Arc::clone(&game);
                let arguments = arguments.to_string();
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
                    game.store
                        .resolve_pending_action(
                            approval_pending.location_gm_agent_id,
                            approval_pending.world_id,
                            action_id,
                            inference_id,
                            "approved",
                        )
                        .await
                        .with_context(|| format!("approving action {action_id}"))?;
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
            move |arguments, inference_id| {
                let game = Arc::clone(&game);
                let arguments = arguments.to_string();
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
                    game.store
                        .resolve_pending_action(
                            pending.location_gm_agent_id,
                            pending.world_id,
                            action_id,
                            inference_id,
                            &format!("rejected: {}", rejection.reason),
                        )
                        .await
                        .with_context(|| format!("rejecting action {action_id}"))?;
                    Ok(ToolOutcome::Completed(format!(
                        "Action {action_id} rejected: {}",
                        rejection.reason
                    )))
                }) as ToolFuture
            },
        );
        let scene = serde_json::to_string(
            &self
                .store
                .gm_scene(pending.world_id, pending.location_gm_agent_id)
                .await
                .context("loading location GM scene for prompt")?,
        )
        .context("serializing location GM scene for prompt")?;
        let actor = self.store.character_name(pending.character_id).await?;
        let role = Message::text(
            Role::System,
            format!(
                "You arbitrate this location for the characters present, listed in the scene packet. Judge each action from that packet, call approve_action with its id if it can happen or reject it with a reason, then narrate the outcome to the characters present. The current scene packet is {scene}."
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
        let response = agent::complete_with_call_context(
            &self.store,
            &self.backend,
            &budget,
            agent::Turn {
                agent_id: pending.location_gm_agent_id,
                static_messages: &[role, request],
                tools: &[gm_tool, reject_tool],
                sampling: self.sampling.clone(),
                model: &self.model,
            },
            &call.child(pending.inference_id),
            |_| {},
            |_| {},
        )
        .await
        .with_context(|| format!("arbitrating action {action_id}"))?;
        let Content::Text(narration) = response.content else {
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
            text: narration,
        })
    }

    async fn deliver_narration(
        &self,
        member: &PlayerAgent,
        location_id: i64,
        narration: &str,
        broadcasts: &LocationBroadcasts,
    ) -> Result<()> {
        self.store
            .append_location_narration(member.world_id, location_id, narration)
            .await
            .context("storing GM narration before the player agent follows up")?;
        // A player may disconnect while the GM resolves. The narration remains
        // stored for reload; active viewers receive this exact same text.
        let _ = location_broadcast(broadcasts, location_id)
            .await
            .send(narration.to_owned());
        Ok(())
    }
}

async fn location_broadcast(
    broadcasts: &Arc<Mutex<HashMap<i64, broadcast::Sender<String>>>>,
    location_id: i64,
) -> broadcast::Sender<String> {
    let mut broadcasts = broadcasts.lock().await;
    broadcasts
        .entry(location_id)
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
        let companion = store
            .create_player_character(owner.id, installed.world_id)
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
                    .history_segments(installed.member.agent_id)
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
        let requests = recorded_requests.lock().unwrap();
        let gm_packet = requests
            .iter()
            .flat_map(|request| request.messages.iter())
            .find_map(|message| match &message.content {
                MessageContent::Text(text) if text.starts_with("You arbitrate this location") => {
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
                .history_segments(member.agent_id)
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

        let summary = store
            .latest_summary(member.agent_id)
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
