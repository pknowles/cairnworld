use anyhow::{Context, Result};
use mistralrs::{
    ChatCompletionChunkResponse, ChunkChoice, Delta, GgufModelBuilder, Model, RequestBuilder,
    Response as MrResponse, SamplingParams, TextMessageRole, TextMessages,
};

use crate::llm::{Backend, Content, Request, Response, Role, Usage};

pub struct MistralRsBackend {
    model: Model,
}

impl MistralRsBackend {
    pub async fn load(model_id_or_path: &str) -> Result<Self> {
        let (dir, file) = model_id_or_path.rsplit_once('/').context(
            "--model must be a path or repo id containing a GGUF filename, e.g. dir/model.gguf",
        )?;
        let model = GgufModelBuilder::new(dir, vec![file])
            .build()
            .await
            .with_context(|| format!("loading GGUF model from {model_id_or_path}"))?;
        Ok(Self { model })
    }
}

impl Backend for MistralRsBackend {
    async fn complete(&self, request: Request, mut on_token: impl FnMut(&str)) -> Result<Response> {
        let mut messages = TextMessages::new();
        for message in request.messages {
            let role = match message.role {
                Role::System => TextMessageRole::System,
                Role::User => TextMessageRole::User,
                Role::Assistant => TextMessageRole::Assistant,
                Role::Tool => TextMessageRole::Tool,
            };
            messages = messages.add_message(role, message.content);
        }
        let request_builder = RequestBuilder::from(messages)
            .set_sampling(SamplingParams::neutral())
            .set_sampler_temperature(request.sampling.temperature as f64);

        let mut stream = self
            .model
            .stream_chat_request(request_builder)
            .await
            .context("starting streamed inference")?;

        // The streaming path never emits a terminal `Response::Done`; the
        // last `Chunk` (carrying `finish_reason` and `usage`) is the
        // completion signal, so the final response is assembled from the
        // accumulated deltas rather than read back from the backend.
        let mut content = String::new();
        let mut usage = Usage {
            input_tokens: 0,
            output_tokens: 0,
        };
        loop {
            let chunk = stream
                .next()
                .await
                .context("inference stream ended without a final chunk")?;
            match chunk {
                MrResponse::Chunk(ChatCompletionChunkResponse {
                    choices,
                    usage: chunk_usage,
                    ..
                }) => {
                    let finished = choices
                        .first()
                        .is_some_and(|choice| choice.finish_reason.is_some());
                    if let Some(ChunkChoice {
                        delta:
                            Delta {
                                content: Some(delta),
                                ..
                            },
                        ..
                    }) = choices.first()
                    {
                        on_token(delta);
                        content.push_str(delta);
                    }
                    if let Some(chunk_usage) = chunk_usage {
                        usage = Usage {
                            input_tokens: chunk_usage.prompt_tokens,
                            output_tokens: chunk_usage.completion_tokens,
                        };
                    }
                    if finished {
                        return Ok(Response {
                            content: Content::Text(content),
                            usage,
                        });
                    }
                }
                MrResponse::ModelError(message, _) => {
                    anyhow::bail!("model error during inference: {message}")
                }
                MrResponse::InternalError(error) | MrResponse::ValidationError(error) => {
                    return Err(anyhow::anyhow!(error).context("inference stream error"));
                }
                _ => anyhow::bail!("unexpected response variant from chat stream"),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::{Message, Sampling};
    use crate::settings::Settings;

    #[tokio::test]
    #[ignore = "requires a CUDA-capable device and a configured GGUF model"]
    async fn stream_and_final_response_agree() {
        let model_path = Settings::load()
            .expect("settings should load")
            .model
            .expect("set `model` in local.toml to a local GGUF path to run this test");
        let backend = MistralRsBackend::load(&model_path)
            .await
            .expect("model should load");

        let request = Request {
            messages: vec![Message {
                role: Role::User,
                content: "Reply with exactly the word: hello".to_string(),
            }],
            tools: vec![],
            sampling: Sampling { temperature: 0.0 },
        };

        let mut streamed = String::new();
        let response = backend
            .complete(request, |token| streamed.push_str(token))
            .await
            .expect("completion should succeed");

        let Content::Text(final_text) = response.content else {
            panic!("expected text content");
        };
        assert!(!streamed.is_empty());
        assert!(!final_text.is_empty());
        assert_eq!(streamed, final_text);
    }
}
