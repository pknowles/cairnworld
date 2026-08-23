use anyhow::{Context, Result};
#[cfg(test)]
use schemars::{JsonSchema, schema_for};
#[cfg(test)]
use serde::Deserialize;

use crate::llm::{ToolCall, ToolDefinition};

pub struct Tool {
    definition: ToolDefinition,
    execute: fn(&str) -> Result<String>,
}

impl Tool {
    pub fn definition(&self) -> ToolDefinition {
        self.definition.clone()
    }

    pub fn execute(&self, arguments: &str) -> Result<String> {
        (self.execute)(arguments)
    }
}

/// A test fixture for exercising the generic tool-call loop. Game tools belong
/// to their authenticated game service and are not exposed by the sandbox.
#[cfg(test)]
pub fn test_echo() -> Tool {
    Tool {
        definition: ToolDefinition {
            name: "test_echo".to_string(),
            description: "Return the supplied test text.".to_string(),
            schema: serde_json::to_value(schema_for!(TestEcho))
                .expect("test echo schema should serialize"),
        },
        execute: test_echo_arguments,
    }
}

pub fn definitions(tools: &[Tool]) -> Vec<ToolDefinition> {
    tools.iter().map(Tool::definition).collect()
}

pub fn execute(tools: &[Tool], call: &ToolCall) -> Result<String> {
    let tool = tools
        .iter()
        .find(|tool| tool.definition.name == call.name)
        .with_context(|| format!("unknown tool `{}`", call.name))?;
    tool.execute(&call.arguments)
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
