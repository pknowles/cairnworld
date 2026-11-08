//! World generation steps (design.md's Storyteller world creation). The world
//! worker runs one finite step at a time, like an NPC step. The Questioner
//! answers its meta-questions once. It then asks the Storyteller questions with
//! `ask_questions`: the call runs the Storyteller (answers, then a new world
//! summary) and reports the change as its result, so the Questioner keeps
//! going in the same turn until it declares the summary complete. The
//! Storyteller names the world and creates its objects, and the Questioner
//! reviews them the same way: `ask_questions` there runs a Storyteller fix and
//! returns the objects. A step either commits its outputs with the next stage
//! or fails the world visibly; nothing is retried.

use std::sync::Arc;

use anyhow::{Context, Result, bail, ensure};
use schemars::{JsonSchema, schema_for};
use serde::Deserialize;

use super::{Budget, CallContext, Game, authoring};
use crate::{
    agent::{self, DynamicContext, Settled},
    inspection::DevEvent,
    llm::{Backend, Content, Message, Role, Sampling},
    prompts,
    store::{Advance, AgentBranch, Generation, GenerationStage, ReviewEntry},
    tools::{Tool, ToolFuture, ToolOutcome},
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

/// What the Storyteller does about the questions the Questioner asks.
#[derive(Clone, Copy)]
enum Round {
    /// Answer them, then rewrite the world summary.
    Summary,
    /// Answer them by changing the world's objects.
    Objects,
}

/// The Questioner's tools while the summary develops: it asks, or ends its turn
/// by declaring the summary complete.
fn summary_tools<B>(game: &Arc<Game<B>>, scope: &StepScope) -> Result<Vec<Tool>>
where
    B: Backend + Send + Sync + 'static,
{
    let complete = Tool::terminal(
        prompts::current()?
            .questioner
            .tools
            .tool("world_summary_complete", prompts::no_arguments())?,
        |_| Box::pin(async { Ok(ToolOutcome::Completed("World recorded as complete.".into())) }),
    );
    Ok(vec![
        ask_questions_tool(game, scope, Round::Summary)?,
        complete,
    ])
}

/// `ask_questions`: runs the Storyteller's answer inside the call and returns
/// its effect as the result.
fn ask_questions_tool<B>(game: &Arc<Game<B>>, scope: &StepScope, round: Round) -> Result<Tool>
where
    B: Backend + Send + Sync + 'static,
{
    let game = Arc::clone(game);
    let scope = scope.clone();
    Ok(Tool::new(
        prompts::current()?.questioner.tools.tool(
            "ask_questions",
            serde_json::to_value(schema_for!(AskQuestions))
                .expect("ask_questions schema should serialize"),
        )?,
        move |context| {
            let arguments = context.arguments.to_string();
            let game = Arc::clone(&game);
            // The Storyteller's turns are nested under this call.
            let scope = scope.under(context.tool_execution_id);
            Box::pin(async move {
                let questions = match parse_questions(&arguments) {
                    Ok(questions) => questions,
                    Err(error) => return Ok(ToolOutcome::rejected(error)),
                };
                let result = match round {
                    Round::Summary => game.summary_round(&scope, &questions).await?,
                    Round::Objects => game.objects_round(&scope, &questions).await?,
                };
                Ok(ToolOutcome::Completed(result))
            }) as ToolFuture
        },
    ))
}

/// `count` of the items, picked at random, as one bullet per line.
fn sampled_bullets(items: &[String], count: usize) -> String {
    use rand::seq::IndexedRandom;
    let picked: Vec<String> = items
        .choose_multiple(&mut rand::rng(), count)
        .cloned()
        .collect();
    bullets(&picked)
}

/// The names the text marks with double square brackets, once each, in order.
fn marked_names(text: &str) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    for part in text.split("[[").skip(1) {
        let Some((name, _)) = part.split_once("]]") else {
            continue;
        };
        let name = name.trim();
        if !name.is_empty() && !names.iter().any(|seen| seen.eq_ignore_ascii_case(name)) {
            names.push(name.to_string());
        }
    }
    names
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

/// What the Storyteller did to the world's objects, one change per line. An
/// object that was reviewed and no longer is has been modified.
fn describe_changes(before: &[ReviewEntry], after: &[ReviewEntry]) -> String {
    let find = |list: &'_ [ReviewEntry], entry: &ReviewEntry| -> Option<usize> {
        list.iter()
            .position(|other| other.kind == entry.kind && other.id == entry.id)
    };
    let mut lines = String::new();
    for entry in after {
        match find(before, entry).map(|index| &before[index]) {
            None => lines.push_str(&format!(
                "- added {} {} {}\n",
                entry.kind, entry.id, entry.name
            )),
            Some(old) if old.reviewed && !entry.reviewed || old.name != entry.name => lines
                .push_str(&format!(
                    "- changed {} {} {}\n",
                    entry.kind, entry.id, entry.name
                )),
            Some(_) => {}
        }
    }
    for entry in before {
        if find(after, entry).is_none() {
            lines.push_str(&format!(
                "- removed {} {} {}\n",
                entry.kind, entry.id, entry.name
            ));
        }
    }
    if lines.is_empty() {
        lines.push_str("- none\n");
    }
    lines
}

