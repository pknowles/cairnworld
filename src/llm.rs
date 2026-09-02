use std::{fmt, future::Future};

use anyhow::Result;
use serde::{Deserialize, Serialize};

/// The model rejected a request before inference because its fixed KV-cache
/// reservation cannot admit the requested sequence. This is distinct from a
/// model or transport failure: compaction may recover it by reducing history.
#[derive(Debug)]
pub struct ContextCapacityExceeded {
    pub requested_tokens: usize,
    pub max_context_tokens: usize,
}

impl fmt::Display for ContextCapacityExceeded {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "model request needs {} tokens but the fixed context capacity is {}",
            self.requested_tokens, self.max_context_tokens
        )
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

    /// Count the exact input tokens for a request as the loaded model's chat
    /// template will receive it, including tool protocol. This is reserved for
    /// the exceptional fixed-capacity compaction fallback: completed ordinary
    /// inferences already report their exact next-context usage, so counting
    /// them again would be pure overhead.
    fn input_tokens(&self, request: &Request) -> impl Future<Output = Result<usize>> + Send;

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
    System,
    User,
    Assistant,
    Tool,
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

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct Sampling {
    pub temperature: f32,
    pub enable_thinking: bool,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct Usage {
    pub input_tokens: usize,
    pub output_tokens: usize,
}
