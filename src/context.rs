use std::time::Instant;

use anyhow::{Context, Result, ensure};

use crate::{
    llm::{
        Backend, ContextCapacityExceeded, Message, MessageContent, Response, Sampling,
        ToolDefinition,
    },
    store::{AgentBranch, CompactionPolicy, InferenceInput, InferenceOutcome, Segment, Store},
};

/// Everything required to record one agent inference from its visible context.
pub struct RecordedCompletion<'a> {
    pub agent_id: i64,
    /// The already-validated branch this completion reads and appends to.
    pub branch: AgentBranch,
    pub sequence_id: Option<i64>,
    pub invocation_id: i64,
    pub static_messages: &'a [Message],
    pub dynamic_context: &'a [&'a str],
    pub tools: &'a [ToolDefinition],
    pub sampling: Sampling,
    pub model: &'a str,
    pub compact_before_next_input_tokens: usize,
}

/// Runs one recipe as an ordinary chat turn and finishes it through
/// `finish_chat_inference`: the outcome, its own message (a tool-calling
/// reply or the settled final reply) and any due compaction obligation, all
/// in one transaction - see `complete_recipe`'s doc comment. Compaction is
/// enqueued only when the outcome settled with a text reply whose exact
/// next-context size reaches `completion.compact_before_next_input_tokens`.
pub async fn complete_recorded<B: Backend>(
    store: &Store,
    backend: &B,
    completion: RecordedCompletion<'_>,
    mut on_token: impl FnMut(&str) + Send,
) -> Result<Completion> {
    let mut last_fixed_kv_rejection = None;
    loop {
        let segments = segments(
            store,
            completion.branch,
            completion.static_messages,
            completion.dynamic_context,
            completion.tools,
        )
        .await?;
        let static_segments_snapshot = static_segments(&segments);
        match complete_recipe(
            store,
            backend,
            RecipeCompletion {
                agent_id: completion.agent_id,
                sequence_id: completion.sequence_id,
                invocation_id: completion.invocation_id,
                segments: &segments,
                sampling: completion.sampling.clone(),
                model: completion.model,
            },
            CompactionPolicy {
                sequence_id: completion.sequence_id,
                compact_before_next_input_tokens: completion.compact_before_next_input_tokens,
                sampling: &completion.sampling,
                model: completion.model,
                static_segments: &static_segments_snapshot,
            },
            &mut on_token,
        )
        .await
        {
            Ok(result) => return Ok(result),
            Err(error) if error.downcast_ref::<ContextCapacityExceeded>().is_some() => {
                // This is the one normal-agent completion boundary. A tool
                // result can make its persisted history exceed fixed KV
                // capacity before a final reply exists, so persist the same
                // deferred obligation here, wait for it, and retry. Candidate
                // summaries are admitted only by attempting real inference.
                let capacity = error
                    .downcast_ref::<ContextCapacityExceeded>()
                    .expect("capacity error was checked above");
                let ContextCapacityExceeded::FixedKv {
                    requested_tokens, ..
                } = capacity;
                if let Some(previous) = last_fixed_kv_rejection {
                    ensure!(
                        *requested_tokens < previous,
                        "compaction did not reduce the fixed-KV request: was {previous} tokens and is {requested_tokens} tokens"
                    );
                }
                last_fixed_kv_rejection = Some(*requested_tokens);
                let static_segments = static_segments(&segments);
                store
                    .enqueue_capacity_compaction(
                        completion.agent_id,
                        Some(completion.branch.branch_id()),
                        *requested_tokens,
                        CompactionPolicy {
                            sequence_id: completion.sequence_id,
                            compact_before_next_input_tokens: completion
                                .compact_before_next_input_tokens,
                            sampling: &completion.sampling,
                            model: completion.model,
                            static_segments: &static_segments,
                        },
                    )
                    .await
                    .context("persisting capacity recovery compaction")?;
                backend
                    .after_agent(
                        store,
                        completion.agent_id,
                        Some(completion.branch.branch_id()),
                    )
                    .await
                    .context("admitting capacity recovery compaction")?;
                backend
                    .before_agent(
                        store,
                        completion.agent_id,
                        Some(completion.branch.branch_id()),
                    )
                    .await
                    .context("waiting for capacity recovery compaction")?;
                let still_pending = store
                    .pending_compaction_for_branch(completion.branch.branch_id())
                    .await?;
                ensure!(
                    still_pending.is_none(),
                    "backend did not resolve capacity recovery compaction for agent {}; use the scheduled model backend",
                    completion.agent_id
                );
            }
            Err(error) => return Err(error),
        }
    }
}

