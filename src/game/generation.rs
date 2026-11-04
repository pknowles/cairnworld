//! World generation steps (design.md's Storyteller world creation). The world
//! worker runs one finite step at a time, like an NPC step: the Questioner
//! answers its meta-questions once, then each round asks questions, the
//! Storyteller answers them one at a time as text and replies with the world
//! summary, until the Questioner is satisfied and the Storyteller names the
//! world. A step either commits its outputs with the next stage or fails the
//! world visibly; nothing is retried.

use std::sync::Arc;

use anyhow::{Context, Result, bail, ensure};
use schemars::{JsonSchema, schema_for};
use serde::Deserialize;

use super::{Budget, CallContext, Game};
use crate::{
    agent::{self, Settled},
    inspection::DevEvent,
    llm::{Backend, Content, Message, Role},
    prompts,
    store::{Advance, AgentBranch, Generation, GenerationStage},
    tools::{Tool, ToolOutcome},
    world::World,
};

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct AskQuestions {
    // The questions, each one a single question the Storyteller can answer.
    questions: Vec<String>,
}

fn parse_questions(arguments: &str) -> Result<Vec<String>> {
    let asked: AskQuestions =
        serde_json::from_str(arguments).context("parsing ask_questions arguments")?;
    ensure!(!asked.questions.is_empty(), "ask at least one question");
    Ok(asked.questions)
}

/// The Questioner ends its turn with exactly one of these.
fn questioner_tools() -> Result<Vec<Tool>> {
    let prompts = prompts::current()?;
    let ask = Tool::terminal(
        prompts.questioner.tools.tool(
            "ask_questions",
            serde_json::to_value(schema_for!(AskQuestions))
                .expect("ask_questions schema should serialize"),
        )?,
        |context| {
            let arguments = context.arguments.to_string();
            Box::pin(async move {
                Ok(match parse_questions(&arguments) {
                    Ok(_) => ToolOutcome::Completed("Questions recorded.".into()),
                    Err(error) => ToolOutcome::rejected(error),
                })
            })
        },
    );
    let complete = Tool::terminal(
        prompts
            .questioner
            .tools
            .tool("world_is_complete", prompts::no_arguments())?,
        |_| Box::pin(async { Ok(ToolOutcome::Completed("World recorded as complete.".into())) }),
    );
    Ok(vec![ask, complete])
}

/// A list from `prompts.toml` as one bullet per line.
fn bullets(items: &[String]) -> String {
    items.iter().map(|item| format!("- {item}\n")).collect()
}

/// What an agent replied in plain text.
fn reply_text(settled: &Settled) -> Result<&str> {
    match &settled.response.content {
        Content::Text(text) => Ok(text),
        Content::ToolCalls(_) => bail!("the agent called a tool where text was expected"),
    }
}

/// Where one generation step runs: its world, its sequence and its budget.
struct StepScope<'a> {
    world: &'a World,
    sequence_id: i64,
    budget: &'a Budget,
}

/// One agent turn: the request goes into the branch's history as an
/// instruction and the agent answers, ending with a tool call only when it has
/// tools.
struct Request<'a> {
    branch: AgentBranch,
    role: &'a str,
    text: String,
    /// Current state shown with this request and replaced by the next one, so
    /// it does not pile up in the history.
    context: Vec<String>,
    tools: Vec<Tool>,
    terminal_reminder: Option<&'a str>,
}

impl<'a> Request<'a> {
    /// A request for a plain-text reply.
    fn reply(branch: AgentBranch, role: &'a str, text: String) -> Self {
        Self {
            branch,
            role,
            text,
            context: vec![],
            tools: vec![],
            terminal_reminder: None,
        }
    }

    fn context_refs(&self) -> Vec<&str> {
        self.context.iter().map(String::as_str).collect()
    }
}

