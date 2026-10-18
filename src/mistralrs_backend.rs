use std::{collections::BTreeMap, path::Path, time::Instant};

#[cfg(test)]
use std::sync::{Arc, Mutex};

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
    #[cfg(test)]
    observed_prompts: Option<Arc<Mutex<Vec<ObservedPrompt>>>>,
}

/// A test-only capture at mistral.rs's formatting and tokenization boundary.
/// Production retains only the checksums needed to verify reconstruction.
#[cfg(test)]
#[derive(Debug, PartialEq)]
pub(crate) struct ObservedPrompt {
    pub request: Request,
    pub prompt: String,
    pub tokens: Vec<u32>,
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
            let budgets = model
                .inner()
                .validate_cuda_prompt_capacity(None, limits.max_context_tokens)
                .await
                .context(
                    "validating startup GPU workspace for the configured context and concurrency",
                )?;
            for budget in budgets {
                tracing::info!(
                    device = budget.device,
                    pool_used_mib = budget.pool_used_bytes / (1024 * 1024),
                    attention_workspace_mib = budget.attention_workspace_bytes / (1024 * 1024),
                    allocator_headroom_mib = budget.allocator_headroom_bytes / (1024 * 1024),
                    safety_headroom_mib = budget.safety_headroom_bytes / (1024 * 1024),
                    estimated_peak_mib = (budget.pool_used_bytes
                        + budget.attention_workspace_bytes
                        + budget.allocator_headroom_bytes
                        + budget.safety_headroom_bytes)
                        .div_ceil(1024 * 1024),
                    additional_required_mib = budget.additional_device_bytes.div_ceil(1024 * 1024),
                    available_mib = budget.available_device_bytes / (1024 * 1024),
                    max_context_tokens = limits.max_context_tokens,
                    max_concurrent_inferences = limits.max_concurrent_inferences,
                    "startup GPU workspace capacity verified"
                );
            }
            let gpu_used_after_mib = log_gpu_memory("after model load")?;
            tracing::info!(
                allocated_mib = gpu_used_after_mib.saturating_sub(gpu_used_before_mib),
                "model and KV-cache startup memory delta"
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
            #[cfg(test)]
            observed_prompts: None,
        })
    }

    #[cfg(test)]
    pub(crate) fn observe_prompts(&mut self) -> Arc<Mutex<Vec<ObservedPrompt>>> {
        let observed = Arc::new(Mutex::new(Vec::new()));
        self.observed_prompts = Some(Arc::clone(&observed));
        observed
    }
}

fn log_gpu_memory(phase: &str) -> Result<usize> {
    let device = best_device(false).context("selecting CUDA device for model telemetry")?;
    let memory = mistralrs::core::MemoryUsage
        .query(&device)
        .context("querying CUDA memory for model telemetry")?;
    let total_mib = memory.total() / (1024 * 1024);
    let available_mib = memory.available() / (1024 * 1024);
    let allocator = mistralrs::core::MemoryUsage
        .query_cuda_allocator(&device)
        .context("querying CUDA driver memory for model telemetry")?;
    anyhow::ensure!(
        !matches!(device, mistralrs::Device::Cpu),
        "CUDA is unavailable; the model must fit entirely in GPU memory (use --allow-cpu only for explicit CPU/GPU execution)"
    );
    tracing::info!(
        phase,
        total_mib,
        available_mib,
        used_mib = total_mib.saturating_sub(available_mib),
        allocator = ?allocator,
        "CUDA memory telemetry"
    );
    Ok(total_mib.saturating_sub(available_mib))
}