/// The seed and summary as the agents are told them, with stand-ins for what
/// does not exist yet.
fn seed_and_summary(prompts: &prompts::Prompts, generation: &Generation) -> (String, String) {
    let or = |text: &str, none: &str| {
        if text.trim().is_empty() {
            none.to_string()
        } else {
            text.to_string()
        }
    };
    (
        or(&generation.seed, &prompts.world.no_seed),
        or(&generation.summary, &prompts.world.no_summary),
    )
}

/// Where one generation step runs: its world, its sequence and its budget.
#[derive(Clone)]
struct StepScope {
    world: Arc<World>,
    sequence_id: i64,
    budget: Budget,
    call: CallContext,
}

impl StepScope {
    /// The same step, for agents called by the tool execution `tool_execution_id`.
    fn under(&self, tool_execution_id: i64) -> Self {
        Self {
            call: self.call.child(tool_execution_id),
            ..self.clone()
        }
    }
}

/// Which current state an agent is shown with a request.
#[derive(Clone, Copy)]
enum ContextKind {
    None,
    /// The seed and the world summary.
    Storyteller,
    /// The seed, the summary and the objects that exist, for creating more.
    StorytellerCreatingObjects,
    /// The Questioner's view while the summary develops: the summary alone,
    /// since the seed is in its first message.
    QuestionerSummary,
    /// The seed, the summary and the objects with their reviewed marks.
    Review,
}

/// One agent turn: the request goes into the branch's history as an
/// instruction and the agent answers, ending with a tool call only when it has
/// tools.
struct Request {
    branch: AgentBranch,
    role: String,
    text: String,
    /// Current state shown with every inference of this request, read again
    /// each time, so it is never stale and never piles up in the history.
    /// Developer Mode shows it in the agent's history when it changes.
    context: ContextKind,
    tools: Vec<Tool>,
    terminal_reminder: Option<String>,
    /// Replaces the model's own temperature for this request.
    temperature: Option<f32>,
}

