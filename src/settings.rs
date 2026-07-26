use anyhow::{Context, Result};
use serde::Deserialize;

#[derive(Deserialize, Default)]
pub struct Settings {
    pub model: Option<String>,
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