pub(crate) fn request_builder(
    request: &Request,
    max_output_tokens: usize,
) -> Result<RequestBuilder> {
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
    let sampling = &request.sampling;
    let mut request_builder = request_builder
        .set_sampling(SamplingParams::neutral())
        .set_sampler_temperature(sampling.temperature as f64)
        .set_sampler_max_len(max_output_tokens);
    if let Some(top_p) = sampling.top_p {
        request_builder = request_builder.set_sampler_topp(top_p as f64);
    }
    if let Some(top_k) = sampling.top_k {
        request_builder = request_builder.set_sampler_topk(top_k);
    }
    if let Some(min_p) = sampling.min_p {
        request_builder = request_builder.set_sampler_minp(min_p as f64);
    }
    if let Some(presence_penalty) = sampling.presence_penalty {
        request_builder = request_builder.set_sampler_presence_penalty(presence_penalty);
    }
    Ok(request_builder
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
        on_token: impl FnMut(&str) + Send,
    ) -> Result<Response> {
        let builder = request_builder(&request, self.max_output_tokens)?;
        self.generate(builder, on_token).await
    }

    async fn complete_with_recording(
        &self,
        request: Request,
        mut on_token: impl FnMut(&str) + Send,
        store: &crate::store::Store,
        inference_id: i64,
    ) -> Result<Response> {
        let max_output_tokens = self.max_output_tokens;
        #[cfg(test)]
        let observed_prompts = self.observed_prompts.clone();
        #[cfg(test)]
        let observed_request = request.clone();
        let builder = request_builder(&request, max_output_tokens)?.with_prompt_observer({
            let store = store.clone();
            std::sync::Arc::new(move |prompt, tokens, format| {
                #[cfg(test)]
                let observed = if let Some(observed_prompts) = &observed_prompts {
                    observed_prompts
                        .lock()
                        .map_err(|_| anyhow::anyhow!("prompt observation lock poisoned"))
                        .map(|mut observed_prompts| {
                            observed_prompts.push(ObservedPrompt {
                                request: observed_request.clone(),
                                prompt: prompt.to_owned(),
                                tokens: tokens.to_vec(),
                            });
                        })
                } else {
                    Ok(())
                };
                let store = store.clone();
                let evidence = format
                    .context("model did not provide reconstructable prompt formatting")
                    .and_then(|format| {
                        crate::store::PromptEvidence::new(format, prompt, tokens, max_output_tokens)
                    });
                Box::pin(async move {
                    #[cfg(test)]
                    observed?;
                    store.record_prompt(inference_id, evidence?).await
                })
            })
        });
        // Resolved once before streaming starts, not per token: Developer
        // Mode's live token broadcast otherwise has no cheap way to know
        // which world's viewers to reach from inside this streaming loop.
        // Live broadcast is not essential to a correct inference, so a
        // lookup failure here is logged and degrades to no live tokens for
        // this one inference rather than failing the inference itself.
        let dev_world_id = match store.world_id_for_inference(inference_id).await {
            Ok(world_id) => Some(world_id),
            Err(error) => {
                tracing::warn!(inference_id, error = %format!("{error:#}"), "could not resolve world for live token broadcast");
                None
            }
        };
        let store = store.clone();
        let mut emitted_bytes = 0usize;
        self.generate(builder, move |token| {
            if let Some(world_id) = dev_world_id {
                store.publish_token(world_id, inference_id, emitted_bytes, token);
            }
            emitted_bytes += token.len();
            on_token(token);
        })
        .await
    }
}