impl<B> Game<B>
where
    B: Backend + Send + Sync + 'static,
{
    /// Run one step of a world's initialization. A failing step is recorded on
    /// its sequence and fails the world; only a failure to record that is
    /// returned, since the same step would then run again.
    pub(super) async fn run_generation_step(
        self: &Arc<Self>,
        world: &Arc<World>,
        generation: Generation,
    ) -> Result<()> {
        let sequence = self
            .store
            .begin_sequence(world.id, "world generation")
            .await
            .context("starting a world generation sequence")?;
        let budget = Budget::restore(self.limits, generation.spent);
        let result: Result<()> = async {
            let scope = StepScope {
                world,
                sequence_id: sequence.id,
                budget: &budget,
            };
            let advance = self.generate(&scope, &generation).await?;
            self.store
                .advance_generation(world.id, advance, budget.spent())
                .await
        }
        .await;
        let error = result.as_ref().err().map(|error| format!("{error:#}"));
        self.store
            .finish_sequence(sequence.id, error.as_deref())
            .await
            .context("finishing a world generation sequence")?;
        if let Some(error) = error {
            tracing::error!(world_id = world.id, sequence_id = sequence.id, %error, "world generation failed");
            self.store
                .fail_generation(world.id)
                .await
                .with_context(|| format!("also failed to mark generation failed: {error}"))?;
        }
        world.publish(DevEvent::Changed {
            sequence_id: Some(sequence.id),
            inference_id: None,
        });
        Ok(())
    }

    async fn generate(&self, scope: &StepScope<'_>, generation: &Generation) -> Result<Advance> {
        let prompts = prompts::current()?;
        let seed: &str = if generation.seed.trim().is_empty() {
            &prompts.world.no_seed
        } else {
            &generation.seed
        };
        let summary: &str = if generation.summary.trim().is_empty() {
            &prompts.world.no_summary
        } else {
            &generation.summary
        };
        let questioner = &prompts.questioner;
        let storyteller = &prompts.storyteller;
        // Every Storyteller request is shown the seed and the one current
        // summary as context, never as part of its chat.
        let storyteller_request = |text: String| -> Result<Request<'_>> {
            Ok(Request {
                context: vec![prompts::fill(
                    &storyteller.world_state,
                    &[("seed", seed), ("summary", summary)],
                )?],
                ..Request::reply(generation.storyteller, &storyteller.role, text)
            })
        };
        match generation.stage {
            GenerationStage::Meta => {
                let text = prompts::fill(
                    &questioner.meta_request,
                    &[
                        ("seed", seed),
                        ("examples", &bullets(&questioner.example_questions)),
                        ("questions", &bullets(&questioner.meta_questions)),
                    ],
                )?;
                self.generation_turn(
                    scope,
                    Request::reply(generation.questioner, &questioner.role, text),
                )
                .await?;
                Ok(Advance::MetaQuestionsAnswered)
            }
            GenerationStage::Ask => {
                let world_state = prompts::fill(
                    &questioner.world_state,
                    &[("seed", seed), ("summary", summary)],
                )?;
                let settled = self
                    .generation_turn(
                        scope,
                        Request {
                            branch: generation.questioner,
                            role: &questioner.role,
                            text: questioner.ask_request.clone(),
                            context: vec![world_state],
                            tools: questioner_tools()?,
                            terminal_reminder: Some(&questioner.ask_reminder),
                        },
                    )
                    .await?;
                let Content::ToolCalls(calls) = &settled.response.content else {
                    bail!("the Questioner ended its turn without calling a tool");
                };
                let call = calls.last().context("the Questioner made no tool call")?;
                match call.name.as_str() {
                    "ask_questions" => Ok(Advance::Questions(parse_questions(&call.arguments)?)),
                    "world_is_complete" => Ok(Advance::WorldIsComplete),
                    other => bail!("the Questioner ended its turn with `{other}`"),
                }
            }
            GenerationStage::Answer => {
                let number = generation.next_question + 1;
                let question = generation.questions.get(generation.next_question);
                let question = question
                    .with_context(|| format!("question {number} of the round is missing"))?;
                let text = if generation.next_question == 0 {
                    prompts::fill(
                        &storyteller.round_start,
                        &[
                            ("count", &generation.questions.len().to_string()),
                            ("question", question),
                        ],
                    )?
                } else {
                    prompts::fill(
                        &storyteller.next_question,
                        &[("number", &number.to_string()), ("question", question)],
                    )?
                };
                self.generation_turn(scope, storyteller_request(text)?)
                    .await?;
                Ok(Advance::QuestionAnswered)
            }
            GenerationStage::Summarize => {
                // A side inference: the new summary replaces the old one, so the
                // request and reply stay out of the chat, and the Storyteller
                // never has two versions in its context.
                let settled = self
                    .generation_aside(
                        scope,
                        storyteller_request(storyteller.summary_request.clone())?,
                    )
                    .await?;
                Ok(Advance::Summary(reply_text(&settled)?.to_string()))
            }
            GenerationStage::Name => {
                let settled = self
                    .generation_turn(
                        scope,
                        storyteller_request(storyteller.name_request.clone())?,
                    )
                    .await?;
                Ok(Advance::Name(reply_text(&settled)?.to_string()))
            }
            stage @ (GenerationStage::ConceptDone
            | GenerationStage::Failed
            | GenerationStage::Complete) => {
                bail!(
                    "world {} has no generation step at {stage:?}",
                    scope.world.id
                )
            }
        }
    }

    async fn generation_turn(
        &self,
        scope: &StepScope<'_>,
        request: Request<'_>,
    ) -> Result<Settled> {
        self.store
            .append_instruction(
                request.branch,
                &Message::text(Role::User, request.text.clone()),
                scope.sequence_id,
            )
            .await
            .context("storing the generation request")?;
        let role = [Message::text(Role::System, request.role)];
        let context = request.context_refs();
        agent::complete_with_call_context(
            &self.store,
            &self.backend,
            scope.budget,
            self.turn(scope, &request, &role, &context),
            &CallContext::root(Some(scope.sequence_id)),
            |_| {},
            |_| {},
        )
        .await
        .context("running a generation turn")
    }

    /// The request is asked of the agent without entering its chat, and so is
    /// its reply.
    async fn generation_aside(
        &self,
        scope: &StepScope<'_>,
        request: Request<'_>,
    ) -> Result<Settled> {
        let role = [Message::text(Role::System, request.role)];
        let context = request.context_refs();
        agent::complete_aside(
            &self.store,
            &self.backend,
            scope.budget,
            self.turn(scope, &request, &role, &context),
            &request.text,
            &CallContext::root(Some(scope.sequence_id)),
        )
        .await
        .context("running a generation side inference")
    }

    fn turn<'a>(
        &'a self,
        scope: &StepScope<'_>,
        request: &'a Request<'_>,
        role: &'a [Message],
        context: &'a [&'a str],
    ) -> agent::Turn<'a> {
        agent::Turn {
            events: scope.world.events.clone(),
            branch: request.branch,
            static_messages: role,
            dynamic_context: context,
            tools: &request.tools,
            terminal_reminder: request.terminal_reminder,
            sampling: self.sampling.clone(),
            model: &self.model,
        }
    }
}
