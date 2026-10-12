use std::fmt;

use anyhow::Result;
use serde::{Deserialize, Serialize};

/// A model context could not run. Compaction may recover it by reducing stored
/// history; transport and model failures remain ordinary errors.
#[derive(Debug)]
pub enum ContextCapacityExceeded {
    /// The fixed paged KV cache rejected the template-expanded request before
    /// inference and reported the exact request length.
    FixedKv {
        requested_tokens: usize,
        max_context_tokens: usize,
    },
}

impl fmt::Display for ContextCapacityExceeded {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::FixedKv {
                requested_tokens,
                max_context_tokens,
            } => write!(
                formatter,
                "model request needs {requested_tokens} tokens but the fixed context capacity is {max_context_tokens}"
            ),
        }
    }
}

impl std::error::Error for ContextCapacityExceeded {}

pub trait Backend {
    async fn before_agent(&self, _store: &crate::store::Store, _agent_id: i64) -> Result<()> {
        Ok(())
    }

    async fn after_agent(&self, _store: &crate::store::Store, _agent_id: i64) -> Result<()> {
        Ok(())
    }

    fn complete(
        &self,
        request: Request,
        on_token: impl FnMut(&str) + Send,
    ) -> impl Future<Output = Result<Response>> + Send;
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct Request {
    pub messages: Vec<Message>,
    pub tools: Vec<ToolDefinition>,
    pub sampling: Sampling,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct Response {
    pub content: Content,
    pub reasoning: String,
    pub usage: Usage,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "type", content = "value")]
pub enum Content {
    Text(String),
    ToolCalls(Vec<ToolCall>),
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub enum Role {
    /// The agent's standing directive. Only the first message of a request.
    System,
    User,
    Assistant,
    Tool,
    /// A GM narration delivered into this history. Every character present sees
    /// it; a backend presents it to the model as input attributed to the GM.
    Narration,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct Message {
    pub role: Role,
    pub content: MessageContent,
    pub reasoning: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "type", content = "value")]
pub enum MessageContent {
    Text(String),
    ToolCalls(Vec<ToolCall>),
    ToolResult {
        tool_call_id: String,
        content: String,
    },
}

impl Message {
    pub fn text(role: Role, content: impl Into<String>) -> Self {
        Self {
            role,
            content: MessageContent::Text(content.into()),
            reasoning: String::new(),
        }
    }

    pub fn assistant(content: Content, reasoning: String) -> Self {
        let content = match content {
            Content::Text(content) => MessageContent::Text(content),
            Content::ToolCalls(calls) => MessageContent::ToolCalls(calls),
        };
        Self {
            role: Role::Assistant,
            content,
            reasoning,
        }
    }

    pub fn tool_result(tool_call_id: String, content: String) -> Self {
        Self {
            role: Role::Tool,
            content: MessageContent::ToolResult {
                tool_call_id,
                content,
            },
            reasoning: String::new(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub schema: serde_json::Value,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct Sampling {
    pub temperature: f32,
    /// Nucleus, top-k, and minimum-probability cutoffs. `None` leaves that
    /// truncation off; sampling the full distribution of a small model at a
    /// normal temperature produces incoherent tails.
    pub top_p: Option<f32>,
    pub top_k: Option<usize>,
    pub min_p: Option<f32>,
    /// One-shot penalty applied to tokens that have already appeared.
    pub presence_penalty: Option<f32>,
    pub enable_thinking: bool,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct Usage {
    pub input_tokens: usize,
    pub output_tokens: usize,
}
