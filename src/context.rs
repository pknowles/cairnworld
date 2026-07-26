use std::time::Instant;

use anyhow::{Context, Result};

use crate::{
    llm::{Backend, Message, Response, Sampling},
    store::{InferenceOutcome, Segment, Store},
};

pub async fn complete<B: Backend>(
    store: &Store,
    backend: &B,
    agent_id: i64,
    static_messages: &[Message],
    sampling: Sampling,
    model: &str,
    on_token: impl FnMut(&str),
) -> Result<Response> {
    let mut segments = Vec::new();
    for message in static_messages {
        let text = store
            .put_text(&message.content)
            .await
            .context("storing static context text")?;
        segments.push(Segment::Text {
            text,
            role: message.role.clone(),
        });
    }
    if let Some(messages) = store
        .message_segment(agent_id)
        .await
        .context("assembling agent message context")?
    {
        segments.push(messages);
    }
    complete_recipe(
        store, backend, agent_id, &segments, sampling, model, on_token,
    )
    .await
}

pub async fn complete_recipe<B: Backend>(
    store: &Store,
    backend: &B,
    agent_id: i64,
    segments: &[Segment],
    sampling: Sampling,
    model: &str,
    on_token: impl FnMut(&str),
) -> Result<Response> {
    let request = store
        .request_for_segments(agent_id, segments, sampling)
        .await
        .context("assembling inference request")?;
    let started_at = Instant::now();
    match backend.complete(request.clone(), on_token).await {
        Ok(response) => {
            store
                .record_inference(
                    agent_id,
                    segments,
                    &request,
                    InferenceOutcome::Response(response.clone()),
                    model,
                    u64::try_from(started_at.elapsed().as_millis())
                        .context("inference duration exceeds supported range")?,
                )
                .await
                .context("recording completed inference")?;
            Ok(response)
        }
        Err(error) => {
            store
                .record_inference(
                    agent_id,
                    segments,
                    &request,
                    InferenceOutcome::Error(format!("{error:#}")),
                    model,
                    u64::try_from(started_at.elapsed().as_millis())
                        .context("inference duration exceeds supported range")?,
                )
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
            _on_token: impl FnMut(&str),
        ) -> Result<Response> {
            anyhow::bail!("connection lost")
        }
    }

    impl Backend for StreamingBackend {
        async fn complete(
            &self,
            _request: crate::llm::Request,
            mut on_token: impl FnMut(&str),
        ) -> Result<Response> {
            on_token("hello");
            Ok(Response {
                content: Content::Text("hello".to_string()),
                usage: Usage {
                    input_tokens: 3,
                    output_tokens: 1,
                },
            })
        }
    }

    async fn test_agent(store: &Store) -> i64 {
        let world = store.create_world("test world").await.unwrap();
        let agent = store
            .create_agent(world, "sandbox", "test agent")
            .await
            .unwrap();
        store
            .append_message(
                agent,
                &Message {
                    role: Role::User,
                    content: "Hello".to_string(),
                },
            )
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
        let response = complete(
            &store,
            &StreamingBackend,
            agent,
            &[Message {
                role: Role::System,
                content: "Be concise.".to_string(),
            }],
            Sampling { temperature: 0.0 },
            "streaming-model",
            |token| streamed.push_str(token),
        )
        .await
        .unwrap();
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

        let error = complete(
            &store,
            &FailingBackend,
            agent,
            &[],
            Sampling { temperature: 0.0 },
            "failing-model",
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
