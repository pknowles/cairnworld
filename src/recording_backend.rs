use std::time::Instant;

use anyhow::{Context, Result};

use crate::{
    llm::{Backend, Request, Response},
    store::Store,
};

pub struct RecordingBackend<B> {
    backend: B,
    store: Store,
    model: String,
}

impl<B> RecordingBackend<B> {
    pub fn new(backend: B, store: Store, model: String) -> Self {
        Self {
            backend,
            store,
            model,
        }
    }
}

impl<B: Backend> Backend for RecordingBackend<B> {
    async fn complete(&self, request: Request, on_token: impl FnMut(&str)) -> Result<Response> {
        let started_at = Instant::now();
        let response = self
            .backend
            .complete(request.clone(), on_token)
            .await
            .context("running recorded inference")?;
        self.store
            .record(
                &request,
                &response,
                &self.model,
                u64::try_from(started_at.elapsed().as_millis())
                    .context("inference duration exceeds supported range")?,
            )
            .await
            .context("recording completed inference")?;
        Ok(response)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::{Content, Message, Role, Sampling, Usage};

    struct FakeBackend;

    impl Backend for FakeBackend {
        async fn complete(
            &self,
            _request: Request,
            mut on_token: impl FnMut(&str),
        ) -> Result<Response> {
            on_token("streamed");
            Ok(Response {
                content: Content::Text("streamed".to_string()),
                usage: Usage {
                    input_tokens: 3,
                    output_tokens: 1,
                },
            })
        }
    }

    #[tokio::test]
    async fn records_the_same_request_that_it_streams() {
        let path = std::env::temp_dir().join(format!(
            "cairnworld-recording-test-{}-{}.sqlite",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system clock before Unix epoch")
                .as_nanos()
        ));
        let store = Store::open(&path).await.expect("store should open");
        let backend = RecordingBackend::new(FakeBackend, store.clone(), "fake-model".to_string());
        let request = Request {
            messages: vec![Message {
                role: Role::User,
                content: "Say streamed".to_string(),
            }],
            tools: vec![],
            sampling: Sampling { temperature: 0.0 },
        };
        let mut tokens = String::new();
        let response = backend
            .complete(request.clone(), |token| tokens.push_str(token))
            .await
            .expect("recorded completion should succeed");

        assert_eq!(tokens, "streamed");
        assert_eq!(response.content, Content::Text("streamed".to_string()));
        let recorded = store
            .reconstruct_inference(1)
            .await
            .expect("recorded completion should reconstruct");
        assert_eq!(recorded.request, request);
        assert_eq!(recorded.response, response);
        assert_eq!(recorded.model, "fake-model");
        drop(backend);
        store.close().await;
        std::fs::remove_file(path).expect("test database should be removable");
    }
}
