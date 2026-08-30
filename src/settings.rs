use std::{collections::BTreeMap, path::PathBuf};

use anyhow::{Context, Result, ensure};
use serde::Deserialize;

#[derive(Deserialize, Default)]
pub struct Settings {
    /// Which entry of `models` to use when `--model` is not given.
    pub model: Option<String>,
    #[serde(default)]
    pub models: BTreeMap<String, Model>,
    #[serde(default)]
    pub limits: Limits,
    pub web: Option<Web>,
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
/// the others do not.
#[derive(Clone, Debug, Deserialize)]
pub struct Model {
    pub path: String,
    pub chat_template: Option<PathBuf>,
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
        })
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
    /// Compact after a completed inference reports this many input tokens.
    pub compact_at_input_tokens: usize,
    /// Token capacity reserved after compaction's input trigger for one
    /// completion. Together they define the fixed per-inference context.
    pub max_completion_tokens: usize,
    /// Exact number of newest raw messages retained after a summary.
    pub keep_tail_messages: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_concurrent_inferences: 4,
            max_inferences_per_chat: 8,
            max_inferences_total: 64,
            compact_at_input_tokens: 16_000,
            max_completion_tokens: 1_024,
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
            self.compact_at_input_tokens > 0,
            "limits.compact_at_input_tokens must be greater than zero"
        );
        ensure!(
            self.max_completion_tokens > 0,
            "limits.max_completion_tokens must be greater than zero"
        );
        ensure!(
            self.keep_tail_messages > 0,
            "limits.keep_tail_messages must be greater than zero"
        );
        self.cache_context_tokens()?;
        Ok(())
    }

    /// Fixed complete context capacity implied by the existing compaction
    /// policy and the bounded completion reservation.
    pub fn context_tokens(self) -> Result<usize> {
        self.compact_at_input_tokens
            .checked_add(self.max_completion_tokens)
            .context("limits.compact_at_input_tokens plus limits.max_completion_tokens overflowed")
    }

    /// Total token pool required by paged attention for every simultaneously
    /// admitted inference to use its complete configured context.
    pub fn cache_context_tokens(self) -> Result<usize> {
        self.context_tokens()?
            .checked_mul(self.max_concurrent_inferences)
            .context(
                "derived per-inference context multiplied by limits.max_concurrent_inferences overflowed",
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
    use super::Limits;

    #[test]
    fn compaction_policy_reserves_each_admitted_inference() {
        let limits = Limits {
            max_concurrent_inferences: 3,
            max_inferences_per_chat: 1,
            max_inferences_total: 1,
            compact_at_input_tokens: 800,
            max_completion_tokens: 200,
            keep_tail_messages: 1,
        };

        limits.validate().unwrap();
        assert_eq!(limits.context_tokens().unwrap(), 1_000);
        assert_eq!(limits.cache_context_tokens().unwrap(), 3_000);
    }

    #[test]
    fn compaction_policy_rejects_context_capacity_overflow() {
        let limits = Limits {
            max_concurrent_inferences: 1,
            max_inferences_per_chat: 1,
            max_inferences_total: 1,
            compact_at_input_tokens: usize::MAX,
            max_completion_tokens: 1,
            keep_tail_messages: 1,
        };

        assert!(limits.validate().is_err());
    }
}
