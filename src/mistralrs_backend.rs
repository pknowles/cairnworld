use std::{collections::BTreeMap, path::Path, time::Instant};

use anyhow::{Context, Result, ensure};
use mistralrs::{
    CalledFunction, ChatCompletionChunkResponse, ChunkChoice, Delta, DeviceLayerMapMetadata,
    DeviceMapMetadata, DeviceMapSetting, Function, GgufModelBuilder, MemoryGpuConfig, Model,
    PagedAttentionMetaBuilder, RequestBuilder, Response as MrResponse, SamplingParams,
    TextMessageRole, Tool, ToolCallResponse, ToolCallType, ToolType, best_device,
};

use crate::llm::{
    Backend, Content, ContextCapacityExceeded, MessageContent, Request, Response, Role, ToolCall,
    ToolDefinition, Usage,
};
use crate::settings::Limits;

pub struct MistralRsBackend {
    model: Model,
    max_context_tokens: usize,
    max_output_tokens: usize,
}

impl MistralRsBackend {
    /// `chat_template` overrides the one embedded in the GGUF. Some files ship
    /// a template with no tool support at all - the Hermes 3 GGUF carries bare
    /// ChatML, which silently drops every tool definition - so the tool surface
    /// is unusable without supplying one (see templates/).
    pub async fn load(
        model_id_or_path: &str,
        chat_template: Option<&Path>,
        source_model: Option<&str>,
        limits: Limits,
        allow_cpu: bool,
    ) -> Result<Self> {
        let started = Instant::now();
        limits.validate()?;
        let cache_context_tokens = limits.cache_context_tokens()?;
        tracing::info!(
            model = model_id_or_path,
            chat_template = chat_template.map(|path| path.display().to_string()),
            source_model,
            max_concurrent_inferences = limits.max_concurrent_inferences,
            max_context_tokens = limits.max_context_tokens,
            max_output_tokens = limits.max_output_tokens,
            cache_context_tokens,
            "starting game model load"
        );
        let (dir, file) = model_id_or_path.rsplit_once('/').context(
            "--model must be a path or repo id containing a GGUF filename, e.g. dir/model.gguf",
        )?;
        let gpu_used_before_mib = if allow_cpu {
            0
        } else {
            log_gpu_memory("before model load")?
        };
        ensure!(
            mistralrs::paged_attn_supported(),
            "this mistral.rs build does not support PagedAttention, which Cairnworld requires for fixed VRAM allocation"
        );
        let paged_attention = PagedAttentionMetaBuilder::default()
            .with_gpu_memory(MemoryGpuConfig::ContextSize(cache_context_tokens))
            .build()
            .context("configuring fixed paged KV-cache capacity")?;
        let mut builder = GgufModelBuilder::new(dir, vec![file])
            .with_max_num_seqs(limits.max_concurrent_inferences)
            .with_prefix_cache_n(None)
            .with_paged_attn(paged_attention);
        if let Some(source_model) = source_model {
            builder = builder.with_tok_model_id(source_model);
        }
        if !allow_cpu {
            builder = builder.with_device_mapping(DeviceMapSetting::Map(
                DeviceMapMetadata::from_num_device_layers(vec![DeviceLayerMapMetadata {
                    ordinal: 0,
                    layers: usize::MAX,
                }]),
            ));
        }
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
            .inspect_err(|error| {
                tracing::error!(
                    model = model_id_or_path,
                    elapsed = ?started.elapsed(),
                    error = %error,
                    "game model failed to load"
                )
            })
            .with_context(|| {
                if allow_cpu {
                    format!("loading GGUF model from {model_id_or_path}")
                } else {
                    format!(
                        "loading GGUF model from {model_id_or_path}; the model must fit entirely in GPU memory (use --allow-cpu only for explicit CPU/GPU execution)"
                    )
                }
            })?;
        if !allow_cpu {
            let gpu_used_after_mib = log_gpu_memory("after model load")?;
            tracing::info!(
                allocated_mib = gpu_used_after_mib.saturating_sub(gpu_used_before_mib),
                "fixed model and KV-cache GPU allocation"
            );
        }
        tracing::info!(
            model = model_id_or_path,
            elapsed = ?started.elapsed(),
            "game model load completed"
        );
        Ok(Self {
            model,
            max_context_tokens: limits.max_context_tokens,
            max_output_tokens: limits.max_output_tokens,
        })
    }
}

