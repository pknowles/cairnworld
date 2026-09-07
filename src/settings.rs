use std::{collections::BTreeMap, path::PathBuf};

use anyhow::{Context, Result, ensure};
use serde::Deserialize;

use crate::llm::Sampling;

#[derive(Deserialize, Default)]
pub struct Settings {
    /// Which entry of `models` to use when `--model` is not given.
    pub model: Option<String>,
    #[serde(default)]
    pub models: BTreeMap<String, Model>,
    #[serde(default)]
    pub limits: Limits,
    /// Sampling shared by every model; `[models.<name>.sampling]` overrides it
    /// field by field.
    #[serde(default)]
    pub sampling: SamplingConfig,
    pub web: Option<Web>,
}

/// Every sampling knob is optional so a common `[sampling]` block and a
/// per-model one merge cleanly. `temperature` is the only one with a default
/// when nothing sets it.
#[derive(Clone, Copy, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SamplingConfig {
    pub temperature: Option<f32>,
    pub top_p: Option<f32>,
    pub top_k: Option<usize>,
    pub min_p: Option<f32>,
    pub presence_penalty: Option<f32>,
    pub enable_thinking: Option<bool>,
}

impl SamplingConfig {
    /// Fields set in `over` win; unset fields keep `self`.
    fn merged(self, over: SamplingConfig) -> SamplingConfig {
        SamplingConfig {
            temperature: over.temperature.or(self.temperature),
            top_p: over.top_p.or(self.top_p),
            top_k: over.top_k.or(self.top_k),
            min_p: over.min_p.or(self.min_p),
            presence_penalty: over.presence_penalty.or(self.presence_penalty),
            enable_thinking: over.enable_thinking.or(self.enable_thinking),
        }
    }

    fn resolve(self) -> Sampling {
        Sampling {
            temperature: self.temperature.unwrap_or(1.0),
            top_p: self.top_p,
            top_k: self.top_k,
            min_p: self.min_p,
            presence_penalty: self.presence_penalty,
            enable_thinking: self.enable_thinking.unwrap_or(false),
        }
    }
}

/// Deployment configuration for the OAuth-only browser interface. It is
/// optional in shared configuration because the model REPL has no web
/// dependency, but `serve` requires every field.
#[derive(Clone, Deserialize)]
pub struct Web {
    pub bind: String,
    pub google_client_id: String,
    pub google_client_secret: String,
    pub google_redirect_url: String,
}

/// A model and everything needed to talk to it. The chat template travels with
/// the weights because it is a property of the file: the Hermes 3 GGUF ships
/// one that silently drops tool definitions, so it needs a replacement while
/// the others do not. Some GGUFs also need their original model's configuration,
/// tokenizer, and template, which `source_model` supplies.
#[derive(Clone, Debug, Deserialize)]
pub struct Model {
    pub path: String,
    pub chat_template: Option<PathBuf>,
    pub source_model: Option<String>,
    #[serde(default)]
    pub sampling: SamplingConfig,
}

impl Settings {
    /// Resolve a model by name, or the configured default when none is given.
    /// A path is accepted directly so an unconfigured file can still be run.
    pub fn model(&self, requested: Option<&str>) -> Result<Model> {
        let name = requested
            .or(self.model.as_deref())
            .context("no --model given and no `model` key set in local.toml/default.toml")?;
        if let Some(model) = self.models.get(name) {
            return Ok(model.clone());
        }
        anyhow::ensure!(
            name.contains('/') || name.ends_with(".gguf"),
            "unknown model `{name}`; configured models are: {}",
            self.models.keys().cloned().collect::<Vec<_>>().join(", ")
        );
        Ok(Model {
            path: name.to_string(),
            chat_template: None,
            source_model: None,
            sampling: SamplingConfig::default(),
        })
    }

    /// Sampling for a model: the common `[sampling]` block with that model's
    /// own `[models.<name>.sampling]` fields layered on top.
    pub fn sampling(&self, model: &Model) -> Sampling {
        self.sampling.merged(model.sampling).resolve()
    }

    pub fn web(&self) -> Result<&Web> {
        self.web.as_ref().context(
            "missing [web] configuration; `serve` requires bind, google_client_id, google_client_secret, and google_redirect_url",
        )
    }
}

/// Runaway bounds. These exist to stop a loop that is already wrong, so
/// hitting one is a hard error that propagates to the user - never something
/// an agent can see and react to.
#[derive(Clone, Copy, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Limits {
    /// Maximum requests admitted to the loaded model at once.
    pub max_concurrent_inferences: usize,
    /// Inferences one chat turn may run before its tool loop is abandoned.
    pub max_inferences_per_chat: u32,
    /// Inferences one external trigger may run across every agent it reaches,
    /// including recursive agent-to-agent calls.
    pub max_inferences_total: u32,
    /// Fixed total input plus generated-token capacity reserved for each
    /// admitted inference. This is the per-sequence part of the paged KV pool.
    pub max_context_tokens: usize,
    /// Maximum generated tokens in one inference, including a tool call.
    pub max_output_tokens: usize,
    /// After a completed turn, compact when its input plus output tokens would
    /// make the next inference reach this size. The reply is delivered first;
    /// compaction is queued for idle time, and that agent's next inference
    /// waits for the queued work rather than re-running the same history.
    /// This is a lazy history-maintenance threshold, not a KV-cache size.
    pub compact_before_next_input_tokens: usize,
    /// Exact number of newest raw messages retained after a summary.
    pub keep_tail_messages: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_concurrent_inferences: 4,
            max_inferences_per_chat: 8,
            max_inferences_total: 64,
            max_context_tokens: 16_384,
            max_output_tokens: 1_024,
            compact_before_next_input_tokens: 15_360,
            keep_tail_messages: 32,
        }
    }
}

