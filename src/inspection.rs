//! The data shared between the server and the browser for Developer Mode
//! (user_declarations.md's "Developer Mode"): plain, `Serialize`/`Deserialize`
//! records of inferences, turns, raw agent history and world objects, with no
//! server-only dependency (sqlx, tokio, mistralrs). `store.rs` builds these
//! from the database and re-exports them; `ui.rs` renders them directly, so
//! the browser sees exactly the same shape the server produced - no
//! hand-mirrored, narrower struct in between that could silently drop a field.

use serde::{Deserialize, Serialize};

// --- Model request/response (moved from llm.rs, which re-exports these) ---

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
    /// Words a character said, delivered to each actual hearer. The message
    /// holds only the words; readers attribute them from its speech record.
    Speech,
}

/// Why a message was added to a chat history. This does not alter its model
/// role: an instruction remains a user turn to the model while inspection
/// names its actual game-side source.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageOrigin {
    #[default]
    Conversation,
    Instruction,
}

impl MessageOrigin {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Conversation => "conversation",
            Self::Instruction => "instruction",
        }
    }
}

impl std::str::FromStr for MessageOrigin {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "conversation" => Ok(Self::Conversation),
            "instruction" => Ok(Self::Instruction),
            _ => Err(format!("unknown message origin {value:?}")),
        }
    }
}

impl Role {
    pub fn as_str(&self) -> &'static str {
        match self {
            Role::System => "system",
            Role::User => "user",
            Role::Assistant => "assistant",
            Role::Tool => "tool",
            Role::Narration => "narration",
            Role::Speech => "speech",
        }
    }
}

impl std::fmt::Display for Role {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct Message {
    pub role: Role,
    /// Game-side provenance; the backend still uses `role` for the model.
    #[serde(default)]
    pub origin: MessageOrigin,
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
    /// An utterance in a hearer's history: the speaker's authored text with
    /// who said it kept as structure.
    Speech(SpeechContent),
}

/// Speech as one character said it. The names are frozen at the time, and the
/// text is not rewritten to add the speaker.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct SpeechContent {
    pub speaker_id: i64,
    pub speaker: String,
    pub addressee_id: Option<i64>,
    pub addressee: Option<String>,
    /// `say` or `whisper`.
    pub kind: String,
    /// What the GM showed hearers before the text, if anything.
    pub prefix: Option<String>,
    pub text: String,
}

impl SpeechContent {
    /// What a player reads: the GM's prefix, then the authored text.
    pub fn display(&self) -> String {
        match &self.prefix {
            Some(prefix) => format!("{prefix} {}", self.text),
            None => self.text.clone(),
        }
    }
}

impl Message {
    pub fn text(role: Role, content: impl Into<String>) -> Self {
        Self {
            role,
            origin: MessageOrigin::Conversation,
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
            origin: MessageOrigin::Conversation,
            content,
            reasoning,
        }
    }

    pub fn tool_result(tool_call_id: String, content: String) -> Self {
        Self {
            role: Role::Tool,
            origin: MessageOrigin::Conversation,
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

// --- Chat history recipe (moved from store.rs, which re-exports these) ----

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(untagged)]
pub enum Segment {
    Text {
        text: i64,
        role: Role,
    },
    /// Current state supplied for this inference, folded into the leading
    /// system message without becoming conversation history.
    Context {
        context: i64,
    },
    Tools {
        text: i64,
    },
    Summary {
        summary: i64,
    },
    Messages {
        messages: PositionRange,
    },
}

/// Live Developer Mode event shared by the server and hydrated browser.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DevEvent {
    DeveloperModeChanged {
        enabled: bool,
    },
    AccessChanged,
    InferenceStarted {
        inference_id: i64,
        agent_id: i64,
        sequence_id: Option<i64>,
        parent_inference_id: Option<i64>,
    },
    Token {
        inference_id: i64,
        offset: usize,
        text: String,
    },
    InferenceCompleted {
        inference_id: i64,
        error: Option<String>,
    },
    Changed {
        sequence_id: Option<i64>,
        inference_id: Option<i64>,
    },
    ResyncRequired {
        skipped: u64,
    },
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct PositionRange {
    pub branch_id: i64,
    pub first_position: i64,
    pub last_position: i64,
}

// --- Developer Mode inspection view types (moved from store.rs) -----------

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub enum RecordedOutcome {
    Running,
    Response(Response),
    Error(String),
}

/// Independent evidence captured at the model boundary for a rendered prompt.
/// `None` on an inference means that backend did not supply prompt evidence.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct PromptEvidenceView {
    pub format_id: i64,
    pub rendered_at: String,
    pub prompt_hash: String,
    pub tokens_hash: String,
    pub max_output_tokens: usize,
}

/// The complete stored record of one model invocation.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct RecordedInference {
    pub id: i64,
    pub agent_id: i64,
    pub branch_id: i64,
    pub history_entry_id: i64,
    pub sequence_id: Option<i64>,
    /// The inference whose tool call caused this one, resolved by joining
    /// this inference's invocation back to its parent_tool_execution_id
    /// (design.md's actual nesting link, replacing the old direct
    /// inference-to-inference pointer). `None` for a root or deferred
    /// invocation's inference, which has no requesting tool call.
    pub parent_inference_id: Option<i64>,
    /// Set only for a deferred compaction caused by a completed inference.
    pub cause_inference_id: Option<i64>,
    pub segments: Vec<Segment>,
    pub request: Request,
    pub model_prompt: Option<String>,
    pub prompt_evidence: Option<PromptEvidenceView>,
    pub outcome: RecordedOutcome,
    pub model: String,
    pub duration_ms: Option<u64>,
}

