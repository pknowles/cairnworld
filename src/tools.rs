use std::{future::Future, pin::Pin, sync::Arc};

use anyhow::{Context, Result};
#[cfg(test)]
use schemars::{JsonSchema, schema_for};
#[cfg(test)]
use serde::Deserialize;

use crate::llm::{ToolCall, ToolDefinition};

type ToolExecution = dyn Fn(&str, i64) -> ToolFuture + Send + Sync;

pub struct Tool {
    definition: ToolDefinition,
    execute: Arc<ToolExecution>,
}

pub type ToolFuture = Pin<Box<dyn Future<Output = Result<String>> + Send>>;

impl Tool {
    pub fn definition(&self) -> ToolDefinition {
        self.definition.clone()
    }

    pub async fn execute(&self, arguments: &str, inference_id: i64) -> Result<String> {
        (self.execute)(arguments, inference_id).await
    }
}

impl Tool {
    pub fn new(
        definition: ToolDefinition,
        execute: impl Fn(&str, i64) -> ToolFuture + Send + Sync + 'static,
    ) -> Self {
        Self {
            definition,
            execute: Arc::new(execute),
        }
    }
}

/// A test fixture for exercising the generic tool-call loop. Game tools belong
/// to their authenticated game service and are not exposed by the sandbox.
#[cfg(test)]
pub fn test_echo() -> Tool {
    Tool::new(
        ToolDefinition {
            name: "test_echo".to_string(),
            description: "Return the supplied test text.".to_string(),
            schema: serde_json::to_value(schema_for!(TestEcho))
                .expect("test echo schema should serialize"),
        },
        |arguments, _| {
            let arguments = arguments.to_string();
            Box::pin(async move { test_echo_arguments(&arguments) })
        },
    )
}

pub fn definitions(tools: &[Tool]) -> Vec<ToolDefinition> {
    tools.iter().map(Tool::definition).collect()
}

pub async fn execute(tools: &[Tool], call: &ToolCall, inference_id: i64) -> Result<String> {
    let tool = tools
        .iter()
        .find(|tool| tool.definition.name == call.name)
        .with_context(|| format!("unknown tool `{}`", call.name))?;
    tool.execute(&call.arguments, inference_id)
        .await
        .with_context(|| format!("executing tool `{}`", call.name))
}

#[cfg(test)]
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct TestEcho {
    /// Text to return.
    text: String,
}

#[cfg(test)]
fn test_echo_arguments(arguments: &str) -> Result<String> {
    Ok(serde_json::from_str::<TestEcho>(arguments)
        .context("parsing test echo arguments")?
        .text)
}
