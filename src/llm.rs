use std::fmt;

use anyhow::Result;

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
    fn complete_with_recording(
        &self,
        request: Request,
        on_token: impl FnMut(&str) + Send,
        _store: &crate::store::Store,
        _inference_id: i64,
    ) -> impl Future<Output = Result<Response>> + Send {
        self.complete(request, on_token)
    }

    /// Finish earlier work for the exact history selected by this turn.
    async fn before_agent(
        &self,
        _store: &crate::store::Store,
        _branch: crate::store::AgentBranch,
        _events: &tokio::sync::broadcast::Sender<crate::inspection::DevEvent>,
    ) -> Result<()> {
        Ok(())
    }

    async fn after_agent(
        &self,
        _store: &crate::store::Store,
        _branch: crate::store::AgentBranch,
        _events: &tokio::sync::broadcast::Sender<crate::inspection::DevEvent>,
    ) -> Result<()> {
        Ok(())
    }

    fn complete(
        &self,
        request: Request,
        on_token: impl FnMut(&str) + Send,
    ) -> impl Future<Output = Result<Response>> + Send;
}

// Request/Response and their nested types live in `crate::inspection`, so the
// browser can render exactly what the server sent/received with no separate
// mirrored struct (see that module's doc comment).
pub use crate::inspection::{
    Content, Message, MessageContent, Request, Response, Role, Sampling, ToolCall, ToolDefinition,
    Usage,
};
