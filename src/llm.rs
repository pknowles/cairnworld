use anyhow::Result;

pub trait Backend {
    async fn complete(
        &self,
        request: Request,
        on_token: impl FnMut(&str),
    ) -> Result<Response>;
}

pub struct Request {
    pub messages: Vec<Message>,
    pub tools: Vec<ToolDefinition>,
    pub sampling: Sampling,
}

pub struct Response {
    pub content: Content,
    pub usage: Usage,
}

pub enum Content {
    Text(String),
    ToolCalls(Vec<ToolCall>),
}

pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: String,
}

#[derive(Clone)]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

#[derive(Clone)]
pub struct Message {
    pub role: Role,
    pub content: String,
}

pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub schema: serde_json::Value,
}

pub struct Sampling {
    pub temperature: f32,
}

pub struct Usage {
    pub input_tokens: usize,
    pub output_tokens: usize,
}