pub async fn segments(
    store: &Store,
    branch: AgentBranch,
    static_messages: &[Message],
    dynamic_context: &[&str],
    tools: &[ToolDefinition],
) -> Result<Vec<Segment>> {
    let mut segments = Vec::new();
    for message in static_messages {
        let MessageContent::Text(content) = &message.content else {
            anyhow::bail!("static context messages must contain text");
        };
        let text = store
            .store_prompt_text(content)
            .await
            .context("storing static context text")?;
        segments.push(Segment::Text {
            text,
            role: message.role.clone(),
        });
    }
    for context in dynamic_context {
        segments.push(Segment::Context {
            context: store
                .store_prompt_text(context)
                .await
                .context("storing dynamic context")?,
        });
    }
    if !tools.is_empty() {
        let definitions = serde_json::to_string(tools).context("serializing tool definitions")?;
        segments.push(Segment::Tools {
            text: store
                .store_prompt_text(&definitions)
                .await
                .context("storing tool definitions")?,
        });
    }
    segments.extend(
        store
            .live_segments(branch)
            .await
            .context("assembling agent message context")?,
    );
    Ok(segments)
}

/// Keep the stored static prompt/tool portion of a normal recipe. Deferred
/// compaction rebuilds raw history live, but needs this exact static context if
/// it must measure an exceptional capacity fallback after a restart.
pub fn static_segments(segments: &[Segment]) -> Vec<Segment> {
    segments
        .iter()
        .filter(|segment| matches!(segment, Segment::Text { .. } | Segment::Tools { .. }))
        .cloned()
        .collect()
}

#[derive(Debug)]
pub struct Completion {
    pub response: Response,
    /// The inference this response came from, so a tool-calling turn can
    /// attribute its results to the call that requested them.
    pub inference_id: i64,
    /// The recorded_content row this response's own reply message
    /// references, so a caller can deliver that exact reference to other
    /// recipients (e.g. GM narration) instead of re-recording the same text.
    /// `None` for a compaction summary, which is never delivered to a
    /// recipient this way.
    pub output_content_id: Option<i64>,
}

/// Everything required to record one inference from already assembled segments.
pub struct RecipeCompletion<'a> {
    pub agent_id: i64,
    pub sequence_id: Option<i64>,
    /// Every inference belongs to one invocation (design.md's `invocation` is
    /// "one agent loop"). Detached replay (`main.rs`'s `replay` command)
    /// creates its own throwaway sequence/invocation for this rather than
    /// running outside the concept entirely, so `inference.invocation_id`
    /// never needs a NULL case.
    pub invocation_id: i64,
    pub segments: &'a [Segment],
    pub sampling: Sampling,
    pub model: &'a str,
}

