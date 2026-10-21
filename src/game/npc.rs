//! NPC response steps (design.md's NPC speech and response rounds). The world
//! worker runs one finite step at a time between foreground events: collect one
//! NPC's decision, select the winners, or take their words to the GM. Each step
//! shares the discussion's budget and either completes whole or fails the
//! discussion visibly.

use std::sync::Arc;

use anyhow::{Context, Result, ensure};
use schemars::{JsonSchema, schema_for};
use serde::Deserialize;

use super::{Budget, CallContext, GM_ROLE, Game, Narration, no_argument_schema};
use crate::{
    agent,
    context::{self, RecordedCompletion},
    inspection::DevEvent,
    llm::{Backend, Content, Message, Role, ToolDefinition},
    store::{
        BatchChoice, Decision, ExecutionClaim, Hearing, InvalidHearing, MAX_PREFIX_CHARS, NpcRound,
        NpcStep, ProposeAction, SayAction, WhisperAction,
    },
    tools::{Tool, ToolFuture, ToolOutcome},
    world::{ChatChange, World},
};

const NPC_ROLE: &str = "You are the character described below, in a scene you share with others. Speak only for yourself. Decide whether to respond to what you just heard: call do_nothing, or call propose_action with the words you would say (set whisper to speak privately to those in `to`) and how strongly you want to say them now. Most of the time the right choice is do_nothing: stay silent unless you were spoken to or have something new that matters to say, and let a conversation end once your point has been made. The GM decides whether your words are said, and others may speak first or at the same time.";

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ResolveSpeechBatch {
    /// One decision for every presented action.
    decisions: Vec<SpeechDecision>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct SpeechDecision {
    /// The visible action id.
    action_id: i64,
    /// Whether the character can say it.
    approve: bool,
    /// When not approved, why it cannot be said.
    #[serde(default)]
    reason: Option<String>,
    /// Characters who also hear it, by name.
    #[serde(default)]
    include: Vec<String>,
    /// Characters who must not hear it, by name.
    #[serde(default)]
    exclude: Vec<String>,
    /// Normally omitted. A short phrase every hearer is shown before the words,
    /// only for how they are heard (such as "(Muffled through the door)"); never
    /// the speaker's name or manner.
    #[serde(default)]
    #[schemars(length(max = MAX_PREFIX_CHARS))]
    prefix: Option<String>,
}

impl<B> Game<B>
where
    B: Backend + Send + Sync + 'static,
{
    /// Run one finite step of NPC work. A failure is recorded on the
    /// discussion, announced to the players it concerns and logged; it is
    /// never retried, and the world stays usable.
    pub(super) async fn run_npc_step_logged(self: &Arc<Self>, world: &Arc<World>, step: NpcStep) {
        let round = match &step {
            NpcStep::Decide { round, .. }
            | NpcStep::Refresh(round)
            | NpcStep::Select(round)
            | NpcStep::Execute(round) => *round,
        };
        let result = match step {
            NpcStep::Refresh(round) => self.store.refresh_round(&round).await,
            NpcStep::Select(round) => self.store.select_round(&round).await,
            NpcStep::Decide {
                round,
                character_id,
            } => self.decide(world, round, character_id).await,
            NpcStep::Execute(round) => self.execute_round(world, round).await,
        };
        let Err(error) = result else { return };
        let message = format!("{error:#}");
        tracing::error!(world_id = world.id, round_id = round.id, group_id = round.group_id, error = %message, "NPC step failed");
        // A step that failed before it had a sequence of its own (or whose
        // own failure could not be recorded) must still stop the discussion,
        // or the same step would run again.
        match self.store.ensure_discussion_failed(&round, &message).await {
            Ok(Some(sequence_id)) => {
                let _no_live_viewers = world.broadcasts.send(ChatChange::Failed { sequence_id });
            }
            Ok(None) => {}
            Err(failure) => {
                tracing::error!(world_id = world.id, error = %format!("{failure:#}"), "recording a failed NPC discussion")
            }
        }
    }

    async fn decide(
        self: &Arc<Self>,
        world: &Arc<World>,
        round: NpcRound,
        character_id: i64,
    ) -> Result<()> {
        let claim = self.store.claim_npc_decision(&round, character_id).await?;
        let budget = Budget::restore(
            self.limits,
            self.store.discussion_spent(round.root_sequence_id).await?,
        );
        let mut inference_id = None;
        let result: Result<()> = async {
            budget.spend(0).context("the discussion's budget")?;
            self.backend
                .before_agent(&self.store, claim.branch, &world.events)
                .await
                .context("waiting for earlier deferred work for this NPC")?;
            let scene = serde_json::to_string(&self.store.npc_scene(character_id).await?)
                .context("serializing the NPC's scene")?;
            let facts = format!("Your situation:\n{scene}");
            let tools = [
                ToolDefinition {
                    name: "do_nothing".to_string(),
                    description: "Stay silent for now.".to_string(),
                    schema: no_argument_schema(),
                },
                ToolDefinition {
                    name: "propose_action".to_string(),
                    description: "Propose what you would do now.".to_string(),
                    schema: serde_json::to_value(schema_for!(ProposeAction))
                        .expect("propose_action schema should serialize"),
                },
            ];
            let completion = context::complete_recorded(
                &self.store,
                &self.backend,
                RecordedCompletion {
                    events: world.events.clone(),
                    branch: claim.branch,
                    sequence_id: Some(claim.sequence_id),
                    invocation_id: claim.invocation_id,
                    static_messages: &[Message::text(Role::System, NPC_ROLE)],
                    dynamic_context: &[facts.as_str()],
                    tools: &tools,
                    sampling: self.sampling.clone(),
                    model: &self.model,
                    compact_before_next_input_tokens: self.limits.compact_before_next_input_tokens,
                },
                |_| {},
            )
            .await
            .context("running the NPC's decision inference")?;
            inference_id = Some(completion.inference_id);
            let (call, decision) = Decision::parse(&completion.response.content)?;
            let recorded = match decision {
                Decision::Nothing => "You stay silent.",
                Decision::Propose(_) => "Your proposal is recorded. It is said only if the GM allows it and nobody wants to act more.",
            };
            self.store
                .finish_npc_decision(&claim, completion.inference_id, &call, recorded, budget.spent())
                .await
        }
        .await;
        self.finish_npc_step(
            world,
            (claim.root_sequence_id, claim.sequence_id),
            Some((claim.invocation_id, inference_id)),
            result,
        )
        .await
    }

    /// Publish the step's outcome: a failure is recorded on the discussion
    /// (ending the step's own records, including an `unfinished` decision
    /// invocation) and announced to the players it concerns.
    async fn finish_npc_step(
        &self,
        world: &World,
        (root, sequence_id): (i64, i64),
        unfinished: Option<(i64, Option<i64>)>,
        result: Result<()>,
    ) -> Result<()> {
        match result {
            Ok(()) => {
                world.publish(DevEvent::Changed {
                    sequence_id: Some(sequence_id),
                    inference_id: None,
                });
                let _no_live_viewers = world.broadcasts.send(ChatChange::Entries);
                Ok(())
            }
            Err(error) => {
                let message = format!("{error:#}");
                self.store
                    .fail_npc_discussion(root, sequence_id, unfinished, &message)
                    .await
                    .with_context(|| format!("also failed to record the failure: {message}"))?;
                let _no_live_viewers = world.broadcasts.send(ChatChange::Failed { sequence_id });
                Err(error)
            }
        }
    }

    async fn execute_round(self: &Arc<Self>, world: &Arc<World>, round: NpcRound) -> Result<()> {
        let claim = self.store.claim_npc_execution(&round).await?;
        let budget = Budget::restore(
            self.limits,
            self.store.discussion_spent(round.root_sequence_id).await?,
        );
        let result = self.take_words_to_the_gm(world, &claim, &budget).await;
        let finished = match &result {
            Ok(()) => {
                self.store
                    .finish_npc_execution(&claim, budget.spent())
                    .await
            }
            Err(_) => Ok(()),
        };
        self.finish_npc_step(
            world,
            (claim.root_sequence_id, claim.sequence_id),
            None,
            result.and(finished),
        )
        .await
    }

    /// A fresh GM invocation judges the winners' words together.
    async fn take_words_to_the_gm(
        self: &Arc<Self>,
        world: &Arc<World>,
        claim: &ExecutionClaim,
        budget: &Budget,
    ) -> Result<()> {
        let branch = self.store.group_branch(claim.group_id).await?;
        let scene = serde_json::to_string(
            &self
                .store
                .gm_scene(branch.world_id(), branch.branch_id())
                .await
                .context("loading group GM scene for NPC speech")?,
        )
        .context("serializing group GM scene")?;
        let scene_packet = format!("Current scene packet:\n{scene}");
        let mut listing = String::new();
        for proposal in &claim.proposals {
            listing.push_str(&format!(
                "\n- Action {}: {}",
                proposal.pending.id,
                describe(
                    &proposal.speaker_name,
                    &proposal.pending.tool,
                    &proposal.pending.args
                )?
            ));
        }
        let request = Message::text(
            Role::User,
            format!(
                "Characters replied to what was just said, each without hearing the others:{listing}\nJudge each from the current scene: whether the character can say it and who hears it. Call resolve_speech_batch once with a decision for every action. Delivered words are heard as spoken; then narrate anything else that happens."
            ),
        );
        self.store
            .append_instruction(branch, &request, claim.sequence_id)
            .await
            .context("storing the NPC speech request")?;
        let proposals: Vec<_> = claim
            .proposals
            .iter()
            .map(|proposal| (proposal.pending.id, proposal.speaker_name.clone()))
            .collect();
        let gm_agent_id = branch.agent_id();
        let world_id = branch.world_id();
        let game = Arc::clone(self);
        let batch_tool = Tool::new(
            ToolDefinition {
                name: "resolve_speech_batch".to_string(),
                description: "Decide every presented reply at once: approve (optionally adjusting who hears it or adding a short prefix) or reject with a reason.".to_string(),
                schema: serde_json::to_value(schema_for!(ResolveSpeechBatch))
                    .expect("resolve_speech_batch schema should serialize"),
            },
            move |context| {
                let arguments = context.arguments.to_string();
                let tool_execution_id = context.tool_execution_id;
                let game = Arc::clone(&game);
                let proposals = proposals.clone();
                Box::pin(async move {
                    let batch: ResolveSpeechBatch = match serde_json::from_str(&arguments)
                        .context("parsing speech batch decisions")
                    {
                        Ok(batch) => batch,
                        Err(error) => return Ok(ToolOutcome::rejected(error)),
                    };
                    let mut choices = Vec::new();
                    for decision in batch.decisions {
                        let choice = if decision.approve {
                            BatchChoice::Approve(Hearing {
                                include: decision.include,
                                exclude: decision.exclude,
                                prefix: decision.prefix,
                            })
                        } else {
                            match decision.reason.filter(|reason| !reason.trim().is_empty()) {
                                Some(reason) => BatchChoice::Reject(reason),
                                None => {
                                    return Ok(ToolOutcome::Rejected(format!(
                                        "Tool arguments were rejected: say why action {} is rejected",
                                        decision.action_id
                                    )));
                                }
                            }
                        };
                        choices.push((decision.action_id, choice));
                    }
                    let mut presented: Vec<i64> = proposals.iter().map(|(id, _)| *id).collect();
                    let mut decided: Vec<i64> = choices.iter().map(|(id, _)| *id).collect();
                    presented.sort_unstable();
                    decided.sort_unstable();
                    if presented != decided {
                        return Ok(ToolOutcome::Rejected(format!(
                            "Tool arguments were rejected: decide exactly the presented actions {presented:?} once each, not {decided:?}"
                        )));
                    }
                    let resolved = match game
                        .store
                        .resolve_speech_batch(gm_agent_id, world_id, tool_execution_id, &choices)
                        .await
                    {
                        Ok(resolved) => resolved,
                        Err(error) => match error.downcast_ref::<InvalidHearing>() {
                            Some(invalid) => {
                                return Ok(ToolOutcome::Rejected(format!(
                                    "Tool arguments were rejected: {invalid}"
                                )));
                            }
                            None => return Err(error.context("resolving the speech batch")),
                        },
                    };
                    let mut report = Vec::new();
                    for resolved in resolved {
                        let speaker = proposals
                            .iter()
                            .find(|(id, _)| *id == resolved.id)
                            .map_or("", |(_, name)| name.as_str());
                        report.push(match resolved.outcome {
                            crate::store::ActionOutcome::Applied(_) if !resolved.heard_by.is_empty() => format!(
                                "Action {} ({speaker}) approved. Delivered as spoken to {}.",
                                resolved.id,
                                resolved.heard_by.join(", ")
                            ),
                            crate::store::ActionOutcome::Applied(_) => {
                                format!("Action {} ({speaker}) approved.", resolved.id)
                            }
                            crate::store::ActionOutcome::Failed(reason) => format!(
                                "Action {} ({speaker}) approved, but could not be said: {reason}",
                                resolved.id
                            ),
                            crate::store::ActionOutcome::Rejected(_) => {
                                format!("Action {} ({speaker}) rejected.", resolved.id)
                            }
                        });
                    }
                    Ok(ToolOutcome::Completed(report.join("\n")))
                }) as ToolFuture
            },
        );
        let call = CallContext::root(Some(claim.sequence_id));
        let settled = agent::complete_with_call_context(
            &self.store,
            &self.backend,
            budget,
            agent::Turn {
                events: world.events.clone(),
                branch,
                static_messages: &[Message::text(Role::System, GM_ROLE)],
                dynamic_context: &[scene_packet.as_str()],
                tools: &[batch_tool],
                sampling: self.sampling.clone(),
                model: &self.model,
            },
            &call,
            |_| {},
            |_| {},
        )
        .await
        .context("asking the GM to judge the NPC replies")?;
        ensure!(
            matches!(settled.response.content, Content::Text(_)),
            "the GM did not settle after judging the NPC replies"
        );
        for proposal in &claim.proposals {
            ensure!(
                !self
                    .store
                    .has_pending_action(proposal.pending.world_id, proposal.pending.id)
                    .await?,
                "the GM narrated without deciding action {}",
                proposal.pending.id
            );
        }
        // Narration of the replies reaches only those who heard them.
        let mut audience = Vec::new();
        let mut spoken = false;
        for proposal in &claim.proposals {
            if let Some(heard) = self
                .store
                .speech_audience(proposal.pending.requesting_tool_execution_id)
                .await?
            {
                spoken = true;
                audience.extend(heard);
            }
        }
        self.deliver_narration(
            world_id,
            &Narration {
                group_id: claim.group_id,
                content_id: settled.output_content_id,
                audience: spoken.then_some(audience),
            },
        )
        .await
    }
}

/// How one proposal reads to the GM.
fn describe(speaker: &str, tool: &str, args: &str) -> Result<String> {
    Ok(match tool {
        "say" => {
            let say: SayAction = serde_json::from_str(args).context("parsing a proposed say")?;
            match say.to {
                Some(to) => format!("{speaker} says to {to}: \"{}\"", say.words),
                None => format!("{speaker} says: \"{}\"", say.words),
            }
        }
        "whisper" => {
            let whisper: WhisperAction =
                serde_json::from_str(args).context("parsing a proposed whisper")?;
            format!(
                "{speaker} whispers to {}: \"{}\"",
                whisper.to.join(", "),
                whisper.words
            )
        }
        tool => anyhow::bail!("`{tool}` is not a proposed speech"),
    })
}
