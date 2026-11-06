//! NPC response steps (design.md's NPC speech and responses). The world
//! worker runs one finite step at a time between foreground events: collect one
//! NPC's decision, select the winners, or take their words to the GM. Each step
//! shares the discussion's budget and either completes whole or fails the
//! discussion visibly.

use std::sync::Arc;

use anyhow::{Context, Result};

use super::{Budget, CallContext, Game};
use crate::{
    context::{self, RecordedCompletion},
    inspection::DevEvent,
    llm::{Backend, Message, Role, ToolCall},
    prompts,
    store::{AgentBranch, Decision, NpcStep, ResponseCollection, with_eagerness},
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
        let collection = *step.collection();
        let result = match step {
            NpcStep::Refresh(collection) => self.store.recollect_responses(&collection).await,
            NpcStep::Select(collection) => self.store.select_winning_responses(&collection).await,
            NpcStep::Decide {
                collection,
                character_id,
            } => self.decide(world, collection, character_id).await,
            NpcStep::Execute(collection) => self.execute_selected_response(world, collection).await,
        };
        let Err(error) = result else { return };
        let message = format!("{error:#}");
        tracing::error!(world_id = world.id, collection_id = collection.id, group_id = collection.group_id, error = %message, "NPC step failed");
        // A step that failed before it had a sequence of its own (or whose
        // own failure could not be recorded) must still stop the discussion,
        // or the same step would run again.
        match self
            .store
            .ensure_discussion_failed(&collection, &message)
            .await
        {
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
        collection: ResponseCollection,
        character_id: i64,
    ) -> Result<()> {
        let claim = self
            .store
            .claim_npc_decision(&collection, character_id)
            .await?;
        let budget = Budget::restore(
            self.limits,
            self.store
                .discussion_spent(collection.root_sequence_id)
                .await?,
        );
        let mut inference_id = None;
        let result: Result<()> = async {
            budget.spend(0).context("the discussion's budget")?;
            // The last thing the NPC reads before it chooses, whoever it is
            // answering, so a character that has said its piece lets others act.
            self.store
                .append_instruction(
                    claim.branch,
                    &Message::text(Role::User, &prompts::current()?.npc.before_deciding),
                    claim.sequence_id,
                )
                .await?;
            let (decision_inference, call, decision) = self
                .ask_npc(
                    world,
                    claim.branch,
                    claim.sequence_id,
                    claim.invocation_id,
                    character_id,
                    &mut inference_id,
                )
                .await?;
            self.store
                .finish_npc_decision(
                    &claim,
                    decision_inference,
                    &call,
                    decision_result(&decision),
                    budget.spent(),
                )
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

    /// Ask an NPC for one decision from its own history: exactly one
    /// `do_nothing` or `speak`. `inference_id` is set as soon as the
    /// model has answered, so a caller can close a call that then fails.
    pub(super) async fn ask_npc(
        self: &Arc<Self>,
        world: &Arc<World>,
        branch: AgentBranch,
        sequence_id: i64,
        invocation_id: i64,
        character_id: i64,
        inference_id: &mut Option<i64>,
    ) -> Result<(i64, ToolCall, Decision)> {
        self.backend
            .before_agent(&self.store, branch, &world.events)
            .await
            .context("waiting for earlier deferred work for this NPC")?;
        let scene = serde_json::to_string(&self.store.npc_scene(character_id).await?)
            .context("serializing the NPC's scene")?;
        let prompts = prompts::current()?;
        let facts = prompts::fill(&prompts.npc.situation, &[("scene", &scene)])?;
        let kinds = super::action_kinds()?;
        let mut tools = vec![
            prompts
                .npc
                .tools
                .tool("do_nothing", prompts::no_arguments())?,
        ];
        for kind in &kinds {
            let mut definition = kind.definition.clone();
            definition.schema = with_eagerness(definition.schema, &prompts.npc.eagerness);
            tools.push(definition);
        }
        let completion = context::complete_recorded(
            &self.store,
            &self.backend,
            RecordedCompletion {
                events: world.events.clone(),
                branch,
                sequence_id: Some(sequence_id),
                invocation_id,
                static_messages: &[Message::text(Role::System, &prompts.npc.role)],
                dynamic_context: &[facts.as_str()],
                tools: &tools,
                ending_tools: &[],
                sampling: self.sampling.clone(),
                model: &self.model,
                compact_before_next_input_tokens: self.limits.compact_before_next_input_tokens,
                perceiver: None,
            },
            |_| {},
        )
        .await
        .context("running the NPC's decision inference")?;
        *inference_id = Some(completion.inference_id);
        let (call, decision) = Decision::parse(&completion.response.content)?;
        if let Decision::Propose(proposal) = &decision {
            let kind = kinds
                .iter()
                .find(|kind| kind.definition.name == proposal.tool)
                .with_context(|| {
                    format!(
                        "the NPC called `{}`, which it was not offered",
                        proposal.tool
                    )
                })?;
            (kind.validate)(&proposal.arguments)?;
        }
        Ok((completion.inference_id, call, decision))
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

    async fn execute_selected_response(
        self: &Arc<Self>,
        world: &Arc<World>,
        collection: ResponseCollection,
    ) -> Result<()> {
        let claim = self.store.claim_npc_execution(&collection).await?;
        let budget = Budget::restore(
            self.limits,
            self.store
                .discussion_spent(collection.root_sequence_id)
                .await?,
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
            // Nothing to deliver while the GM waits for players it asked.
            if let Some(narration) = narration {
                self.deliver_narration(collection.world_id, &narration)
                    .await?;
            }
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

/// What an NPC is told once its decision is recorded.
pub(super) fn decision_result(decision: &Decision) -> &'static str {
    match decision {
        Decision::Nothing => "You stay silent.",
        Decision::Propose(_) => {
            "Your words are recorded. They are said only if the GM allows it and nobody wants to act more."
        }
    }
}
