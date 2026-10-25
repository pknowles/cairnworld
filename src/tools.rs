use std::{future::Future, pin::Pin, sync::Arc};

use anyhow::{Context, Result};
#[cfg(test)]
use schemars::{JsonSchema, schema_for};
#[cfg(test)]
use serde::Deserialize;

use crate::llm::{ToolCall, ToolDefinition};

/// The context every tool execution receives: the model call's arguments and
/// the tool_execution row already created for this exact call (one per output
/// ordinal, before any call in the batch runs). Nested game calls thread this
/// id through instead of an inference id/call-id pair, so an action or its
/// result links to the exact originating call by row rather than matching on
/// model-supplied text.
pub struct ToolCallContext<'a> {
    pub arguments: &'a str,
    pub tool_execution_id: i64,
}

type ToolExecution = dyn Fn(ToolCallContext<'_>) -> ToolFuture + Send + Sync;
pub type AfterToolResult = Pin<Box<dyn Future<Output = Result<()>> + Send>>;

/// A group partition or merge, proposed by a grouping tool call. Never
/// applied at the point the tool executes - the batch loop collects these in
/// call order and applies them all in one transaction at the end of the
/// current model-returned tool batch (design.md's "Tool boundary": "Other
/// calls in that batch execute in returned order against the pre-transition
/// group, which is the context in which the model generated them").
pub enum TopologyChange<G = i64> {
    Split {
        group_id: G,
        partitions: Vec<GroupPartition>,
        continue_with_character_id: Option<i64>,
    },
    Merge {
        group_ids: Vec<G>,
        name: String,
        description: String,
    },
}

pub struct GroupPartition {
    pub character_ids: Vec<i64>,
    pub name: String,
    pub description: String,
}

/// The model receives a rejected call as an ordinary tool result and can
/// correct its request. Infrastructure failures remain `Err` so they retain
/// their full causal chain and immediately reach the player.
pub enum ToolOutcome {
    Completed(String),
    CompletedAfter {
        content: String,
        after_result: AfterToolResult,
    },
    Rejected(String),
    /// A proposed split/merge, not yet applied - see `TopologyChange`.
    PendingTopologyChange(TopologyChange),
}

impl ToolOutcome {
    pub fn rejected(error: anyhow::Error) -> Self {
        Self::Rejected(format!("Tool arguments were rejected: {error:#}"))
    }
}

#[derive(Clone)]
pub struct Tool {
    definition: ToolDefinition,
    execute: Arc<ToolExecution>,
    /// A completed call to this tool ends the agent's turn: the model is not
    /// run again to read its result.
    terminal: bool,
}

pub type ToolFuture = Pin<Box<dyn Future<Output = Result<ToolOutcome>> + Send>>;

impl Tool {
    pub fn definition(&self) -> ToolDefinition {
        self.definition.clone()
    }

    pub async fn execute(&self, context: ToolCallContext<'_>) -> Result<ToolOutcome> {
        (self.execute)(context).await
    }

    pub fn is_terminal(&self) -> bool {
        self.terminal
    }
}

impl Tool {
    pub fn new(
        definition: ToolDefinition,
        execute: impl Fn(ToolCallContext<'_>) -> ToolFuture + Send + Sync + 'static,
    ) -> Self {
        Self {
            definition,
            execute: Arc::new(execute),
            terminal: false,
        }
    }

    /// A tool whose completed call ends the turn. It must be the last call in
    /// its response, after calls that all succeeded.
    pub fn terminal(
        definition: ToolDefinition,
        execute: impl Fn(ToolCallContext<'_>) -> ToolFuture + Send + Sync + 'static,
    ) -> Self {
        Self {
            terminal: true,
            ..Self::new(definition, execute)
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
        |context| {
            let arguments = context.arguments.to_string();
            Box::pin(async move {
                match test_echo_arguments(&arguments) {
                    Ok(content) => Ok(ToolOutcome::Completed(content)),
                    Err(error) => Ok(ToolOutcome::rejected(error)),
                }
            })
        },
    )
}

/// A test fixture: a terminal tool that returns its supplied text.
#[cfg(test)]
pub fn test_finish() -> Tool {
    Tool::terminal(
        ToolDefinition {
            name: "test_finish".to_string(),
            description: "Return the supplied test text and end the turn.".to_string(),
            schema: serde_json::to_value(schema_for!(TestEcho))
                .expect("test echo schema should serialize"),
        },
        |context| {
            let arguments = context.arguments.to_string();
            Box::pin(async move {
                match test_echo_arguments(&arguments) {
                    Ok(content) => Ok(ToolOutcome::Completed(content)),
                    Err(error) => Ok(ToolOutcome::rejected(error)),
                }
            })
        },
    )
}

/// Return the model-facing definitions after enforcing the schema contract that
/// every Cairnworld tool declares its object properties explicitly.  In
/// particular, a no-argument tool declares `"properties": {}`: omission means
/// something different to the Qwen tool grammar and must fail before inference.
pub fn definitions(tools: &[Tool]) -> Result<Vec<ToolDefinition>> {
    tools
        .iter()
        .map(|tool| {
            let definition = tool.definition();
            let schema_type = definition
                .schema
                .get("type")
                .and_then(serde_json::Value::as_str);
            if schema_type != Some("object") {
                anyhow::bail!(
                    "tool `{}` schema must be an object, got {:?}",
                    definition.name,
                    schema_type
                );
            }
            if !definition
                .schema
                .get("properties")
                .is_some_and(serde_json::Value::is_object)
            {
                anyhow::bail!(
                    "tool `{}` object schema must explicitly declare `properties`",
                    definition.name
                );
            }
            Ok(definition)
        })
        .collect()
}

/// Whether a call to this tool ends the turn once it completes.
pub fn is_terminal(tools: &[Tool], name: &str) -> bool {
    tools
        .iter()
        .any(|tool| tool.terminal && tool.definition.name == name)
}

/// Run one call. `cannot_end_turn` says why this response cannot end the turn
/// at this call, which a terminal tool requires it can: it is not the last call
/// of its response, or an earlier call in the response was rejected.
pub async fn execute(
    tools: &[Tool],
    call: &ToolCall,
    tool_execution_id: i64,
    cannot_end_turn: Option<&str>,
) -> Result<ToolOutcome> {
    let Some(tool) = tools.iter().find(|tool| tool.definition.name == call.name) else {
        return Ok(ToolOutcome::Rejected(format!(
            "Tool `{}` is unavailable in this conversation.",
            call.name
        )));
    };
    if let Some(reason) = cannot_end_turn.filter(|_| tool.terminal) {
        return Ok(ToolOutcome::Rejected(format!(
            "`{}` ends your turn, so it must be the last call in a response and every call before it must succeed: {reason}. Call it again once that is so.",
            call.name,
        )));
    }
    tool.execute(ToolCallContext {
        arguments: &call.arguments,
        tool_execution_id,
    })
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn definitions_rejects_an_object_schema_without_properties() {
        let tool = Tool::new(
            ToolDefinition {
                name: "invalid".into(),
                description: "Invalid fixture.".into(),
                schema: serde_json::json!({"type": "object"}),
            },
            |_| Box::pin(async { Ok(ToolOutcome::Completed(String::new())) }),
        );

        let error = definitions(&[tool]).expect_err("missing properties must fail fast");
        assert!(
            error
                .to_string()
                .contains("tool `invalid` object schema must explicitly declare `properties`")
        );
    }
}