fn log_gpu_memory(phase: &str) -> Result<usize> {
    let device = best_device(false).context("selecting CUDA device for model telemetry")?;
    let memory = mistralrs::core::MemoryUsage
        .query(&device)
        .context("querying CUDA memory for model telemetry")?;
    let total_mib = memory.total() / (1024 * 1024);
    let available_mib = memory.available() / (1024 * 1024);
    anyhow::ensure!(
        !matches!(device, mistralrs::Device::Cpu),
        "CUDA is unavailable; the model must fit entirely in GPU memory (use --allow-cpu only for explicit CPU/GPU execution)"
    );
    tracing::info!(
        phase,
        total_mib,
        available_mib,
        used_mib = total_mib.saturating_sub(available_mib),
        "CUDA memory telemetry"
    );
    Ok(total_mib.saturating_sub(available_mib))
}

fn request_builder(request: &Request, max_output_tokens: usize) -> Result<RequestBuilder> {
    let mut request_builder = RequestBuilder::new();
    for message in &request.messages {
        match &message.content {
            MessageContent::Text(content) => {
                let (role, content) = match &message.role {
                    Role::System => (TextMessageRole::System, content.clone()),
                    Role::User => (TextMessageRole::User, content.clone()),
                    Role::Assistant => (TextMessageRole::Assistant, content.clone()),
                    Role::Tool => (TextMessageRole::Tool, content.clone()),
                    // The model has no narrator role; deliver it as input that
                    // names the GM as the source.
                    Role::Narration => (TextMessageRole::User, format!("GM narration:\n{content}")),
                };
                request_builder = request_builder.add_message(role, content);
            }
            MessageContent::ToolCalls(calls) => {
                request_builder = request_builder.add_message_with_tool_call(
                    TextMessageRole::Assistant,
                    "",
                    calls
                        .iter()
                        .cloned()
                        .enumerate()
                        .map(to_mistral_call)
                        .collect(),
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
        .set_sampler_max_len(max_output_tokens)
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
    async fn complete(
        &self,
        request: Request,
        mut on_token: impl FnMut(&str) + Send,
    ) -> Result<Response> {
        let started = Instant::now();
        tracing::info!(
            messages = request.messages.len(),
            tools = request.tools.len(),
            max_context_tokens = self.max_context_tokens,
            max_output_tokens = self.max_output_tokens,
            temperature = request.sampling.temperature,
            thinking = request.sampling.enable_thinking,
            "starting model inference"
        );
        let request_builder = request_builder(&request, self.max_output_tokens)?;

        let mut stream = self
            .model
            .stream_chat_request(request_builder)
            .await
            .inspect_err(|error| {
                tracing::error!(error = %error, elapsed = ?started.elapsed(), "model inference could not start")
            })
            .context("starting streamed inference")?;

        // mistral.rs's streaming API ends after its final Chunk. That chunk
        // contains the complete response metadata, so returning at that point
        // is the API contract; no separate Done response is guaranteed.
        let mut content = String::new();
        let mut reasoning = String::new();
        let mut tool_calls = BTreeMap::new();
        let mut usage = Usage {
            input_tokens: 0,
            output_tokens: 0,
        };
        let mut streaming = false;
        let mut chunks = 0usize;
        loop {
            let Some(chunk) = stream.next().await else {
                tracing::error!(
                    elapsed = ?started.elapsed(),
                    chunks,
                    "model inference stream ended without a final chunk"
                );
                anyhow::bail!("inference stream ended without a final chunk");
            };
            if !streaming {
                streaming = true;
                tracing::info!(elapsed = ?started.elapsed(), "model inference began streaming");
            }
            match chunk {
                MrResponse::Chunk(ChatCompletionChunkResponse {
                    choices,
                    usage: chunk_usage,
                    ..
                }) => {
                    chunks += 1;
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
                        let response = assemble(
                            content,
                            reasoning,
                            tool_calls.into_values().collect(),
                            usage,
                        );
                        tracing::info!(
                            elapsed = ?started.elapsed(),
                            chunks,
                            input_tokens = response.usage.input_tokens,
                            output_tokens = response.usage.output_tokens,
                            tool_calls = matches!(&response.content, Content::ToolCalls(calls) if !calls.is_empty()),
                            "model inference completed"
                        );
                        return Ok(response);
                    }
                }
                MrResponse::ModelError(message, _) => {
                    tracing::error!(elapsed = ?started.elapsed(), %message, "model reported an inference error");
                    anyhow::bail!("model error during inference: {message}")
                }
                MrResponse::ContextLengthExceeded { requested_tokens } => {
                    let error = ContextCapacityExceeded::FixedKv {
                        requested_tokens,
                        max_context_tokens: self.max_context_tokens,
                    };
                    tracing::error!(elapsed = ?started.elapsed(), %error, "model rejected request before inference");
                    return Err(error.into());
                }
                MrResponse::InternalError(error) | MrResponse::ValidationError(error) => {
                    tracing::error!(elapsed = ?started.elapsed(), %error, "model inference stream error");
                    return Err(anyhow::anyhow!(error).context("inference stream error"));
                }
                _ => {
                    tracing::error!(elapsed = ?started.elapsed(), "model returned an unexpected chat stream response");
                    anyhow::bail!("unexpected response variant from chat stream")
                }
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
    use crate::llm::{Message, Sampling, ToolDefinition};
    use crate::settings::Settings;

    fn show_inference_lifecycle() {
        let _ = tracing_subscriber::fmt()
            .with_env_filter("cairnworld=info")
            .with_test_writer()
            .try_init();
    }

    #[tokio::test]
    #[ignore = "requires a CUDA-capable device and a configured GGUF model"]
    async fn streaming_completion_leaves_the_model_available_for_the_next_turn() {
        show_inference_lifecycle();
        let settings = Settings::load().expect("settings should load");
        let requested_model = std::env::var("CAIRNWORLD_TEST_MODEL").ok();
        let model = settings
            .model(requested_model.as_deref())
            .expect("configure a model in local.toml or default.toml to run this test");
        let backend = MistralRsBackend::load(
            &model.path,
            model.chat_template.as_deref(),
            model.source_model.as_deref(),
            settings.limits,
            false,
        )
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

        for turn in 1..=2 {
            let mut streamed = String::new();
            let response = backend
                .complete(request.clone(), |token| streamed.push_str(token))
                .await
                .unwrap_or_else(|error| {
                    panic!("streamed completion {turn} should succeed: {error:#}")
                });

            let Content::Text(final_text) = response.content else {
                panic!("streamed completion {turn} should produce text");
            };
            assert!(!streamed.is_empty());
            assert!(!final_text.is_empty());
            assert_eq!(streamed, final_text);
        }
    }

    #[tokio::test]
    #[ignore = "requires a CUDA-capable device and a configured GGUF model"]
    async fn opening_turn_with_tools_has_a_valid_chat_template_shape() {
        show_inference_lifecycle();
        let settings = Settings::load().expect("settings should load");
        let model = settings
            .model(None)
            .expect("configure a model in local.toml or default.toml to run this test");
        let backend = MistralRsBackend::load(
            &model.path,
            model.chat_template.as_deref(),
            model.source_model.as_deref(),
            settings.limits,
            false,
        )
        .await
        .expect("model should load");
        let request = Request {
            messages: vec![
                Message::text(Role::System, "You are the player's guide."),
                Message::text(
                    Role::User,
                    "The player has entered the world. Begin character creation by speaking directly to them.",
                ),
            ],
            tools: vec![ToolDefinition {
                name: "roll_hit_protection".into(),
                description: "Roll starting Hit Protection.".into(),
                schema: serde_json::json!({"type": "object", "properties": {}}),
            }],
            sampling: Sampling {
                temperature: 0.0,
                enable_thinking: false,
            },
        };
        backend.complete(request, |_| {}).await.expect(
            "opening request must reach the configured model without a chat-template error",
        );
    }

    #[tokio::test]
    #[ignore = "requires a CUDA-capable device and the configured Qwen GGUF model"]
    async fn qwen_zero_argument_tool_cannot_emit_placeholder_arguments() {
        show_inference_lifecycle();
        let settings = Settings::load().expect("settings should load");
        let model = settings
            .model(Some("dev-qwen3"))
            .expect("configure the dev-qwen3 model");
        let backend = MistralRsBackend::load(
            &model.path,
            model.chat_template.as_deref(),
            model.source_model.as_deref(),
            settings.limits,
            false,
        )
        .await
        .expect("model should load");
        let response = backend
            .complete(
                Request {
                    messages: vec![
                        Message::text(
                            Role::System,
                            "Follow the user's request using the available tool.",
                        ),
                        Message::text(
                            Role::User,
                            "Call roll_attributes now. Do not write a reply.",
                        ),
                    ],
                    tools: vec![ToolDefinition {
                        name: "roll_attributes".to_string(),
                        description: "Roll the character's attributes.".to_string(),
                        schema: serde_json::json!({
                            "type": "object",
                            "additionalProperties": false,
                            "properties": {},
                        }),
                    }],
                    sampling: Sampling {
                        temperature: 0.0,
                        enable_thinking: false,
                    },
                },
                |_| {},
            )
            .await
            .expect("Qwen zero-argument tool request should complete");
        let Content::ToolCalls(calls) = response.content else {
            panic!("Qwen should call the requested zero-argument tool");
        };
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "roll_attributes");
        assert_eq!(calls[0].arguments, "{}");
    }

    /// A tool-using turn feeds the prior assistant tool call and its result
    /// back to the model. The chat template must render that stored call; the
    /// Qwen3.5 template iterates `arguments` as key/value pairs, so it must
    /// reach the template as a decoded object, not the OpenAI wire string.
    #[tokio::test]
    #[ignore = "requires a CUDA-capable device and the configured GGUF model"]
    async fn a_prior_tool_call_message_renders_for_the_next_turn() {
        show_inference_lifecycle();
        let settings = Settings::load().expect("settings should load");
        let model = settings
            .model(None)
            .expect("configure a model in local.toml or default.toml to run this test");
        let backend = MistralRsBackend::load(
            &model.path,
            model.chat_template.as_deref(),
            model.source_model.as_deref(),
            settings.limits,
            false,
        )
        .await
        .expect("model should load");
        let request = Request {
            messages: vec![
                Message::text(Role::System, "You are the player's guide."),
                Message::text(Role::User, "Attack the goblin with my sword."),
                Message {
                    role: Role::Assistant,
                    content: MessageContent::ToolCalls(vec![ToolCall {
                        id: "c1".to_string(),
                        name: "attack".to_string(),
                        arguments: r#"{"target":"goblin","weapon":"sword"}"#.to_string(),
                    }]),
                    reasoning: String::new(),
                },
                Message::tool_result("c1".to_string(), "You hit for 4 damage.".to_string()),
                Message::text(Role::User, "What now?"),
            ],
            tools: vec![ToolDefinition {
                name: "attack".into(),
                description: "Attack a target with a weapon.".into(),
                schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "target": {"type": "string"},
                        "weapon": {"type": "string"},
                    },
                }),
            }],
            sampling: Sampling {
                temperature: 0.0,
                enable_thinking: false,
            },
        };
        backend
            .complete(request, |_| {})
            .await
            .expect("a prior tool-call message must reach the model without a chat-template error");
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
