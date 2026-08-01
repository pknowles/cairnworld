use std::{collections::BTreeMap, path::PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;

#[derive(Deserialize, Default)]
pub struct Settings {
    /// Which entry of `models` to use when `--model` is not given.
    pub model: Option<String>,
    #[serde(default)]
    pub models: BTreeMap<String, Model>,
    #[serde(default)]
    pub limits: Limits,
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
}

/// Runaway bounds. These exist to stop a loop that is already wrong, so
/// hitting one is a hard error that propagates to the user - never something
/// an agent can see and react to.
#[derive(Clone, Copy, Deserialize)]
pub struct Limits {
    /// Inferences one chat turn may run before its tool loop is abandoned.
    pub max_inferences_per_chat: u32,
    /// Inferences one external trigger may run across every agent it reaches,
    /// including recursive agent-to-agent calls.
    pub max_inferences_total: u32,
    /// Compact an agent after its assembled context reaches this many tokens.
    pub compact_at_tokens: usize,
    /// Maximum characters of recent raw history retained after a summary.
    pub keep_tail_chars: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_inferences_per_chat: 8,
            max_inferences_total: 64,
            compact_at_tokens: 16_000,
            keep_tail_chars: 16_000,
        }
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
