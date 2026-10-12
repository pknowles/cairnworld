use std::time::Instant;

use anyhow::{Context, Result, ensure};

use crate::{
    llm::{
        Backend, ContextCapacityExceeded, Message, MessageContent, Response, Sampling,
        ToolDefinition,
    },
    store::{InferenceOutcome, InferenceRecord, Segment, Store},
};

/// Everything required to record one agent inference from its visible context.
pub struct RecordedCompletion<'a> {
    pub agent_id: i64,
    pub sequence_id: Option<i64>,
    pub parent_inference_id: Option<i64>,
    pub static_messages: &'a [Message],
    pub tools: &'a [ToolDefinition],
    pub sampling: Sampling,
    pub model: &'a str,
}

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
            completion.agent_id,
            completion.static_messages,
            completion.tools,
        )
        .await?;
        match complete_recipe(
            store,
            backend,
            RecipeCompletion {
                agent_id: completion.agent_id,
                sequence_id: completion.sequence_id,
                parent_inference_id: completion.parent_inference_id,
                segments: &segments,
                sampling: completion.sampling.clone(),
                model: completion.model,
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
                        *requested_tokens,
                        &completion.sampling,
                        completion.model,
                        &static_segments,
                    )
                    .await
                    .context("persisting capacity recovery compaction")?;
                backend
                    .after_agent(store, completion.agent_id)
                    .await
                    .context("admitting capacity recovery compaction")?;
                backend
                    .before_agent(store, completion.agent_id)
                    .await
                    .context("waiting for capacity recovery compaction")?;
                ensure!(
                    store
                        .pending_compaction(completion.agent_id)
                        .await?
                        .is_none(),
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
    agent_id: i64,
    static_messages: &[Message],
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
            .history_segments(agent_id)
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
    pub inference_id: i64,
    /// Stored recipe used for this completion. A final reply retains its
    /// static subset with any deferred compaction job, while raw history is
    /// always rebuilt from the store.
    pub segments: Vec<Segment>,
}

/// Everything required to record one inference from already assembled segments.
pub struct RecipeCompletion<'a> {
    pub agent_id: i64,
    pub sequence_id: Option<i64>,
    pub parent_inference_id: Option<i64>,
    pub segments: &'a [Segment],
    pub sampling: Sampling,
    pub model: &'a str,
}

pub async fn complete_recipe<B: Backend>(
    store: &Store,
    backend: &B,
    completion: RecipeCompletion<'_>,
    on_token: impl FnMut(&str) + Send,
) -> Result<Completion> {
    let request = store
        .request_for_segments(
            completion.agent_id,
            completion.segments,
            completion.sampling,
        )
        .await
        .context("assembling inference request")?;
    let started_at = Instant::now();
    match backend.complete(request.clone(), on_token).await {
        Ok(response) => {
            let inference_id = store
                .record(InferenceRecord {
                    agent_id: completion.agent_id,
                    sequence_id: completion.sequence_id,
                    parent_inference_id: completion.parent_inference_id,
                    segments: completion.segments,
                    request: &request,
                    outcome: InferenceOutcome::Response(response.clone()),
                    model: completion.model,
                    duration_ms: u64::try_from(started_at.elapsed().as_millis())
                        .context("inference duration exceeds supported range")?,
                })
                .await
                .context("recording completed inference")?;
            Ok(Completion {
                response,
                inference_id,
                segments: completion.segments.to_vec(),
            })
        }
        Err(error) => {
            store
                .record(InferenceRecord {
                    agent_id: completion.agent_id,
                    sequence_id: completion.sequence_id,
                    parent_inference_id: completion.parent_inference_id,
                    segments: completion.segments,
                    request: &request,
                    outcome: InferenceOutcome::Error(format!("{error:#}")),
                    model: completion.model,
                    duration_ms: u64::try_from(started_at.elapsed().as_millis())
                        .context("inference duration exceeds supported range")?,
                })
                .await
                .with_context(|| format!("recording failed inference: {error:#}"))?;
            Err(error).context("running inference")
        }
    }
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

    #[tokio::test]
    async fn assembled_completion_streams_and_records_a_history_recipe() {
        let db = crate::store::TestDatabase::new("context-test");
        let store = Store::open(db.path()).await.expect("store should open");
        let agent = test_agent(&store).await;
        let mut streamed = String::new();
        let response = complete_recorded(
            &store,
            &StreamingBackend,
            RecordedCompletion {
                agent_id: agent,
                sequence_id: None,
                parent_inference_id: None,
                static_messages: &[Message::text(Role::System, "Be concise.")],
                tools: &[],
                sampling: Sampling {
                    temperature: 0.0,
                    ..Default::default()
                },
                model: "streaming-model",
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

        let error = complete_recorded(
            &store,
            &FailingBackend,
            RecordedCompletion {
                agent_id: agent,
                sequence_id: None,
                parent_inference_id: None,
                static_messages: &[],
                tools: &[],
                sampling: Sampling {
                    temperature: 0.0,
                    ..Default::default()
                },
                model: "failing-model",
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
            },
        )
        .unwrap()
        .foreground();
        let error = complete_recorded(
            &store,
            &backend,
            RecordedCompletion {
                agent_id: agent,
                sequence_id: None,
                parent_inference_id: None,
                static_messages: &[],
                tools: &[],
                sampling: Sampling {
                    temperature: 0.0,
                    ..Default::default()
                },
                model: "capacity-test",
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