/// One chat entry an inference produced, with its provenance for the developer view.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct InferenceMessage {
    pub id: i64,
    pub role: Role,
    /// The tool_execution this entry answers, when it is a tool result.
    pub answers_tool_execution_id: Option<i64>,
}

/// One action (tool call) an inference produced, with its provenance for the
/// developer view - resolved from `action`/`tool_execution` join, not stored
/// on the inference row itself.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct RecordedAction {
    pub id: i64,
    pub call_id: Option<String>,
    /// The GM inference that approved a character action, when one
    /// arbitrated it (resolved through its approving tool_execution).
    pub approval_inference_id: Option<i64>,
    pub tool: String,
    pub args: String,
    pub result: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct SequenceSummary {
    pub id: i64,
    pub trigger: String,
    pub started_at: String,
    pub inference_count: i64,
}

/// One turn as Developer Mode shows it: its inference tree, and the earlier
/// turns whose events caused it (an NPC reply is caused by the speech it
/// answers).
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct SequenceInspection {
    /// What began the turn, e.g. `player message`, or `npc response` for an
    /// NPC's decision (a proposal, not an action) and `npc speech` for the
    /// approval of one.
    pub trigger: String,
    pub caused_by: Vec<i64>,
    pub nodes: Vec<SequenceNode>,
}

/// One inference in a sequence tree, with the entries it produced. The tree
/// shape comes from each node's `RecordedInference::parent_inference_id`, a
/// derived field resolved from `invocation.parent_tool_execution_id` (see its
/// own doc comment) - not a raw `inference` table column.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct SequenceNode {
    pub inference: RecordedInference,
    pub messages: Vec<InferenceMessage>,
    pub actions: Vec<RecordedAction>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct AgentInfo {
    pub id: i64,
    pub role: String,
    pub character_name: Option<String>,
    pub location_name: Option<String>,
}

/// One entry of a raw agent history page. Raw messages, summary markers that
/// anchor after a message, and failed inferences appear in recorded order.
/// Distinct from context assembly: no summary folding.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub enum RawHistoryEntry {
    Message {
        id: i64,
        role: Role,
        origin: MessageOrigin,
        content: MessageContent,
        reasoning: String,
        source_inference_id: Option<i64>,
        answers_tool_execution_id: Option<i64>,
    },
    Summary {
        id: i64,
        covered_ranges: Vec<PositionRange>,
        inference_id: i64,
        text: String,
        reason: String,
        retained_messages: i64,
    },
    Inference {
        id: i64,
        history_entry_id: i64,
        error: Option<String>,
    },
}

/// One cursor-paged raw history response shared by the inspection route and
/// the browser that resumes it.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct RawHistoryPageResponse {
    pub entries: Vec<RawHistoryEntry>,
    pub cursor: Option<RawHistoryCursor>,
}

/// An opaque paging cursor over one agent's raw history: the head it is
/// paging (so a page never silently starts describing a different agent) and
/// the branch/position to resume after.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
pub struct RawHistoryCursor {
    pub branch_id: i64,
    pub cut: i64,
    pub next_branch_id: i64,
    pub next_position: i64,
}

/// One branch in a world_gm agent's history DAG: its ordered bounded parents
/// (design.md's split/merge), and, when a non-retired group currently uses it
/// as its writable head, that group's id/name (user_declarations.md: "the
/// GM's chat history view needs to authentically display the DAG, including
/// multiple heads - one per Group"). A retired source branch (a completed
/// split/merge's prior group) has no head - it still renders, just without a
/// "current" label.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct BranchNode {
    pub branch_id: i64,
    /// Ordered bounded parents (`history_parent`'s own ordinal order): this
    /// branch's history is each parent's own entries up to `through_position`,
    /// in this order, followed by this branch's own entries.
    pub parents: Vec<BranchParent>,
    pub head: Option<BranchGroup>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct BranchParent {
    pub branch_id: i64,
    pub through_position: i64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct BranchGroup {
    pub group_id: i64,
    pub group_name: String,
}

/// A stable object kind. Together with the database id this identifies one
/// current game object and makes a relationship navigable without guessing
/// from its display name.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ObjectType {
    Character,
    Connection,
    Group,
    Item,
    ItemType,
    Location,
}

impl ObjectType {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "character" => Some(Self::Character),
            "connection" => Some(Self::Connection),
            "group" => Some(Self::Group),
            "item" => Some(Self::Item),
            "item_type" => Some(Self::ItemType),
            "location" => Some(Self::Location),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Character => "character",
            Self::Connection => "connection",
            Self::Group => "group",
            Self::Item => "item",
            Self::ItemType => "item_type",
            Self::Location => "location",
        }
    }
}

/// An opaque stable boundary for the world-object index.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct WorldObjectCursor {
    pub object_type: ObjectType,
    pub id: i64,
}

/// A typed current-state reference, rendered as a direct object link.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ObjectLink {
    pub object_type: ObjectType,
    pub id: i64,
    pub label: String,
}

/// A verbatim current-state field whose value is not itself an object link.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ObjectField {
    pub label: String,
    pub value: String,
}

/// One current object. Present state only - never a `Segment`, never
/// assembled into a model request.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct GameObject {
    pub object_type: ObjectType,
    pub id: i64,
    pub name: String,
    pub description: String,
    pub gm_notes: String,
    pub storyteller_notes: String,
    pub fields: Vec<ObjectField>,
    pub relationships: Vec<ObjectLink>,
}

/// One stable page of a world's current objects and world-level notes.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct WorldObjects {
    pub name: String,
    pub time: i64,
    pub initial_prompt: String,
    pub storyteller_summary: String,
    pub gm_notes: String,
    pub storyteller_notes: String,
    pub objects: Vec<GameObject>,
    pub cursor: Option<WorldObjectCursor>,
}