/// A model outcome not yet recorded as complete. Every recipe completion
/// stops here rather than finishing itself, so the caller can choose the
/// finishing transaction its record kind needs: `finish_chat_inference` for
/// an ordinary agent turn, `finish_summary_inference` for compaction, or
/// plain `finish_inference` for detached replay - the three producer
/// operations design.md's Chat schema describes, all sharing this run step.
pub struct RunOutcome {
    pub inference_id: i64,
    pub agent_id: i64,
    pub outcome: InferenceOutcome,
    pub duration_ms: u64,
    /// `Ok` only when the outcome was a successful response; carries the
    /// same information as `outcome` in a form callers can inspect (e.g. tool
    /// calls vs. text) without re-matching `InferenceOutcome`. The original
    /// error and its type are preserved (not just its message) so a caller
    /// can still `downcast_ref` it - e.g. `complete_recorded`'s
    /// `ContextCapacityExceeded` retry, which needs the concrete error type,
    /// not a re-wrapped string.
    pub result: Result<Response>,
}

/// Run one recipe against the backend and record its start, without deciding
/// how to finish it - see `RunOutcome`.
pub async fn run_recipe<B: Backend>(
    store: &Store,
    backend: &B,
    completion: RecipeCompletion<'_>,
    on_token: impl FnMut(&str) + Send,
) -> Result<RunOutcome> {
    let request = store
        .request_for_segments(
            completion.agent_id,
            completion.segments,
            completion.sampling,
        )
        .await
        .context("assembling inference request")?;
    let inference_id = store
        .start_inference(InferenceInput {
            agent_id: completion.agent_id,
            sequence_id: completion.sequence_id,
            invocation_id: completion.invocation_id,
            segments: completion.segments,
            request: &request,
            model: completion.model,
        })
        .await
        .context("recording inference before execution")?;
    let started_at = Instant::now();
    let result = backend
        .complete_with_recording(request, on_token, store, inference_id)
        .await;
    let duration_ms = u64::try_from(started_at.elapsed().as_millis())
        .context("inference duration exceeds supported range")?;
    let outcome = match &result {
        Ok(response) => InferenceOutcome::Response(response.clone()),
        Err(error) => InferenceOutcome::Error(format!("{error:#}")),
    };
    Ok(RunOutcome {
        inference_id,
        agent_id: completion.agent_id,
        outcome,
        duration_ms,
        result,
    })
}

/// Run a recipe and finish it as an ordinary chat turn (design.md's
/// `finish_chat_inference`): the outcome, its own reply message (a
/// tool-calling turn's assistant message or the settled final reply) and any
/// due compaction obligation, in one transaction. `policy` supplies the
/// compaction threshold/recipe; whether this outcome is even eligible
/// (a settled text reply, not tool calls) is decided from the outcome itself.
pub async fn complete_recipe<B: Backend>(
    store: &Store,
    backend: &B,
    completion: RecipeCompletion<'_>,
    policy: CompactionPolicy<'_>,
    on_token: impl FnMut(&str) + Send,
) -> Result<Completion> {
    let run = run_recipe(store, backend, completion, on_token).await?;
    let succeeded = run.result.is_ok();
    let outcome = store
        .finish_chat_inference(
            run.inference_id,
            run.agent_id,
            run.outcome,
            run.duration_ms,
            policy,
        )
        .await
        .with_context(|| {
            if succeeded {
                format!("recording completed inference {}", run.inference_id)
            } else {
                format!("recording failed inference {}", run.inference_id)
            }
        })?;
    Ok(Completion {
        response: run.result.context("running inference")?,
        inference_id: run.inference_id,
        output_content_id: outcome.output_content_id,
    })
}

/// Run a recipe and finish it as a compaction inference (design.md's
/// `finish_summary_inference`): the outcome, its summary activation (when
/// `covered_ranges` names an eligible range) and completion of `job`'s
/// obligation, all in one transaction.
pub async fn complete_summary_recipe<B: Backend>(
    store: &Store,
    backend: &B,
    completion: RecipeCompletion<'_>,
    job: &crate::store::PendingCompaction,
    covered_ranges: Option<&[crate::store::PositionRange]>,
    summary_metadata: (&str, i64),
    on_token: impl FnMut(&str) + Send,
) -> Result<Completion> {
    let run = run_recipe(store, backend, completion, on_token).await?;
    let succeeded = run.result.is_ok();
    store
        .finish_summary_inference(
            run.inference_id,
            job,
            run.outcome,
            run.duration_ms,
            covered_ranges,
            summary_metadata,
        )
        .await
        .with_context(|| {
            if succeeded {
                format!(
                    "recording completed compaction inference {}",
                    run.inference_id
                )
            } else {
                format!("recording failed compaction inference {}", run.inference_id)
            }
        })?;
    Ok(Completion {
        response: run.result.context("running compaction inference")?,
        inference_id: run.inference_id,
        output_content_id: None,
    })
}

