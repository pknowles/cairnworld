//! NPC response steps (design.md's NPC speech and response rounds). The world
//! worker runs one finite step at a time between foreground events: collect one
//! NPC's decision, select the winners, or take their words to the GM. Each step
//! shares the discussion's budget and either completes whole or fails the
//! discussion visibly.

use std::sync::Arc;

use anyhow::{Context, Result};
use schemars::schema_for;

use super::{Budget, CallContext, Game};
use crate::{
    context::{self, RecordedCompletion},
    inspection::DevEvent,
    llm::{Backend, Message, Role},
    prompts,
    store::{Decision, NpcRound, NpcStep, SayAction, WhisperAction, with_eagerness},
    world::{ChatChange, World},
};

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
            let prompts = prompts::current()?;
            let facts = prompts::fill(&prompts.npc.situation, &[("scene", &scene)])?;
            let speech = |name: &str, schema: serde_json::Value| -> Result<_> {
                let mut definition = prompts.shared.tools.tool(name, schema)?;
                definition.schema = with_eagerness(definition.schema, &prompts.npc.eagerness);
                Ok(definition)
            };
            let tools = [
                prompts.npc.tools.tool("do_nothing", prompts::no_arguments())?,
                speech(
                    "say",
                    serde_json::to_value(schema_for!(SayAction))
                        .expect("say action schema should serialize"),
                )?,
                speech(
                    "whisper",
                    serde_json::to_value(schema_for!(WhisperAction))
                        .expect("whisper action schema should serialize"),
                )?,
            ];
            let completion = context::complete_recorded(
                &self.store,
                &self.backend,
                RecordedCompletion {
                    events: world.events.clone(),
                    branch: claim.branch,
                    sequence_id: Some(claim.sequence_id),
                    invocation_id: claim.invocation_id,
                    static_messages: &[Message::text(Role::System, &prompts.npc.role)],
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
            if let Decision::Propose(proposal) = &decision {
                match proposal.tool {
                    "say" => super::validate_say_action(&proposal.arguments)?,
                    _ => super::validate_whisper_action(&proposal.arguments)?,
                }
            }
            let recorded = match decision {
                Decision::Nothing => "You stay silent.",
                Decision::Propose(_) => "Your words are recorded. They are said only if the GM allows it and nobody wants to act more.",
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
        let result = async {
            let narration = self
                .arbitrate(
                    claim.pending.clone(),
                    budget.clone(),
                    CallContext::root(Some(claim.sequence_id)),
                    claim
                        .tied
                        .then_some(prompts::current()?.gm.tied_note.as_str()),
                )
                .await?;
            self.deliver_narration(round.world_id, &narration).await?;
            self.store
                .finish_npc_execution(&claim, budget.spent())
                .await
        }
        .await;
        self.finish_npc_step(
            world,
            (claim.root_sequence_id, claim.sequence_id),
            None,
            result,
        )
        .await
    }
}
