use std::{collections::BTreeMap, path::Path};

use anyhow::{Context, Result, ensure};
use mistralrs::{
    CalledFunction, ChatCompletionChunkResponse, ChunkChoice, Delta, Function, GgufModelBuilder,
    Model, RequestBuilder, Response as MrResponse, SamplingParams, TextMessageRole, Tool,
    ToolCallResponse, ToolCallType, ToolType,
};

use crate::llm::{
    Backend, Content, MessageContent, Request, Response, Role, ToolCall, ToolDefinition, Usage,
};

pub struct MistralRsBackend {
    model: Model,
}

impl MistralRsBackend {
    /// `chat_template` overrides the one embedded in the GGUF. Some files ship
    /// a template with no tool support at all - the Hermes 3 GGUF carries bare
    /// ChatML, which silently drops every tool definition - so the tool surface
    /// is unusable without supplying one (see templates/).
    pub async fn load(model_id_or_path: &str, chat_template: Option<&Path>) -> Result<Self> {
        let (dir, file) = model_id_or_path.rsplit_once('/').context(
            "--model must be a path or repo id containing a GGUF filename, e.g. dir/model.gguf",
        )?;
        let mut builder = GgufModelBuilder::new(dir, vec![file]);
        if let Some(template) = chat_template {
            ensure!(
                template.exists(),
                "chat template {} does not exist",
                template.display()
            );
            builder = builder.with_chat_template(template.display().to_string());
        }
        let model = builder
            .build()
            .await
            .with_context(|| format!("loading GGUF model from {model_id_or_path}"))?;
        Ok(Self { model })
    }
}

fn request_builder(request: Request) -> Result<RequestBuilder> {
    let mut request_builder = RequestBuilder::new();
    for message in request.messages {
        match message.content {
            MessageContent::Text(content) => {
                let role = match message.role {
                    Role::System => TextMessageRole::System,
                    Role::User => TextMessageRole::User,
                    Role::Assistant => TextMessageRole::Assistant,
                    Role::Tool => TextMessageRole::Tool,
                };
                request_builder = request_builder.add_message(role, content);
            }
            MessageContent::ToolCalls(calls) => {
                request_builder = request_builder.add_message_with_tool_call(
                    TextMessageRole::Assistant,
                    "",
                    calls.into_iter().enumerate().map(to_mistral_call).collect(),
                )
            }
            MessageContent::ToolResult {
                tool_call_id,
                content,
            } => request_builder = request_builder.add_tool_message(content, tool_call_id),
        }
    }
    Ok(request_builder
        .set_sampling(SamplingParams::neutral())
        .set_sampler_temperature(request.sampling.temperature as f64)
        .set_tools(
            request
                .tools
                .iter()
                .map(to_mistral_tool)
                .collect::<Result<Vec<_>>>()
                .context("translating tool definitions for mistral.rs")?,
        )
        .enable_thinking(request.sampling.enable_thinking))
}