#[cfg(test)]
mod tests {
    use std::{collections::VecDeque, sync::Mutex};

    use super::*;
    use crate::{
        inference::InferenceScheduler,
        llm::{Content, Role, Usage},
        store::RecordedOutcome,
    };

    struct FailingBackend;

    struct StreamingBackend;

    struct CapacityBackend {
        responses: Mutex<VecDeque<Result<Response>>>,
    }

    impl Backend for FailingBackend {
        async fn complete(
            &self,
            _request: crate::llm::Request,
            _on_token: impl FnMut(&str) + Send,
        ) -> Result<Response> {
            anyhow::bail!("connection lost")
        }
    }

    impl Backend for StreamingBackend {
        async fn complete(
            &self,
            _request: crate::llm::Request,
            mut on_token: impl FnMut(&str) + Send,
        ) -> Result<Response> {
            on_token("hello");
            Ok(Response {
                content: Content::Text("hello".to_string()),
                reasoning: String::new(),
                usage: Usage {
                    input_tokens: 3,
                    output_tokens: 1,
                },
            })
        }
    }

    impl Backend for CapacityBackend {
        async fn complete(
            &self,
            _request: crate::llm::Request,
            _on_token: impl FnMut(&str) + Send,
        ) -> Result<Response> {
            self.responses
                .lock()
                .unwrap()
                .pop_front()
                .context("unexpected capacity-recovery inference")?
        }
    }

    async fn test_agent(store: &Store) -> i64 {
        let world = store.create_world("test world").await.unwrap();
        let agent = store.create_agent(world).await.unwrap();
        store
            .append_message(agent, &Message::text(Role::User, "Hello"))
            .await
            .unwrap();
        agent
    }