impl MistralRsBackend {
    async fn generate(
        &self,
        request_builder: RequestBuilder,
        mut on_token: impl FnMut(&str) + Send,
    ) -> Result<Response> {
        let started = Instant::now();
        tracing::info!(
            max_context_tokens = self.max_context_tokens,
            max_output_tokens = self.max_output_tokens,
            "starting model inference"
        );
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

    /// A configured model resolved from checked-in `default.toml`, at a context
    /// size that fits an 8 GB GPU. No `local.toml`.
    fn device_test_model(name: &str) -> (crate::settings::Model, crate::settings::Limits) {
        let settings = Settings::for_device_test(name).expect("default.toml should load");
        let model = settings
            .model(None)
            .expect("default.toml should configure the device-test model");
        (model, settings.limits)
    }

    #[tokio::test]
    #[ignore = "device test: needs a CUDA GPU and the models/ GGUF from default.toml"]
    async fn generation_prompt_observation_matches_request_reconstruction() {
        use std::sync::{Arc, Mutex};
        show_inference_lifecycle();
        let (model, limits) = device_test_model("dev-qwen35");
        let backend = MistralRsBackend::load(
            &model.path,
            model.chat_template.as_deref(),
            model.source_model.as_deref(),
            limits,
            false,
        )
        .await
        .expect("model should load");
        let request = Request {
            messages: vec![
                Message::text(Role::System, "Reply briefly."),
                Message::text(
                    Role::Narration,
                    "Rain falls. <gate> & café\nThe mill creaks.",
                ),
                Message::text(Role::User, "Say hello."),
            ],
            tools: vec![ToolDefinition {
                name: "look".into(),
                description: "Inspect the scene.".into(),
                schema: serde_json::json!({"type": "object", "properties": {}}),
            }],
            sampling: Sampling {
                temperature: 0.0,
                ..Default::default()
            },
        };
        let database = crate::store::TestDatabase::new("generation-evidence");
        let store = crate::store::Store::open(database.path()).await.unwrap();
        let world = store.create_world("prompt evidence").await.unwrap();
        let agent = store.create_agent(world).await.unwrap();
        let segments = crate::context::segments(
            &store,
            store.agent_branch(agent).await.unwrap(),
            &request.messages,
            &[],
            &request.tools,
        )
        .await
        .unwrap();
        let invocation_id = store
            .start_invocation(None, agent, None, None, None)
            .await
            .unwrap();
        let inference_id = store
            .start_inference(crate::store::InferenceInput {
                agent_id: agent,
                sequence_id: None,
                invocation_id,
                segments: &segments,
                request: &request,
                model: "dev-qwen35",
            })
            .await
            .unwrap();
        let recording_store = store.clone();
        let observed = Arc::new(Mutex::new(Vec::new()));
        let capture = Arc::clone(&observed);
        let recorded_request = serde_json::to_vec(&request).unwrap();
        let builder = request_builder(&request, 16)
            .unwrap()
            .with_prompt_observer(Arc::new(move |prompt, tokens, format| {
                use mistralrs::RequestLike;
                let format = format.expect("text chat format");
                let recorded_format = serde_json::to_vec(format).unwrap();
                let restored_format: mistralrs::ChatTemplateFormat =
                    serde_json::from_slice(&recorded_format).unwrap();
                let restored_request: Request = serde_json::from_slice(&recorded_request).unwrap();
                let mut restored = request_builder(&restored_request, 16).unwrap();
                let tools = restored.take_tools().expect("tools supplied").0;
                let mistralrs::RequestMessage::Chat { messages, .. } = restored.take_messages()
                else {
                    panic!("text request expected");
                };
                assert_eq!(restored_format.render(messages, tools).unwrap(), prompt);
                capture.lock().unwrap().push((
                    prompt.to_owned(),
                    tokens.to_vec(),
                    serde_json::to_value(format).unwrap(),
                ));
                let evidence =
                    crate::store::PromptEvidence::new(format, prompt, tokens, 16).unwrap();
                let store = recording_store.clone();
                Box::pin(async move { store.record_prompt(inference_id, evidence).await })
            }));
        let mut stream = backend.model.stream_chat_request(builder).await.unwrap();
        loop {
            match stream.next().await.expect("generation must complete") {
                MrResponse::Chunk(chunk) => {
                    if chunk
                        .choices
                        .iter()
                        .any(|choice| choice.finish_reason.is_some())
                    {
                        break;
                    }
                }
                other => panic!(
                    "unexpected inference response: {}",
                    match other {
                        MrResponse::InternalError(error) | MrResponse::ValidationError(error) =>
                            error.to_string(),
                        _ => "non-chat response".into(),
                    }
                ),
            }
        }
        let reconstructed_tokens = backend
            .model
            .tokenize_chat_request(request_builder(&request, 16).unwrap())
            .await
            .unwrap();
        let reconstructed = store.reconstruct_inference(inference_id).await.unwrap();
        assert_eq!(reconstructed.request, request);
        assert_eq!(
            reconstructed.model_prompt.as_deref(),
            Some(observed.lock().unwrap()[0].0.as_str())
        );
        {
            let observations = observed.lock().unwrap();
            assert_eq!(
                observations.len(),
                1,
                "observe the normal tokenization exactly once"
            );
            let (prompt, tokens, format) = &observations[0];
            assert!(format["rendered_at"].is_string());
            assert_eq!(tokens, &reconstructed_tokens);
            assert!(prompt.contains("GM narration:\nRain falls. <gate> & café\nThe mill creaks."));
            assert!(prompt.contains("look"));
        }

        let failed_observer =
            request_builder(&request, 16)
                .unwrap()
                .with_prompt_observer(Arc::new(|_, _, _| {
                    Box::pin(async { anyhow::bail!("prompt recording unavailable") })
                }));
        let mut stream = backend
            .model
            .stream_chat_request(failed_observer)
            .await
            .unwrap();
        match stream
            .next()
            .await
            .expect("observer failure must be returned")
        {
            MrResponse::InternalError(error) => {
                let error = anyhow::Error::from_boxed(error);
                assert!(
                    format!("{error:#}").contains("prompt recording unavailable"),
                    "observer errors must retain their cause: {error:#}"
                );
            }
            _ => panic!("failed prompt observation must prevent generation"),
        }
        assert!(
            stream.next().await.is_none(),
            "an observer failure must end the stream without generated tokens"
        );
    }

    #[tokio::test]
    #[ignore = "device test: needs a CUDA GPU and the models/ GGUF from default.toml"]
    async fn configured_context_capacity_rejects_overflow_and_remains_usable() {
        show_inference_lifecycle();
        let settings = Settings::for_test("model = \"dev-qwen35\"").unwrap();
        let model = settings.model(None).unwrap();
        let limits = settings.limits;
        let backend = MistralRsBackend::load(
            &model.path,
            model.chat_template.as_deref(),
            model.source_model.as_deref(),
            limits,
            false,
        )
        .await
        .unwrap();
        let request_for = |repetitions| Request {
            messages: vec![
                Message::text(Role::System, "Reply with one short word."),
                Message::text(Role::User, "x ".repeat(repetitions)),
            ],
            tools: vec![],
            sampling: Sampling {
                temperature: 0.0,
                ..Default::default()
            },
        };
        let max_input = limits.max_context_tokens - limits.max_output_tokens;
        let mut lower = 1;
        let mut upper = limits.max_context_tokens * 2;
        // Size the boundary fixture with the runtime tokenizer; production admission still uses the generation pass.
        while lower < upper {
            let middle = lower + (upper - lower).div_ceil(2);
            let tokens = backend
                .model
                .tokenize_chat_request(
                    request_builder(&request_for(middle), limits.max_output_tokens).unwrap(),
                )
                .await
                .unwrap();
            if tokens.len() <= max_input {
                lower = middle;
            } else {
                upper = middle - 1;
            }
        }
        let request = request_for(lower);
        let tokens = backend
            .model
            .tokenize_chat_request(request_builder(&request, limits.max_output_tokens).unwrap())
            .await
            .unwrap();
        assert!(tokens.len() >= max_input - 1);
        let device = best_device(false).unwrap();
        log_gpu_memory("before configured-capacity inference").unwrap();
        let startup_budget = backend
            .model
            .inner()
            .validate_cuda_prompt_capacity(None, limits.max_context_tokens)
            .await
            .unwrap();
        let model_limit = backend.model.max_sequence_length().unwrap().unwrap();
        let rejected = backend
            .model
            .inner()
            .validate_cuda_prompt_capacity(None, model_limit)
            .await
            .expect_err("the model's much larger native context must not fit this device budget");
        assert!(
            rejected.to_string().contains("additional device bytes"),
            "{rejected:#}"
        );
        for _ in 0..3 {
            let response = backend.complete(request.clone(), |_| {}).await.unwrap();
            assert_eq!(response.usage.input_tokens, tokens.len());
            log_gpu_memory("after configured-capacity inference").unwrap();
            let pool = mistralrs::core::MemoryUsage
                .query_cuda_allocator(&device)
                .unwrap()
                .unwrap()
                .async_pool
                .unwrap();
            let budget = &startup_budget[0];
            let estimated_peak = budget.pool_used_bytes
                + budget.attention_workspace_bytes
                + budget.allocator_headroom_bytes
                + budget.safety_headroom_bytes;
            assert!(
                pool.reserved_high <= estimated_peak,
                "startup estimate {estimated_peak} did not cover actual pool reservation {pool:?}"
            );
            tracing::info!(
                allocator = ?mistralrs::core::MemoryUsage.query_cuda_allocator(&device).unwrap(),
                "configured-capacity CUDA allocation, including pool high-water marks"
            );
        }
        let error = backend
            .complete(request_for(limits.max_context_tokens * 2), |_| {})
            .await
            .expect_err("oversized input must be rejected by the fixed cache");
        assert!(
            matches!(
                error.downcast_ref::<ContextCapacityExceeded>(),
                Some(ContextCapacityExceeded::FixedKv { .. })
            ),
            "expected a fixed-cache rejection, got {error:#}"
        );
        backend.complete(request_for(1), |_| {}).await.unwrap();
        log_gpu_memory("after fixed-cache rejection and recovery").unwrap();
    }

    #[tokio::test]
    #[ignore = "device test: needs a CUDA GPU and the models/ GGUF from default.toml"]
    async fn streaming_completion_leaves_the_model_available_for_the_next_turn() {
        show_inference_lifecycle();
        let (model, limits) = device_test_model("dev-qwen35");
        let backend = MistralRsBackend::load(
            &model.path,
            model.chat_template.as_deref(),
            model.source_model.as_deref(),
            limits,
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
                ..Default::default()
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
    #[ignore = "device test: needs a CUDA GPU and the models/ GGUF from default.toml"]
    async fn opening_turn_with_tools_has_a_valid_chat_template_shape() {
        show_inference_lifecycle();
        let (model, limits) = device_test_model("dev-qwen35");
        let backend = MistralRsBackend::load(
            &model.path,
            model.chat_template.as_deref(),
            model.source_model.as_deref(),
            limits,
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
                ..Default::default()
            },
        };
        backend.complete(request, |_| {}).await.expect(
            "opening request must reach the configured model without a chat-template error",
        );
    }

    #[tokio::test]
    #[ignore = "device test: needs a CUDA GPU and the models/ GGUF from default.toml"]
    async fn qwen_zero_argument_tool_cannot_emit_placeholder_arguments() {
        show_inference_lifecycle();
        let (model, limits) = device_test_model("dev-qwen3");
        let backend = MistralRsBackend::load(
            &model.path,
            model.chat_template.as_deref(),
            model.source_model.as_deref(),
            limits,
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
                        ..Default::default()
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
    #[ignore = "device test: needs a CUDA GPU and the models/ GGUF from default.toml"]
    async fn a_prior_tool_call_message_renders_for_the_next_turn() {
        show_inference_lifecycle();
        let (model, limits) = device_test_model("dev-qwen35");
        let backend = MistralRsBackend::load(
            &model.path,
            model.chat_template.as_deref(),
            model.source_model.as_deref(),
            limits,
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
                    origin: crate::inspection::MessageOrigin::Conversation,
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
                ..Default::default()
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