impl Backend for MistralRsBackend {
    async fn complete(&self, request: Request, mut on_token: impl FnMut(&str)) -> Result<Response> {
        let request_builder = request_builder(request)?;

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
        let mut reasoning = String::new();
        let mut tool_calls = BTreeMap::new();
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
                    if let Some(ChunkChoice {
                        delta:
                            Delta {
                                reasoning_content: Some(delta),
                                ..
                            },
                        ..
                    }) = choices.first()
                    {
                        reasoning.push_str(delta);
                    }
                    if let Some(ChunkChoice {
                        delta:
                            Delta {
                                tool_calls: Some(deltas),
                                ..
                            },
                        ..
                    }) = choices.first()
                    {
                        for delta in deltas {
                            append_tool_delta(&mut tool_calls, delta)?;
                        }
                    }
                    if let Some(chunk_usage) = chunk_usage {
                        usage = Usage {
                            input_tokens: chunk_usage.prompt_tokens,
                            output_tokens: chunk_usage.completion_tokens,
                        };
                    }
                    if finished {
                        return Ok(assemble(
                            content,
                            reasoning,
                            tool_calls.into_values().collect(),
                            usage,
                        ));
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

/// Build the final response from one generation's accumulated parts.
///
/// Models routinely narrate before calling a tool - observed from Hermes 3
/// ("we need to make a DEX save...") and Llama 3.1. The call is the action the
/// model took; the prose is working-out, so it joins reasoning rather than
/// being treated as a reply or rejected. Reasoning is recorded and shown in the
/// developer view but never re-fed into a later context, exactly like a
/// model's `<think>` output.
fn assemble(content: String, reasoning: String, calls: Vec<ToolCall>, usage: Usage) -> Response {
    if calls.is_empty() {
        return Response {
            content: Content::Text(content),
            reasoning,
            usage,
        };
    }
    let reasoning = match (reasoning.is_empty(), content.is_empty()) {
        (_, true) => reasoning,
        (true, false) => content,
        (false, false) => format!("{reasoning}\n{content}"),
    };
    Response {
        content: Content::ToolCalls(calls),
        reasoning,
        usage,
    }
}

fn to_mistral_tool(tool: &ToolDefinition) -> Result<Tool> {
    Ok(Tool {
        tp: ToolType::Function,
        function: Function {
            description: Some(tool.description.clone()),
            name: tool.name.clone(),
            parameters: Some(
                serde_json::from_value(tool.schema.clone())
                    .with_context(|| format!("tool `{}` has a non-object schema", tool.name))?,
            ),
            // Constrain generation to the argument schema. Small models
            // otherwise emit arguments that are only schema-shaped: observed
            // quoted integers and a markdown-escaped key name.
            strict: Some(true),
        },
    })
}

fn to_mistral_call((index, call): (usize, ToolCall)) -> ToolCallResponse {
    ToolCallResponse {
        index,
        id: call.id,
        tp: ToolCallType::Function,
        function: CalledFunction {
            name: call.name,
            arguments: call.arguments,
        },
    }
}

fn append_tool_delta(
    calls: &mut BTreeMap<usize, ToolCall>,
    delta: &ToolCallResponse,
) -> Result<()> {
    match calls.get_mut(&delta.index) {
        Some(call) => {
            ensure!(call.id == delta.id, "tool call {} changed id", delta.index);
            if !delta.function.name.is_empty() {
                ensure!(
                    call.name == delta.function.name,
                    "tool call {} changed name",
                    delta.index
                );
            }
            call.arguments.push_str(&delta.function.arguments);
        }
        None => {
            calls.insert(
                delta.index,
                ToolCall {
                    id: delta.id.clone(),
                    name: delta.function.name.clone(),
                    arguments: delta.function.arguments.clone(),
                },
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::{Message, Sampling};
    use crate::settings::Settings;

    #[tokio::test]
    #[ignore = "requires a CUDA-capable device and a configured GGUF model"]
    async fn stream_and_final_response_agree() {
        let settings = Settings::load().expect("settings should load");
        let model = settings
            .model(None)
            .expect("configure a model in local.toml or default.toml to run this test");
        let backend = MistralRsBackend::load(&model.path, model.chat_template.as_deref())
            .await
            .expect("model should load");

        let request = Request {
            messages: vec![Message::text(
                Role::User,
                "Reply with exactly the word: hello",
            )],
            tools: vec![],
            sampling: Sampling {
                temperature: 0.0,
                enable_thinking: false,
            },
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

    #[test]
    fn narration_alongside_a_tool_call_becomes_reasoning() {
        // Verbatim shape observed from Hermes 3: prose explaining the save,
        // streamed alongside a structured call. The call must survive and the
        // prose must not be mistaken for a reply to the player.
        let call = ToolCall {
            id: "c1".to_string(),
            name: "save".to_string(),
            arguments: r#"{"character":"Rook"}"#.to_string(),
        };
        let usage = Usage {
            input_tokens: 1,
            output_tokens: 1,
        };
        let response = assemble(
            "we need to make a DEX save.".to_string(),
            String::new(),
            vec![call.clone()],
            usage.clone(),
        );
        assert_eq!(response.content, Content::ToolCalls(vec![call.clone()]));
        assert_eq!(response.reasoning, "we need to make a DEX save.");

        // Where the model also emitted real reasoning, both are kept.
        let response = assemble(
            "then the save.".to_string(),
            "first the ledge.".to_string(),
            vec![call.clone()],
            usage.clone(),
        );
        assert_eq!(response.reasoning, "first the ledge.\nthen the save.");

        // With no call, prose is the reply and must stay content.
        let response = assemble("You slip.".to_string(), String::new(), vec![], usage);
        assert_eq!(response.content, Content::Text("You slip.".to_string()));
        assert!(response.reasoning.is_empty());
    }

    #[test]
    fn fragmented_tool_arguments_are_assembled_by_call_index() {
        let mut calls = BTreeMap::new();
        append_tool_delta(
            &mut calls,
            &ToolCallResponse {
                index: 1,
                id: "second".to_string(),
                tp: ToolCallType::Function,
                function: CalledFunction {
                    name: "save".to_string(),
                    arguments: r#"{"character":"Ma"#.to_string(),
                },
            },
        )
        .unwrap();
        // Deltas arrive out of order and a later call may start before an
        // earlier one finishes, so assembly keys on index, not arrival.
        append_tool_delta(
            &mut calls,
            &ToolCallResponse {
                index: 0,
                id: "first".to_string(),
                tp: ToolCallType::Function,
                function: CalledFunction {
                    name: "save".to_string(),
                    arguments: r#"{"character":"Rook"}"#.to_string(),
                },
            },
        )
        .unwrap();
        append_tool_delta(
            &mut calls,
            &ToolCallResponse {
                index: 1,
                id: "second".to_string(),
                tp: ToolCallType::Function,
                function: CalledFunction {
                    name: String::new(),
                    arguments: r#"ra"}"#.to_string(),
                },
            },
        )
        .unwrap();
        assert_eq!(
            calls.into_values().collect::<Vec<_>>(),
            vec![
                ToolCall {
                    id: "first".to_string(),
                    name: "save".to_string(),
                    arguments: r#"{"character":"Rook"}"#.to_string()
                },
                ToolCall {
                    id: "second".to_string(),
                    name: "save".to_string(),
                    arguments: r#"{"character":"Mara"}"#.to_string()
                },
            ]
        );
    }
}