    /// A real invocation, so `RecordedCompletion`/`RecipeCompletion` can
    /// thread a genuine `invocation_id` rather than a synthetic one - every
    /// inference belongs to one (design.md's `invocation`).
    async fn test_invocation(store: &Store, agent: i64) -> i64 {
        let world_id = store.world_id_for_agent(agent).await.unwrap();
        let sequence = store.begin_sequence(world_id, "test").await.unwrap();
        store
            .start_invocation(Some(sequence.id), agent, None, None, None)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn assembled_completion_streams_and_records_a_history_recipe() {
        let db = crate::store::TestDatabase::new("context-test");
        let store = Store::open(db.path()).await.expect("store should open");
        let agent = test_agent(&store).await;
        let invocation_id = test_invocation(&store, agent).await;
        let mut streamed = String::new();
        let response = complete_recorded(
            &store,
            &StreamingBackend,
            RecordedCompletion {
                agent_id: agent,
                branch: store.agent_branch(agent).await.unwrap(),
                sequence_id: None,
                invocation_id,
                static_messages: &[Message::text(Role::System, "Be concise.")],
                dynamic_context: &[],
                tools: &[],
                sampling: Sampling {
                    temperature: 0.0,
                    ..Default::default()
                },
                model: "streaming-model",
                compact_before_next_input_tokens: usize::MAX,
            },
            |token| streamed.push_str(token),
        )
        .await
        .unwrap()
        .response;
        assert_eq!(streamed, "hello");
        assert_eq!(response.content, Content::Text("hello".to_string()));
        let recorded = store.reconstruct_inference(1).await.unwrap();
        assert_eq!(recorded.request.messages.len(), 2);
        assert_eq!(recorded.outcome, RecordedOutcome::Response(response));
    }

    #[tokio::test]
    async fn failed_completion_is_reconstructable_without_an_assistant_message() {
        let db = crate::store::TestDatabase::new("context-test");
        let store = Store::open(db.path()).await.expect("store should open");
        let agent = test_agent(&store).await;
        let invocation_id = test_invocation(&store, agent).await;

        let error = complete_recorded(
            &store,
            &FailingBackend,
            RecordedCompletion {
                agent_id: agent,
                branch: store.agent_branch(agent).await.unwrap(),
                sequence_id: None,
                invocation_id,
                static_messages: &[],
                dynamic_context: &[],
                tools: &[],
                sampling: Sampling {
                    temperature: 0.0,
                    ..Default::default()
                },
                model: "failing-model",
                compact_before_next_input_tokens: usize::MAX,
            },
            |_| {},
        )
        .await
        .expect_err("backend failure should propagate");
        assert!(error.to_string().contains("running inference"));
        let recorded = store.reconstruct_inference(1).await.unwrap();
        assert_eq!(recorded.request.messages.len(), 1);
        assert_eq!(
            recorded.outcome,
            RecordedOutcome::Error("connection lost".to_string())
        );
        assert!(
            store.pending_compaction(agent).await.unwrap().is_none(),
            "ordinary model failures must not be misclassified as compactable context pressure"
        );
    }

    #[tokio::test]
    async fn capacity_recovery_does_not_retry_an_unchanged_fixed_kv_request() {
        let db = crate::store::TestDatabase::new("context-test");
        let store = Store::open(db.path()).await.expect("store should open");
        let agent = test_agent(&store).await;
        let invocation_id = test_invocation(&store, agent).await;
        store
            .append_message(agent, &Message::text(Role::Assistant, "A first reply."))
            .await
            .unwrap();
        store
            .append_message(agent, &Message::text(Role::User, "The latest question."))
            .await
            .unwrap();
        let backend = InferenceScheduler::new(
            CapacityBackend {
                responses: Mutex::new(VecDeque::from([
                    Err(ContextCapacityExceeded::FixedKv {
                        requested_tokens: 101,
                        max_context_tokens: 100,
                    }
                    .into()),
                    Ok(Response {
                        content: Content::Text("A concise summary.".into()),
                        reasoning: String::new(),
                        usage: Usage {
                            input_tokens: 1,
                            output_tokens: 1,
                        },
                    }),
                    Err(ContextCapacityExceeded::FixedKv {
                        requested_tokens: 101,
                        max_context_tokens: 100,
                    }
                    .into()),
                ])),
            },
            crate::settings::Limits {
                max_concurrent_inferences: 1,
                max_inferences_per_chat: 8,
                max_inferences_total: 64,
                max_context_tokens: 100,
                max_output_tokens: 10,
                compact_before_next_input_tokens: 90,
                keep_tail_messages: 1,
                merge_branch_messages: 64,
                presence_departure_seconds: 60,
            },
        )
        .unwrap()
        .foreground();
        let error = complete_recorded(
            &store,
            &backend,
            RecordedCompletion {
                agent_id: agent,
                branch: store.agent_branch(agent).await.unwrap(),
                sequence_id: None,
                invocation_id,
                static_messages: &[],
                dynamic_context: &[],
                tools: &[],
                sampling: Sampling {
                    temperature: 0.0,
                    ..Default::default()
                },
                model: "capacity-test",
                compact_before_next_input_tokens: 90,
            },
            |_| {},
        )
        .await
        .expect_err("unchanged fixed-KV rejection must fail instead of looping");
        assert!(
            error
                .to_string()
                .contains("compaction did not reduce the fixed-KV request"),
            "unexpected error: {error:#}"
        );
        assert!(store.pending_compaction(agent).await.unwrap().is_none());
    }
}