impl Limits {
    /// Validate the one runtime policy before a model is loaded. A paged cache
    /// is shared by every admitted sequence, so its capacity is the per-turn
    /// context multiplied by the concurrency cap.
    pub fn validate(self) -> Result<()> {
        ensure!(
            self.max_concurrent_inferences > 0,
            "limits.max_concurrent_inferences must be greater than zero"
        );
        ensure!(
            self.max_inferences_per_chat > 0,
            "limits.max_inferences_per_chat must be greater than zero"
        );
        ensure!(
            self.max_inferences_total > 0,
            "limits.max_inferences_total must be greater than zero"
        );
        ensure!(
            self.max_context_tokens > 0,
            "limits.max_context_tokens must be greater than zero"
        );
        ensure!(
            self.max_output_tokens > 0,
            "limits.max_output_tokens must be greater than zero"
        );
        ensure!(
            self.max_context_tokens > self.max_output_tokens,
            "limits.max_context_tokens must exceed limits.max_output_tokens"
        );
        ensure!(
            self.compact_before_next_input_tokens > 0,
            "limits.compact_before_next_input_tokens must be greater than zero"
        );
        ensure!(
            self.compact_before_next_input_tokens
                <= self.max_context_tokens - self.max_output_tokens,
            "limits.compact_before_next_input_tokens must leave room for limits.max_output_tokens within limits.max_context_tokens"
        );
        ensure!(
            self.keep_tail_messages > 0,
            "limits.keep_tail_messages must be greater than zero"
        );
        self.cache_context_tokens()?;
        Ok(())
    }

    /// Total token pool required by paged attention for every simultaneously
    /// admitted inference to use its complete configured context.
    pub fn cache_context_tokens(self) -> Result<usize> {
        self.max_context_tokens
            .checked_mul(self.max_concurrent_inferences)
            .context(
                "limits.max_context_tokens multiplied by limits.max_concurrent_inferences overflowed",
            )
    }
}

impl Settings {
    pub fn load() -> Result<Self> {
        config::Config::builder()
            .add_source(config::File::with_name("default"))
            .add_source(config::File::with_name("local").required(false))
            .build()
            .context("loading default.toml and local.toml")?
            .try_deserialize()
            .context("parsing configuration")
    }
}

#[cfg(test)]
mod tests {
    use super::{Limits, Settings};

    #[test]
    fn source_model_is_available_to_a_named_gguf() {
        let settings: Settings = config::Config::builder()
            .add_source(config::File::from_str(
                r#"
                    [models.dev-qwen35]
                    path = "models/Qwen_Qwen3.5-4B-Q4_K_M.gguf"
                    source_model = "Qwen/Qwen3.5-4B"
                "#,
                config::FileFormat::Toml,
            ))
            .build()
            .unwrap()
            .try_deserialize()
            .unwrap();

        let model = settings.model(Some("dev-qwen35")).unwrap();
        assert_eq!(model.source_model.as_deref(), Some("Qwen/Qwen3.5-4B"));
    }

    #[test]
    fn per_model_sampling_overrides_the_common_block_field_by_field() {
        let settings: Settings = config::Config::builder()
            .add_source(config::File::from_str(
                r#"
                    [sampling]
                    temperature = 0.7
                    top_p = 0.8
                    top_k = 20

                    [models.tight]
                    path = "models/tight.gguf"
                    [models.tight.sampling]
                    top_p = 0.1
                    presence_penalty = 1.0

                    [models.plain]
                    path = "models/plain.gguf"
                "#,
                config::FileFormat::Toml,
            ))
            .build()
            .unwrap()
            .try_deserialize()
            .unwrap();

        let tight = settings.sampling(&settings.model(Some("tight")).unwrap());
        assert_eq!(tight.temperature, 0.7); // from the common block
        assert_eq!(tight.top_p, Some(0.1)); // overridden
        assert_eq!(tight.top_k, Some(20)); // from the common block
        assert_eq!(tight.presence_penalty, Some(1.0)); // model-only

        let plain = settings.sampling(&settings.model(Some("plain")).unwrap());
        assert_eq!(plain.top_p, Some(0.8));
        assert_eq!(plain.presence_penalty, None);

        // An unconfigured path falls back to the temperature default.
        let bare = settings.sampling(&settings.model(Some("x/y.gguf")).unwrap());
        assert_eq!(bare.top_p, Some(0.8));
    }

    #[test]
    fn fixed_context_capacity_is_independent_of_lazy_compaction() {
        let limits = Limits {
            max_concurrent_inferences: 3,
            max_inferences_per_chat: 1,
            max_inferences_total: 1,
            max_context_tokens: 2_048,
            max_output_tokens: 1_024,
            compact_before_next_input_tokens: 800,
            keep_tail_messages: 1,
        };

        limits.validate().unwrap();
        assert_eq!(limits.cache_context_tokens().unwrap(), 6_144);
    }

    #[test]
    fn compaction_trigger_must_leave_output_room_in_fixed_context() {
        let limits = Limits {
            max_concurrent_inferences: 3,
            max_inferences_per_chat: 1,
            max_inferences_total: 1,
            max_context_tokens: 1_000,
            max_output_tokens: 200,
            compact_before_next_input_tokens: 801,
            keep_tail_messages: 1,
        };

        assert!(limits.validate().is_err());
    }
}
