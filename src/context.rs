use std::time::Instant;

use anyhow::{Context, Result};

use crate::{
    llm::{Backend, Message, MessageContent, Response, Sampling, ToolDefinition},
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
    on_token: impl FnMut(&str) + Send,
) -> Result<Completion> {
    let segments = segments(
        store,
        completion.agent_id,
        completion.static_messages,
        completion.tools,
    )
    .await?;
    complete_recipe(
        store,
        backend,
        RecipeCompletion {
            agent_id: completion.agent_id,
            sequence_id: completion.sequence_id,
            parent_inference_id: completion.parent_inference_id,
            segments: &segments,
            sampling: completion.sampling,
            model: completion.model,
        },
        on_token,
    )
    .await
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

#[derive(Debug)]
pub struct Completion {
    pub response: Response,
    pub inference_id: i64,
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
    use super::*;
    use crate::{
        llm::{Content, Role, Usage},
        store::RecordedOutcome,
    };

    struct FailingBackend;

    struct StreamingBackend;

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
        let path = std::env::temp_dir().join(format!(
            "cairnworld-context-test-{}-{}.sqlite",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system clock before Unix epoch")
                .as_nanos()
        ));
        let store = Store::open(&path).await.expect("store should open");
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
                    enable_thinking: false,
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
        drop(store);
        std::fs::remove_file(path).unwrap();
    }

    #[tokio::test]
    async fn failed_completion_is_reconstructable_without_an_assistant_message() {
        let path = std::env::temp_dir().join(format!(
            "cairnworld-context-test-{}-{}.sqlite",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system clock before Unix epoch")
                .as_nanos()
        ));
        let store = Store::open(&path).await.expect("store should open");
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
                    enable_thinking: false,
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
        drop(store);
        std::fs::remove_file(path).unwrap();
    }
}