impl Request {
    /// A request for a plain-text reply.
    fn reply(branch: AgentBranch, role: &str, text: String) -> Self {
        Self {
            branch,
            role: role.to_string(),
            text,
            context: ContextKind::None,
            tools: vec![],
            terminal_reminder: None,
            temperature: None,
        }
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
                world: Arc::clone(world),
                sequence_id: sequence.id,
                budget: budget.clone(),
                call: CallContext::root(Some(sequence.id)),
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

    async fn generate(
        self: &Arc<Self>,
        scope: &StepScope,
        generation: &Generation,
    ) -> Result<Advance> {
        let prompts = prompts::current()?;
        let questioner = &prompts.questioner;
        let storyteller = &prompts.storyteller;
        match generation.stage {
            GenerationStage::Meta => {
                let (seed, _) = seed_and_summary(&prompts, generation);
                let text = prompts::fill(
                    &questioner.meta_request,
                    &[
                        ("seed", &seed),
                        (
                            "examples",
                            &sampled_bullets(&questioner.example_questions, questioner.sample_size),
                        ),
                        (
                            "questions",
                            &sampled_bullets(&questioner.meta_questions, questioner.sample_size),
                        ),
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
                let settled = self
                    .generation_turn(
                        scope,
                        Request {
                            context: ContextKind::QuestionerSummary,
                            tools: summary_tools(self, scope)?,
                            terminal_reminder: Some(questioner.ask_reminder.clone()),
                            ..Request::reply(
                                generation.questioner,
                                &questioner.role,
                                questioner.ask_request.clone(),
                            )
                        },
                    )
                    .await?;
                let Content::ToolCalls(calls) = &settled.response.content else {
                    bail!("the Questioner ended its turn without calling a tool");
                };
                let call = calls.last().context("the Questioner made no tool call")?;
                match call.name.as_str() {
                    "world_summary_complete" => Ok(Advance::WorldSummaryComplete),
                    other => bail!("the Questioner ended its turn with `{other}`"),
                }
            }
            GenerationStage::Name => {
                let settled = self
                    .generation_turn(
                        scope,
                        self.storyteller_request(generation, storyteller.name_request.clone())?,
                    )
                    .await?;
                Ok(Advance::Name(reply_text(&settled)?.to_string()))
            }
            GenerationStage::CreateObjects => {
                self.storyteller_creates_objects(scope, &[]).await?;
                Ok(Advance::ObjectsCreated)
            }
            GenerationStage::Review => {
                let mut tools = vec![ask_questions_tool(self, scope, Round::Objects)?];
                tools.extend(authoring::review_tools(&self.store, scope.world.id)?);
                let settled = self
                    .generation_turn(
                        scope,
                        Request {
                            context: ContextKind::Review,
                            tools,
                            terminal_reminder: Some(questioner.review_reminder.clone()),
                            ..Request::reply(
                                generation.questioner,
                                &questioner.role,
                                questioner.review_request.clone(),
                            )
                        },
                    )
                    .await?;
                let Content::ToolCalls(calls) = &settled.response.content else {
                    bail!("the Questioner ended its review without calling a tool");
                };
                let call = calls.last().context("the Questioner made no tool call")?;
                match call.name.as_str() {
                    "core_objects_created" => Ok(Advance::CoreObjectsCreated),
                    other => bail!("the Questioner ended its review with `{other}`"),
                }
            }
            stage @ (GenerationStage::Failed | GenerationStage::Complete) => {
                bail!(
                    "world {} has no generation step at {stage:?}",
                    scope.world.id
                )
            }
        }
    }

    /// A request to the Storyteller, shown the seed and the one current summary
    /// as context, never as part of its chat.
    fn storyteller_request(&self, generation: &Generation, text: String) -> Result<Request> {
        Ok(Request {
            context: ContextKind::Storyteller,
            ..Request::reply(
                generation.storyteller,
                &prompts::current()?.storyteller.role,
                text,
            )
        })
    }

    /// The current state texts for a request, as they are right now.
    async fn context_texts(&self, scope: &StepScope, kind: ContextKind) -> Result<Vec<String>> {
        if matches!(kind, ContextKind::None) {
            return Ok(vec![]);
        }
        let prompts = prompts::current()?;
        let generation = self.current_generation(scope).await?;
        let (seed, summary) = seed_and_summary(&prompts, &generation);
        let world_id = scope.world.id;
        let mut texts = vec![];
        match kind {
            ContextKind::None => {}
            ContextKind::Storyteller | ContextKind::StorytellerCreatingObjects => {
                texts.push(prompts::fill(
                    &prompts.storyteller.world_state,
                    &[("seed", &seed), ("summary", &summary)],
                )?);
                if matches!(kind, ContextKind::StorytellerCreatingObjects) {
                    // What already exists, so it neither repeats nor invents what is there.
                    let objects: String = self
                        .store
                        .review_list(world_id)
                        .await?
                        .iter()
                        .map(|entry| format!("- {} {} {}\n", entry.kind, entry.id, entry.name))
                        .collect();
                    if !objects.is_empty() {
                        texts.push(prompts::fill(
                            &prompts.storyteller.objects_state,
                            &[("objects", &objects)],
                        )?);
                    }
                }
            }
            ContextKind::QuestionerSummary => texts.push(prompts::fill(
                &prompts.questioner.summary_state,
                &[("summary", &summary)],
            )?),
            ContextKind::Review => {
                let mut state = prompts::fill(
                    &prompts.questioner.review_state,
                    &[
                        ("seed", &seed),
                        ("summary", &summary),
                        ("objects", &self.review_objects(world_id).await?),
                    ],
                )?;
                if let Some(missing) = self.missing_note(world_id, &summary).await? {
                    state.push('\n');
                    state.push_str(&missing);
                }
                texts.push(state);
            }
        }
        Ok(texts)
    }

    /// The world's generation as it is now.
    async fn current_generation(&self, scope: &StepScope) -> Result<Generation> {
        self.store
            .next_generation(scope.world.id)
            .await?
            .with_context(|| format!("world {} is no longer being generated", scope.world.id))
    }

    /// The Storyteller answers the Questioner's questions one at a time as text,
    /// then rewrites the world summary, which replaces the old one. The result
    /// for the Questioner says so; the summary is in its context.
    async fn summary_round(
        self: &Arc<Self>,
        scope: &StepScope,
        questions: &[String],
    ) -> Result<String> {
        let prompts = prompts::current()?;
        let storyteller = &prompts.storyteller;
        let generation = self.current_generation(scope).await?;
        // Only the first round, before any summary exists, runs hot.
        let first_round = generation.summary.trim().is_empty();
        for (index, question) in questions.iter().enumerate() {
            let text = if index == 0 {
                prompts::fill(
                    &storyteller.round_start,
                    &[
                        ("count", &questions.len().to_string()),
                        ("question", question),
                    ],
                )?
            } else {
                prompts::fill(
                    &storyteller.next_question,
                    &[("number", &(index + 1).to_string()), ("question", question)],
                )?
            };
            let request = Request {
                temperature: first_round.then_some(storyteller.first_round_temperature),
                ..self.storyteller_request(&generation, text)?
            };
            self.generation_turn(scope, request).await?;
        }
        // A side inference: the new summary replaces the old one, so the
        // request and reply stay out of the chat, and the Storyteller never
        // has two versions in its context.
        let settled = self
            .generation_aside(
                scope,
                self.storyteller_request(&generation, storyteller.summary_request.clone())?,
            )
            .await?;
        let summary = reply_text(&settled)?.to_string();
        self.store
            .save_world_summary(scope.world.id, &summary)
            .await?;
        scope.world.publish(DevEvent::Changed {
            sequence_id: None,
            inference_id: None,
        });
        Ok(prompts.questioner.summary_result.clone())
    }

    /// The Storyteller changes the world to answer the Questioner's questions.
    /// The result for the Questioner is its own account and the changes made.
    async fn objects_round(
        self: &Arc<Self>,
        scope: &StepScope,
        questions: &[String],
    ) -> Result<String> {
        let before = self.store.review_list(scope.world.id).await?;
        let settled = self.storyteller_creates_objects(scope, questions).await?;
        let after = self.store.review_list(scope.world.id).await?;
        let prompts = prompts::current()?;
        let mut result = prompts::fill(
            &prompts.questioner.objects_result,
            &[
                ("reply", reply_text(&settled)?),
                ("changes", &describe_changes(&before, &after)),
            ],
        )?;
        let generation = self.current_generation(scope).await?;
        if let Some(missing) = self
            .missing_note(scope.world.id, &generation.summary)
            .await?
        {
            result.push('\n');
            result.push_str(&missing);
        }
        Ok(result)
    }

    /// One Storyteller turn creating objects: first the summary's objects, then
    /// whatever the Questioner's questions found missing.
    async fn storyteller_creates_objects(
        self: &Arc<Self>,
        scope: &StepScope,
        questions: &[String],
    ) -> Result<Settled> {
        let prompts = prompts::current()?;
        let storyteller = &prompts.storyteller;
        let generation = self.current_generation(scope).await?;
        let mut text = if questions.is_empty() {
            storyteller.create_objects_request.clone()
        } else {
            prompts::fill(
                &storyteller.fix_request,
                &[("questions", &bullets(questions))],
            )?
        };
        // A reminder, not a requirement: the Storyteller decides which marked
        // names need an object.
        let uncreated = self
            .uncreated_names(scope.world.id, &generation.summary)
            .await?;
        if !uncreated.is_empty() {
            text.push_str("\n\n");
            text.push_str(&prompts::fill(
                &storyteller.uncreated_mentions,
                &[("names", &uncreated.join(", "))],
            )?);
        }
        let request = Request {
            tools: authoring::storyteller_tools(&self.store, &scope.world)?,
            context: ContextKind::StorytellerCreatingObjects,
            ..self.storyteller_request(&generation, text)?
        };
        self.generation_turn(scope, request).await
    }

    /// The world's objects, one per line, each marked once the Questioner
    /// reviewed it and it has not changed since.
    async fn review_objects(&self, world_id: i64) -> Result<String> {
        Ok(self
            .store
            .review_list(world_id)
            .await?
            .iter()
            .map(|entry| {
                let mark = if entry.reviewed { "x" } else { " " };
                format!("- [{mark}] {} {} {}\n", entry.kind, entry.id, entry.name)
            })
            .collect())
    }

    /// Comparing the summary's marked names with the objects is plain code, so
    /// the Questioner is told the result rather than left to notice what is
    /// absent.
    async fn missing_note(&self, world_id: i64, summary: &str) -> Result<Option<String>> {
        let uncreated = self.uncreated_names(world_id, summary).await?;
        if uncreated.is_empty() {
            return Ok(None);
        }
        prompts::fill(
            &prompts::current()?.questioner.review_missing,
            &[("names", &uncreated.join(", "))],
        )
        .map(Some)
    }

    /// The names the summary marks that no object of the world has.
    async fn uncreated_names(&self, world_id: i64, summary: &str) -> Result<Vec<String>> {
        let created = self.store.object_names(world_id).await?;
        Ok(marked_names(summary)
            .into_iter()
            .filter(|name| !created.iter().any(|made| made.eq_ignore_ascii_case(name)))
            .collect())
    }

    async fn generation_turn(&self, scope: &StepScope, request: Request) -> Result<Settled> {
        self.store
            .append_instruction(
                request.branch,
                &Message::text(Role::User, request.text.clone()),
                scope.sequence_id,
            )
            .await
            .context("storing the generation request")?;
        let role = [Message::text(Role::System, &request.role)];
        let read =
            || -> agent::ContextFuture<'_> { Box::pin(self.context_texts(scope, request.context)) };
        agent::complete_with_call_context(
            &self.store,
            &self.backend,
            &scope.budget,
            self.turn(scope, &request, &role, DynamicContext::Fresh(&read)),
            &scope.call,
            |_| {},
            |_| {},
        )
        .await
        .context("running a generation turn")
    }

    /// The request is asked of the agent without entering its chat, and so is
    /// its reply.
    async fn generation_aside(&self, scope: &StepScope, request: Request) -> Result<Settled> {
        let role = [Message::text(Role::System, &request.role)];
        let read =
            || -> agent::ContextFuture<'_> { Box::pin(self.context_texts(scope, request.context)) };
        agent::complete_aside(
            &self.store,
            &self.backend,
            &scope.budget,
            self.turn(scope, &request, &role, DynamicContext::Fresh(&read)),
            &request.text,
            &scope.call,
        )
        .await
        .context("running a generation side inference")
    }

    fn turn<'a>(
        &'a self,
        scope: &StepScope,
        request: &'a Request,
        role: &'a [Message],
        dynamic_context: DynamicContext<'a>,
    ) -> agent::Turn<'a> {
        agent::Turn {
            events: scope.world.events.clone(),
            branch: request.branch,
            static_messages: role,
            dynamic_context,
            tools: &request.tools,
            terminal_reminder: request.terminal_reminder.as_deref(),
            sampling: Sampling {
                temperature: request.temperature.unwrap_or(self.sampling.temperature),
                ..self.sampling.clone()
            },
            model: &self.model,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::marked_names;

    #[test]
    fn only_names_in_double_brackets_are_marked_once_each() {
        assert_eq!(
            marked_names("[[Ysolde]] met [[the Bell]], then [[ysolde]] and [[ ]] left [[Open"),
            ["Ysolde", "the Bell"]
        );
        assert!(marked_names("Ysolde, [one] and 'the Bell'").is_empty());
    }
}
