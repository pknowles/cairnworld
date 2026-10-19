mod content;
mod history;
mod prompt;
use history::EntryKind;
pub use history::{AgentBranch, PositionRange, RawCursor};
pub use prompt::PromptEvidence;

// Developer Mode's inspection view types are shared with the browser; see
// crate::inspection's doc comment. Re-exported here since this module's
// queries build and return them.
pub use crate::inspection::{
    AgentInfo, BranchGroup, BranchNode, BranchParent, DevEvent, GameObject, InferenceMessage,
    MessageOrigin, ObjectField, ObjectLink, ObjectType, RawHistoryCursor, RawHistoryEntry,
    RecordedAction, RecordedInference, RecordedOutcome, Segment, SequenceNode, SequenceSummary,
    WorldObjectCursor, WorldObjects,
};

use std::{
    collections::HashMap,
    path::Path,
    sync::{Arc, Mutex},
};

use anyhow::{Context, Result, ensure};
use rand::{Rng, distr::Alphanumeric};
use serde::{Deserialize, Serialize};
use sqlx::{
    FromRow, Sqlite, SqlitePool, Transaction,
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions},
};
use tokio::sync::broadcast;

use crate::llm::{
    Content, Message, MessageContent, Request, Response, Role, Sampling, ToolCall, ToolDefinition,
    Usage,
};
use crate::scenario::{
    Connection as ScenarioConnection, Group as ScenarioGroup, Item, ItemType, Location, Notes, Npc,
    Scenario,
};
use crate::tools;

/// The result of `Store::access_for`, the one indexed query every inspection
/// authorization decision is based on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Access {
    pub active_member: bool,
    pub developer_mode: bool,
}

#[derive(Clone)]
pub struct Store {
    pool: SqlitePool,
    dev_events: Arc<Mutex<HashMap<i64, broadcast::Sender<DevEvent>>>>,
}

/// Hash model input without game-side provenance. Preserve the original wire
/// shape so saved inference hashes still verify when inspection gains metadata.
fn inference_input_hash(request: &Request) -> Result<String> {
    #[derive(Serialize)]
    struct InputMessage<'a> {
        role: &'a Role,
        content: &'a MessageContent,
        reasoning: &'a str,
    }
    #[derive(Serialize)]
    struct Input<'a> {
        messages: Vec<InputMessage<'a>>,
        tools: &'a [ToolDefinition],
        sampling: &'a Sampling,
    }
    let input = Input {
        messages: request
            .messages
            .iter()
            .map(|message| InputMessage {
                role: &message.role,
                content: &message.content,
                reasoning: &message.reasoning,
            })
            .collect(),
        tools: &request.tools,
        sampling: &request.sampling,
    };
    let bytes = serde_json::to_vec(&input).context("serializing inference input")?;
    Ok(blake3::hash(&bytes).to_hex().to_string())
}

/// A test database at a unique path that deletes its files on drop, including
/// after a panic. `cargo test` runs every test in one process on many threads,
/// so a wall-clock timestamp is not a unique name; a process-wide counter is.
#[cfg(test)]
pub(crate) struct TestDatabase {
    path: std::path::PathBuf,
}

#[cfg(test)]
impl TestDatabase {
    pub(crate) fn new(prefix: &str) -> Self {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "cairnworld-{prefix}-{}-{}.sqlite",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed),
        ));
        Self { path }
    }

    pub(crate) fn path(&self) -> &std::path::Path {
        &self.path
    }
}

#[cfg(test)]
impl Drop for TestDatabase {
    fn drop(&mut self) {
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", self.path.display()));
        }
    }
}

#[derive(Clone, Debug, FromRow, PartialEq)]
pub struct User {
    pub id: i64,
    pub email: String,
    pub display_name: String,
}

#[derive(Clone, Debug, FromRow, PartialEq)]
pub struct PlayerAgent {
    pub member_id: i64,
    pub world_id: i64,
    pub user_id: i64,
    pub character_id: i64,
    pub agent_id: i64,
}

/// A GM agent's identity plus the exact branch to use, passed explicitly to
/// context assembly since agent_id alone cannot select a GM history once a
/// world_gm plays several groups (design.md's "Groups and shared GM history").
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GroupHistory {
    pub agent_id: i64,
    pub branch_id: i64,
}

/// One character in `party_presence()`'s result: identity, display name and
/// current online state.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct PartyMember {
    pub character_id: i64,
    pub name: String,
    pub online: bool,
}

/// A group's announced online set and any outstanding arrival/departure
/// transition (design.md's `presence_narration`).
#[derive(Clone, Debug, PartialEq)]
pub struct PresenceNarration {
    pub group_id: i64,
    pub last_announced_character_ids: Vec<i64>,
    pub due_at: Option<String>,
    pub generation: i64,
}

#[derive(FromRow)]
struct PresenceNarrationRow {
    group_id: i64,
    last_announced_character_ids: String,
    due_at: Option<String>,
    generation: i64,
}

impl TryFrom<PresenceNarrationRow> for PresenceNarration {
    type Error = anyhow::Error;

    fn try_from(row: PresenceNarrationRow) -> Result<Self> {
        Ok(Self {
            group_id: row.group_id,
            last_announced_character_ids: serde_json::from_str(&row.last_announced_character_ids)
                .context("decoding announced character ids")?,
            due_at: row.due_at,
            generation: row.generation,
        })
    }
}

#[derive(Clone, Debug, FromRow, PartialEq)]
pub struct Invitation {
    pub token: String,
    pub world_id: i64,
    pub max_uses: Option<i64>,
    pub uses: i64,
}

#[derive(Clone, Debug, FromRow, PartialEq)]
pub struct World {
    pub id: i64,
    pub name: String,
    pub owner_id: i64,
    /// Owner-controlled inspection toggle. See `set_developer_mode`.
    pub developer_mode: bool,
}

#[derive(Clone, Debug, FromRow, PartialEq)]
pub struct WorldMember {
    pub user_id: i64,
    pub display_name: String,
    pub access: String,
    pub characters: Vec<WorldCharacter>,
}

#[derive(Clone, Debug, FromRow, PartialEq)]
pub struct WorldCharacter {
    pub id: i64,
    pub name: String,
}

#[derive(FromRow)]
struct WorldMemberCharacterRow {
    member_id: i64,
    user_id: i64,
    display_name: String,
    access: String,
    character_id: i64,
    character_name: String,
}

#[derive(Debug, FromRow)]
struct PlayerChatRow {
    id: i64,
    role: String,
    content: i64,
    trigger_sequence_id: Option<i64>,
    inference_sequence_id: Option<i64>,
}

/// A text entry safe to render in the player-facing chat. Tool calls and tool
/// results remain in the stored agent history but never cross this boundary.
#[derive(Clone, Debug, PartialEq)]
pub struct PlayerChatEntry {
    pub id: i64,
    pub role: Role,
    pub text: String,
    /// The sequence this entry belongs to, for Developer Mode's chat-entry
    /// inspect button: `trigger_sequence_id` for player input that began a
    /// turn, or the producing inference's sequence for anything an inference
    /// emitted. `None` only for content with neither, such as a manually
    /// inserted fixture message.
    pub sequence_id: Option<i64>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Sequence {
    pub id: i64,
    pub world_id: i64,
    pub trigger: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PendingAction {
    pub world_id: i64,
    pub id: i64,
    pub sequence_id: i64,
    /// The player's tool_execution row that submitted this action; its model
    /// call ID/name/arguments resolve from its inference's output when
    /// needed (design.md: "point to the original call").
    pub requesting_tool_execution_id: i64,
    pub character_id: i64,
    pub gm_agent_id: i64,
    pub gm_branch_id: i64,
    pub tool: String,
    pub args: String,
}

/// The persisted, authoritative outcome of resolving one pending action -
/// `resolve_pending_action`'s sole return shape whether this call newly
/// applied the decision or the action was already resolved. The caller
/// compares `approved` against the decision it requested rather than
/// `resolve_pending_action` itself erroring on a mismatch, so a conflicting
/// second resolve becomes an ordinary tool rejection at the call site, not a
/// generic infrastructure error.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ResolvedAction {
    pub id: i64,
    pub approved: bool,
    pub result_content_id: i64,
}

/// Player-visible state at the member's current location. It is derived from
/// the relationship graph rather than accepted from the browser or a model.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct PlayerScene {
    pub character_name: String,
    pub character_description: String,
    pub sheet: serde_json::Value,
    pub location_name: String,
    pub location_description: String,
    pub items: Vec<SceneItem>,
    pub inventory: Vec<SceneItem>,
    pub npcs: Vec<SceneNpc>,
}

#[derive(Clone, Debug, FromRow, PartialEq, Serialize)]
pub struct SceneItem {
    pub name: String,
    pub description: String,
}

#[derive(Clone, Debug, FromRow, PartialEq, Serialize)]
pub struct SceneNpc {
    pub name: String,
    pub description: String,
}

/// The location GM's stored scene packet. GM-only notes stay out of the
/// player packet even though both are assembled from the same world state.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct GmScene {
    pub location_name: String,
    pub location_description: String,
    pub gm_notes: serde_json::Value,
    /// The interacting group's own description and GM notes - distinct from
    /// the physical location's, since multiple groups may share one location
    /// (design.md's "Groups and shared GM history").
    pub group_description: String,
    pub group_gm_notes: serde_json::Value,
    pub items: Vec<SceneItem>,
    pub npcs: Vec<GmSceneNpc>,
    pub players: Vec<GmScenePlayer>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct GmSceneNpc {
    pub name: String,
    pub description: String,
    pub background: String,
    pub motive: String,
    pub ambition: String,
    pub gm_notes: serde_json::Value,
}

/// A player character physically present at the GM's location. Player-agent
/// history is private, but the GM needs this current character state to
/// narrate and arbitrate a shared scene consistently.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct GmScenePlayer {
    pub name: String,
    pub description: String,
    pub background: String,
    pub motive: String,
    pub ambition: String,
    pub sheet: serde_json::Value,
}

#[derive(FromRow)]
struct GmSceneNpcRow {
    name: String,
    description: String,
    background: String,
    motive: String,
    ambition: String,
    gm_notes: String,
}

#[derive(FromRow)]
struct GmScenePlayerRow {
    name: String,
    description: String,
    background: String,
    motive: String,
    ambition: String,
    sheet: String,
}

#[derive(Debug, FromRow)]
struct PendingActionRow {
    world_id: i64,
    id: i64,
    sequence_id: i64,
    requesting_tool_execution_id: i64,
    character_id: i64,
    gm_agent_id: i64,
    gm_branch_id: i64,
    tool: String,
    args: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TakeItemArguments {
    item: String,
}

impl From<PendingActionRow> for PendingAction {
    fn from(row: PendingActionRow) -> Self {
        Self {
            world_id: row.world_id,
            id: row.id,
            sequence_id: row.sequence_id,
            requesting_tool_execution_id: row.requesting_tool_execution_id,
            character_id: row.character_id,
            gm_agent_id: row.gm_agent_id,
            gm_branch_id: row.gm_branch_id,
            tool: row.tool,
            args: row.args,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct InstalledWorld {
    pub world_id: i64,
    pub member: PlayerAgent,
    pub character_handle: String,
}

struct JoinedWorld {
    member_id: i64,
    character_id: i64,
    agent_id: i64,
    character_tool_id: i64,
}

struct CharacterSeed<'a> {
    role: &'a str,
    name: &'a str,
    description: &'a str,
    background: &'a str,
    motive: &'a str,
    ambition: &'a str,
    sheet: &'a serde_json::Value,
    notes: &'a Notes,
}

fn decode_notes(gm: &str, storyteller: &str) -> Result<Notes> {
    Ok(Notes {
        gm: serde_json::from_str(gm).context("decoding GM notes")?,
        storyteller: serde_json::from_str(storyteller).context("decoding Storyteller notes")?,
    })
}

#[derive(Debug)]
pub enum InferenceOutcome {
    Response(Response),
    Error(String),
}

/// What `finish_chat_inference` produced: the shared content reference a
/// caller can pass to other recipients (e.g. GM narration delivered to every
/// present character) without re-recording the same text.
#[derive(Debug, Clone, Copy)]
pub struct ChatInferenceOutcome {
    pub output_content_id: Option<i64>,
}

/// Immutable input identity exists before execution, including failed attempts.
pub struct InferenceInput<'a> {
    pub agent_id: i64,
    pub sequence_id: Option<i64>,
    /// The invocation this inference belongs to. Every inference, including a
    /// standalone recipe run outside a live agent loop (e.g. detached replay,
    /// `main.rs`'s `replay` command), runs inside one - `inference.invocation_id`
    /// is `NOT NULL` (design.md's `invocation` is "one agent loop"; a detached
    /// replay still "needs its own throwaway sequence/invocation to satisfy
    /// `inference.invocation_id`"). Its ordinal within the invocation is
    /// allocated automatically (design.md: "`inference.invocation_id` plus
    /// ordinal orders successive inferences within that invocation").
    pub invocation_id: i64,
    pub segments: &'a [Segment],
    pub request: &'a Request,
    pub model: &'a str,
}

#[derive(Debug, FromRow)]
struct InferenceRow {
    id: i64,
    agent_id: i64,
    branch_id: i64,
    history_entry_id: i64,
    sequence_id: Option<i64>,
    invocation_id: i64,
    cause_inference_id: Option<i64>,
    segments: String,
    sampling: String,
    output: Option<i64>,
    error: Option<String>,
    input_hash: String,
    input_tokens: Option<i64>,
    output_tokens: Option<i64>,
    duration_ms: Option<i64>,
    model: String,
}

#[derive(Debug, FromRow)]
struct InferenceMessageRow {
    id: i64,
    role: String,
    source_tool_execution_id: Option<i64>,
}

impl TryFrom<InferenceMessageRow> for InferenceMessage {
    type Error = anyhow::Error;

    fn try_from(row: InferenceMessageRow) -> Result<Self> {
        Ok(Self {
            id: row.id,
            role: serde_json::from_str(&row.role).context("decoding message role")?,
            answers_tool_execution_id: row.source_tool_execution_id,
        })
    }
}

// design.md's `action` no longer copies `tool`/`args`/`result` text:
// `tool`/`args`/`call_id` resolve from the requesting tool_execution's
// originating inference output at its ordinal (`Store::resolve_tool_call`),
// and `result` from `result_content_id` - both decoded once by
// `inference_actions` rather than by every reader of `RecordedAction`.
#[derive(Debug, FromRow)]
struct RecordedActionRow {
    id: i64,
    /// `None` only for the server-driven test path with no requesting
    /// tool_execution at all (`action.requesting_tool_execution_id` NULL).
    request_inference_id: Option<i64>,
    request_ordinal: Option<i64>,
    request_output: Option<i64>,
    approval_inference_id: Option<i64>,
    result_content_id: i64,
}

// --- Developer Mode inspection view types (crate::inspection) -------------

#[derive(Debug, FromRow)]
struct SequenceSummaryRow {
    id: i64,
    trigger: String,
    started_at: String,
    inference_count: i64,
}

impl TryFrom<SequenceSummaryRow> for SequenceSummary {
    type Error = anyhow::Error;

    fn try_from(row: SequenceSummaryRow) -> Result<Self> {
        Ok(Self {
            id: row.id,
            trigger: row.trigger,
            started_at: row.started_at,
            inference_count: row.inference_count,
        })
    }
}

#[derive(Debug, FromRow)]
struct AgentInfoRow {
    id: i64,
    role: String,
    character_name: Option<String>,
    location_name: Option<String>,
}

impl TryFrom<AgentInfoRow> for AgentInfo {
    type Error = anyhow::Error;

    fn try_from(row: AgentInfoRow) -> Result<Self> {
        Ok(Self {
            id: row.id,
            role: row.role,
            character_name: row.character_name,
            location_name: row.location_name,
        })
    }
}

#[derive(Debug, FromRow)]
struct RawMessageRow {
    id: i64,
    role: String,
    origin: String,
    content: i64,
    source_inference_id: Option<i64>,
    source_tool_execution_id: Option<i64>,
}

#[derive(Debug, FromRow)]
struct RawSummaryRow {
    id: i64,
    covered_ranges: String,
    inference_id: i64,
    content: String,
    reason: String,
    retained_messages: i64,
}

#[derive(Debug, FromRow)]
struct WorldObjectRow {
    name: String,
    time: i64,
    initial_prompt: String,
    storyteller_summary: String,
    gm_notes: String,
    storyteller_notes: String,
}

#[derive(Debug, FromRow)]
struct ObjectIndexRow {
    object_type: String,
    id: i64,
    name: String,
    description: String,
    gm_notes: String,
    storyteller_notes: String,
}

#[derive(Debug, FromRow)]
struct ObjectLinkRow {
    object_type: String,
    id: i64,
    label: String,
}

/// The filter and stable boundary for one current-world object page.
#[derive(Clone, Debug)]
pub struct WorldObjectsQuery {
    pub object_type: Option<ObjectType>,
    pub object_id: Option<i64>,
    pub search: Option<String>,
    pub after: Option<WorldObjectCursor>,
    pub limit: i64,
}

#[derive(Debug, FromRow)]
struct MessageRow {
    id: i64,
    role: String,
    origin: String,
    content: i64,
}

#[derive(Debug, FromRow)]
struct SummaryRow {
    id: i64,
    history_entry_id: i64,
    agent_id: i64,
    covered_ranges: String,
    content: String,
    inference_id: i64,
    reason: String,
    retained_messages: i64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Summary {
    pub id: i64,
    pub history_entry_id: i64,
    pub agent_id: i64,
    pub covered_ranges: Vec<PositionRange>,
    pub content: String,
    pub inference_id: i64,
    pub reason: String,
    pub retained_messages: i64,
}

impl TryFrom<SummaryRow> for Summary {
    type Error = anyhow::Error;

    fn try_from(row: SummaryRow) -> Result<Self> {
        Ok(Self {
            id: row.id,
            history_entry_id: row.history_entry_id,
            agent_id: row.agent_id,
            covered_ranges: serde_json::from_str(&row.covered_ranges)
                .context("decoding summary covered ranges")?,
            content: row.content,
            inference_id: row.inference_id,
            reason: row.reason,
            retained_messages: row.retained_messages,
        })
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct PendingCompaction {
    pub agent_id: i64,
    pub branch_id: i64,
    /// The branch position this obligation covers: the reply (or
    /// capacity-rejected history) that made compaction due.
    pub position: i64,
    /// The sequence whose final reply queued this job, carried onto the
    /// compaction inference. NULL for a fixed-KV recovery outside any sequence.
    pub sequence_id: Option<i64>,
    /// The inference whose reply queued this job, carried onto the deferred
    /// compaction invocation as its `cause_inference_id`. NULL for a fixed-KV
    /// rejection, which has no settled inference to attribute it to.
    pub cause_inference_id: Option<i64>,
    pub next_input_tokens: usize,
    pub sampling: Sampling,
    pub model: String,
    /// Static prompt and tool segments from the normal request that queued
    /// this job. Raw history is always rebuilt from its current stored form.
    pub static_segments: Vec<Segment>,
}

/// The compaction policy `finish_chat_inference` checks a settled (non-tool-
/// call) reply against. Whether the outcome actually was a settled reply -
/// the only case that can enqueue compaction - is decided from the outcome
/// itself (`Content::Text`, not `ToolCalls`), not by the caller.
#[derive(Clone, Copy)]
pub struct CompactionPolicy<'a> {
    pub sequence_id: Option<i64>,
    pub compact_before_next_input_tokens: usize,
    pub sampling: &'a Sampling,
    pub model: &'a str,
    pub static_segments: &'a [Segment],
}

#[derive(Debug, FromRow)]
struct PendingCompactionRow {
    agent_id: i64,
    branch_id: i64,
    position: i64,
    sequence_id: Option<i64>,
    cause_inference_id: Option<i64>,
    next_input_tokens: i64,
    sampling: String,
    model: String,
    static_segments: String,
}

impl TryFrom<PendingCompactionRow> for PendingCompaction {
    type Error = anyhow::Error;

    fn try_from(row: PendingCompactionRow) -> Result<Self> {
        Ok(Self {
            agent_id: row.agent_id,
            branch_id: row.branch_id,
            position: row.position,
            sequence_id: row.sequence_id,
            cause_inference_id: row.cause_inference_id,
            next_input_tokens: usize::try_from(row.next_input_tokens)
                .context("pending compaction has a negative next-input token count")?,
            sampling: serde_json::from_str(&row.sampling)
                .context("deserializing pending compaction sampling")?,
            model: row.model,
            static_segments: serde_json::from_str(&row.static_segments)
                .context("deserializing pending compaction static segments")?,
        })
    }
}

impl Store {
    pub async fn world_ids(&self) -> Result<Vec<i64>> {
        sqlx::query_scalar("SELECT id FROM world ORDER BY id")
            .fetch_all(&self.pool)
            .await
            .context("loading worlds")
    }

    pub async fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating database directory {}", parent.display()))?;
        }
        let options = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            .foreign_keys(true)
            .journal_mode(SqliteJournalMode::Wal);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .with_context(|| format!("opening SQLite database {}", path.display()))?;
        sqlx::migrate!("./migrations")
            .run(&pool)
            .await
            .context("running database migrations")?;
        Ok(Self {
            pool,
            dev_events: Arc::new(Mutex::new(HashMap::new())),
        })
    }

    /// Sessions and game records intentionally share SQLite's durability and
    /// process lifetime. The session crate owns its session tables; Store owns
    /// every Cairnworld table.
    pub fn pool(&self) -> SqlitePool {
        self.pool.clone()
    }

    /// Subscribes to one world's live Developer Mode events. Deliberately
    /// unauthenticated by itself - per the live inspection protocol, a
    /// caller subscribes *before* running the initial access query, so a
    /// membership/mode change racing the subscribe is never missed between
    /// "check access" and "start listening". The connection holder is
    /// responsible for calling `access_for` before treating anything as
    /// authorized. The channel is created lazily on first subscribe and kept
    /// for the process lifetime - worlds are few and long-lived, unlike
    /// per-location chat broadcasts.
    pub fn subscribe_dev_events(&self, world_id: i64) -> broadcast::Receiver<DevEvent> {
        self.dev_events
            .lock()
            .expect("dev_events lock poisoned")
            .entry(world_id)
            .or_insert_with(|| broadcast::channel(256).0)
            .subscribe()
    }

    /// Publishes one live event to `world_id`'s Developer Mode subscribers, if
    /// any are connected. A dropped send (no receivers) is not an error - see
    /// `DevEvent`'s doc comment: every event is recoverable from durable state.
    fn publish_dev_event(&self, world_id: i64, event: DevEvent) {
        if let Some(sender) = self
            .dev_events
            .lock()
            .expect("dev_events lock poisoned")
            .get(&world_id)
        {
            let _ = sender.send(event);
        }
    }

    pub async fn create_world(&self, name: &str) -> Result<i64> {
        let result = sqlx::query("INSERT INTO world (name) VALUES (?)")
            .bind(name)
            .execute(&self.pool)
            .await
            .context("creating world")?;
        Ok(result.last_insert_rowid())
    }

    pub async fn create_agent(&self, world_id: i64) -> Result<i64> {
        let mut transaction =
            self.pool.begin().await.with_context(|| {
                format!("starting agent creation transaction for world {world_id}")
            })?;
        let agent_id = sqlx::query("INSERT INTO agent (world_id) VALUES (?)")
            .bind(world_id)
            .execute(&mut *transaction)
            .await
            .with_context(|| format!("creating agent in world {world_id}"))?
            .last_insert_rowid();
        Self::create_agent_history(&mut transaction, agent_id).await?;
        transaction
            .commit()
            .await
            .with_context(|| format!("committing agent creation for world {world_id}"))?;
        Ok(agent_id)
    }

    /// Create the one writable history branch every chat agent needs
    /// (`create_agent`'s single choke point, plus scenario installation's
    /// direct `agent` inserts for location GMs, NPCs and the initial player
    /// character). A GM agent gets a row here like any other in this slice;
    /// GM heads move to groups once groups exist.
    async fn create_agent_history(
        transaction: &mut Transaction<'_, Sqlite>,
        agent_id: i64,
    ) -> Result<i64> {
        let branch_id = sqlx::query("INSERT INTO history_branch (agent_id) VALUES (?)")
            .bind(agent_id)
            .execute(&mut **transaction)
            .await
            .with_context(|| format!("creating history branch for agent {agent_id}"))?
            .last_insert_rowid();
        sqlx::query("INSERT INTO agent_history (agent_id, branch_id) VALUES (?, ?)")
            .bind(agent_id)
            .bind(branch_id)
            .execute(&mut **transaction)
            .await
            .with_context(|| format!("linking history branch for agent {agent_id}"))?;
        Ok(branch_id)
    }

    /// Toggle the world's developer inspection views, in either direction, as
    /// its owner. Recording is unaffected - it runs for every game regardless.
    /// Enabling only decides whether active members can open the inspection
    /// views, which expose complete internal game state.
    pub async fn set_developer_mode(
        &self,
        owner_id: i64,
        world_id: i64,
        enabled: bool,
    ) -> Result<()> {
        let result = sqlx::query(
            "UPDATE world SET developer_mode = ? \
             WHERE id = ? AND EXISTS (SELECT 1 FROM world_owner WHERE world_id = ? AND user_id = ?)",
        )
        .bind(enabled)
        .bind(world_id)
        .bind(world_id)
        .bind(owner_id)
        .execute(&self.pool)
        .await
        .with_context(|| format!("setting developer mode for world {world_id}"))?;
        ensure!(
            result.rows_affected() == 1,
            "user {owner_id} does not own world {world_id}"
        );
        self.publish_dev_event(world_id, DevEvent::DeveloperModeChanged { enabled });
        self.publish_dev_event(world_id, DevEvent::AccessChanged);
        Ok(())
    }

    /// The one indexed query behind every inspection authorization decision:
    /// whether `viewer_id` is currently an active member of `world_id`, and
    /// whether that world's Developer Mode is currently enabled. Shared by
    /// `inspecting` (HTTP routes) and the live inspection websocket, which
    /// re-runs it before every private outbound frame - see design.md's
    /// "Live inspection protocol". A single query keeps membership and mode
    /// authoritative from one source rather than caching either half.
    pub async fn access_for(&self, viewer_id: i64, world_id: i64) -> Result<Access> {
        let row: Option<(bool,)> = sqlx::query_as(
            "SELECT world.developer_mode FROM world_member \
             JOIN world ON world.id = world_member.world_id \
             WHERE world_member.world_id = ? AND world_member.user_id = ? \
               AND world_member.access = 'active'",
        )
        .bind(world_id)
        .bind(viewer_id)
        .fetch_optional(&self.pool)
        .await
        .with_context(|| format!("checking inspection access for user {viewer_id}"))?;
        Ok(match row {
            Some((developer_mode,)) => Access {
                active_member: true,
                developer_mode,
            },
            None => Access {
                active_member: false,
                developer_mode: false,
            },
        })
    }

    /// Fail unless `viewer_id` is an active member of `world_id` and its
    /// developer mode is enabled. Every inspection HTTP query calls this
    /// first, so a disabled world or a removed/banned member is rejected
    /// before any data query runs.
    async fn inspecting(&self, viewer_id: i64, world_id: i64) -> Result<()> {
        let access = self.access_for(viewer_id, world_id).await?;
        // One combined check and one message: a non-member must not learn
        // whether the world exists or what its developer-mode state is.
        ensure!(
            access.active_member && access.developer_mode,
            "user {viewer_id} cannot inspect world {world_id}: not an active member, or developer mode is off"
        );
        Ok(())
    }

    /// Create a revocable, unguessable invitation as the world's owner. A
    /// membership remains the only long-lived access relationship.
    pub async fn create_invitation(
        &self,
        owner_id: i64,
        world_id: i64,
        max_uses: Option<i64>,
    ) -> Result<Invitation> {
        if let Some(max_uses) = max_uses {
            ensure!(max_uses > 0, "an invitation use limit must be positive");
        }
        let owns: Option<i64> =
            sqlx::query_scalar("SELECT 1 FROM world_owner WHERE world_id = ? AND user_id = ?")
                .bind(world_id)
                .bind(owner_id)
                .fetch_optional(&self.pool)
                .await
                .with_context(|| format!("checking ownership of world {world_id}"))?;
        ensure!(
            owns.is_some(),
            "user {owner_id} does not own world {world_id}"
        );
        let token = rand::rng()
            .sample_iter(Alphanumeric)
            .take(32)
            .map(char::from)
            .collect::<String>();
        sqlx::query("INSERT INTO world_invitation (token, world_id, max_uses) VALUES (?, ?, ?)")
            .bind(&token)
            .bind(world_id)
            .bind(max_uses)
            .execute(&self.pool)
            .await
            .with_context(|| format!("creating invitation for world {world_id}"))?;
        Ok(Invitation {
            token,
            world_id,
            max_uses,
            uses: 0,
        })
    }

    /// Accept a live invitation. Existing memberships regain access rather
    /// than silently creating a second player history or Adventurer.
    pub async fn accept_invitation(&self, user: &User, token: &str) -> Result<PlayerAgent> {
        let mut transaction = self
            .pool
            .begin()
            .await
            .context("starting invitation acceptance")?;
        let invitation: Invitation = sqlx::query_as(
            "SELECT token, world_id, max_uses, uses FROM world_invitation WHERE token = ?",
        )
        .bind(token)
        .fetch_optional(&mut *transaction)
        .await
        .context("loading invitation")?
        .with_context(|| format!("invitation `{token}` does not exist or was revoked"))?;
        let existing: Option<PlayerAgent> = sqlx::query_as(
            "SELECT member.id AS member_id, member.world_id, member.user_id, player.character_id, player.agent_id \
             FROM world_member AS member \
             JOIN player_character AS player ON player.member_id = member.id \
             WHERE member.world_id = ? AND member.user_id = ?",
        )
        .bind(invitation.world_id)
        .bind(user.id)
        .fetch_optional(&mut *transaction)
        .await
        .context("loading existing invitation membership")?;
        if let Some(member) = existing {
            sqlx::query(
                "UPDATE world_member SET access = 'active' WHERE id = ? AND access = 'removed'",
            )
            .bind(member.member_id)
            .execute(&mut *transaction)
            .await
            .context("restoring invitation membership")?;
            transaction
                .commit()
                .await
                .context("committing returning member")?;
            return Ok(member);
        }
        let consumed = sqlx::query(
            "UPDATE world_invitation SET uses = uses + 1 \
             WHERE token = ? AND (max_uses IS NULL OR uses < max_uses)",
        )
        .bind(token)
        .execute(&mut *transaction)
        .await
        .context("consuming invitation slot")?
        .rows_affected();
        ensure!(consumed == 1, "invitation `{token}` has no remaining slots");
        let starting_group_id: i64 = sqlx::query_scalar(
            "SELECT character_group.group_id FROM player_character \
             JOIN character_group ON character_group.character_id = player_character.character_id \
             JOIN world_member ON world_member.id = player_character.member_id \
             JOIN world_owner ON world_owner.world_id = world_member.world_id AND world_owner.user_id = world_member.user_id \
             WHERE world_member.world_id = ? ORDER BY player_character.member_id LIMIT 1",
        )
        .bind(invitation.world_id)
        .fetch_optional(&mut *transaction)
        .await
        .context("finding invitation world starting group")?
        .with_context(|| {
            format!(
                "world {} has no owner Adventurer group",
                invitation.world_id
            )
        })?;
        let joined = Self::join_world(
            &mut transaction,
            user,
            invitation.world_id,
            starting_group_id,
        )
        .await
        .context("creating invited player membership")?;
        transaction
            .commit()
            .await
            .context("committing invitation acceptance")?;
        Ok(PlayerAgent {
            member_id: joined.member_id,
            world_id: invitation.world_id,
            user_id: user.id,
            character_id: joined.character_id,
            agent_id: joined.agent_id,
        })
    }

    pub async fn revoke_invitation(&self, owner_id: i64, world_id: i64, token: &str) -> Result<()> {
        let result = sqlx::query(
            "DELETE FROM world_invitation WHERE token = ? AND world_id = ? \
             AND EXISTS (SELECT 1 FROM world_owner WHERE world_id = ? AND user_id = ?)",
        )
        .bind(token)
        .bind(world_id)
        .bind(world_id)
        .bind(owner_id)
        .execute(&self.pool)
        .await
        .with_context(|| format!("revoking invitation `{token}` for world {world_id}"))?;
        ensure!(
            result.rows_affected() == 1,
            "invitation `{token}` is not owned by user {owner_id} in world {world_id}"
        );
        Ok(())
    }

    /// An owner may revoke a member's current access without deleting the
    /// membership, player history, or character it owns.
    pub async fn remove_member(&self, owner_id: i64, world_id: i64, user_id: i64) -> Result<()> {
        let result = sqlx::query(
            "UPDATE world_member SET access = 'removed' WHERE world_id = ? AND user_id = ? \
             AND access = 'active' AND user_id != (SELECT user_id FROM world_owner WHERE world_id = ?) \
             AND EXISTS (SELECT 1 FROM world_owner WHERE world_id = ? AND user_id = ?)",
        )
        .bind(world_id)
        .bind(user_id)
        .bind(world_id)
        .bind(world_id)
        .bind(owner_id)
        .execute(&self.pool)
        .await
        .with_context(|| format!("removing user {user_id} from world {world_id}"))?;
        ensure!(
            result.rows_affected() == 1,
            "user {user_id} is not an active removable member of world {world_id} owned by user {owner_id}"
        );
        self.publish_dev_event(world_id, DevEvent::AccessChanged);
        Ok(())
    }

    /// Start one externally visible event. All work it triggers carries this
    /// id, including recursive agent calls and their tool executions.
    pub async fn begin_sequence(&self, world_id: i64, trigger: &str) -> Result<Sequence> {
        let result = sqlx::query("INSERT INTO sequence (world_id, trigger) VALUES (?, ?)")
            .bind(world_id)
            .bind(trigger)
            .execute(&self.pool)
            .await
            .with_context(|| format!("starting {trigger} sequence in world {world_id}"))?;
        Ok(Sequence {
            id: result.last_insert_rowid(),
            world_id,
            trigger: trigger.to_string(),
        })
    }

    /// Finish the sequence when its foreground (root, player-triggered)
    /// invocation returns. Deferred work caused by this sequence's own reply
    /// (e.g. compaction) is not part of its wall time (design.md: "Pending
    /// compaction is shown as caused deferred work rather than extending the
    /// foreground reply's wall time").
    pub async fn finish_sequence(&self, sequence_id: i64, error: Option<&str>) -> Result<()> {
        let result = sqlx::query(
            "UPDATE sequence SET finished_at = CURRENT_TIMESTAMP, error = ? \
             WHERE id = ? AND finished_at IS NULL",
        )
        .bind(error)
        .bind(sequence_id)
        .execute(&self.pool)
        .await
        .with_context(|| format!("finishing sequence {sequence_id}"))?;
        ensure!(
            result.rows_affected() == 1,
            "sequence {sequence_id} is missing or already finished"
        );
        Ok(())
    }

    /// Start one agent loop (design.md's `invocation`): the root of a
    /// sequence (`parent_tool_execution_id` NULL, `cause_inference_id` NULL),
    /// a nested call spawned by an exact tool_execution, or deferred
    /// compaction (its triggering inference as `cause_inference_id`).
    /// `branch_id` selects an explicit branch (a world_gm agent has one per
    /// group - design.md's `GroupHistory { agent_id, branch_id }`); `None`
    /// resolves the agent's single `agent_history` branch as before.
    pub async fn start_invocation(
        &self,
        sequence_id: Option<i64>,
        agent_id: i64,
        branch_id: Option<i64>,
        parent_tool_execution_id: Option<i64>,
        cause_inference_id: Option<i64>,
    ) -> Result<i64> {
        let branch_id = match branch_id {
            Some(branch_id) => branch_id,
            None => self.branch_for_agent(agent_id).await?,
        };
        let result = sqlx::query(
            "INSERT INTO invocation (sequence_id, agent_id, branch_id, parent_tool_execution_id, cause_inference_id) \
             VALUES (?, ?, ?, ?, ?)",
        )
        .bind(sequence_id)
        .bind(agent_id)
        .bind(branch_id)
        .bind(parent_tool_execution_id)
        .bind(cause_inference_id)
        .execute(&self.pool)
        .await
        .with_context(|| format!("starting invocation for agent {agent_id}"))?;
        Ok(result.last_insert_rowid())
    }

    /// Finish an invocation's own outcome on every exit path, including
    /// schema rejection, execution error and safety-budget exhaustion
    /// (design.md: "An invocation wrapper finishes its own outcome on every
    /// exit"). `outcome` is a short label ("settled", "error", "interrupted"),
    /// not a duplicate of the failing inference's own recorded error.
    pub async fn finish_invocation(&self, invocation_id: i64, outcome: &str) -> Result<()> {
        let result = sqlx::query(
            "UPDATE invocation SET finished_at = CURRENT_TIMESTAMP, outcome = ? \
             WHERE id = ? AND finished_at IS NULL",
        )
        .bind(outcome)
        .bind(invocation_id)
        .execute(&self.pool)
        .await
        .with_context(|| format!("finishing invocation {invocation_id}"))?;
        ensure!(
            result.rows_affected() == 1,
            "invocation {invocation_id} is missing or already finished"
        );
        Ok(())
    }

    /// The inference whose tool call caused `invocation_id` to start, for
    /// display as an inference's `parent_inference_id` (the DevEvent/UI
    /// shape that consumed the old direct pointer). Resolved by joining this
    /// invocation's `parent_tool_execution_id` back to the inference that
    /// produced it. `None` for a root or deferred-compaction invocation.
    async fn invocation_parent_inference_id(&self, invocation_id: i64) -> Result<Option<i64>> {
        sqlx::query_scalar(
            "SELECT tool_execution.inference_id FROM invocation \
             JOIN tool_execution ON tool_execution.id = invocation.parent_tool_execution_id \
             WHERE invocation.id = ?",
        )
        .bind(invocation_id)
        .fetch_optional(&self.pool)
        .await
        .with_context(|| format!("resolving parent inference for invocation {invocation_id}"))
    }

    /// Create an account only when its verified OAuth email is first seen.
    /// Display names deliberately participate in neither identity nor lookup.
    pub async fn find_or_create_user(&self, email: &str, display_name: &str) -> Result<User> {
        sqlx::query(
            "INSERT INTO user (email, display_name) VALUES (?, ?) ON CONFLICT(email) DO NOTHING",
        )
        .bind(email)
        .bind(display_name)
        .execute(&self.pool)
        .await
        .with_context(|| format!("creating user for {email}"))?;
        sqlx::query_as("SELECT id, email, display_name FROM user WHERE email = ?")
            .bind(email)
            .fetch_one(&self.pool)
            .await
            .with_context(|| format!("loading user for {email}"))
    }

    pub async fn user(&self, user_id: i64) -> Result<Option<User>> {
        sqlx::query_as("SELECT id, email, display_name FROM user WHERE id = ?")
            .bind(user_id)
            .fetch_optional(&self.pool)
            .await
            .with_context(|| format!("loading user {user_id}"))
    }

    /// Change presentation only; OAuth email remains the account identity.
    pub async fn rename_user(&self, user_id: i64, display_name: &str) -> Result<User> {
        sqlx::query("UPDATE user SET display_name = ? WHERE id = ?")
            .bind(display_name)
            .bind(user_id)
            .execute(&self.pool)
            .await
            .with_context(|| format!("updating display name for user {user_id}"))?;
        self.user(user_id)
            .await?
            .with_context(|| format!("display-name update references missing user {user_id}"))
    }

    pub async fn owned_worlds(&self, owner_id: i64) -> Result<Vec<World>> {
        sqlx::query_as(
            "SELECT world.id, world.name, world_owner.user_id AS owner_id, world.developer_mode FROM world \
             JOIN world_owner ON world_owner.world_id = world.id \
             WHERE world_owner.user_id = ? ORDER BY world.id",
        )
        .bind(owner_id)
        .fetch_all(&self.pool)
        .await
        .with_context(|| format!("loading worlds owned by user {owner_id}"))
    }

    pub async fn world(&self, world_id: i64) -> Result<Option<World>> {
        sqlx::query_as(
            "SELECT world.id, world.name, world_owner.user_id AS owner_id, world.developer_mode FROM world \
             JOIN world_owner ON world_owner.world_id = world.id WHERE world.id = ?",
        )
        .bind(world_id)
        .fetch_optional(&self.pool)
        .await
        .with_context(|| format!("loading world {world_id}"))
    }

    pub async fn world_members(&self, world_id: i64) -> Result<Vec<WorldMember>> {
        let rows: Vec<WorldMemberCharacterRow> = sqlx::query_as(
            "SELECT member.id AS member_id, member.user_id, user.display_name, member.access, character.id AS character_id, character.name AS character_name \
             FROM world_member AS member \
             JOIN user ON user.id = member.user_id \
             JOIN player_character ON player_character.member_id = member.id \
             JOIN character ON character.id = player_character.character_id \
             WHERE member.world_id = ? ORDER BY member.id, character.id",
        )
        .bind(world_id)
        .fetch_all(&self.pool)
        .await
        .with_context(|| format!("loading members of world {world_id}"))?;
        let mut members = Vec::<WorldMember>::new();
        let mut member_id = None;
        for row in rows {
            if member_id != Some(row.member_id) {
                member_id = Some(row.member_id);
                members.push(WorldMember {
                    user_id: row.user_id,
                    display_name: row.display_name,
                    access: row.access,
                    characters: vec![],
                });
            }
            members
                .last_mut()
                .expect("member row was just added")
                .characters
                .push(WorldCharacter {
                    id: row.character_id,
                    name: row.character_name,
                });
        }
        Ok(members)
    }

    pub async fn invitations(&self, owner_id: i64, world_id: i64) -> Result<Vec<Invitation>> {
        sqlx::query_as(
            "SELECT invitation.token, invitation.world_id, invitation.max_uses, invitation.uses \
             FROM world_invitation AS invitation JOIN world_owner \
             ON world_owner.world_id = invitation.world_id \
             WHERE invitation.world_id = ? AND world_owner.user_id = ? ORDER BY invitation.rowid",
        )
        .bind(world_id)
        .bind(owner_id)
        .fetch_all(&self.pool)
        .await
        .with_context(|| format!("loading invitations for world {world_id}"))
    }

    /// Install a checked-in scenario and join its owner in one transaction.
    /// This is the same membership construction invitations will use: a real
    /// player world cannot exist without an active member, player history, and
    /// initial Adventurer.
    pub async fn install_scenario(
        &self,
        owner: &User,
        scenario: &Scenario,
    ) -> Result<InstalledWorld> {
        scenario.validate()?;
        let mut transaction = self
            .pool
            .begin()
            .await
            .with_context(|| format!("starting scenario transaction for {}", owner.email))?;
        let world_id = sqlx::query(
            "INSERT INTO world (name, initial_prompt, storyteller_summary, gm_notes, storyteller_notes) \
             VALUES (?, ?, ?, ?, ?)",
        )
            .bind(&scenario.name)
            .bind(&scenario.initial_prompt)
            .bind(&scenario.storyteller_summary)
            .bind(serde_json::to_string(&scenario.notes.gm)?)
            .bind(serde_json::to_string(&scenario.notes.storyteller)?)
            .execute(&mut *transaction)
            .await
            .context("creating scenario world")?
            .last_insert_rowid();
        sqlx::query("INSERT INTO world_owner (world_id, user_id) VALUES (?, ?)")
            .bind(world_id)
            .bind(owner.id)
            .execute(&mut *transaction)
            .await
            .with_context(|| format!("recording owner for world {world_id}"))?;

        let mut locations = HashMap::new();
        for location in &scenario.locations {
            let location_id = sqlx::query(
                "INSERT INTO location (world_id, name, kind, description, gm_notes, storyteller_notes) \
                 VALUES (?, ?, ?, ?, ?, ?)",
            )
            .bind(world_id)
            .bind(&location.name)
            .bind(&location.kind)
            .bind(&location.description)
            .bind(serde_json::to_string(&location.notes.gm)?)
            .bind(serde_json::to_string(&location.notes.storyteller)?)
            .execute(&mut *transaction)
            .await
            .with_context(|| format!("creating location {}", location.name))?
            .last_insert_rowid();
            locations.insert(location.name.as_str(), location_id);
        }

        for connection in &scenario.connections {
            sqlx::query(
                "INSERT INTO connection (world_id, from_location_id, to_location_id, travel_time, description, gm_notes, storyteller_notes) \
                 VALUES (?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(world_id)
            .bind(locations[connection.from.as_str()])
            .bind(locations[connection.to.as_str()])
            .bind(connection.travel_time)
            .bind(&connection.description)
            .bind(serde_json::to_string(&connection.notes.gm)?)
            .bind(serde_json::to_string(&connection.notes.storyteller)?)
            .execute(&mut *transaction)
            .await
            .context("creating scenario connection")?;
        }

        let world_gm_agent_id = sqlx::query("INSERT INTO agent (world_id) VALUES (?)")
            .bind(world_id)
            .execute(&mut *transaction)
            .await
            .with_context(|| format!("creating world GM for world {world_id}"))?
            .last_insert_rowid();
        sqlx::query("INSERT INTO world_gm (world_id, agent_id) VALUES (?, ?)")
            .bind(world_id)
            .bind(world_gm_agent_id)
            .execute(&mut *transaction)
            .await
            .with_context(|| format!("linking world GM for world {world_id}"))?;

        let mut groups = HashMap::new();
        for group in &scenario.groups {
            let branch_id = sqlx::query("INSERT INTO history_branch (agent_id) VALUES (?)")
                .bind(world_gm_agent_id)
                .execute(&mut *transaction)
                .await
                .with_context(|| format!("creating history branch for group {}", group.name))?
                .last_insert_rowid();
            let group_id = sqlx::query(
                "INSERT INTO \"group\" (world_id, name, location_id, description, branch_id, gm_notes, storyteller_notes) \
                 VALUES (?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(world_id)
            .bind(&group.name)
            .bind(locations[group.location.as_str()])
            .bind(&group.description)
            .bind(branch_id)
            .bind(serde_json::to_string(&group.notes.gm)?)
            .bind(serde_json::to_string(&group.notes.storyteller)?)
            .execute(&mut *transaction)
            .await
            .with_context(|| format!("creating group {}", group.name))?
            .last_insert_rowid();
            groups.insert(group.name.as_str(), group_id);
        }

        let mut item_types = HashMap::new();
        for item_type in &scenario.item_types {
            let item_type_id = sqlx::query(
                "INSERT INTO item_type (world_id, name, description, stats, gm_notes, storyteller_notes) VALUES (?, ?, ?, ?, ?, ?)",
            )
            .bind(world_id)
            .bind(&item_type.name)
            .bind(&item_type.description)
            .bind(serde_json::to_string(&item_type.stats)?)
            .bind(serde_json::to_string(&item_type.notes.gm)?)
            .bind(serde_json::to_string(&item_type.notes.storyteller)?)
            .execute(&mut *transaction)
            .await
            .with_context(|| format!("creating item type {}", item_type.name))?
            .last_insert_rowid();
            item_types.insert(item_type.name.as_str(), item_type_id);
        }
        for spell_type in &scenario.spell_types {
            sqlx::query(
                "INSERT INTO spell_type (world_id, name, description, stats, gm_notes, storyteller_notes) VALUES (?, ?, ?, ?, ?, ?)",
            )
            .bind(world_id)
            .bind(&spell_type.name)
            .bind(&spell_type.description)
            .bind(serde_json::to_string(&spell_type.stats)?)
            .bind(serde_json::to_string(&spell_type.notes.gm)?)
            .bind(serde_json::to_string(&spell_type.notes.storyteller)?)
            .execute(&mut *transaction)
            .await
            .with_context(|| format!("creating spell type {}", spell_type.name))?;
        }
        for item in &scenario.items {
            let item_id = sqlx::query(
                "INSERT INTO item (world_id, item_type_id, name, description, gm_notes, storyteller_notes) \
                 VALUES (?, ?, ?, ?, ?, ?)",
            )
            .bind(world_id)
            .bind(item_types[item.item_type.as_str()])
            .bind(&item.name)
            .bind(&item.description)
            .bind(serde_json::to_string(&item.notes.gm)?)
            .bind(serde_json::to_string(&item.notes.storyteller)?)
            .execute(&mut *transaction)
            .await
            .with_context(|| format!("creating item {}", item.name))?
            .last_insert_rowid();
            sqlx::query("INSERT INTO item_location (item_id, location_id) VALUES (?, ?)")
                .bind(item_id)
                .bind(locations[item.location.as_str()])
                .execute(&mut *transaction)
                .await
                .with_context(|| format!("placing item {}", item.name))?;
        }

        for npc in &scenario.npcs {
            let character_id = Self::insert_character(
                &mut transaction,
                world_id,
                CharacterSeed {
                    role: "npc",
                    name: &npc.name,
                    description: &npc.description,
                    background: &npc.background,
                    motive: &npc.motive,
                    ambition: &npc.ambition,
                    sheet: &npc.sheet,
                    notes: &npc.notes,
                },
            )
            .await?;
            sqlx::query(
                "INSERT INTO character_group (character_id, group_id, description) VALUES (?, ?, '')",
            )
            .bind(character_id)
            .bind(groups[npc.group.as_str()])
            .execute(&mut *transaction)
            .await
            .with_context(|| format!("placing NPC {}", npc.name))?;
            let agent_id = sqlx::query("INSERT INTO agent (world_id) VALUES (?)")
                .bind(world_id)
                .execute(&mut *transaction)
                .await
                .with_context(|| format!("creating NPC agent for {}", npc.name))?
                .last_insert_rowid();
            Self::create_agent_history(&mut transaction, agent_id).await?;
            sqlx::query("INSERT INTO npc_agent (character_id, agent_id) VALUES (?, ?)")
                .bind(character_id)
                .bind(agent_id)
                .execute(&mut *transaction)
                .await
                .with_context(|| format!("linking NPC agent for {}", npc.name))?;
        }

        sqlx::query("INSERT INTO world_starting_location (world_id, location_id) VALUES (?, ?)")
            .bind(world_id)
            .bind(locations[scenario.starting_location.as_str()])
            .execute(&mut *transaction)
            .await
            .with_context(|| format!("recording starting location for world {world_id}"))?;

        let member = Self::join_world(
            &mut transaction,
            owner,
            world_id,
            groups[scenario.starting_group.as_str()],
        )
        .await?;
        transaction
            .commit()
            .await
            .with_context(|| format!("committing scenario world {world_id}"))?;
        Ok(InstalledWorld {
            world_id,
            character_handle: format!("char{}", member.character_tool_id),
            member: PlayerAgent {
                member_id: member.member_id,
                world_id,
                user_id: owner.id,
                character_id: member.character_id,
                agent_id: member.agent_id,
            },
        })
    }

    /// The single access lookup used before a member may read or affect a world.
    pub async fn active_player_character(
        &self,
        user_id: i64,
        world_id: i64,
        character_id: i64,
    ) -> Result<Option<PlayerAgent>> {
        sqlx::query_as(
            "SELECT member.id AS member_id, member.world_id, member.user_id, player.character_id, player.agent_id \
             FROM world_member AS member \
             JOIN player_character AS player ON player.member_id = member.id \
             WHERE member.user_id = ? AND member.world_id = ? AND player.character_id = ? AND member.access = 'active'",
        )
        .bind(user_id)
        .bind(world_id)
        .bind(character_id)
        .fetch_optional(&self.pool)
        .await
        .with_context(|| {
            format!("resolving active character {character_id} for user {user_id} in world {world_id}")
        })
    }

    pub async fn has_active_membership(&self, user_id: i64, world_id: i64) -> Result<bool> {
        Ok(sqlx::query_scalar::<_, i64>(
            "SELECT 1 FROM world_member WHERE user_id = ? AND world_id = ? AND access = 'active'",
        )
        .bind(user_id)
        .bind(world_id)
        .fetch_optional(&self.pool)
        .await
        .context("checking active membership")?
        .is_some())
    }

    /// Create a new blank Adventurer for an active membership. Each character
    /// receives its own player agent and is placed at the durable world start.
    pub async fn create_player_character(
        &self,
        user_id: i64,
        world_id: i64,
    ) -> Result<PlayerAgent> {
        let mut transaction = self
            .pool
            .begin()
            .await
            .context("starting player-character creation")?;
        let member_id: i64 = sqlx::query_scalar(
            "SELECT id FROM world_member WHERE user_id = ? AND world_id = ? AND access = 'active'",
        )
        .bind(user_id)
        .bind(world_id)
        .fetch_optional(&mut *transaction)
        .await
        .context("resolving active membership for new character")?
        .with_context(|| format!("user {user_id} has no active access to world {world_id}"))?;
        let starting_group_id: i64 = sqlx::query_scalar(
            "SELECT character_group.group_id FROM player_character \
             JOIN character_group ON character_group.character_id = player_character.character_id \
             JOIN world_member ON world_member.id = player_character.member_id \
             JOIN world_owner ON world_owner.world_id = world_member.world_id AND world_owner.user_id = world_member.user_id \
             WHERE world_member.world_id = ? ORDER BY player_character.member_id LIMIT 1",
        )
        .bind(world_id)
        .fetch_optional(&mut *transaction)
        .await
        .context("loading world starting group")?
        .with_context(|| format!("world {world_id} has no starting group"))?;
        let (character_id, agent_id, _) =
            Self::create_adventurer(&mut transaction, member_id, world_id, starting_group_id)
                .await?;
        transaction
            .commit()
            .await
            .context("committing player-character creation")?;
        Ok(PlayerAgent {
            member_id,
            world_id,
            user_id,
            character_id,
            agent_id,
        })
    }

    /// Load exactly the player-visible state for the member's current location.
    /// It rechecks the membership relation so a stale browser connection cannot
    /// retain a scene after its access has been removed.
    pub async fn player_scene(&self, member: &PlayerAgent) -> Result<PlayerScene> {
        let (character_id, character_name, character_description, sheet, location_id, location_name, location_description):
            (i64, String, String, String, i64, String, String) = sqlx::query_as(
            "SELECT character.id, character.name, character.description, character.sheet, location.id, location.name, location.description \
             FROM world_member AS member \
             JOIN player_character ON player_character.member_id = member.id \
             JOIN character ON character.id = player_character.character_id \
             JOIN character_group ON character_group.character_id = character.id \
             JOIN \"group\" ON \"group\".id = character_group.group_id \
             JOIN location ON location.id = \"group\".location_id \
             WHERE member.id = ? AND member.world_id = ? AND member.user_id = ? AND player_character.character_id = ? AND member.access = 'active'",
        )
        .bind(member.member_id)
        .bind(member.world_id)
        .bind(member.user_id)
        .bind(member.character_id)
        .fetch_optional(&self.pool)
        .await
        .context("loading player scene")?
        .with_context(|| format!("member {} has no active placed character", member.member_id))?;
        let items = Self::location_items(&self.pool, location_id).await?;
        let npcs = Self::location_npcs(&self.pool, location_id).await?;
        Ok(PlayerScene {
            character_name,
            character_description,
            sheet: serde_json::from_str(&sheet).context("decoding player character sheet")?,
            location_name,
            location_description,
            items,
            inventory: Self::character_items(&self.pool, character_id).await?,
            npcs,
        })
    }

    /// Read the active player's location for narration-isolation assertions.
    #[cfg(test)]
    pub async fn player_location_id(&self, member: &PlayerAgent) -> Result<i64> {
        sqlx::query_scalar(
            "SELECT \"group\".location_id FROM world_member AS member \
             JOIN player_character ON player_character.member_id = member.id \
             JOIN character_group ON character_group.character_id = player_character.character_id \
             JOIN \"group\" ON \"group\".id = character_group.group_id \
             WHERE member.id = ? AND member.world_id = ? AND member.user_id = ? AND player_character.character_id = ? AND member.access = 'active'",
        )
        .bind(member.member_id)
        .bind(member.world_id)
        .bind(member.user_id)
        .bind(member.character_id)
        .fetch_optional(&self.pool)
        .await
        .context("loading member location")?
        .with_context(|| format!("member {} has no active location", member.member_id))
    }

    /// Record a character's zero-to-one connected transition, bumping its
    /// presence generation so a later, now-stale timer event can recognize
    /// itself as superseded (design.md's Membership and presence). Returns
    /// the new generation.
    pub async fn mark_character_present(&self, character_id: i64) -> Result<i64> {
        sqlx::query_scalar(
            "INSERT INTO character_presence (character_id, last_connected_at, absent_since, generation) \
             VALUES (?, CURRENT_TIMESTAMP, NULL, 1) \
             ON CONFLICT (character_id) DO UPDATE SET \
                 last_connected_at = CURRENT_TIMESTAMP, absent_since = NULL, generation = generation + 1 \
             RETURNING generation",
        )
        .bind(character_id)
        .fetch_one(&self.pool)
        .await
        .with_context(|| format!("recording presence for character {character_id}"))
    }

    /// Record a character's one-to-zero connected transition. Returns the new
    /// generation.
    pub async fn mark_character_absent(&self, character_id: i64) -> Result<i64> {
        sqlx::query_scalar(
            "UPDATE character_presence SET absent_since = CURRENT_TIMESTAMP, generation = generation + 1 \
             WHERE character_id = ? RETURNING generation",
        )
        .bind(character_id)
        .fetch_optional(&self.pool)
        .await
        .with_context(|| format!("recording absence for character {character_id}"))?
        .with_context(|| format!("character {character_id} was never marked present"))
    }

    /// The group a character currently belongs to. Every active character
    /// (player or NPC) has exactly one.
    pub async fn character_group_id(&self, character_id: i64) -> Result<i64> {
        sqlx::query_scalar("SELECT group_id FROM character_group WHERE character_id = ?")
            .bind(character_id)
            .fetch_optional(&self.pool)
            .await
            .with_context(|| format!("loading group for character {character_id}"))?
            .with_context(|| format!("character {character_id} has no group"))
    }

    /// The world a group belongs to.
    pub async fn world_id_for_group(&self, group_id: i64) -> Result<i64> {
        sqlx::query_scalar("SELECT world_id FROM \"group\" WHERE id = ?")
            .bind(group_id)
            .fetch_optional(&self.pool)
            .await
            .with_context(|| format!("looking up world for group {group_id}"))?
            .with_context(|| format!("group {group_id} does not exist"))
    }

    /// This group's own branch, if it currently exists and is not retired -
    /// `None` otherwise, for a caller that only wants to act on a group
    /// still eligible to participate in a topology change (the real
    /// existence/retirement rejection is `apply_merge`/`apply_split`'s own
    /// job inside their transaction; this is a read-only pre-check for work
    /// that must happen before that transaction, e.g. merge-time compaction,
    /// so it fails soft rather than erroring on a proposal that will end up
    /// rejected anyway).
    pub async fn active_group_branch(&self, group_id: i64) -> Result<Option<i64>> {
        sqlx::query_scalar("SELECT branch_id FROM \"group\" WHERE id = ? AND retired_at IS NULL")
            .bind(group_id)
            .fetch_optional(&self.pool)
            .await
            .with_context(|| format!("checking group {group_id} is active"))
    }

    /// A group's world_gm identity and its own branch (design.md's
    /// `GroupHistory { agent_id, branch_id }` - agent_id alone cannot select
    /// a GM history once a world_gm plays several groups).
    pub async fn group_gm_history(&self, group_id: i64) -> Result<GroupHistory> {
        let (agent_id, branch_id): (i64, i64) = sqlx::query_as(
            "SELECT world_gm.agent_id, \"group\".branch_id FROM \"group\" \
             JOIN world_gm ON world_gm.world_id = \"group\".world_id \
             WHERE \"group\".id = ?",
        )
        .bind(group_id)
        .fetch_optional(&self.pool)
        .await
        .with_context(|| format!("looking up GM for group {group_id}"))?
        .with_context(|| format!("group {group_id} does not exist"))?;
        Ok(GroupHistory {
            agent_id,
            branch_id,
        })
    }

    /// Every branch this agent owns, with its ordered bounded parents and
    /// (when a non-retired group currently uses it as its writable head)
    /// that group's identity - the whole DAG a world_gm agent's history can
    /// form once groups split and merge (user_declarations.md: "the GM's
    /// chat history view needs to authentically display the DAG, including
    /// multiple heads - one per Group"). A plain player/NPC/Storyteller
    /// agent still returns its one branch here, with no parents and no head
    /// (its head is `agent_history`, not a group) - the caller distinguishes
    /// "one branch, no parents" from a real DAG to decide how to render it.
    /// Unpaged: branch count only grows with split/merge events, never with
    /// ordinary chat volume.
    pub async fn gm_branch_graph(
        &self,
        viewer_id: i64,
        world_id: i64,
        agent_id: i64,
    ) -> Result<Vec<BranchNode>> {
        self.inspecting(viewer_id, world_id).await?;
        ensure!(
            self.world_id_for_agent(agent_id).await? == world_id,
            "agent {agent_id} is not in world {world_id}"
        );
        let branch_ids: Vec<i64> =
            sqlx::query_scalar("SELECT id FROM history_branch WHERE agent_id = ? ORDER BY id")
                .bind(agent_id)
                .fetch_all(&self.pool)
                .await
                .with_context(|| format!("loading branches for agent {agent_id}"))?;
        let mut nodes = Vec::with_capacity(branch_ids.len());
        for branch_id in branch_ids {
            let parents = sqlx::query_as::<_, (i64, i64)>(
                "SELECT parent_branch_id, through_position FROM history_parent \
                 WHERE branch_id = ? ORDER BY ordinal",
            )
            .bind(branch_id)
            .fetch_all(&self.pool)
            .await
            .with_context(|| format!("loading parents of branch {branch_id}"))?
            .into_iter()
            .map(|(branch_id, through_position)| BranchParent {
                branch_id,
                through_position,
            })
            .collect();
            let head: Option<(i64, String)> = sqlx::query_as(
                "SELECT id, name FROM \"group\" WHERE branch_id = ? AND retired_at IS NULL",
            )
            .bind(branch_id)
            .fetch_optional(&self.pool)
            .await
            .with_context(|| format!("checking current group head for branch {branch_id}"))?;
            nodes.push(BranchNode {
                branch_id,
                parents,
                head: head.map(|(group_id, group_name)| BranchGroup {
                    group_id,
                    group_name,
                }),
            });
        }
        Ok(nodes)
    }

    /// Validate and apply every grouping proposal collected from one tool
    /// batch, in call order, in a single transaction (design.md's "Tool
    /// boundary"): each split/merge is checked against state as it stands at
    /// this point (which may already reflect an earlier proposal in the same
    /// batch), a stale or invalid one is rejected rather than applied, and
    /// every proposal's tool-result message is appended in call order
    /// alongside the ordinary calls this same batch already committed.
    pub async fn apply_topology_changes(
        &self,
        inference_id: i64,
        changes: Vec<(String, i64, tools::TopologyChange)>,
    ) -> Result<()> {
        let mut transaction = self
            .pool
            .begin()
            .await
            .context("starting topology change transaction")?;
        // The branch every proposal's own tool-result message is appended
        // to - the same branch every ordinary call in this batch already
        // used (complete_tool_result's own comment: the inference's own
        // recorded branch, not a fresh agent_history lookup).
        let branch_id: i64 = sqlx::query_scalar(
            "SELECT history_entry.branch_id FROM inference \
             JOIN history_entry ON history_entry.id = inference.history_entry_id \
             WHERE inference.id = ?",
        )
        .bind(inference_id)
        .fetch_optional(&mut *transaction)
        .await
        .context("loading topology change batch's own branch")?
        .with_context(|| format!("inference {inference_id} does not exist"))?;
        for (tool_call_id, tool_execution_id, change) in changes {
            let result = match change {
                tools::TopologyChange::Split {
                    group_id,
                    partitions,
                    continue_with_character_id,
                } => {
                    Self::apply_split(
                        &mut transaction,
                        group_id,
                        partitions,
                        continue_with_character_id,
                    )
                    .await?
                }
                tools::TopologyChange::Merge {
                    group_ids,
                    name,
                    description,
                } => Self::apply_merge(&mut transaction, group_ids, name, description).await?,
            };
            let status = if result.is_ok() {
                "completed"
            } else {
                "rejected"
            };
            let content = result.unwrap_or_else(|rejection| rejection);
            let (_message_id, content_id) = Self::insert_message(
                &mut transaction,
                branch_id,
                &Message::tool_result(tool_call_id.clone(), content),
                Some(inference_id),
                Some(tool_execution_id),
                None,
                MessageOrigin::Conversation,
            )
            .await?;
            let updated = sqlx::query(
                "UPDATE tool_execution SET status = ?, result_content_id = ?, finished_at = CURRENT_TIMESTAMP \
                 WHERE id = ? AND finished_at IS NULL",
            )
            .bind(status)
            .bind(content_id)
            .bind(tool_execution_id)
            .execute(&mut *transaction)
            .await
            .with_context(|| format!("completing topology change tool_execution {tool_execution_id}"))?;
            ensure!(
                updated.rows_affected() == 1,
                "tool_execution {tool_execution_id} is missing or already finished"
            );
        }
        transaction
            .commit()
            .await
            .context("committing topology changes")?;
        Ok(())
    }

    /// Create one resulting group from a bounded parent, sharing the given
    /// world/location and cut through the parent branch's currently settled
    /// position. Returns the new group's id and branch id.
    async fn spawn_group_branch(
        transaction: &mut Transaction<'_, Sqlite>,
        world_id: i64,
        world_gm_agent_id: i64,
        location_id: i64,
        name: &str,
        description: &str,
        parents: &[(i64, i64)],
    ) -> Result<(i64, i64)> {
        let branch_id = sqlx::query_scalar::<_, i64>(
            "INSERT INTO history_branch (agent_id) VALUES (?) RETURNING id",
        )
        .bind(world_gm_agent_id)
        .fetch_one(&mut **transaction)
        .await
        .context("creating history branch for new group")?;
        for (ordinal, (parent_branch_id, through_position)) in parents.iter().enumerate() {
            sqlx::query(
                "INSERT INTO history_parent (branch_id, agent_id, ordinal, parent_branch_id, through_position) \
                 VALUES (?, ?, ?, ?, ?)",
            )
            .bind(branch_id)
            .bind(world_gm_agent_id)
            .bind(i64::try_from(ordinal).expect("parent ordinal fits in i64"))
            .bind(parent_branch_id)
            .bind(through_position)
            .execute(&mut **transaction)
            .await
            .context("linking new group's branch to its parent")?;
        }
        let group_id = sqlx::query_scalar::<_, i64>(
            "INSERT INTO \"group\" (world_id, name, location_id, description, branch_id) \
             VALUES (?, ?, ?, ?, ?) RETURNING id",
        )
        .bind(world_id)
        .bind(name)
        .bind(location_id)
        .bind(description)
        .bind(branch_id)
        .fetch_one(&mut **transaction)
        .await
        .context("creating new group")?;
        Ok((group_id, branch_id))
    }

    /// `split_group`: partition an active group's exact current membership
    /// into the proposed groups, each a new branch parented on the source's
    /// settled position, then retire the source (design.md's "Split").
    /// Returns the tool result text - `Ok` for the model-visible success
    /// text, `Err` for an ordinary rejection (never an infrastructure error,
    /// which still propagates via `?`).
    async fn apply_split(
        transaction: &mut Transaction<'_, Sqlite>,
        group_id: i64,
        partitions: Vec<tools::GroupPartition>,
        continue_with_character_id: Option<i64>,
    ) -> Result<Result<String, String>> {
        let Some((world_id, location_id, branch_id, retired_at)): Option<(
            i64,
            i64,
            i64,
            Option<String>,
        )> = sqlx::query_as(
            "SELECT world_id, location_id, branch_id, retired_at FROM \"group\" WHERE id = ?",
        )
        .bind(group_id)
        .fetch_optional(&mut **transaction)
        .await
        .with_context(|| format!("loading group {group_id} for split"))?
        else {
            return Ok(Err(format!(
                "Tool arguments were rejected: group {group_id} does not exist"
            )));
        };
        if retired_at.is_some() {
            return Ok(Err(format!(
                "Tool arguments were rejected: group {group_id} is already retired"
            )));
        }
        let current: Vec<i64> = sqlx::query_scalar(
            "SELECT character_id FROM character_group WHERE group_id = ? ORDER BY character_id",
        )
        .bind(group_id)
        .fetch_all(&mut **transaction)
        .await
        .with_context(|| format!("loading group {group_id}'s current membership"))?;
        let mut partitioned: Vec<i64> = partitions
            .iter()
            .flat_map(|partition| partition.character_ids.iter().copied())
            .collect();
        partitioned.sort_unstable();
        let mut current_sorted = current.clone();
        current_sorted.sort_unstable();
        let is_disjoint = partitioned.len()
            == partitions
                .iter()
                .map(|partition| partition.character_ids.len())
                .sum::<usize>();
        if partitions.is_empty()
            || partitions
                .iter()
                .any(|partition| partition.character_ids.is_empty())
            || !is_disjoint
            || partitioned != current_sorted
        {
            return Ok(Err(format!(
                "Tool arguments were rejected: partitions must be a complete, nonempty, disjoint split of group {group_id}'s current membership"
            )));
        }
        if let Some(continue_with) = continue_with_character_id
            && !current.contains(&continue_with)
        {
            return Ok(Err(format!(
                "Tool arguments were rejected: continue_with_character_id {continue_with} is not a member of group {group_id}"
            )));
        }
        let through_position: i64 =
            sqlx::query_scalar("SELECT next_position - 1 FROM history_branch WHERE id = ?")
                .bind(branch_id)
                .fetch_one(&mut **transaction)
                .await
                .context("loading source branch's settled position")?;
        let world_gm_agent_id = Self::world_gm_agent_id_tx(transaction, world_id).await?;
        let mut result_group_ids = Vec::new();
        for partition in &partitions {
            let (new_group_id, _new_branch_id) = Self::spawn_group_branch(
                transaction,
                world_id,
                world_gm_agent_id,
                location_id,
                &partition.name,
                &partition.description,
                &[(branch_id, through_position)],
            )
            .await?;
            for character_id in &partition.character_ids {
                sqlx::query("UPDATE character_group SET group_id = ?, description = ? WHERE character_id = ?")
                    .bind(new_group_id)
                    .bind(&partition.description)
                    .bind(character_id)
                    .execute(&mut **transaction)
                    .await
                    .with_context(|| format!("moving character {character_id} into split group {new_group_id}"))?;
            }
            result_group_ids.push(new_group_id);
        }
        sqlx::query("UPDATE \"group\" SET retired_at = CURRENT_TIMESTAMP WHERE id = ?")
            .bind(group_id)
            .execute(&mut **transaction)
            .await
            .with_context(|| format!("retiring split group {group_id}"))?;
        Ok(Ok(format!(
            "Group {group_id} split into groups {}.",
            result_group_ids
                .iter()
                .map(i64::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        )))
    }

    /// `merge_groups`: union distinct, active, co-located groups in one world
    /// into a new group whose branch bounds all of their settled positions,
    /// sorted ascending for parent order, then retire the sources (design.md's
    /// "Merge").
    async fn apply_merge(
        transaction: &mut Transaction<'_, Sqlite>,
        mut group_ids: Vec<i64>,
        name: String,
        description: String,
    ) -> Result<Result<String, String>> {
        group_ids.sort_unstable();
        group_ids.dedup();
        if group_ids.len() < 2 {
            return Ok(Err(
                "Tool arguments were rejected: merge_groups requires at least two distinct groups"
                    .to_string(),
            ));
        }
        let mut world_id = None;
        let mut location_id = None;
        let mut parents = Vec::new();
        for group_id in &group_ids {
            let Some((group_world_id, group_location_id, branch_id, retired_at)): Option<(
                i64,
                i64,
                i64,
                Option<String>,
            )> = sqlx::query_as(
                "SELECT world_id, location_id, branch_id, retired_at FROM \"group\" WHERE id = ?",
            )
            .bind(group_id)
            .fetch_optional(&mut **transaction)
            .await
            .with_context(|| format!("loading group {group_id} for merge"))?
            else {
                return Ok(Err(format!(
                    "Tool arguments were rejected: group {group_id} does not exist"
                )));
            };
            if retired_at.is_some() {
                return Ok(Err(format!(
                    "Tool arguments were rejected: group {group_id} is already retired"
                )));
            }
            if *world_id.get_or_insert(group_world_id) != group_world_id {
                return Ok(Err(
                    "Tool arguments were rejected: merge_groups requires groups in the same world"
                        .to_string(),
                ));
            }
            if *location_id.get_or_insert(group_location_id) != group_location_id {
                return Ok(Err(
                    "Tool arguments were rejected: merge_groups requires groups at one location"
                        .to_string(),
                ));
            }
            let through_position: i64 =
                sqlx::query_scalar("SELECT next_position - 1 FROM history_branch WHERE id = ?")
                    .bind(branch_id)
                    .fetch_one(&mut **transaction)
                    .await
                    .context("loading source branch's settled position")?;
            parents.push((branch_id, through_position));
        }
        let world_id = world_id.expect("at least two group ids were validated above");
        let location_id = location_id.expect("at least two group ids were validated above");
        let world_gm_agent_id = Self::world_gm_agent_id_tx(transaction, world_id).await?;
        let (new_group_id, _new_branch_id) = Self::spawn_group_branch(
            transaction,
            world_id,
            world_gm_agent_id,
            location_id,
            &name,
            &description,
            &parents,
        )
        .await?;
        for group_id in &group_ids {
            sqlx::query("UPDATE character_group SET group_id = ? WHERE group_id = ?")
                .bind(new_group_id)
                .bind(group_id)
                .execute(&mut **transaction)
                .await
                .with_context(|| {
                    format!("moving group {group_id}'s characters into merged group {new_group_id}")
                })?;
            sqlx::query("UPDATE \"group\" SET retired_at = CURRENT_TIMESTAMP WHERE id = ?")
                .bind(group_id)
                .execute(&mut **transaction)
                .await
                .with_context(|| format!("retiring merged group {group_id}"))?;
        }
        Ok(Ok(format!(
            "Groups {} merged into group {new_group_id}.",
            group_ids
                .iter()
                .map(i64::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        )))
    }

    async fn world_gm_agent_id_tx(
        transaction: &mut Transaction<'_, Sqlite>,
        world_id: i64,
    ) -> Result<i64> {
        sqlx::query_scalar("SELECT agent_id FROM world_gm WHERE world_id = ?")
            .bind(world_id)
            .fetch_optional(&mut **transaction)
            .await
            .with_context(|| format!("looking up world GM for world {world_id}"))?
            .with_context(|| format!("world {world_id} has no world GM"))
    }

    /// Display names for a set of characters, in the order given by the
    /// database (not necessarily `character_ids`' own order) - used only to
    /// compose an arrival/departure narration prompt, never to identify a
    /// specific character positionally.
    pub async fn character_names(&self, character_ids: &[i64]) -> Result<Vec<String>> {
        if character_ids.is_empty() {
            return Ok(Vec::new());
        }
        let ids = serde_json::to_string(character_ids).context("serializing character ids")?;
        sqlx::query_scalar(
            "SELECT character.name FROM character \
             JOIN json_each(?) AS requested ON requested.value = character.id \
             ORDER BY character.id",
        )
        .bind(ids)
        .fetch_all(&self.pool)
        .await
        .context("loading character names")
    }

    /// The caller's group's characters, their names and online state
    /// (design.md's `party_presence()`: "resolves the caller's group and
    /// returns character IDs, names and online state from this data"). Does
    /// not mutate group membership.
    pub async fn party_presence(&self, character_id: i64) -> Result<Vec<PartyMember>> {
        let group_id = self.character_group_id(character_id).await?;
        let rows: Vec<(i64, String, bool)> = sqlx::query_as(
            "SELECT character.id, character.name, \
                    character_presence.character_id IS NOT NULL AND character_presence.absent_since IS NULL AS online \
             FROM character_group \
             JOIN character ON character.id = character_group.character_id \
             LEFT JOIN character_presence ON character_presence.character_id = character_group.character_id \
             WHERE character_group.group_id = ? ORDER BY character.id",
        )
        .bind(group_id)
        .fetch_all(&self.pool)
        .await
        .with_context(|| format!("loading party presence for group {group_id}"))?;
        Ok(rows
            .into_iter()
            .map(|(character_id, name, online)| PartyMember {
                character_id,
                name,
                online,
            })
            .collect())
    }

    /// Every character in a group whose `character_presence` is currently
    /// connected (a non-null `last_connected_at` with no later
    /// `absent_since`), for computing the announced-set difference.
    pub async fn group_online_character_ids(&self, group_id: i64) -> Result<Vec<i64>> {
        sqlx::query_scalar(
            "SELECT character_group.character_id FROM character_group \
             JOIN character_presence ON character_presence.character_id = character_group.character_id \
             WHERE character_group.group_id = ? \
               AND character_presence.absent_since IS NULL \
             ORDER BY character_group.character_id",
        )
        .bind(group_id)
        .fetch_all(&self.pool)
        .await
        .with_context(|| format!("loading online characters for group {group_id}"))
    }

    /// The current announced set and any outstanding transition deadline for
    /// a group, creating the row (empty announced set, no deadline) if this
    /// is its first presence event.
    pub async fn presence_narration(&self, group_id: i64) -> Result<PresenceNarration> {
        sqlx::query_as::<_, PresenceNarrationRow>(
            "INSERT INTO presence_narration (group_id) VALUES (?) \
             ON CONFLICT (group_id) DO UPDATE SET group_id = group_id \
             RETURNING group_id, last_announced_character_ids, due_at, generation",
        )
        .bind(group_id)
        .fetch_one(&self.pool)
        .await
        .with_context(|| format!("loading presence narration state for group {group_id}"))?
        .try_into()
    }

    /// Persist a group's newly announced set, clearing any pending deadline
    /// (an immediate arrival, or a deadline firing with someone still
    /// online). Fails the update if `generation` no longer matches - a stale
    /// caller must reload and recheck rather than overwrite a newer change.
    pub async fn announce_presence(
        &self,
        group_id: i64,
        character_ids: &[i64],
        generation: i64,
    ) -> Result<()> {
        let ids = serde_json::to_string(character_ids).context("serializing announced set")?;
        let updated = sqlx::query(
            "UPDATE presence_narration SET last_announced_character_ids = ?, due_at = NULL, generation = generation + 1 \
             WHERE group_id = ? AND generation = ?",
        )
        .bind(ids)
        .bind(group_id)
        .bind(generation)
        .execute(&self.pool)
        .await
        .with_context(|| format!("announcing presence for group {group_id}"))?
        .rows_affected();
        ensure!(
            updated == 1,
            "presence narration for group {group_id} changed before announcing"
        );
        Ok(())
    }

    /// Schedule (or reschedule) a group's pending deadline `after_seconds`
    /// from now, bumping its generation so an earlier-scheduled timer
    /// recognizes itself as stale. Fails if `generation` no longer matches -
    /// a stale caller must reload. Returns the new generation; the caller's
    /// own `tokio::time::sleep(Duration::from_secs(after_seconds))` is the
    /// actual scheduling mechanism, so `due_at` here is a durable record for
    /// recovery/display, not something re-parsed back for timing.
    pub async fn schedule_presence_deadline(
        &self,
        group_id: i64,
        after_seconds: i64,
        generation: i64,
    ) -> Result<i64> {
        sqlx::query_scalar(
            "UPDATE presence_narration \
             SET due_at = datetime(CURRENT_TIMESTAMP, ? || ' seconds'), generation = generation + 1 \
             WHERE group_id = ? AND generation = ? RETURNING generation",
        )
        .bind(after_seconds)
        .bind(group_id)
        .bind(generation)
        .fetch_optional(&self.pool)
        .await
        .with_context(|| format!("scheduling presence deadline for group {group_id}"))?
        .with_context(|| format!("presence narration for group {group_id} changed before scheduling"))
    }

    /// The display name of one character, for attributing an action to the GM.
    pub async fn character_name(&self, character_id: i64) -> Result<String> {
        sqlx::query_scalar("SELECT name FROM character WHERE id = ?")
            .bind(character_id)
            .fetch_optional(&self.pool)
            .await
            .context("loading character name")?
            .with_context(|| format!("character {character_id} does not exist"))
    }

    /// Resolve the world GM from the authenticated player character's current
    /// group. The player agent never chooses which GM receives the opening
    /// request. `agent_id` alone cannot select a GM history once a world_gm
    /// plays several groups (design.md's `GroupHistory { agent_id, branch_id
    /// }`), so this also returns the group's own branch.
    pub async fn player_group_history(&self, member: &PlayerAgent) -> Result<GroupHistory> {
        let (agent_id, branch_id): (i64, i64) = sqlx::query_as(
            "SELECT world_gm.agent_id, \"group\".branch_id FROM world_member AS member \
             JOIN player_character ON player_character.member_id = member.id \
             JOIN character_group ON character_group.character_id = player_character.character_id \
             JOIN \"group\" ON \"group\".id = character_group.group_id \
             JOIN world_gm ON world_gm.world_id = \"group\".world_id \
             WHERE member.id = ? AND member.world_id = ? AND member.user_id = ? AND player_character.character_id = ? AND member.access = 'active'",
        )
        .bind(member.member_id)
        .bind(member.world_id)
        .bind(member.user_id)
        .bind(member.character_id)
        .fetch_optional(&self.pool)
        .await
        .context("loading member group GM")?
        .with_context(|| format!("member {} has no active group GM", member.member_id))?;
        Ok(GroupHistory {
            agent_id,
            branch_id,
        })
    }

    /// Load the text-only portion of one active membership's player-agent
    /// history in stored message order. This is used by the game page on
    /// every reload; it does not rely on a websocket's in-memory lifetime.
    pub async fn player_chat(&self, member: &PlayerAgent) -> Result<Vec<PlayerChatEntry>> {
        self.player_chat_after(member, None).await
    }

    /// Whether this player agent has completed its opening turn. This is
    /// intentionally based on durable history rather than player chat: a
    /// normal empty assistant completion has no player-chat entry.
    pub async fn player_agent_has_assistant_reply(&self, agent_id: i64) -> Result<bool> {
        let reply: Option<i64> = sqlx::query_scalar(
            "SELECT 1 FROM agent_history \
             JOIN history_entry ON history_entry.branch_id = agent_history.branch_id \
             JOIN message ON message.id = history_entry.id \
             WHERE agent_history.agent_id = ? AND message.role = ? LIMIT 1",
        )
        .bind(agent_id)
        .bind(serde_json::to_string(&Role::Assistant).context("encoding assistant role")?)
        .fetch_optional(&self.pool)
        .await
        .context("checking player agent assistant history")?;
        Ok(reply.is_some())
    }

    /// Load player-visible chat entries newer than a stored message cursor.
    /// A browser renders a snapshot, then asks the websocket for this delta so
    /// work completed between rendering and connection is neither lost nor
    /// duplicated.
    pub async fn player_chat_after(
        &self,
        member: &PlayerAgent,
        after_message_id: Option<i64>,
    ) -> Result<Vec<PlayerChatEntry>> {
        let active: Option<i64> = sqlx::query_scalar(
            "SELECT 1 FROM world_member AS member \
             JOIN player_character ON player_character.member_id = member.id \
             WHERE member.id = ? AND member.world_id = ? AND member.user_id = ? \
             AND player_character.character_id = ? AND player_character.agent_id = ? AND member.access = 'active'",
        )
        .bind(member.member_id)
        .bind(member.world_id)
        .bind(member.user_id)
        .bind(member.character_id)
        .bind(member.agent_id)
        .fetch_optional(&self.pool)
        .await
        .context("checking active player chat membership")?;
        ensure!(
            active.is_some(),
            "member {} has no active player chat in world {}",
            member.member_id,
            member.world_id
        );
        let rows = sqlx::query_as::<_, PlayerChatRow>(
            "SELECT message.id, message.role, message.content, message.trigger_sequence_id, inference.sequence_id AS inference_sequence_id \
             FROM world_member AS member \
             JOIN player_character ON player_character.member_id = member.id \
             JOIN agent_history ON agent_history.agent_id = player_character.agent_id \
             JOIN history_entry ON history_entry.branch_id = agent_history.branch_id \
             JOIN message ON message.id = history_entry.id \
             LEFT JOIN inference ON inference.id = message.source_inference_id \
             WHERE member.id = ? AND member.world_id = ? AND member.user_id = ? \
             AND player_character.character_id = ? AND player_character.agent_id = ? AND member.access = 'active' AND message.id > ? \
             AND message.origin = 'conversation' \
             ORDER BY history_entry.position",
        )
        .bind(member.member_id)
        .bind(member.world_id)
        .bind(member.user_id)
        .bind(member.character_id)
        .bind(member.agent_id)
        .bind(after_message_id.unwrap_or_default())
        .fetch_all(&self.pool)
        .await
        .context("loading player chat history")?;
        let mut entries = Vec::new();
        for row in rows {
            let role = serde_json::from_str(&row.role).context("decoding player chat role")?;
            let (MessageContent::Text(text), _) = self.recorded_content(row.content).await? else {
                continue;
            };
            if matches!(role, Role::User | Role::Assistant | Role::Narration)
                && !(role == Role::Assistant && text.is_empty())
            {
                entries.push(PlayerChatEntry {
                    id: row.id,
                    role,
                    text,
                    sequence_id: row.trigger_sequence_id.or(row.inference_sequence_id),
                });
            }
        }
        Ok(entries)
    }

    /// Persist a resolved group narration in every connected player-agent
    /// history currently in that group, all referencing the one
    /// `recorded_content` row the narrating GM's own reply already wrote -
    /// not a fresh copy of the text per recipient. Call this only after the
    /// triggering player agent has settled, so its tool-call/result sequence
    /// stays intact.
    pub async fn append_group_narration(
        &self,
        world_id: i64,
        group_id: i64,
        content_id: i64,
    ) -> Result<()> {
        // Recipients are connected PCs in the interacting group (design.md's
        // Membership and presence: "Narration recipients are NPCs and
        // connected PCs in the interacting group; offline PCs do not receive
        // it"). NPCs have no player-agent history to append to here; their
        // own narration delivery is not yet implemented.
        let agents: Vec<i64> = sqlx::query_scalar(
            "SELECT player_character.agent_id FROM world_member \
             JOIN player_character ON player_character.member_id = world_member.id \
             JOIN character_group ON character_group.character_id = player_character.character_id \
             JOIN \"group\" ON \"group\".id = character_group.group_id \
             JOIN character_presence ON character_presence.character_id = player_character.character_id \
             WHERE world_member.world_id = ? AND world_member.access = 'active' \
             AND \"group\".id = ? AND character_presence.absent_since IS NULL \
             ORDER BY player_character.agent_id",
        )
        .bind(world_id)
        .bind(group_id)
        .fetch_all(&self.pool)
        .await
        .context("finding connected player agents in narrated group")?;
        let source_inference_id: Option<i64> =
            sqlx::query_scalar("SELECT id FROM inference WHERE output = ?")
                .bind(content_id)
                .fetch_optional(&self.pool)
                .await
                .context("resolving GM narration's producing inference")?;
        for agent_id in agents {
            let mut transaction = self.pool.begin().await.with_context(|| {
                format!("starting narration delivery transaction for agent {agent_id}")
            })?;
            let branch_id = Self::branch_for_agent_tx(&mut transaction, agent_id).await?;
            Self::insert_message_with_content(
                &mut transaction,
                branch_id,
                (Role::Narration, MessageOrigin::Conversation),
                content_id,
                source_inference_id,
                None,
                None,
            )
            .await
            .with_context(|| format!("storing group narration for player agent {agent_id}"))?;
            transaction.commit().await.with_context(|| {
                format!("committing location narration for player agent {agent_id}")
            })?;
        }
        Ok(())
    }

    /// Load the context a group's GM may use to judge one action. This is
    /// addressed through the group's own branch, never a model-provided
    /// location id - a world_gm plays several groups, so agent_id alone
    /// cannot select which one (design.md's `GroupHistory`).
    pub async fn gm_scene(&self, world_id: i64, branch_id: i64) -> Result<GmScene> {
        let (group_id, location_id, location_name, location_description, gm_notes, group_description, group_gm_notes): (
            i64,
            i64,
            String,
            String,
            String,
            String,
            String,
        ) = sqlx::query_as(
            "SELECT \"group\".id, location.id, location.name, location.description, location.gm_notes, \
                    \"group\".description, \"group\".gm_notes \
             FROM \"group\" JOIN location ON location.id = \"group\".location_id \
             WHERE \"group\".branch_id = ? AND \"group\".world_id = ?",
        )
        .bind(branch_id)
        .bind(world_id)
        .fetch_optional(&self.pool)
        .await
        .context("loading group GM scene")?
        .with_context(|| format!("branch {branch_id} is not a group's GM history in world {world_id}"))?;
        Ok(GmScene {
            location_name,
            location_description,
            gm_notes: serde_json::from_str(&gm_notes).context("decoding location GM notes")?,
            group_description,
            group_gm_notes: serde_json::from_str(&group_gm_notes)
                .context("decoding group GM notes")?,
            items: Self::location_items(&self.pool, location_id).await?,
            npcs: Self::group_gm_npcs(&self.pool, group_id).await?,
            players: Self::group_gm_players(&self.pool, group_id).await?,
        })
    }

    async fn location_items(pool: &SqlitePool, location_id: i64) -> Result<Vec<SceneItem>> {
        sqlx::query_as(
            "SELECT item.name, item.description FROM item_location \
             JOIN item ON item.id = item_location.item_id \
             WHERE item_location.location_id = ? ORDER BY item.id",
        )
        .bind(location_id)
        .fetch_all(pool)
        .await
        .context("loading location items")
    }

    async fn character_items(pool: &SqlitePool, character_id: i64) -> Result<Vec<SceneItem>> {
        sqlx::query_as(
            "SELECT item.name, item.description FROM item_character \
             JOIN item ON item.id = item_character.item_id \
             WHERE item_character.character_id = ? ORDER BY item.id",
        )
        .bind(character_id)
        .fetch_all(pool)
        .await
        .context("loading character inventory")
    }

    async fn location_npcs(pool: &SqlitePool, location_id: i64) -> Result<Vec<SceneNpc>> {
        sqlx::query_as(
            "SELECT character.name, character.description FROM character_group \
             JOIN \"group\" ON \"group\".id = character_group.group_id \
             JOIN character ON character.id = character_group.character_id \
             WHERE \"group\".location_id = ? AND character.role = 'npc' ORDER BY character.id",
        )
        .bind(location_id)
        .fetch_all(pool)
        .await
        .context("loading location NPCs")
    }

    async fn group_gm_npcs(pool: &SqlitePool, group_id: i64) -> Result<Vec<GmSceneNpc>> {
        let rows: Vec<GmSceneNpcRow> = sqlx::query_as(
            "SELECT character.name, character.description, character.background, character.motive, character.ambition, character.gm_notes \
             FROM character_group JOIN character ON character.id = character_group.character_id \
             WHERE character_group.group_id = ? AND character.role = 'npc' ORDER BY character.id",
        )
        .bind(group_id)
        .fetch_all(pool)
        .await
        .context("loading group GM NPCs")?;
        rows.into_iter()
            .map(|row| {
                Ok(GmSceneNpc {
                    name: row.name,
                    description: row.description,
                    background: row.background,
                    motive: row.motive,
                    ambition: row.ambition,
                    gm_notes: serde_json::from_str(&row.gm_notes)
                        .context("decoding NPC GM notes")?,
                })
            })
            .collect()
    }

    async fn group_gm_players(pool: &SqlitePool, group_id: i64) -> Result<Vec<GmScenePlayer>> {
        let rows: Vec<GmScenePlayerRow> = sqlx::query_as(
            "SELECT character.name, character.description, character.background, character.motive, character.ambition, character.sheet \
             FROM character_group \
             JOIN player_character ON player_character.character_id = character_group.character_id \
             JOIN character ON character.id = player_character.character_id \
             WHERE character_group.group_id = ? ORDER BY character.id",
        )
        .bind(group_id)
        .fetch_all(pool)
        .await
        .context("loading group player characters for GM")?;
        rows.into_iter()
            .map(|row| {
                let sheet = serde_json::from_str(&row.sheet)
                    .context("decoding player character sheet for GM")?;
                Ok(GmScenePlayer {
                    name: row.name,
                    description: row.description,
                    background: row.background,
                    motive: row.motive,
                    ambition: row.ambition,
                    sheet,
                })
            })
            .collect()
    }

    /// Roll the Adventurer's starting Hit Protection once. Repeating a tool
    /// call returns the stored result instead of offering a reroll.
    pub async fn roll_hit_protection(
        &self,
        member: &PlayerAgent,
        sequence_id: i64,
        call: Option<i64>,
    ) -> Result<i64> {
        let mut transaction = self.pool.begin().await.context("starting HP roll")?;
        let (character_id, sheet): (i64, String) = Self::player_sheet(&mut transaction, member)
            .await
            .context("loading Adventurer for HP roll")?;
        let mut sheet: serde_json::Value =
            serde_json::from_str(&sheet).context("decoding Adventurer sheet")?;
        if let Some(hp) = sheet.get("hp").and_then(serde_json::Value::as_i64) {
            transaction
                .commit()
                .await
                .context("committing existing HP roll")?;
            return Ok(hp);
        }
        let hp = rand::rng().random_range(1..=6);
        sheet["hp"] = serde_json::json!(hp);
        Self::store_sheet(&mut transaction, character_id, &sheet).await?;
        let result = Self::content_in(
            &mut transaction,
            &MessageContent::Text(format!(r#"{{"hp":{hp}}}"#)),
            "",
        )
        .await?;
        Self::record_action(&mut transaction, sequence_id, call, None, result).await?;
        transaction.commit().await.context("committing HP roll")?;
        Ok(hp)
    }

    /// Roll STR, DEX, and WIL in order, once, using Cairn's 3d6 rule.
    pub async fn roll_attributes(
        &self,
        member: &PlayerAgent,
        sequence_id: i64,
        call: Option<i64>,
    ) -> Result<(i64, i64, i64)> {
        let mut transaction = self
            .pool
            .begin()
            .await
            .context("starting attribute rolls")?;
        let (character_id, sheet): (i64, String) = Self::player_sheet(&mut transaction, member)
            .await
            .context("loading Adventurer for attribute rolls")?;
        let mut sheet: serde_json::Value =
            serde_json::from_str(&sheet).context("decoding Adventurer sheet")?;
        let values = ["str", "dex", "wil"]
            .map(|attribute| sheet.get(attribute).and_then(serde_json::Value::as_i64));
        if let [Some(strength), Some(dexterity), Some(will)] = values {
            transaction
                .commit()
                .await
                .context("committing existing attribute rolls")?;
            return Ok((strength, dexterity, will));
        }
        ensure!(
            values.iter().all(Option::is_none),
            "Adventurer has a partial attribute roll"
        );
        let (strength, dexterity, will) = {
            let mut rng = rand::rng();
            let mut roll = || (0..3).map(|_| rng.random_range(1..=6)).sum::<i64>();
            (roll(), roll(), roll())
        };
        sheet["str"] = serde_json::json!(strength);
        sheet["dex"] = serde_json::json!(dexterity);
        sheet["wil"] = serde_json::json!(will);
        Self::store_sheet(&mut transaction, character_id, &sheet).await?;
        let result = Self::content_in(
            &mut transaction,
            &MessageContent::Text(format!(
                r#"{{"str":{strength},"dex":{dexterity},"wil":{will}}}"#
            )),
            "",
        )
        .await?;
        Self::record_action(&mut transaction, sequence_id, call, None, result).await?;
        transaction
            .commit()
            .await
            .context("committing attribute rolls")?;
        Ok((strength, dexterity, will))
    }

    pub async fn ready_to_begin(
        &self,
        member: &PlayerAgent,
        sequence_id: i64,
        call: Option<i64>,
    ) -> Result<()> {
        let mut transaction = self.pool.begin().await.context("starting ready-to-begin")?;
        let (character_id, sheet): (i64, String) = Self::player_sheet(&mut transaction, member)
            .await
            .context("loading Adventurer to mark ready")?;
        let mut sheet: serde_json::Value =
            serde_json::from_str(&sheet).context("decoding Adventurer sheet")?;
        ensure!(
            sheet
                .get("hp")
                .and_then(serde_json::Value::as_i64)
                .is_some()
                && ["str", "dex", "wil"].iter().all(|attribute| sheet
                    .get(*attribute)
                    .and_then(serde_json::Value::as_i64)
                    .is_some()),
            "Adventurer must roll Hit Protection and all attributes before beginning"
        );
        sheet["ready"] = serde_json::json!(true);
        Self::store_sheet(&mut transaction, character_id, &sheet).await?;
        let result = Self::content_in(
            &mut transaction,
            &MessageContent::Text(r#"{"ready":true}"#.to_string()),
            "",
        )
        .await?;
        Self::record_action(&mut transaction, sequence_id, call, None, result).await?;
        transaction
            .commit()
            .await
            .context("committing ready-to-begin")
    }

    pub async fn is_ready_to_begin(&self, member: &PlayerAgent) -> Result<bool> {
        let mut transaction = self
            .pool
            .begin()
            .await
            .context("checking Adventurer readiness")?;
        let (_, sheet) = Self::player_sheet(&mut transaction, member).await?;
        transaction
            .commit()
            .await
            .context("committing Adventurer readiness check")?;
        Ok(serde_json::from_str::<serde_json::Value>(&sheet)
            .context("decoding Adventurer sheet")?
            .get("ready")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false))
    }

    /// Presence narration describes established characters, never an
    /// Adventurer still in character creation. The creation owner performs
    /// its own opening narration when it records readiness.
    pub async fn character_is_ready(&self, character_id: i64) -> Result<bool> {
        let sheet: Option<String> = sqlx::query_scalar("SELECT sheet FROM character WHERE id = ?")
            .bind(character_id)
            .fetch_optional(&self.pool)
            .await
            .with_context(|| format!("loading readiness for character {character_id}"))?;
        let sheet = sheet.with_context(|| format!("character {character_id} does not exist"))?;
        Ok(serde_json::from_str::<serde_json::Value>(&sheet)
            .context("decoding Adventurer sheet")?
            .get("ready")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false))
    }

    /// Retain a validated character action and assign the next world-local id
    /// before forwarding it to the location GM. The caller supplies only an
    /// already-resolved membership; this lookup determines its character and
    /// GM, so neither can be selected by model or browser input.
    pub async fn create_pending_action(
        &self,
        member: &PlayerAgent,
        sequence_id: i64,
        requesting_tool_execution_id: i64,
        tool: &str,
        args: &str,
    ) -> Result<PendingAction> {
        let mut transaction = self.pool.begin().await.with_context(|| {
            format!(
                "starting pending {tool} action for member {}",
                member.member_id
            )
        })?;
        let (character_id, gm_agent_id, gm_branch_id): (i64, i64, i64) = sqlx::query_as(
            "SELECT character.id, world_gm.agent_id, \"group\".branch_id \
             FROM world_member AS member \
             JOIN player_character ON player_character.member_id = member.id \
             JOIN character ON character.id = player_character.character_id \
             JOIN character_group ON character_group.character_id = character.id \
             JOIN \"group\" ON \"group\".id = character_group.group_id \
             JOIN world_gm ON world_gm.world_id = \"group\".world_id \
             WHERE member.id = ? AND member.world_id = ? AND member.user_id = ? \
               AND member.access = 'active' AND player_character.character_id = ? AND player_character.agent_id = ?",
        )
        .bind(member.member_id)
        .bind(member.world_id)
        .bind(member.user_id)
        .bind(member.character_id)
        .bind(member.agent_id)
        .fetch_optional(&mut *transaction)
        .await
        .context("resolving action character and group GM")?
        .context("active membership has no playable character and group GM")?;
        let sequence_matches: Option<i64> =
            sqlx::query_scalar("SELECT id FROM sequence WHERE id = ? AND world_id = ?")
                .bind(sequence_id)
                .bind(member.world_id)
                .fetch_optional(&mut *transaction)
                .await
                .context("checking action sequence world")?;
        ensure!(
            sequence_matches.is_some(),
            "sequence {sequence_id} is not in world {}",
            member.world_id
        );
        let action_id: i64 = sqlx::query_scalar(
            "UPDATE world SET next_action_id = next_action_id + 1 WHERE id = ? \
             RETURNING next_action_id",
        )
        .bind(member.world_id)
        .fetch_one(&mut *transaction)
        .await
        .with_context(|| format!("allocating action id in world {}", member.world_id))?;
        sqlx::query(
            "INSERT INTO pending_action \
             (world_id, id, sequence_id, requesting_tool_execution_id, character_id, gm_agent_id, gm_branch_id, tool, args) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(member.world_id)
        .bind(action_id)
        .bind(sequence_id)
        .bind(requesting_tool_execution_id)
        .bind(character_id)
        .bind(gm_agent_id)
        .bind(gm_branch_id)
        .bind(tool)
        .bind(args)
        .execute(&mut *transaction)
        .await
        .with_context(|| format!("storing pending {tool} action {action_id}"))?;
        transaction
            .commit()
            .await
            .with_context(|| format!("committing pending {tool} action {action_id}"))?;
        Ok(PendingAction {
            world_id: member.world_id,
            id: action_id,
            sequence_id,
            requesting_tool_execution_id,
            character_id,
            gm_agent_id,
            gm_branch_id,
            tool: tool.to_string(),
            args: args.to_string(),
        })
    }

    /// Resolve an action only from the GM relationship it was assigned to.
    /// Moving it into the immutable action log and retiring the pending row are
    /// one transaction, so a later call cannot execute its effect twice. This
    /// is the sole authority for action resolution (design.md: "They cannot be
    /// approved twice") - no caller-side flag mirrors this state. A repeated
    /// resolve attempt for an `action_id` that is no longer pending, because
    /// this exact GM already resolved it, returns the recorded outcome
    /// verbatim instead of failing or re-running `execute_approved_action`;
    /// the caller compares its requested decision against the returned one
    /// and reports a conflicting decision as an ordinary tool rejection, not
    /// an error - a later rejection reason can never replace the original
    /// result. An `action_id` that was never valid for this world still
    /// fails loudly.
    pub async fn resolve_pending_action(
        &self,
        gm_agent_id: i64,
        world_id: i64,
        action_id: i64,
        approving_tool_execution_id: i64,
        result: &str,
    ) -> Result<ResolvedAction> {
        let mut transaction =
            self.pool.begin().await.with_context(|| {
                format!("resolving action {action_id} through GM {gm_agent_id}")
            })?;
        let pending: Option<PendingAction> = sqlx::query_as::<_, PendingActionRow>(
            "SELECT world_id, id, sequence_id, requesting_tool_execution_id, character_id, gm_agent_id, gm_branch_id, tool, args \
             FROM pending_action WHERE world_id = ? AND id = ? AND gm_agent_id = ?",
        )
        .bind(world_id)
        .bind(action_id)
        .bind(gm_agent_id)
        .fetch_optional(&mut *transaction)
        .await
        .context("loading pending action for group GM")?
        .map(Into::into);
        let Some(action) = pending else {
            // Not pending: either this GM already resolved it (return the
            // recorded outcome verbatim, without re-executing anything) or
            // the action_id was never valid for this world (fail loudly).
            // Only the decision and result reference are needed here, so
            // this reads the settled `action` row directly rather than
            // re-deriving tool/args/character_id through the original call.
            let resolved: Option<(bool, i64)> = sqlx::query_as(
                "SELECT action.approved, action.result_content_id \
                 FROM action \
                 JOIN sequence ON sequence.id = action.sequence_id \
                 JOIN tool_execution AS approving_tool_execution \
                     ON approving_tool_execution.id = action.approving_tool_execution_id \
                 JOIN inference AS approving_inference \
                     ON approving_inference.id = approving_tool_execution.inference_id \
                 WHERE sequence.world_id = ? AND action.pending_action_id = ? \
                   AND approving_inference.agent_id = ?",
            )
            .bind(world_id)
            .bind(action_id)
            .bind(gm_agent_id)
            .fetch_optional(&mut *transaction)
            .await
            .context("checking for an already-resolved action")?;
            let (approved, result_content_id) = resolved.with_context(|| {
                format!(
                    "action {action_id} is not pending for group GM {gm_agent_id} in world {world_id}"
                )
            })?;
            transaction.commit().await.with_context(|| {
                format!("committing idempotent re-resolve of action {action_id}")
            })?;
            return Ok(ResolvedAction {
                id: action_id,
                approved,
                result_content_id,
            });
        };
        let approved = result == "approved";
        let result_text = if approved {
            Self::execute_approved_action(&mut transaction, &action).await?
        } else {
            result.to_string()
        };
        let result_content_id =
            Self::content_in(&mut transaction, &MessageContent::Text(result_text), "").await?;
        sqlx::query(
            "INSERT INTO action \
             (sequence_id, requesting_tool_execution_id, approving_tool_execution_id, result_content_id, approved, pending_action_id) \
             VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(action.sequence_id)
        .bind(action.requesting_tool_execution_id)
        .bind(approving_tool_execution_id)
        .bind(result_content_id)
        .bind(approved)
        .bind(action.id)
        .execute(&mut *transaction)
        .await
        .with_context(|| format!("recording resolved action {action_id}"))?;
        let deleted = sqlx::query(
            "DELETE FROM pending_action WHERE world_id = ? AND id = ? AND gm_agent_id = ?",
        )
        .bind(world_id)
        .bind(action_id)
        .bind(gm_agent_id)
        .execute(&mut *transaction)
        .await
        .context("retiring resolved pending action")?
        .rows_affected();
        ensure!(
            deleted == 1,
            "pending action {action_id} changed while resolving"
        );
        transaction
            .commit()
            .await
            .with_context(|| format!("committing resolved action {action_id}"))?;
        self.publish_dev_event(
            action.world_id,
            DevEvent::Changed {
                sequence_id: Some(action.sequence_id),
                inference_id: None,
            },
        );
        Ok(ResolvedAction {
            id: action_id,
            approved,
            result_content_id,
        })
    }

    /// Apply the compact set of stateful actions from their stored pending
    /// rows. The GM never supplies an item or character id: the action's
    /// validated arguments and its membership-owned actor are the only input.
    async fn execute_approved_action(
        transaction: &mut Transaction<'_, Sqlite>,
        action: &PendingAction,
    ) -> Result<String> {
        match action.tool.as_str() {
            "look" | "say" => Ok("approved".to_string()),
            "take" => {
                let arguments: TakeItemArguments = serde_json::from_str(&action.args)
                    .context("parsing stored take action arguments")?;
                ensure!(
                    !arguments.item.trim().is_empty(),
                    "stored take action has an empty item name"
                );
                let item_ids: Vec<i64> = sqlx::query_scalar(
                    "SELECT item.id FROM item \
                     JOIN item_location ON item_location.item_id = item.id \
                     JOIN character_group ON character_group.character_id = ? \
                     JOIN \"group\" ON \"group\".id = character_group.group_id \
                        AND \"group\".location_id = item_location.location_id \
                     WHERE item.world_id = ? AND item.name = ?",
                )
                .bind(action.character_id)
                .bind(action.world_id)
                .bind(&arguments.item)
                .fetch_all(&mut **transaction)
                .await
                .context("finding requested item at acting character's location")?;
                ensure!(
                    item_ids.len() == 1,
                    "item `{}` is not uniquely available at the acting character's location",
                    arguments.item
                );
                let item_id = item_ids[0];
                let removed = sqlx::query("DELETE FROM item_location WHERE item_id = ?")
                    .bind(item_id)
                    .execute(&mut **transaction)
                    .await
                    .context("removing item from its location")?
                    .rows_affected();
                ensure!(removed == 1, "item {item_id} changed before transfer");
                sqlx::query("INSERT INTO item_character (item_id, character_id) VALUES (?, ?)")
                    .bind(item_id)
                    .bind(action.character_id)
                    .execute(&mut **transaction)
                    .await
                    .context("transferring item to acting character")?;
                Ok(format!("approved: {} transferred", arguments.item))
            }
            tool => anyhow::bail!("approved action has unsupported tool `{tool}`"),
        }
    }

    pub async fn has_pending_action(&self, world_id: i64, action_id: i64) -> Result<bool> {
        let found: Option<i64> =
            sqlx::query_scalar("SELECT 1 FROM pending_action WHERE world_id = ? AND id = ?")
                .bind(world_id)
                .bind(action_id)
                .fetch_optional(&self.pool)
                .await
                .context("checking whether action remains pending")?;
        Ok(found.is_some())
    }

    /// Export static scenario data without player-specific memberships,
    /// characters, histories, or allocated handles. The result can initialize
    /// another fresh world rather than copying a particular playthrough.
    pub async fn export_scenario(&self, world_id: i64) -> Result<Scenario> {
        let held_items: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM item_character \
             JOIN item ON item.id = item_character.item_id WHERE item.world_id = ?",
        )
        .bind(world_id)
        .fetch_one(&self.pool)
        .await
        .context("checking scenario item holders")?;
        ensure!(
            held_items == 0,
            "world {world_id} has player-held items and cannot be exported as a fresh scenario"
        );
        let (name, initial_prompt, storyteller_summary, gm_notes, storyteller_notes): (
            String,
            String,
            String,
            String,
            String,
        ) = sqlx::query_as(
            "SELECT name, initial_prompt, storyteller_summary, gm_notes, storyteller_notes \
             FROM world WHERE id = ?",
        )
        .bind(world_id)
        .fetch_optional(&self.pool)
        .await
        .context("loading scenario world")?
        .with_context(|| format!("world {world_id} does not exist"))?;
        let notes = decode_notes(&gm_notes, &storyteller_notes)?;
        let locations: Vec<Location> =
            sqlx::query_as::<_, (String, String, String, String, String)>(
                "SELECT name, kind, description, gm_notes, storyteller_notes FROM location \
             WHERE world_id = ? ORDER BY id",
            )
            .bind(world_id)
            .fetch_all(&self.pool)
            .await
            .context("loading scenario locations")?
            .into_iter()
            .map(|(name, kind, description, gm, storyteller)| {
                Ok(Location {
                    name,
                    kind,
                    description,
                    notes: decode_notes(&gm, &storyteller)?,
                })
            })
            .collect::<Result<_>>()?;
        let starting_location: String = sqlx::query_scalar(
            "SELECT location.name FROM player_character \
             JOIN character_group ON character_group.character_id = player_character.character_id \
             JOIN \"group\" ON \"group\".id = character_group.group_id \
             JOIN location ON location.id = \"group\".location_id \
             JOIN world_member ON world_member.id = player_character.member_id \
             JOIN world_owner ON world_owner.world_id = world_member.world_id AND world_owner.user_id = world_member.user_id \
             WHERE world_member.world_id = ? ORDER BY player_character.member_id LIMIT 1",
        )
        .bind(world_id)
        .fetch_optional(&self.pool)
        .await
        .context("loading scenario start location")?
        .context("scenario world has no owner Adventurer location")?;
        let starting_group: String = sqlx::query_scalar(
            "SELECT \"group\".name FROM player_character \
             JOIN character_group ON character_group.character_id = player_character.character_id \
             JOIN \"group\" ON \"group\".id = character_group.group_id \
             JOIN world_member ON world_member.id = player_character.member_id \
             JOIN world_owner ON world_owner.world_id = world_member.world_id AND world_owner.user_id = world_member.user_id \
             WHERE world_member.world_id = ? ORDER BY player_character.member_id LIMIT 1",
        )
        .bind(world_id)
        .fetch_optional(&self.pool)
        .await
        .context("loading scenario starting group")?
        .context("scenario world has no owner Adventurer group")?;
        let connections: Vec<ScenarioConnection> = sqlx::query_as::<_, (String, String, i64, String, String, String)>(
            "SELECT from_location.name, to_location.name, connection.travel_time, connection.description, connection.gm_notes, connection.storyteller_notes \
             FROM connection JOIN location AS from_location ON from_location.id = connection.from_location_id \
             JOIN location AS to_location ON to_location.id = connection.to_location_id \
             WHERE connection.world_id = ? ORDER BY connection.id",
        )
        .bind(world_id)
        .fetch_all(&self.pool)
        .await
        .context("loading scenario connections")?
        .into_iter()
        .map(|(from, to, travel_time, description, gm, storyteller)| {
            Ok(ScenarioConnection { from, to, travel_time, description, notes: decode_notes(&gm, &storyteller)? })
        })
        .collect::<Result<_>>()?;
        let groups: Vec<ScenarioGroup> = sqlx::query_as::<_, (String, String, String, String, String)>(
            "SELECT \"group\".name, location.name, \"group\".description, \"group\".gm_notes, \"group\".storyteller_notes \
             FROM \"group\" JOIN location ON location.id = \"group\".location_id \
             WHERE \"group\".world_id = ? ORDER BY \"group\".id",
        )
        .bind(world_id)
        .fetch_all(&self.pool)
        .await
        .context("loading scenario groups")?
        .into_iter()
        .map(|(name, location, description, gm, storyteller)| {
            Ok(ScenarioGroup {
                name,
                location,
                description,
                notes: decode_notes(&gm, &storyteller)?,
            })
        })
        .collect::<Result<_>>()?;
        let npcs: Vec<Npc> = sqlx::query_as::<_, (String, String, String, String, String, String, String, String, String)>(
            "SELECT character.name, \"group\".name, character.description, character.background, character.motive, character.ambition, character.sheet, character.gm_notes, character.storyteller_notes \
             FROM character JOIN character_group ON character_group.character_id = character.id \
             JOIN \"group\" ON \"group\".id = character_group.group_id \
             WHERE character.world_id = ? AND character.role = 'npc' ORDER BY character.id",
        )
        .bind(world_id)
        .fetch_all(&self.pool)
        .await
        .context("loading scenario NPCs")?
        .into_iter()
        .map(|(name, group, description, background, motive, ambition, sheet, gm, storyteller)| {
            Ok(Npc { name, group, description, background, motive, ambition, sheet: serde_json::from_str(&sheet).context("decoding NPC sheet")?, notes: decode_notes(&gm, &storyteller)? })
        })
        .collect::<Result<_>>()?;
        let item_types = self.export_item_types("item_type", world_id).await?;
        let spell_types = self.export_item_types("spell_type", world_id).await?;
        let items: Vec<Item> = sqlx::query_as::<_, (String, String, String, String, String, String)>(
            "SELECT item.name, item_type.name, location.name, item.description, item.gm_notes, item.storyteller_notes \
             FROM item JOIN item_type ON item_type.id = item.item_type_id \
             JOIN item_location ON item_location.item_id = item.id \
             JOIN location ON location.id = item_location.location_id \
             WHERE item.world_id = ? ORDER BY item.id",
        )
        .bind(world_id)
        .fetch_all(&self.pool)
        .await
        .context("loading scenario items")?
        .into_iter()
        .map(|(name, item_type, location, description, gm, storyteller)| {
            Ok(Item { name, item_type, location, description, notes: decode_notes(&gm, &storyteller)? })
        })
        .collect::<Result<_>>()?;
        Ok(Scenario {
            name,
            initial_prompt,
            storyteller_summary,
            notes,
            starting_location,
            starting_group,
            locations,
            connections,
            groups,
            npcs,
            item_types,
            spell_types,
            items,
        })
    }

    async fn export_item_types(&self, table: &str, world_id: i64) -> Result<Vec<ItemType>> {
        // Both tables have the same schema and the name is private, fixed code.
        let query = format!(
            "SELECT name, description, stats, gm_notes, storyteller_notes FROM {table} WHERE world_id = ? ORDER BY id"
        );
        sqlx::query_as::<_, (String, String, String, String, String)>(&query)
            .bind(world_id)
            .fetch_all(&self.pool)
            .await
            .with_context(|| format!("loading scenario {table}"))?
            .into_iter()
            .map(|(name, description, stats, gm, storyteller)| {
                Ok(ItemType {
                    name,
                    description,
                    stats: serde_json::from_str(&stats).context("decoding item type stats")?,
                    notes: decode_notes(&gm, &storyteller)?,
                })
            })
            .collect()
    }

    async fn join_world(
        transaction: &mut Transaction<'_, Sqlite>,
        user: &User,
        world_id: i64,
        starting_group_id: i64,
    ) -> Result<JoinedWorld> {
        let member_id = sqlx::query(
            "INSERT INTO world_member (world_id, user_id, access) VALUES (?, ?, 'active')",
        )
        .bind(world_id)
        .bind(user.id)
        .execute(&mut **transaction)
        .await
        .with_context(|| format!("joining user {} to world {world_id}", user.email))?
        .last_insert_rowid();
        let (character_id, agent_id, character_tool_id) =
            Self::create_adventurer(transaction, member_id, world_id, starting_group_id).await?;
        Ok(JoinedWorld {
            member_id,
            character_id,
            agent_id,
            character_tool_id,
        })
    }

    async fn create_adventurer(
        transaction: &mut Transaction<'_, Sqlite>,
        member_id: i64,
        world_id: i64,
        starting_group_id: i64,
    ) -> Result<(i64, i64, i64)> {
        let agent_id = sqlx::query("INSERT INTO agent (world_id) VALUES (?)")
            .bind(world_id)
            .execute(&mut **transaction)
            .await
            .with_context(|| format!("creating player agent for world {world_id}"))?
            .last_insert_rowid();
        Self::create_agent_history(transaction, agent_id).await?;
        let sheet = serde_json::json!({});
        let notes = Notes::default();
        let tool_id = Self::next_character_tool_id(transaction).await?;
        let name = format!("Adventurer{tool_id}");
        let character_id = Self::insert_character_with_tool_id(
            transaction,
            world_id,
            tool_id,
            CharacterSeed {
                role: "pc",
                name: &name,
                description: "",
                background: "",
                motive: "",
                ambition: "",
                sheet: &sheet,
                notes: &notes,
            },
        )
        .await?;
        sqlx::query(
            "INSERT INTO player_character (member_id, character_id, agent_id) VALUES (?, ?, ?)",
        )
        .bind(member_id)
        .bind(character_id)
        .bind(agent_id)
        .execute(&mut **transaction)
        .await
        .with_context(|| format!("linking Adventurer for world {world_id}"))?;
        sqlx::query(
            "INSERT INTO character_group (character_id, group_id, description) VALUES (?, ?, '')",
        )
        .bind(character_id)
        .bind(starting_group_id)
        .execute(&mut **transaction)
        .await
        .with_context(|| format!("placing Adventurer in world {world_id}"))?;
        Ok((character_id, agent_id, tool_id))
    }

    async fn player_sheet(
        transaction: &mut Transaction<'_, Sqlite>,
        member: &PlayerAgent,
    ) -> Result<(i64, String)> {
        sqlx::query_as(
            "SELECT character.id, character.sheet FROM world_member AS member \
             JOIN player_character ON player_character.member_id = member.id \
             JOIN character ON character.id = player_character.character_id \
             WHERE member.id = ? AND member.world_id = ? AND member.user_id = ? \
             AND member.access = 'active' AND player_character.character_id = ? AND player_character.agent_id = ?",
        )
        .bind(member.member_id)
        .bind(member.world_id)
        .bind(member.user_id)
        .bind(member.character_id)
        .bind(member.agent_id)
        .fetch_optional(&mut **transaction)
        .await
        .context("resolving active Adventurer sheet")?
        .context("active membership has no Adventurer")
    }

    /// Record one consequential action row. `requesting_tool_execution_id` is
    /// the tool call that performed it (`None` only for the server-driven test
    /// path with no inference); `approving_tool_execution_id` is the GM call
    /// that arbitrated it, when one did. The caller has already recorded
    /// `result_content_id`'s content via `content_in` - this stays a thin
    /// insert, not a second place that decides how to record content. Every
    /// caller of this function (dice rolls, ready_to_begin) always performed
    /// its mutation unconditionally - only `resolve_pending_action`'s GM
    /// arbitration can reject, so `approved` is always true here.
    async fn record_action(
        transaction: &mut Transaction<'_, Sqlite>,
        sequence_id: i64,
        requesting_tool_execution_id: Option<i64>,
        approving_tool_execution_id: Option<i64>,
        result_content_id: i64,
    ) -> Result<()> {
        sqlx::query(
            "INSERT INTO action (sequence_id, requesting_tool_execution_id, approving_tool_execution_id, result_content_id, approved) \
             VALUES (?, ?, ?, ?, TRUE)",
        )
        .bind(sequence_id)
        .bind(requesting_tool_execution_id)
        .bind(approving_tool_execution_id)
        .bind(result_content_id)
        .execute(&mut **transaction)
        .await
        .with_context(|| format!("recording action for sequence {sequence_id}"))?;
        Ok(())
    }

    async fn store_sheet(
        transaction: &mut Transaction<'_, Sqlite>,
        character_id: i64,
        sheet: &serde_json::Value,
    ) -> Result<()> {
        sqlx::query("UPDATE character SET sheet = ? WHERE id = ?")
            .bind(serde_json::to_string(sheet).context("encoding Adventurer sheet")?)
            .bind(character_id)
            .execute(&mut **transaction)
            .await
            .with_context(|| format!("storing Adventurer sheet for character {character_id}"))?;
        Ok(())
    }

    async fn insert_character(
        transaction: &mut Transaction<'_, Sqlite>,
        world_id: i64,
        character: CharacterSeed<'_>,
    ) -> Result<i64> {
        let tool_id = Self::next_character_tool_id(transaction).await?;
        Self::insert_character_with_tool_id(transaction, world_id, tool_id, character).await
    }

    async fn insert_character_with_tool_id(
        transaction: &mut Transaction<'_, Sqlite>,
        world_id: i64,
        tool_id: i64,
        character: CharacterSeed<'_>,
    ) -> Result<i64> {
        Ok(sqlx::query(
            "INSERT INTO character \
             (world_id, tool_id, name, role, description, background, motive, ambition, sheet, gm_notes, storyteller_notes) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(world_id)
        .bind(tool_id)
        .bind(character.name)
        .bind(character.role)
        .bind(character.description)
        .bind(character.background)
        .bind(character.motive)
        .bind(character.ambition)
        .bind(serde_json::to_string(character.sheet)?)
        .bind(serde_json::to_string(&character.notes.gm)?)
        .bind(serde_json::to_string(&character.notes.storyteller)?)
        .execute(&mut **transaction)
        .await
        .with_context(|| format!("creating {} character {}", character.role, character.name))?
        .last_insert_rowid())
    }

    async fn next_character_tool_id(transaction: &mut Transaction<'_, Sqlite>) -> Result<i64> {
        let mut lower = 10_i64;
        loop {
            let upper = lower
                .checked_mul(10)
                .context("character handle range overflow")?;
            let used: Vec<i64> = sqlx::query_scalar(
                "SELECT tool_id FROM character WHERE tool_id >= ? AND tool_id < ?",
            )
            .bind(lower)
            .bind(upper)
            .fetch_all(&mut **transaction)
            .await
            .context("loading allocated character handles")?;
            let candidates = (lower..upper)
                .filter(|candidate| !used.contains(candidate))
                .collect::<Vec<_>>();
            if !candidates.is_empty() {
                return Ok(candidates[rand::rng().random_range(0..candidates.len())]);
            }
            lower = upper;
        }
    }

    /// Append player input or delivered narration: content this agent's own
    /// inferences did not produce, so it carries no source inference.
    pub async fn append_message(&self, agent_id: i64, message: &Message) -> Result<i64> {
        self.append_message_from(agent_id, message, None, None, None)
            .await
    }

    /// Append player input that begins `trigger_sequence_id`, e.g. the message
    /// that started a player turn. No inference exists yet to resolve the
    /// sequence through, so this entry names it directly.
    pub async fn append_message_with_sequence(
        &self,
        agent_id: i64,
        message: &Message,
        trigger_sequence_id: i64,
    ) -> Result<i64> {
        self.append_message_from(agent_id, message, None, None, Some(trigger_sequence_id))
            .await
    }

    /// Append a game-authored instruction that the model receives as a user
    /// turn. Its distinct origin prevents Developer Mode from calling it a
    /// human player's message.
    pub async fn append_instruction(
        &self,
        branch: AgentBranch,
        message: &Message,
        trigger_sequence_id: i64,
    ) -> Result<i64> {
        let mut transaction = self.pool.begin().await.with_context(|| {
            format!(
                "starting event transaction for agent {} on branch {}",
                branch.agent_id(),
                branch.branch_id()
            )
        })?;
        let (message_id, _) = Self::insert_message(
            &mut transaction,
            branch.branch_id(),
            message,
            None,
            None,
            Some(trigger_sequence_id),
            MessageOrigin::Instruction,
        )
        .await?;
        transaction.commit().await.with_context(|| {
            format!(
                "committing event for agent {} on branch {}",
                branch.agent_id(),
                branch.branch_id()
            )
        })?;
        Ok(message_id)
    }

    async fn append_message_from(
        &self,
        agent_id: i64,
        message: &Message,
        source_inference_id: Option<i64>,
        source_tool_execution_id: Option<i64>,
        trigger_sequence_id: Option<i64>,
    ) -> Result<i64> {
        let mut transaction = self
            .pool
            .begin()
            .await
            .with_context(|| format!("starting message transaction for agent {agent_id}"))?;
        let branch_id = Self::branch_for_agent_tx(&mut transaction, agent_id).await?;
        let (message_id, _content_id) = Self::insert_message(
            &mut transaction,
            branch_id,
            message,
            source_inference_id,
            source_tool_execution_id,
            trigger_sequence_id,
            MessageOrigin::Conversation,
        )
        .await?;
        transaction
            .commit()
            .await
            .with_context(|| format!("committing message for agent {agent_id}"))?;
        Ok(message_id)
    }

    /// Record one tool call's result: its message (referencing the exact
    /// `tool_execution` it answers) and that execution's own completion
    /// (`status`, `result_content_id`), in one transaction. `result_content_id`
    /// points at the same `recorded_content` row the message references -
    /// design.md's `tool_execution` shares this content write rather than
    /// duplicating it.
    pub async fn complete_tool_result(
        &self,
        source_inference_id: i64,
        tool_execution_id: i64,
        status: &str,
        message: &Message,
    ) -> Result<()> {
        let mut transaction = self.pool.begin().await.with_context(|| {
            format!("starting tool result transaction for tool_execution {tool_execution_id}")
        })?;
        // The branch the triggering inference's own history_entry lives on -
        // not a fresh agent_history lookup, which a world_gm agent (one
        // branch per group) has no single row for.
        let branch_id: i64 = sqlx::query_scalar(
            "SELECT history_entry.branch_id FROM inference \
             JOIN history_entry ON history_entry.id = inference.history_entry_id \
             WHERE inference.id = ?",
        )
        .bind(source_inference_id)
        .fetch_optional(&mut *transaction)
        .await
        .context("loading tool result's source inference branch")?
        .with_context(|| format!("inference {source_inference_id} does not exist"))?;
        let (_message_id, content_id) = Self::insert_message(
            &mut transaction,
            branch_id,
            message,
            Some(source_inference_id),
            Some(tool_execution_id),
            None,
            MessageOrigin::Conversation,
        )
        .await?;
        let result = sqlx::query(
            "UPDATE tool_execution SET status = ?, result_content_id = ?, finished_at = CURRENT_TIMESTAMP \
             WHERE id = ? AND finished_at IS NULL",
        )
        .bind(status)
        .bind(content_id)
        .bind(tool_execution_id)
        .execute(&mut *transaction)
        .await
        .with_context(|| format!("completing tool_execution {tool_execution_id}"))?;
        ensure!(
            result.rows_affected() == 1,
            "tool_execution {tool_execution_id} is missing or already finished"
        );
        transaction.commit().await.with_context(|| {
            format!("committing tool result for tool_execution {tool_execution_id}")
        })?;
        Ok(())
    }

    /// Allocate this branch's next history entry and insert the message row
    /// sharing its id, inside an already-open transaction. Shared by every
    /// message-producing path (player input, tool-loop turns, and the final
    /// reply written by `finish_chat_inference`). Returns the message's own
    /// id (its history_entry id) and the `recorded_content` row it wrote, so
    /// a caller that also needs the content reference (`complete_tool_result`)
    /// does not re-derive or re-store it.
    async fn insert_message(
        transaction: &mut Transaction<'_, Sqlite>,
        branch_id: i64,
        message: &Message,
        source_inference_id: Option<i64>,
        source_tool_execution_id: Option<i64>,
        trigger_sequence_id: Option<i64>,
        origin: MessageOrigin,
    ) -> Result<(i64, i64)> {
        let content = Self::content_in(transaction, &message.content, &message.reasoning).await?;
        let message_id = Self::insert_message_with_content(
            transaction,
            branch_id,
            (message.role.clone(), origin),
            content,
            source_inference_id,
            source_tool_execution_id,
            trigger_sequence_id,
        )
        .await?;
        Ok((message_id, content))
    }

    /// As `insert_message`, but for an already-recorded content reference
    /// rather than original content to store fresh - e.g. GM narration
    /// delivered to several recipients, which all reference the one
    /// `recorded_content` row the GM's own reply already wrote (design.md's
    /// Chat schema: "every recipient references that same selection").
    async fn insert_message_with_content(
        transaction: &mut Transaction<'_, Sqlite>,
        branch_id: i64,
        (role, origin): (Role, MessageOrigin),
        content: i64,
        source_inference_id: Option<i64>,
        source_tool_execution_id: Option<i64>,
        trigger_sequence_id: Option<i64>,
    ) -> Result<i64> {
        let (history_entry_id, _position) =
            Self::append_history_entry(transaction, branch_id, EntryKind::Message).await?;
        let role = serde_json::to_string(&role).context("serializing message role")?;
        sqlx::query(
            "INSERT INTO message (id, role, origin, content, source_inference_id, source_tool_execution_id, trigger_sequence_id) \
             VALUES (?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(history_entry_id)
        .bind(role)
        .bind(origin.as_str())
        .bind(content)
        .bind(source_inference_id)
        .bind(source_tool_execution_id)
        .bind(trigger_sequence_id)
        .execute(&mut **transaction)
        .await
        .with_context(|| format!("appending message {history_entry_id} on branch {branch_id}"))?;
        Ok(history_entry_id)
    }

    /// Every pending compaction, whichever relation owns its branch: an
    /// ordinary agent's single `agent_history` row, or - for a world_gm
    /// agent, which has no such row - the group whose `branch_id` it is
    /// (design.md's `GroupHistory`). A plain `agent_history` JOIN alone would
    /// silently exclude every world_gm branch's jobs, including at startup
    /// recovery.
    pub async fn pending_compactions(&self) -> Result<Vec<PendingCompaction>> {
        let rows = sqlx::query_as::<_, PendingCompactionRow>(
            "SELECT COALESCE(agent_history.agent_id, world_gm.agent_id) AS agent_id, \
                    pending_compaction.branch_id, position, sequence_id, cause_inference_id, \
                    next_input_tokens, sampling, model, static_segments \
             FROM pending_compaction \
             LEFT JOIN agent_history ON agent_history.branch_id = pending_compaction.branch_id \
             LEFT JOIN \"group\" ON \"group\".branch_id = pending_compaction.branch_id \
             LEFT JOIN world_gm ON world_gm.world_id = \"group\".world_id \
             ORDER BY pending_compaction.branch_id",
        )
        .fetch_all(&self.pool)
        .await
        .context("loading pending compactions")?;
        rows.into_iter().map(PendingCompaction::try_from).collect()
    }

    /// As `pending_compaction_for_branch`, for test assertions on an ordinary
    /// agent's single history branch. Production callers must carry an
    /// explicit branch because a world GM has several.
    #[cfg(test)]
    pub async fn pending_compaction(&self, agent_id: i64) -> Result<Option<PendingCompaction>> {
        self.pending_compaction_for_branch(self.branch_for_agent(agent_id).await?)
            .await
    }

    /// As `pending_compactions`, for one already-known branch - the only
    /// lookup that works for a world_gm agent, which may own several
    /// branches at once and so cannot be resolved by `agent_id` alone.
    pub async fn pending_compaction_for_branch(
        &self,
        branch_id: i64,
    ) -> Result<Option<PendingCompaction>> {
        sqlx::query_as::<_, PendingCompactionRow>(
            "SELECT COALESCE(agent_history.agent_id, world_gm.agent_id) AS agent_id, \
                    pending_compaction.branch_id, position, sequence_id, cause_inference_id, \
                    next_input_tokens, sampling, model, static_segments \
             FROM pending_compaction \
             LEFT JOIN agent_history ON agent_history.branch_id = pending_compaction.branch_id \
             LEFT JOIN \"group\" ON \"group\".branch_id = pending_compaction.branch_id \
             LEFT JOIN world_gm ON world_gm.world_id = \"group\".world_id \
             WHERE pending_compaction.branch_id = ?",
        )
        .bind(branch_id)
        .fetch_optional(&self.pool)
        .await
        .with_context(|| format!("loading pending compaction for branch {branch_id}"))?
        .map(PendingCompaction::try_from)
        .transpose()
    }

    /// Persist compaction required by a request the fixed KV cache rejected
    /// before it could produce a response. This is the exceptional counterpart
    /// to the compaction obligation `finish_chat_inference` enqueues for a
    /// settled reply: tool-loop history may have grown after the last
    /// completed inference, so there is no final reply to attach it to.
    pub async fn enqueue_capacity_compaction(
        &self,
        agent_id: i64,
        branch_id: Option<i64>,
        requested_tokens: usize,
        policy: CompactionPolicy<'_>,
    ) -> Result<()> {
        let branch_id = match branch_id {
            Some(branch_id) => branch_id,
            None => self.branch_for_agent(agent_id).await?,
        };
        let position: i64 =
            sqlx::query_scalar("SELECT next_position - 1 FROM history_branch WHERE id = ?")
                .bind(branch_id)
                .fetch_one(&self.pool)
                .await
                .with_context(|| format!("finding history to compact for agent {agent_id}"))?;
        ensure!(
            position >= 0,
            "model request for agent {agent_id} exceeded fixed context capacity before any history existed to compact"
        );
        sqlx::query(
            "INSERT INTO pending_compaction (branch_id, position, sequence_id, cause_inference_id, next_input_tokens, sampling, model, static_segments) VALUES (?, ?, ?, NULL, ?, ?, ?, ?) ON CONFLICT(branch_id) DO NOTHING",
        )
        .bind(branch_id)
        .bind(position)
        .bind(policy.sequence_id)
        .bind(i64::try_from(requested_tokens).context("capacity-rejected input token count is too large")?)
        .bind(serde_json::to_string(policy.sampling).context("serializing capacity compaction sampling")?)
        .bind(policy.model)
        .bind(serde_json::to_string(policy.static_segments)
            .context("serializing capacity compaction static context")?)
        .execute(&self.pool)
        .await
        .with_context(|| format!("enqueueing capacity recovery compaction for agent {agent_id}"))?;
        Ok(())
    }

    /// Store one static prompt piece and return the id a recipe refers to.
    /// Rows are never updated, so the reference stays true to what was sent.
    pub async fn store_prompt_text(&self, content: &str) -> Result<i64> {
        let mut transaction = self
            .pool
            .begin()
            .await
            .context("starting prompt text transaction")?;
        let id = Self::text_in(&mut transaction, content).await?;
        transaction
            .commit()
            .await
            .context("committing prompt text")?;
        Ok(id)
    }

    async fn text(&self, id: i64) -> Result<String> {
        sqlx::query_scalar("SELECT content FROM text WHERE id = ?")
            .bind(id)
            .fetch_optional(&self.pool)
            .await
            .context("loading prompt text")?
            .with_context(|| format!("inference references missing text {id}"))
    }

    /// The summary currently selected as this agent's live point-in-time
    /// replacement: the newest activation on its branch. A convenience over
    /// `latest_summary_for_branch` for an ordinary agent with exactly one
    /// branch; every production caller already has an explicit branch_id
    /// (a world_gm agent has no single branch to resolve here) and uses that
    /// directly instead.
    #[cfg(test)]
    pub async fn latest_summary(&self, agent_id: i64) -> Result<Option<Summary>> {
        let branch_id = self.branch_for_agent(agent_id).await?;
        self.latest_summary_for_branch(branch_id).await
    }

    /// As `latest_summary`, for an already-known branch - the only lookup
    /// that works for a world_gm agent, which may own several branches at
    /// once (design.md's `GroupHistory`).
    pub async fn latest_summary_for_branch(&self, branch_id: i64) -> Result<Option<Summary>> {
        let row = sqlx::query_as::<_, SummaryRow>(
            "SELECT summary.id, summary.history_entry_id, summary.agent_id, summary.covered_ranges, \
                    json_extract(summary.body, '$.content.value') AS content, summary.inference_id, summary.reason, summary.retained_messages \
             FROM summary_content AS summary \
             JOIN history_entry ON history_entry.id = summary.history_entry_id \
             WHERE history_entry.branch_id = ? ORDER BY history_entry.position DESC LIMIT 1",
        )
        .bind(branch_id)
        .fetch_optional(&self.pool)
        .await
        .with_context(|| format!("loading latest summary for branch {branch_id}"))?;
        row.map(Summary::try_from).transpose()
    }

    /// Load live history through a branch whose ownership was established at
    /// the turn/inspection boundary.
    pub async fn live_segments(&self, branch: AgentBranch) -> Result<Vec<Segment>> {
        self.history_live_segments(branch).await
    }

    /// This branch's own raw message count not yet covered by its own newest
    /// summary - its unique suffix, since a shared ancestor's messages live
    /// on the ancestor's own branch, never copied onto this one (design.md's
    /// merge eligibility check: "each incoming branch's unique suffix is
    /// eligible for compaction if its raw-message count exceeds
    /// merge_branch_messages").
    pub async fn uncovered_message_count(&self, branch_id: i64) -> Result<i64> {
        let covered = self
            .latest_summary_for_branch(branch_id)
            .await?
            .map_or(-1, |summary| {
                summary
                    .covered_ranges
                    .last()
                    .map_or(-1, |range| range.last_position)
            });
        sqlx::query_scalar(
            "SELECT COUNT(*) FROM message \
             JOIN history_entry ON history_entry.id = message.id \
             WHERE history_entry.branch_id = ? AND history_entry.position > ?",
        )
        .bind(branch_id)
        .bind(covered)
        .fetch_one(&self.pool)
        .await
        .with_context(|| format!("counting uncovered messages for branch {branch_id}"))
    }

    /// Find the newest raw suffix containing exactly `keep_tail_messages`
    /// message entries, or every available one when there are fewer. Returns
    /// the branch position just before that suffix starts (the new covered
    /// cut), or the already-covered position when there is nothing to split.
    /// Every caller already has an explicit branch_id (a world_gm agent has
    /// no single branch to resolve here - design.md's `GroupHistory`).
    pub async fn tail_split_for_branch(
        &self,
        branch_id: i64,
        keep_tail_messages: usize,
    ) -> Result<i64> {
        let covered = self
            .latest_summary_for_branch(branch_id)
            .await?
            .map_or(-1, |summary| {
                summary
                    .covered_ranges
                    .last()
                    .map_or(-1, |range| range.last_position)
            });
        let rows = sqlx::query_as::<_, (i64,)>(
            "SELECT history_entry.position FROM message \
             JOIN history_entry ON history_entry.id = message.id \
             WHERE history_entry.branch_id = ? AND history_entry.position > ? \
             ORDER BY history_entry.position DESC LIMIT ?",
        )
        .bind(branch_id)
        .bind(covered)
        .bind(keep_tail_messages as i64)
        .fetch_all(&self.pool)
        .await
        .with_context(|| format!("loading raw tail for branch {branch_id}"))?;
        Ok(rows.last().map_or(covered, |(position,)| position - 1))
    }

    /// Every `Segment::Messages` range's branch must belong to `agent_id` - a
    /// world_gm agent plays several groups, each its own branch, so this
    /// checks each range's actual branch rather than comparing against one
    /// fixed `agent_history` branch (design.md's `GroupHistory`). One query
    /// over every distinct branch a merge's segments reference, rather than
    /// one `branch_owner` lookup per `Segment::Messages` entry.
    async fn check_message_range_ownership(
        &self,
        agent_id: i64,
        segments: &[Segment],
    ) -> Result<()> {
        let mut branch_ids: Vec<i64> = segments
            .iter()
            .filter_map(|segment| match segment {
                Segment::Messages { messages } => Some(messages.branch_id),
                _ => None,
            })
            .collect();
        branch_ids.sort_unstable();
        branch_ids.dedup();
        if branch_ids.is_empty() {
            return Ok(());
        }
        let mut query =
            sqlx::QueryBuilder::new("SELECT id, agent_id FROM history_branch WHERE id IN (");
        let mut separated = query.separated(", ");
        for branch_id in &branch_ids {
            separated.push_bind(branch_id);
        }
        query.push(")");
        let owners: Vec<(i64, i64)> = query
            .build_query_as()
            .fetch_all(&self.pool)
            .await
            .context("loading owners of referenced message-range branches")?;
        for branch_id in &branch_ids {
            let owner_agent_id = owners
                .iter()
                .find(|(id, _)| id == branch_id)
                .map(|(_, agent_id)| *agent_id)
                .with_context(|| format!("history branch {branch_id} does not exist"))?;
            ensure!(
                owner_agent_id == agent_id,
                "inference for agent {agent_id} references messages from branch {branch_id} owned by agent {owner_agent_id}"
            );
        }
        Ok(())
    }

    pub async fn request_for_segments(
        &self,
        agent_id: i64,
        segments: &[Segment],
        sampling: Sampling,
    ) -> Result<Request> {
        self.check_message_range_ownership(agent_id, segments)
            .await?;
        let mut messages = Vec::new();
        let mut tools = Vec::new();
        for segment in segments {
            match segment {
                Segment::Text { text, role } => {
                    messages.push(Message::text(role.clone(), self.text(*text).await?));
                }
                Segment::Context { context } => {
                    let context = self.text(*context).await?;
                    match messages.first_mut() {
                        Some(Message {
                            role: Role::System,
                            content: MessageContent::Text(system),
                            ..
                        }) => {
                            system.push_str("\n\n");
                            system.push_str(&context);
                        }
                        _ => messages.insert(0, Message::text(Role::System, context)),
                    }
                }
                Segment::Tools { text } => {
                    let definitions: Vec<ToolDefinition> =
                        serde_json::from_str(&self.text(*text).await?)
                            .context("deserializing tool definitions")?;
                    tools.extend(definitions);
                }
                Segment::Summary { summary } => {
                    let row = sqlx::query_as::<_, SummaryRow>(
                        "SELECT summary.id, summary.history_entry_id, summary.agent_id, summary.covered_ranges, \
                                json_extract(summary.body, '$.content.value') AS content, summary.inference_id, summary.reason, summary.retained_messages \
                         FROM summary_content AS summary WHERE summary.id = ?",
                    )
                    .bind(summary)
                    .fetch_optional(&self.pool)
                    .await
                    .context("loading summary")?
                    .with_context(|| format!("inference references missing summary {summary}"))?;
                    let summary = Summary::try_from(row)?;
                    ensure!(
                        summary.agent_id == agent_id,
                        "inference for agent {agent_id} references summary {} from agent {}",
                        summary.id,
                        summary.agent_id
                    );
                    // The summary is prior conversation carried in the leading
                    // system message as a marked block, not a second system
                    // turn: strict chat templates reject a system message
                    // anywhere but first. When a recipe has a role prompt it
                    // hosts the block; otherwise the summary is that message.
                    let block = format!("Conversation summary so far:\n{}", summary.content);
                    match messages.first_mut() {
                        Some(Message {
                            role: Role::System,
                            content: MessageContent::Text(system),
                            ..
                        }) => {
                            system.push_str("\n\n");
                            system.push_str(&block);
                        }
                        _ => messages.insert(0, Message::text(Role::System, block)),
                    }
                }
                Segment::Messages { messages: range } => {
                    // Ownership of every range's branch was already checked
                    // once, up front, for the whole request - see
                    // `check_message_range_ownership`.
                    ensure!(
                        range.first_position <= range.last_position,
                        "message range {}..={} is invalid",
                        range.first_position,
                        range.last_position
                    );
                    // The range is a span of the branch's unified position
                    // index, not a contiguous run of messages: an inference
                    // start or summary activation between two
                    // messages occupies a position in range without being a
                    // message row. Only every message-kind entry in range
                    // missing its message row is real corruption.
                    let rows = sqlx::query_as::<_, MessageRow>(
                        "SELECT message.id, message.role, message.origin, message.content FROM message \
                         JOIN history_entry ON history_entry.id = message.id \
                         WHERE history_entry.branch_id = ? AND history_entry.position BETWEEN ? AND ? \
                         ORDER BY history_entry.position",
                    )
                    .bind(range.branch_id)
                    .bind(range.first_position)
                    .bind(range.last_position)
                    .fetch_all(&self.pool)
                    .await
                    .with_context(|| {
                        format!(
                            "loading messages {}..={} on branch {}",
                            range.first_position, range.last_position, range.branch_id
                        )
                    })?;
                    let expected: i64 = sqlx::query_scalar(
                        "SELECT COUNT(*) FROM history_entry \
                         WHERE branch_id = ? AND position BETWEEN ? AND ? AND kind = 'message'",
                    )
                    .bind(range.branch_id)
                    .bind(range.first_position)
                    .bind(range.last_position)
                    .fetch_one(&self.pool)
                    .await
                    .with_context(|| {
                        format!(
                            "counting message entries {}..={} on branch {}",
                            range.first_position, range.last_position, range.branch_id
                        )
                    })?;
                    ensure!(
                        i64::try_from(rows.len())
                            .context("message row count exceeds SQLite range")?
                            == expected,
                        "message range {}..={} on branch {} has missing rows",
                        range.first_position,
                        range.last_position,
                        range.branch_id
                    );
                    for row in rows {
                        let role = serde_json::from_str(&row.role).with_context(|| {
                            format!("deserializing role for message {}", row.id)
                        })?;
                        let (content, _) = self.recorded_content(row.content).await?;
                        messages.push(Message {
                            role,
                            origin: row.origin.parse().map_err(anyhow::Error::msg)?,
                            content,
                            reasoning: String::new(),
                        });
                    }
                }
            }
        }
        // A well-formed conversation for any chat model: at most one system
        // message and only in first place, and a real user turn to answer.
        // Strict templates (Qwen3.5) reject anything else outright; lenient
        // ones hide the malformed input.
        let system_positions = messages
            .iter()
            .enumerate()
            .filter(|(_, message)| message.role == Role::System)
            .map(|(index, _)| index)
            .collect::<Vec<usize>>();
        ensure!(
            system_positions.is_empty() || system_positions == [0],
            "assembled request for agent {agent_id} has system messages at {system_positions:?}, not only first"
        );
        ensure!(
            messages.iter().any(|message| message.role == Role::User),
            "assembled request for agent {agent_id} has no user turn for the model to answer"
        );
        Ok(Request {
            messages,
            tools,
            sampling,
        })
    }

    /// Called by the execution host before resuming jobs, never by read-only
    /// replay. No duration, token usage or game effect is invented for work
    /// lost with the process (design.md: "Process recovery marks open
    /// sequences, invocations, inferences and tools interrupted; it never
    /// invents completed game effects").
    pub async fn interrupt_open_inferences(&self) -> Result<()> {
        sqlx::query("UPDATE inference SET error = 'Inference interrupted by process restart', finished_at = CURRENT_TIMESTAMP WHERE finished_at IS NULL")
            .execute(&self.pool).await.context("marking interrupted inferences at startup")?;
        sqlx::query("UPDATE tool_execution SET status = 'unexecuted', error = 'Interrupted by process restart', finished_at = CURRENT_TIMESTAMP WHERE finished_at IS NULL")
            .execute(&self.pool).await.context("marking interrupted tool executions at startup")?;
        sqlx::query("UPDATE invocation SET outcome = 'interrupted', finished_at = CURRENT_TIMESTAMP WHERE finished_at IS NULL")
            .execute(&self.pool).await.context("marking interrupted invocations at startup")?;
        sqlx::query("UPDATE sequence SET error = 'Interrupted by process restart', finished_at = CURRENT_TIMESTAMP WHERE finished_at IS NULL")
            .execute(&self.pool).await.context("marking interrupted sequences at startup")?;
        Ok(())
    }

    /// Create one `running` tool_execution row per call in a returned batch,
    /// before any of them execute (design.md: "Create rows for every call
    /// before executing the batch"), keyed by output ordinal so two identical
    /// calls in one batch still resolve to distinct rows. Returns each row's
    /// id in the same order as `calls`.
    pub async fn create_tool_executions(
        &self,
        inference_id: i64,
        calls: usize,
    ) -> Result<Vec<i64>> {
        let mut transaction =
            self.pool.begin().await.with_context(|| {
                format!("starting tool executions for inference {inference_id}")
            })?;
        let mut ids = Vec::with_capacity(calls);
        for ordinal in 0..calls {
            let ordinal =
                i64::try_from(ordinal).context("tool call ordinal exceeds SQLite range")?;
            let result = sqlx::query(
                "INSERT INTO tool_execution (inference_id, ordinal, status) VALUES (?, ?, 'running')",
            )
            .bind(inference_id)
            .bind(ordinal)
            .execute(&mut *transaction)
            .await
            .with_context(|| format!("creating tool_execution {ordinal} for inference {inference_id}"))?;
            ids.push(result.last_insert_rowid());
        }
        transaction
            .commit()
            .await
            .with_context(|| format!("committing tool executions for inference {inference_id}"))?;
        Ok(ids)
    }

    /// Complete one tool_execution as `completed`, `rejected`, `failed` or
    /// `unexecuted`, recording its result content when it produced one.
    /// Validated explicitly rather than trusting the caller and relying on
    /// the table's CHECK constraint to reject a bad value with a raw SQL error.
    pub async fn complete_tool_execution(
        &self,
        tool_execution_id: i64,
        status: &str,
        result_content_id: Option<i64>,
        error: Option<&str>,
    ) -> Result<()> {
        ensure!(
            matches!(status, "completed" | "rejected" | "failed" | "unexecuted"),
            "invalid tool_execution status `{status}`"
        );
        let result = sqlx::query(
            "UPDATE tool_execution SET status = ?, result_content_id = ?, error = ?, finished_at = CURRENT_TIMESTAMP \
             WHERE id = ? AND finished_at IS NULL",
        )
        .bind(status)
        .bind(result_content_id)
        .bind(error)
        .bind(tool_execution_id)
        .execute(&self.pool)
        .await
        .with_context(|| format!("completing tool_execution {tool_execution_id}"))?;
        ensure!(
            result.rows_affected() == 1,
            "tool_execution {tool_execution_id} is missing or already finished"
        );
        Ok(())
    }

    /// Mark every tool_execution row still `running` for `inference_id` as
    /// `unexecuted`: an early batch abort (budget/infrastructure failure mid
    /// batch) must not leave a sibling that was never reached looking like it
    /// is still in progress (design.md: "Unexecuted siblings retain an
    /// explicit status").
    pub async fn mark_unexecuted_tool_executions(&self, inference_id: i64) -> Result<()> {
        sqlx::query(
            "UPDATE tool_execution SET status = 'unexecuted', finished_at = CURRENT_TIMESTAMP \
             WHERE inference_id = ? AND status = 'running'",
        )
        .bind(inference_id)
        .execute(&self.pool)
        .await
        .with_context(|| {
            format!("marking unexecuted tool executions for inference {inference_id}")
        })?;
        Ok(())
    }

    pub async fn start_inference(&self, input: InferenceInput<'_>) -> Result<i64> {
        let segments =
            serde_json::to_string(input.segments).context("serializing inference segments")?;
        let sampling = serde_json::to_string(&input.request.sampling)
            .context("serializing inference sampling")?;
        // Verification only: reconstruction uses the bounded recipe, never the hash.
        let input_hash = inference_input_hash(input.request)?;
        let mut transaction = self
            .pool
            .begin()
            .await
            .context("starting inference-start transaction")?;
        // The invocation already resolved its branch at start_invocation time
        // (a world_gm agent has one branch per group); every inference within
        // it appends to that same branch, not a fresh agent_history lookup
        // (migrations/0001_recording.sql's invocation.branch_id: "the branch
        // it started on; each inference within it records its own actual
        // branch").
        let branch_id: i64 = sqlx::query_scalar("SELECT branch_id FROM invocation WHERE id = ?")
            .bind(input.invocation_id)
            .fetch_optional(&mut *transaction)
            .await
            .context("loading invocation branch")?
            .with_context(|| format!("invocation {} does not exist", input.invocation_id))?;
        let (history_entry_id, _position) =
            Self::append_history_entry(&mut transaction, branch_id, EntryKind::Inference).await?;
        // Successive inferences within one invocation are ordered by ordinal
        // (design.md: "`inference.invocation_id` plus ordinal orders
        // successive inferences within that invocation"); allocated as one
        // point update on invocation.next_inference_ordinal (matching
        // allocate_position's UPDATE...RETURNING pattern) rather than a
        // COUNT(*) that rescans every prior inference in the invocation.
        let ordinal: i64 = sqlx::query_scalar(
            "UPDATE invocation SET next_inference_ordinal = next_inference_ordinal + 1 \
             WHERE id = ? RETURNING next_inference_ordinal - 1",
        )
        .bind(input.invocation_id)
        .fetch_optional(&mut *transaction)
        .await
        .context("allocating inference ordinal")?
        .with_context(|| format!("invocation {} does not exist", input.invocation_id))?;
        let result = sqlx::query("INSERT INTO inference (history_entry_id, agent_id, sequence_id, invocation_id, ordinal, segments, sampling, input_hash, model) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)")
            .bind(history_entry_id).bind(input.agent_id).bind(input.sequence_id).bind(input.invocation_id).bind(ordinal)
            .bind(segments).bind(sampling).bind(input_hash).bind(input.model)
            .execute(&mut *transaction).await.context("recording inference start")?;
        let inference_id = result.last_insert_rowid();
        transaction
            .commit()
            .await
            .context("committing inference start")?;
        let parent_inference_id = self
            .invocation_parent_inference_id(input.invocation_id)
            .await?;
        match self.world_id_for_agent(input.agent_id).await {
            Ok(world_id) => self.publish_dev_event(
                world_id,
                DevEvent::InferenceStarted {
                    inference_id,
                    agent_id: input.agent_id,
                    sequence_id: input.sequence_id,
                    parent_inference_id,
                },
            ),
            Err(error) => {
                tracing::warn!(inference_id, error = %format!("{error:#}"), "could not resolve world for live inference-started broadcast");
            }
        }
        Ok(inference_id)
    }

    /// The world an agent belongs to.
    /// The branch backing an agent's single writable history - the branch id
    /// a `Segment::Messages`/`Segment::Summary` range or a compaction job is
    /// keyed by.
    pub async fn branch_id_for_agent(&self, agent_id: i64) -> Result<i64> {
        self.branch_for_agent(agent_id).await
    }

    pub async fn world_id_for_agent(&self, agent_id: i64) -> Result<i64> {
        sqlx::query_scalar("SELECT world_id FROM agent WHERE id = ?")
            .bind(agent_id)
            .fetch_one(&self.pool)
            .await
            .with_context(|| format!("looking up world for agent {agent_id}"))
    }

    /// The world an already-started inference belongs to. A streaming
    /// backend resolves this once before its token loop starts (not per
    /// token) so it can call `publish_token` without a query on every delta;
    /// it only has `inference_id` in scope, not `agent_id`.
    pub async fn world_id_for_inference(&self, inference_id: i64) -> Result<i64> {
        sqlx::query_scalar(
            "SELECT agent.world_id FROM inference JOIN agent ON agent.id = inference.agent_id WHERE inference.id = ?",
        )
        .bind(inference_id)
        .fetch_one(&self.pool)
        .await
        .with_context(|| format!("looking up world for inference {inference_id}"))
    }

    /// Publishes one streamed token to `world_id`'s Developer Mode
    /// subscribers, if any are connected.
    /// `offset` is the number of UTF-8 bytes of this inference's output
    /// already emitted before `text` - the caller's running total, since
    /// only the per-inference token loop knows it cheaply. A receiver that
    /// missed earlier tokens (broadcast lag) can then tell its accumulated
    /// text is short instead of silently rendering a wrong prefix.
    pub fn publish_token(&self, world_id: i64, inference_id: i64, offset: usize, text: &str) {
        self.publish_dev_event(
            world_id,
            DevEvent::Token {
                inference_id,
                offset,
                text: text.to_string(),
            },
        );
    }

    /// Compute the `inference` row's completion columns from an outcome,
    /// writing its content row when it succeeded. Shared by every path that
    /// completes an inference (`finish_inference`, `finish_chat_inference`,
    /// `finish_summary_inference`) so the outcome-to-columns mapping and its
    /// content write exist in exactly one place.
    async fn completion_columns(
        transaction: &mut Transaction<'_, Sqlite>,
        outcome: InferenceOutcome,
    ) -> Result<(Option<i64>, Option<String>, Option<i64>, Option<i64>)> {
        Ok(match outcome {
            InferenceOutcome::Response(response) => (
                Some(
                    Self::content_in(
                        transaction,
                        &Message::assistant(response.content, response.reasoning.clone()).content,
                        &response.reasoning,
                    )
                    .await?,
                ),
                None,
                Some(
                    i64::try_from(response.usage.input_tokens)
                        .context("input token count exceeds SQLite range")?,
                ),
                Some(
                    i64::try_from(response.usage.output_tokens)
                        .context("output token count exceeds SQLite range")?,
                ),
            ),
            InferenceOutcome::Error(error) => (None, Some(error), None, None),
        })
    }

    async fn update_inference_completion(
        transaction: &mut Transaction<'_, Sqlite>,
        id: i64,
        output: Option<i64>,
        error: Option<&str>,
        input_tokens: Option<i64>,
        output_tokens: Option<i64>,
        duration_ms: i64,
    ) -> Result<()> {
        let result = sqlx::query("UPDATE inference SET output = ?, error = ?, input_tokens = ?, output_tokens = ?, duration_ms = ?, finished_at = CURRENT_TIMESTAMP WHERE id = ? AND finished_at IS NULL")
            .bind(output).bind(error).bind(input_tokens).bind(output_tokens).bind(duration_ms).bind(id)
            .execute(&mut **transaction).await.with_context(|| format!("completing inference {id}"))?;
        ensure!(
            result.rows_affected() == 1,
            "inference {id} is missing or already completed"
        );
        Ok(())
    }

    /// Publish the live completion events for `inference_id` after its
    /// completion transaction has committed, so a receiver re-querying sees
    /// the row consistently rather than racing an in-progress transaction.
    async fn publish_inference_completed(&self, inference_id: i64, error: Option<String>) {
        match sqlx::query_as::<_, (i64, Option<i64>)>(
            "SELECT agent.world_id, inference.sequence_id \
             FROM inference JOIN agent ON agent.id = inference.agent_id WHERE inference.id = ?",
        )
        .bind(inference_id)
        .fetch_one(&self.pool)
        .await
        {
            Ok((world_id, sequence_id)) => {
                self.publish_dev_event(
                    world_id,
                    DevEvent::InferenceCompleted {
                        inference_id,
                        error,
                    },
                );
                self.publish_dev_event(
                    world_id,
                    DevEvent::Changed {
                        sequence_id,
                        inference_id: Some(inference_id),
                    },
                );
            }
            Err(lookup_error) => {
                tracing::warn!(inference_id, error = %format!("{lookup_error:#}"), "could not resolve world for live inference-completed broadcast");
            }
        }
    }

    /// Complete an inference with no other side effect: no message, no
    /// compaction obligation. Used only by detached replay (`reconstruct_inference`'s
    /// callers via the CLI `replay` command), which re-runs a historical
    /// recipe purely to compare outcomes and must not mutate live history.
    pub async fn finish_inference(
        &self,
        id: i64,
        outcome: InferenceOutcome,
        duration_ms: u64,
    ) -> Result<()> {
        let duration_ms =
            i64::try_from(duration_ms).context("inference duration exceeds SQLite range")?;
        let mut transaction = self
            .pool
            .begin()
            .await
            .context("starting inference completion")?;
        let (output, error, input_tokens, output_tokens) =
            Self::completion_columns(&mut transaction, outcome).await?;
        Self::update_inference_completion(
            &mut transaction,
            id,
            output,
            error.as_deref(),
            input_tokens,
            output_tokens,
            duration_ms,
        )
        .await?;
        transaction
            .commit()
            .await
            .context("committing inference completion")?;
        self.publish_inference_completed(id, error).await;
        Ok(())
    }

    /// Complete a normal chat inference: the outcome, its assistant message
    /// (a tool-calling turn's intermediate reply or the final reply) and any
    /// due compaction obligation, in one transaction (design.md's Inference
    /// records: "Normal chat completion writes the inference outcome, its
    /// assistant message reference and any due compaction obligation in one
    /// transaction"). The message is always the model's own reply (role
    /// Assistant) - a tool-calling turn's *result* messages are not an
    /// inference outcome and are written separately via
    /// `complete_tool_result`. Compaction is enqueued only when the
    /// outcome settled (`Content::Text`, not tool calls) and its exact
    /// next-context size (input plus generated output, with no re-tokenizing)
    /// reaches `policy.compact_before_next_input_tokens`.
    pub async fn finish_chat_inference(
        &self,
        id: i64,
        agent_id: i64,
        outcome: InferenceOutcome,
        duration_ms: u64,
        policy: CompactionPolicy<'_>,
    ) -> Result<ChatInferenceOutcome> {
        let duration_ms =
            i64::try_from(duration_ms).context("inference duration exceeds SQLite range")?;
        let mut transaction = self
            .pool
            .begin()
            .await
            .context("starting chat inference completion")?;
        let next_input_tokens = match &outcome {
            InferenceOutcome::Response(response)
                if matches!(response.content, Content::Text(_)) =>
            {
                Some(
                    response
                        .usage
                        .input_tokens
                        .checked_add(response.usage.output_tokens)
                        .context("completed inference input plus output token count overflowed")?,
                )
            }
            _ => None,
        };
        let is_error = matches!(outcome, InferenceOutcome::Error(_));
        let (output, error, input_tokens, output_tokens) =
            Self::completion_columns(&mut transaction, outcome).await?;
        Self::update_inference_completion(
            &mut transaction,
            id,
            output,
            error.as_deref(),
            input_tokens,
            output_tokens,
            duration_ms,
        )
        .await?;
        // The branch this exact inference's own history_entry already lives
        // on - not a fresh agent_history lookup, which a world_gm agent (one
        // branch per group) has no single row for.
        let branch_id: i64 = sqlx::query_scalar(
            "SELECT history_entry.branch_id FROM inference \
             JOIN history_entry ON history_entry.id = inference.history_entry_id \
             WHERE inference.id = ?",
        )
        .bind(id)
        .fetch_optional(&mut *transaction)
        .await
        .context("loading inference's own branch")?
        .with_context(|| format!("inference {id} does not exist"))?;
        if !is_error {
            // The message reuses the output content row this inference
            // already wrote via completion_columns, rather than reading it
            // back and writing a second, distinct recorded_content row for
            // the same reply (design.md: "The same output reference supplies
            // any assistant message, summary or narration delivery derived
            // from that inference").
            let message_id = Self::insert_message_with_content(
                &mut transaction,
                branch_id,
                (Role::Assistant, MessageOrigin::Conversation),
                output.expect("response outcome has content"),
                Some(id),
                None,
                None,
            )
            .await?;
            if let Some(next_input_tokens) = next_input_tokens
                && next_input_tokens >= policy.compact_before_next_input_tokens
            {
                sqlx::query(
                    "INSERT INTO pending_compaction (branch_id, position, sequence_id, cause_inference_id, next_input_tokens, sampling, model, static_segments) VALUES (?, (SELECT position FROM history_entry WHERE id = ?), ?, ?, ?, ?, ?, ?) ON CONFLICT(branch_id) DO NOTHING",
                )
                .bind(branch_id)
                .bind(message_id)
                .bind(policy.sequence_id)
                .bind(id)
                .bind(i64::try_from(next_input_tokens).context("next-input token count is too large")?)
                .bind(serde_json::to_string(policy.sampling).context("serializing compaction sampling")?)
                .bind(policy.model)
                .bind(serde_json::to_string(policy.static_segments)
                    .context("serializing static compaction context")?)
                .execute(&mut *transaction)
                .await
                .with_context(|| format!("enqueueing compaction for agent {agent_id}"))?;
            }
        }
        transaction
            .commit()
            .await
            .context("committing chat inference completion")?;
        self.publish_inference_completed(id, error).await;
        Ok(ChatInferenceOutcome {
            // The same recorded_content row this reply's own message
            // references, so a caller (e.g. GM narration, once it settles)
            // can deliver that exact reference to other recipients instead
            // of re-recording the same text - design.md: "The same output
            // reference supplies any assistant message, summary or
            // narration delivery derived from that inference." None only
            // for a failed inference, which produced no message at all.
            output_content_id: output,
        })
    }

    /// Complete a compaction inference: the outcome, its summary activation
    /// (when an older range was eligible) and the completion of
    /// its `pending_compaction` obligation, in one transaction (design.md's
    /// Inference records: "Summary completion writes the outcome, summary
    /// activation and obligation removal together").
    /// A rejected (capacity-exceeded) attempt still records its failed
    /// inference in full - "Failures are as interesting as successes when
    /// debugging prompts and model behaviour" (design.md) - but leaves the
    /// obligation itself untouched, since the caller (compaction::run) is
    /// about to retry it with a smaller recipe; only a successful summary
    /// writes the activation and retires the job.
    pub async fn finish_summary_inference(
        &self,
        id: i64,
        job: &PendingCompaction,
        outcome: InferenceOutcome,
        duration_ms: u64,
        covered_ranges: Option<&[PositionRange]>,
        (reason, retained_messages): (&str, i64),
    ) -> Result<()> {
        let duration_ms =
            i64::try_from(duration_ms).context("inference duration exceeds SQLite range")?;
        let is_error = matches!(outcome, InferenceOutcome::Error(_));
        let mut transaction = self
            .pool
            .begin()
            .await
            .context("starting summary inference completion")?;
        let (output, error, input_tokens, output_tokens) =
            Self::completion_columns(&mut transaction, outcome).await?;
        Self::update_inference_completion(
            &mut transaction,
            id,
            output,
            error.as_deref(),
            input_tokens,
            output_tokens,
            duration_ms,
        )
        .await?;
        if !is_error {
            if let Some(covered_ranges) = covered_ranges {
                let (summary_entry_id, _position) =
                    Self::append_history_entry(&mut transaction, job.branch_id, EntryKind::Summary)
                        .await?;
                sqlx::query(
                    "INSERT INTO summary (history_entry_id, covered_ranges, inference_id, reason, retained_messages) VALUES (?, ?, ?, ?, ?)",
                )
                .bind(summary_entry_id)
                .bind(serde_json::to_string(covered_ranges).context("encoding summary covered ranges")?)
                .bind(id)
                .bind(reason)
                .bind(retained_messages)
                .execute(&mut *transaction)
                .await
                .context("storing summary activation")?;
            }
            let result =
                sqlx::query("DELETE FROM pending_compaction WHERE branch_id = ? AND position = ?")
                    .bind(job.branch_id)
                    .bind(job.position)
                    .execute(&mut *transaction)
                    .await
                    .with_context(|| {
                        format!("retiring compaction job for branch {}", job.branch_id)
                    })?;
            ensure!(
                result.rows_affected() == 1,
                "compaction job for branch {} changed before it could complete",
                job.branch_id
            );
        }
        transaction
            .commit()
            .await
            .context("committing summary inference completion")?;
        self.publish_inference_completed(id, error).await;
        Ok(())
    }

    /// Retire a compaction obligation with no history outside its retained
    /// tail. No compaction occurred, so it creates no history event.
    pub async fn abandon_pending_compaction(&self, job: &PendingCompaction) -> Result<()> {
        let mut transaction = self.pool.begin().await.with_context(|| {
            format!(
                "starting compaction abandonment for branch {}",
                job.branch_id
            )
        })?;
        let result =
            sqlx::query("DELETE FROM pending_compaction WHERE branch_id = ? AND position = ?")
                .bind(job.branch_id)
                .bind(job.position)
                .execute(&mut *transaction)
                .await
                .with_context(|| format!("retiring compaction job for branch {}", job.branch_id))?;
        ensure!(
            result.rows_affected() == 1,
            "compaction job for branch {} changed before it could complete",
            job.branch_id
        );
        transaction
            .commit()
            .await
            .context("committing compaction abandonment")
    }

    pub async fn reconstruct_inference(&self, id: i64) -> Result<RecordedInference> {
        let row = sqlx::query_as::<_, InferenceRow>(
            "SELECT inference.id, inference.agent_id, history_entry.branch_id, inference.history_entry_id, inference.sequence_id, inference.invocation_id, invocation.cause_inference_id, inference.segments, inference.sampling, inference.output, inference.error, inference.input_hash, inference.input_tokens, inference.output_tokens, inference.duration_ms, inference.model \
             FROM inference JOIN history_entry ON history_entry.id = inference.history_entry_id JOIN invocation ON invocation.id = inference.invocation_id WHERE inference.id = ?",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .with_context(|| format!("loading inference {id}"))?
        .with_context(|| format!("inference {id} does not exist"))?;
        let segments: Vec<Segment> =
            serde_json::from_str(&row.segments).context("deserializing inference segments")?;
        let sampling: Sampling =
            serde_json::from_str(&row.sampling).context("deserializing recorded sampling")?;
        let request = self
            .request_for_segments(row.agent_id, &segments, sampling)
            .await
            .with_context(|| format!("reassembling inference {} from its recipe", row.id))?;
        ensure!(
            inference_input_hash(&request)? == row.input_hash,
            "inference {} reconstructed input does not match its hash",
            row.id
        );
        let outcome = match (row.output, row.error, row.input_tokens, row.output_tokens) {
            (Some(output), None, Some(input_tokens), Some(output_tokens)) => {
                let (content, reasoning) = self.recorded_content(output).await?;
                let content = match content {
                    MessageContent::Text(text) => crate::llm::Content::Text(text),
                    MessageContent::ToolCalls(calls) => crate::llm::Content::ToolCalls(calls),
                    MessageContent::ToolResult { .. } => {
                        anyhow::bail!("inference {} output is a tool result", row.id)
                    }
                };
                let response = Response {
                    content,
                    reasoning,
                    usage: Usage {
                        input_tokens: usize::try_from(input_tokens)
                            .context("negative input token count")?,
                        output_tokens: usize::try_from(output_tokens)
                            .context("negative output token count")?,
                    },
                };
                RecordedOutcome::Response(response)
            }
            (None, Some(error), None, None) => RecordedOutcome::Error(error),
            (None, None, None, None) => RecordedOutcome::Running,
            _ => anyhow::bail!(
                "inference {} has an invalid success or failure outcome",
                row.id
            ),
        };
        let prompt_evidence = self.prompt_evidence(id).await?;
        let model_prompt = self
            .reconstruct_model_prompt(id, &request, prompt_evidence.as_ref())
            .await?;
        let parent_inference_id = self
            .invocation_parent_inference_id(row.invocation_id)
            .await?;
        Ok(RecordedInference {
            id: row.id,
            agent_id: row.agent_id,
            branch_id: row.branch_id,
            history_entry_id: row.history_entry_id,
            sequence_id: row.sequence_id,
            parent_inference_id,
            cause_inference_id: row.cause_inference_id,
            segments,
            request,
            model_prompt,
            prompt_evidence,
            outcome,
            model: row.model,
            duration_ms: row
                .duration_ms
                .map(u64::try_from)
                .transpose()
                .context("recorded inference duration is negative")?,
        })
    }

    /// Every inference of a sequence, in recorded order. The sequence tree is
    /// then built from each inference's `parent_inference_id`. Consumed by the
    /// Developer Mode inspection queries.
    pub(crate) async fn sequence_inference_ids(&self, sequence_id: i64) -> Result<Vec<i64>> {
        sqlx::query_scalar("SELECT id FROM inference WHERE sequence_id = ? ORDER BY created_at, id")
            .bind(sequence_id)
            .fetch_all(&self.pool)
            .await
            .with_context(|| format!("loading inferences for sequence {sequence_id}"))
    }

    /// The chat entries an inference produced: its assistant reply and, for a
    /// tool-calling turn, each tool result. Ordered by history position.
    pub(crate) async fn inference_message_ids(
        &self,
        inference_id: i64,
    ) -> Result<Vec<InferenceMessage>> {
        sqlx::query_as::<_, InferenceMessageRow>(
            "SELECT id, role, source_tool_execution_id FROM message \
             WHERE source_inference_id = ? ORDER BY id",
        )
        .bind(inference_id)
        .fetch_all(&self.pool)
        .await
        .with_context(|| format!("loading messages produced by inference {inference_id}"))?
        .into_iter()
        .map(InferenceMessage::try_from)
        .collect()
    }

    /// Resolve one tool call's id/name/arguments from the `ToolCalls` output
    /// of the inference that produced it, at its recorded ordinal. The model
    /// call ID/name/arguments are never copied into `tool_execution` or
    /// `action`; they always resolve back to the original output (design.md:
    /// "Its model call ID/name/arguments resolve from the original output").
    async fn resolve_tool_call(
        &self,
        inference_id: i64,
        ordinal: i64,
        output: i64,
    ) -> Result<ToolCall> {
        let (content, _reasoning) = self.recorded_content(output).await?;
        Self::tool_call_at_ordinal(inference_id, ordinal, content)
    }

    fn tool_call_at_ordinal(
        inference_id: i64,
        ordinal: i64,
        content: MessageContent,
    ) -> Result<ToolCall> {
        let MessageContent::ToolCalls(calls) = content else {
            anyhow::bail!("inference {inference_id} output is not a tool call batch");
        };
        let ordinal = usize::try_from(ordinal).context("tool call ordinal is negative")?;
        calls.into_iter().nth(ordinal).with_context(|| {
            format!("inference {inference_id} has no tool call at ordinal {ordinal}")
        })
    }

    /// Actions performed by an inference's tool calls, in call order. Call order
    /// is the order of the calls in the producing inference's output; `call_id`
    /// keys each action to its exact call. `tool`/`args` resolve from the
    /// requesting tool_execution's originating call; `result` from its
    /// recorded result content.
    pub(crate) async fn inference_actions(&self, inference_id: i64) -> Result<Vec<RecordedAction>> {
        let rows = sqlx::query_as::<_, RecordedActionRow>(
            "SELECT action.id, \
                    tool_execution.inference_id AS request_inference_id, \
                    tool_execution.ordinal AS request_ordinal, \
                    inference.output AS request_output, \
                    approving.inference_id AS approval_inference_id, \
                    action.result_content_id \
             FROM action \
             LEFT JOIN tool_execution ON tool_execution.id = action.requesting_tool_execution_id \
             LEFT JOIN inference ON inference.id = tool_execution.inference_id \
             LEFT JOIN tool_execution AS approving ON approving.id = action.approving_tool_execution_id \
             WHERE tool_execution.inference_id = ? ORDER BY action.id",
        )
        .bind(inference_id)
        .fetch_all(&self.pool)
        .await
        .with_context(|| format!("loading actions from inference {inference_id}"))?;
        let mut actions = Vec::with_capacity(rows.len());
        for row in rows {
            let (call_id, tool, args) = match (
                row.request_inference_id,
                row.request_ordinal,
                row.request_output,
            ) {
                (Some(inference_id), Some(ordinal), Some(output)) => {
                    let call = self
                        .resolve_tool_call(inference_id, ordinal, output)
                        .await?;
                    (Some(call.id), call.name, call.arguments)
                }
                _ => (None, String::new(), String::new()),
            };
            let result = {
                let (content, _reasoning) = self.recorded_content(row.result_content_id).await?;
                match content {
                    MessageContent::Text(text) => text,
                    other => anyhow::bail!(
                        "action {} result content {} is not text: {other:?}",
                        row.id,
                        row.result_content_id
                    ),
                }
            };
            actions.push(RecordedAction {
                id: row.id,
                call_id,
                approval_inference_id: row.approval_inference_id,
                tool,
                args,
                result,
            });
        }
        Ok(actions)
    }

    // --- Developer Mode inspection queries ---------------------------------
    // Every method here takes the viewer id first and calls `inspecting`
    // before touching data, and constrains every subsequent query to
    // `world_id`. A record belonging to another world is rejected by name
    // rather than returned.

    /// External game triggers for a world, newest first, cursor-paged by
    /// sequence id. Each carries its trigger label and inference count.
    pub async fn world_sequences(
        &self,
        viewer_id: i64,
        world_id: i64,
        before_id: Option<i64>,
        limit: i64,
    ) -> Result<Vec<SequenceSummary>> {
        self.inspecting(viewer_id, world_id).await?;
        sqlx::query_as::<_, SequenceSummaryRow>(
            "SELECT sequence.id, sequence.trigger, sequence.started_at, \
                    COUNT(inference.id) AS inference_count \
             FROM sequence LEFT JOIN inference ON inference.sequence_id = sequence.id \
             WHERE sequence.world_id = ? AND sequence.id < ? \
             GROUP BY sequence.id ORDER BY sequence.id DESC LIMIT ?",
        )
        .bind(world_id)
        .bind(before_id.unwrap_or(i64::MAX))
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .with_context(|| format!("loading sequences for world {world_id}"))?
        .into_iter()
        .map(SequenceSummary::try_from)
        .collect()
    }

    /// The full tool-call stack for one trigger: every inference in recorded
    /// order with its parent link, agent, lifecycle and the messages and
    /// actions it produced.
    pub async fn sequence_tree(
        &self,
        viewer_id: i64,
        world_id: i64,
        sequence_id: i64,
    ) -> Result<Vec<SequenceNode>> {
        self.inspecting(viewer_id, world_id).await?;
        let in_world: Option<i64> =
            sqlx::query_scalar("SELECT 1 FROM sequence WHERE id = ? AND world_id = ?")
                .bind(sequence_id)
                .bind(world_id)
                .fetch_optional(&self.pool)
                .await
                .context("checking sequence world")?;
        ensure!(
            in_world.is_some(),
            "sequence {sequence_id} is not in world {world_id}"
        );
        let mut nodes = Vec::new();
        for id in self.sequence_inference_ids(sequence_id).await? {
            let inference = self.reconstruct_inference(id).await?;
            nodes.push(SequenceNode {
                messages: self.inference_message_ids(id).await?,
                actions: self.inference_actions(id).await?,
                inference,
            });
        }
        Ok(nodes)
    }

    /// The exact reconstructed input and output for one inference, after
    /// checking its agent belongs to `world_id`.
    pub async fn inference_evidence(
        &self,
        viewer_id: i64,
        world_id: i64,
        inference_id: i64,
    ) -> Result<RecordedInference> {
        self.inspecting(viewer_id, world_id).await?;
        let in_world: Option<i64> = sqlx::query_scalar(
            "SELECT 1 FROM inference JOIN agent ON agent.id = inference.agent_id \
             WHERE inference.id = ? AND agent.world_id = ?",
        )
        .bind(inference_id)
        .bind(world_id)
        .fetch_optional(&self.pool)
        .await
        .context("checking inference world")?;
        ensure!(
            in_world.is_some(),
            "inference {inference_id} is not in world {world_id}"
        );
        self.reconstruct_inference(inference_id).await
    }

    /// Every chat agent in a world - player, NPC and location GM - with its
    /// game role, for the inspection navigation tree. (The Storyteller is
    /// world data, not a chat agent.)
    pub async fn world_agents(&self, viewer_id: i64, world_id: i64) -> Result<Vec<AgentInfo>> {
        self.inspecting(viewer_id, world_id).await?;
        sqlx::query_as::<_, AgentInfoRow>(
            "SELECT agent.id, \
                    CASE \
                      WHEN player_character.agent_id IS NOT NULL THEN 'player' \
                      WHEN npc_agent.agent_id IS NOT NULL THEN 'npc' \
                      WHEN world_gm.agent_id IS NOT NULL THEN 'world_gm' \
                      ELSE 'unknown' END AS role, \
                    COALESCE(pc.name, npc.name) AS character_name, \
                    NULL AS location_name \
             FROM agent \
             LEFT JOIN player_character ON player_character.agent_id = agent.id \
             LEFT JOIN character AS pc ON pc.id = player_character.character_id \
             LEFT JOIN npc_agent ON npc_agent.agent_id = agent.id \
             LEFT JOIN character AS npc ON npc.id = npc_agent.character_id \
             LEFT JOIN world_gm ON world_gm.agent_id = agent.id \
             WHERE agent.world_id = ? ORDER BY agent.id",
        )
        .bind(world_id)
        .fetch_all(&self.pool)
        .await
        .with_context(|| format!("loading agents for world {world_id}"))?
        .into_iter()
        .map(AgentInfo::try_from)
        .collect()
    }

    /// One raw history page for an agent, in true recorded order: every
    /// history_entry kind - message, summary activation and failed
    /// inference - resolved and paged directly from the shared traversal, so
    /// a failure at a page boundary is neither skipped nor
    /// duplicated (design.md: "summary coverage never determines the
    /// activation marker's position"; "Message-free histories can therefore
    /// contain failed inferences"). This is the raw record, not context
    /// assembly - no summary folding.
    ///
    /// Resolve the first page's branch at the browser boundary. The optional
    /// query parameter exists only because an ordinary agent has one branch
    /// while a GM history must name one of its group heads; the returned value
    /// removes that choice from every page reader.
    pub async fn first_raw_history_cursor(
        &self,
        viewer_id: i64,
        world_id: i64,
        agent_id: i64,
        requested_branch_id: Option<i64>,
    ) -> Result<RawCursor> {
        let branch = match requested_branch_id {
            Some(branch_id) => {
                self.branch_for_viewer(viewer_id, world_id, agent_id, branch_id)
                    .await?
            }
            None => {
                self.inspecting(viewer_id, world_id).await?;
                let branch = self.agent_branch(agent_id).await?;
                ensure!(
                    branch.world_id() == world_id,
                    "agent {agent_id} is not in world {world_id}"
                );
                branch
            }
        };
        self.first_raw_cursor(branch).await
    }

    pub async fn raw_history_cursor_at(
        &self,
        viewer_id: i64,
        world_id: i64,
        agent_id: i64,
        branch_id: i64,
        entry_id: i64,
    ) -> Result<RawCursor> {
        let branch = self
            .branch_for_viewer(viewer_id, world_id, agent_id, branch_id)
            .await?;
        self.raw_cursor_at(branch, entry_id).await
    }

    /// Validate a browser-returned raw cursor before it can select a page.
    pub async fn resume_raw_history_cursor(
        &self,
        viewer_id: i64,
        world_id: i64,
        agent_id: i64,
        cursor: RawHistoryCursor,
    ) -> Result<RawCursor> {
        let branch = self
            .branch_for_viewer(viewer_id, world_id, agent_id, cursor.branch_id)
            .await?;
        self.resume_raw_cursor(
            branch,
            cursor.cut,
            cursor.next_branch_id,
            cursor.next_position,
        )
        .await
    }

    /// Page a raw history from a cursor whose ownership, immutable cut and
    /// next key have already been validated.
    pub async fn agent_raw_history(
        &self,
        viewer_id: i64,
        cursor: RawCursor,
        limit: i64,
    ) -> Result<(Vec<RawHistoryEntry>, Option<RawHistoryCursor>)> {
        self.inspecting(viewer_id, cursor.branch().world_id())
            .await?;
        let (page, next) = self.raw_page(cursor, limit).await?;
        let mut entries = Vec::with_capacity(page.len());
        for entry in page {
            entries.push(self.resolve_raw_history_entry(entry).await?);
        }
        let next_cursor = next.map(|(next_branch_id, next_position)| RawHistoryCursor {
            branch_id: cursor.branch().branch_id(),
            cut: cursor.cut(),
            next_branch_id,
            next_position,
        });
        Ok((entries, next_cursor))
    }

    async fn resolve_raw_history_entry(
        &self,
        entry: history::HistoryEntry,
    ) -> Result<RawHistoryEntry> {
        Ok(match entry.kind {
            history::EntryKind::Message => {
                let row: RawMessageRow = sqlx::query_as(
                    "SELECT id, role, origin, content, source_inference_id, source_tool_execution_id FROM message WHERE id = ?",
                )
                .bind(entry.id)
                .fetch_one(&self.pool)
                .await
                .with_context(|| format!("loading raw history message {}", entry.id))?;
                let (content, reasoning) = self.recorded_content(row.content).await?;
                RawHistoryEntry::Message {
                    id: row.id,
                    role: serde_json::from_str(&row.role).context("decoding raw message role")?,
                    origin: row
                        .origin
                        .parse::<MessageOrigin>()
                        .map_err(anyhow::Error::msg)
                        .context("decoding raw message origin")?,
                    content,
                    reasoning,
                    source_inference_id: row.source_inference_id,
                    answers_tool_execution_id: row.source_tool_execution_id,
                }
            }
            history::EntryKind::Summary => {
                let row: RawSummaryRow = sqlx::query_as(
                    "SELECT summary.id, summary.covered_ranges, summary.inference_id, summary.reason, summary.retained_messages, \
                            json_extract(summary.body, '$.content.value') AS content \
                     FROM summary_content AS summary WHERE summary.history_entry_id = ?",
                )
                .bind(entry.id)
                .fetch_one(&self.pool)
                .await
                .with_context(|| format!("loading raw history summary {}", entry.id))?;
                RawHistoryEntry::Summary {
                    id: row.id,
                    covered_ranges: serde_json::from_str(&row.covered_ranges)
                        .context("decoding raw history summary covered ranges")?,
                    inference_id: row.inference_id,
                    text: row.content,
                    reason: row.reason,
                    retained_messages: row.retained_messages,
                }
            }
            history::EntryKind::Inference => {
                let (inference_id, error): (i64, Option<String>) =
                    sqlx::query_as("SELECT id, error FROM inference WHERE history_entry_id = ?")
                        .bind(entry.id)
                        .fetch_one(&self.pool)
                        .await
                        .with_context(|| format!("loading raw history inference {}", entry.id))?;
                RawHistoryEntry::Inference {
                    id: inference_id,
                    history_entry_id: entry.id,
                    error,
                }
            }
        })
    }

    /// A world's current game objects and notes, for the inspection object
    /// navigator. This is present state, never historical context - it is
    /// deliberately not a `Segment` and is never assembled into a request.
    pub async fn world_objects(
        &self,
        viewer_id: i64,
        world_id: i64,
        query: WorldObjectsQuery,
    ) -> Result<WorldObjects> {
        self.inspecting(viewer_id, world_id).await?;
        let world: WorldObjectRow = sqlx::query_as(
            "SELECT name, time, initial_prompt, storyteller_summary, gm_notes, storyteller_notes \
             FROM world WHERE id = ?",
        )
        .bind(world_id)
        .fetch_one(&self.pool)
        .await
        .with_context(|| format!("loading world object {world_id}"))?;
        let object_type = query.object_type.map(|value| value.as_str().to_owned());
        let search = query.search.filter(|value| !value.is_empty());
        let after_type = query
            .after
            .as_ref()
            .map(|cursor| cursor.object_type.as_str());
        let after_id = query.after.as_ref().map(|cursor| cursor.id);
        let rows = sqlx::query_as::<_, ObjectIndexRow>(
            "WITH object_index AS ( \
                SELECT 'character' AS object_type, id, name, description, gm_notes, storyteller_notes FROM character WHERE world_id = ? \
                UNION ALL SELECT 'connection', connection.id, from_location.name || ' <-> ' || to_location.name, connection.description, connection.gm_notes, connection.storyteller_notes FROM connection JOIN location AS from_location ON from_location.id = connection.from_location_id JOIN location AS to_location ON to_location.id = connection.to_location_id WHERE connection.world_id = ? \
                UNION ALL SELECT 'group', id, name, description, gm_notes, storyteller_notes FROM \"group\" WHERE world_id = ? \
                UNION ALL SELECT 'item', id, name, description, gm_notes, storyteller_notes FROM item WHERE world_id = ? \
                UNION ALL SELECT 'item_type', id, name, description, gm_notes, storyteller_notes FROM item_type WHERE world_id = ? \
                UNION ALL SELECT 'location', id, name, description, gm_notes, storyteller_notes FROM location WHERE world_id = ? \
             ) SELECT object_type, id, name, description, gm_notes, storyteller_notes FROM object_index \
             WHERE (? IS NULL OR object_type = ?) \
               AND (? IS NULL OR id = ?) \
               AND (? IS NULL OR name LIKE '%' || ? || '%' OR object_type LIKE '%' || ? || '%' OR CAST(id AS TEXT) LIKE '%' || ? || '%') \
               AND (? IS NULL OR object_type > ? OR (object_type = ? AND id > ?)) \
             ORDER BY object_type, id LIMIT ?",
        )
        .bind(world_id)
        .bind(world_id)
        .bind(world_id)
        .bind(world_id)
        .bind(world_id)
        .bind(world_id)
        .bind(&object_type)
        .bind(&object_type)
        .bind(query.object_id)
        .bind(query.object_id)
        .bind(&search)
        .bind(&search)
        .bind(&search)
        .bind(&search)
        .bind(after_type)
        .bind(after_type)
        .bind(after_type)
        .bind(after_id)
        .bind(query.limit + 1)
        .fetch_all(&self.pool)
        .await
        .context("loading paged world objects")?;
        let has_more = rows.len() as i64 > query.limit;
        let rows = rows
            .into_iter()
            .take(query.limit as usize)
            .collect::<Vec<_>>();
        let cursor = has_more.then(|| {
            let last = rows.last().expect("a page with more rows has a last row");
            WorldObjectCursor {
                object_type: ObjectType::parse(&last.object_type)
                    .expect("world object index only returns known object types"),
                id: last.id,
            }
        });
        let mut objects = Vec::with_capacity(rows.len());
        for row in rows {
            let object_type = ObjectType::parse(&row.object_type)
                .expect("world object index only returns known object types");
            let (fields, relationships) = self
                .world_object_details(world_id, object_type, row.id)
                .await?;
            objects.push(GameObject {
                object_type,
                id: row.id,
                name: row.name,
                description: row.description,
                gm_notes: row.gm_notes,
                storyteller_notes: row.storyteller_notes,
                fields,
                relationships,
            });
        }
        Ok(WorldObjects {
            name: world.name,
            time: world.time,
            initial_prompt: world.initial_prompt,
            storyteller_summary: world.storyteller_summary,
            gm_notes: world.gm_notes,
            storyteller_notes: world.storyteller_notes,
            objects,
            cursor,
        })
    }

    async fn world_object_details(
        &self,
        world_id: i64,
        object_type: ObjectType,
        id: i64,
    ) -> Result<(Vec<ObjectField>, Vec<ObjectLink>)> {
        let links = |sql: &'static str| async move {
            let rows = sqlx::query_as::<_, ObjectLinkRow>(sql)
                .bind(id)
                .fetch_all(&self.pool)
                .await
                .context("loading world object relationships")?;
            rows.into_iter()
                .map(|row| {
                    Ok::<ObjectLink, anyhow::Error>(ObjectLink {
                        object_type: ObjectType::parse(&row.object_type).with_context(|| {
                            format!("unknown related object type {}", row.object_type)
                        })?,
                        id: row.id,
                        label: row.label,
                    })
                })
                .collect::<Result<Vec<_>>>()
        };
        match object_type {
            ObjectType::Character => Ok((Vec::new(), links(
                "SELECT 'group' AS object_type, \"group\".id, \"group\".name AS label FROM character_group JOIN \"group\" ON \"group\".id = character_group.group_id WHERE character_group.character_id = ?1",
            ).await?)),
            ObjectType::Connection => {
                let travel_time: i64 = sqlx::query_scalar("SELECT travel_time FROM connection WHERE id = ? AND world_id = ?")
                    .bind(id).bind(world_id).fetch_one(&self.pool).await
                    .with_context(|| format!("loading connection {id}"))?;
                Ok((vec![ObjectField { label: "travel time".to_owned(), value: travel_time.to_string() }], links(
                    "SELECT 'location' AS object_type, location.id, location.name AS label FROM connection JOIN location ON location.id IN (connection.from_location_id, connection.to_location_id) WHERE connection.id = ?1 ORDER BY location.id",
                ).await?))
            }
            ObjectType::Group => Ok((Vec::new(), links(
                "SELECT 'location' AS object_type, location.id, location.name AS label FROM \"group\" JOIN location ON location.id = \"group\".location_id WHERE \"group\".id = ?1 UNION ALL SELECT 'character', character.id, character.name FROM character_group JOIN character ON character.id = character_group.character_id WHERE character_group.group_id = ?1 ORDER BY object_type, id",
            ).await?)),
            ObjectType::Item => Ok((Vec::new(), links(
                "SELECT 'item_type' AS object_type, item_type.id, item_type.name AS label FROM item JOIN item_type ON item_type.id = item.item_type_id WHERE item.id = ?1 UNION ALL SELECT 'location', location.id, location.name FROM item_location JOIN location ON location.id = item_location.location_id WHERE item_location.item_id = ?1 UNION ALL SELECT 'character', character.id, character.name FROM item_character JOIN character ON character.id = item_character.character_id WHERE item_character.item_id = ?1 ORDER BY object_type, id",
            ).await?)),
            ObjectType::ItemType => Ok((Vec::new(), links(
                "SELECT 'item' AS object_type, id, name AS label FROM item WHERE item_type_id = ?1 ORDER BY id",
            ).await?)),
            ObjectType::Location => Ok((Vec::new(), links(
                "SELECT 'group' AS object_type, \"group\".id, \"group\".name AS label FROM \"group\" WHERE location_id = ?1 UNION ALL SELECT 'item', item.id, item.name FROM item_location JOIN item ON item.id = item_location.item_id WHERE item_location.location_id = ?1 UNION ALL SELECT 'connection', connection.id, from_location.name || ' <-> ' || to_location.name FROM connection JOIN location AS from_location ON from_location.id = connection.from_location_id JOIN location AS to_location ON to_location.id = connection.to_location_id WHERE connection.from_location_id = ?1 OR connection.to_location_id = ?1 ORDER BY object_type, id",
            ).await?)),
        }
    }

    #[cfg(test)]
    pub(crate) async fn message_source_inference_for_test(
        &self,
        agent_id: i64,
        position: i64,
    ) -> Option<i64> {
        let branch_id = self.branch_for_agent(agent_id).await.unwrap();
        sqlx::query_scalar(
            "SELECT message.source_inference_id FROM message \
             JOIN history_entry ON history_entry.id = message.id \
             WHERE history_entry.branch_id = ? AND history_entry.position = ?",
        )
        .bind(branch_id)
        .bind(position)
        .fetch_one(&self.pool)
        .await
        .expect("message row should exist")
    }

    #[cfg(test)]
    pub(crate) async fn inference_count(&self) -> Result<i64> {
        sqlx::query_scalar("SELECT COUNT(*) FROM inference")
            .fetch_one(&self.pool)
            .await
            .context("counting inference rows")
    }

    #[cfg(test)]
    pub(crate) async fn action_counts(&self) -> Result<(i64, i64)> {
        let actions = sqlx::query_scalar("SELECT COUNT(*) FROM action")
            .fetch_one(&self.pool)
            .await
            .context("counting resolved actions")?;
        let pending = sqlx::query_scalar("SELECT COUNT(*) FROM pending_action")
            .fetch_one(&self.pool)
            .await
            .context("counting pending actions")?;
        Ok((actions, pending))
    }
}

#[cfg(test)]
pub(crate) async fn record_tool_call_for_test(
    store: &Store,
    agent_id: i64,
    branch_id: Option<i64>,
    sequence_id: Option<i64>,
    call_id: &str,
    tool: &str,
    arguments: &str,
) -> i64 {
    let request = Request {
        messages: vec![],
        tools: vec![],
        sampling: Sampling {
            temperature: 0.0,
            ..Default::default()
        },
    };
    let sequence_id = match sequence_id {
        Some(id) => id,
        None => {
            store
                .begin_sequence(
                    store.world_id_for_agent(agent_id).await.unwrap(),
                    "test tool call",
                )
                .await
                .unwrap()
                .id
        }
    };
    let invocation_id = store
        .start_invocation(Some(sequence_id), agent_id, branch_id, None, None)
        .await
        .unwrap();
    let inference_id = store
        .start_inference(InferenceInput {
            agent_id,
            sequence_id: Some(sequence_id),
            invocation_id,
            segments: &[],
            request: &request,
            model: "scripted",
        })
        .await
        .unwrap();
    store
        .finish_inference(
            inference_id,
            InferenceOutcome::Response(Response {
                content: Content::ToolCalls(vec![ToolCall {
                    id: call_id.to_string(),
                    name: tool.to_string(),
                    arguments: arguments.to_string(),
                }]),
                reasoning: String::new(),
                usage: Usage {
                    input_tokens: 1,
                    output_tokens: 1,
                },
            }),
            0,
        )
        .await
        .unwrap();
    store.create_tool_executions(inference_id, 1).await.unwrap()[0]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::{Content, ToolCall};

    async fn record_inference(
        store: &Store,
        agent_id: i64,
        segments: &[Segment],
        request: &Request,
        outcome: InferenceOutcome,
        model: &str,
        duration_ms: u64,
    ) -> Result<i64> {
        let sequence = store
            .begin_sequence(store.world_id_for_agent(agent_id).await?, "test inference")
            .await?;
        let invocation_id = store
            .start_invocation(Some(sequence.id), agent_id, None, None, None)
            .await?;
        let inference_id = store
            .start_inference(InferenceInput {
                agent_id,
                sequence_id: Some(sequence.id),
                invocation_id,
                segments,
                request,
                model,
            })
            .await?;
        store
            .finish_inference(inference_id, outcome, duration_ms)
            .await?;
        Ok(inference_id)
    }

    async fn complete_summary(
        store: &Store,
        agent_id: i64,
        segments: &[Segment],
        request: &Request,
        content: &str,
        covers_through_position: i64,
    ) -> Result<i64> {
        let branch_id = store.agent_branch(agent_id).await?.branch_id();
        let sampling = Sampling::default();
        store
            .enqueue_capacity_compaction(
                agent_id,
                Some(branch_id),
                1,
                CompactionPolicy {
                    sequence_id: None,
                    compact_before_next_input_tokens: 1,
                    sampling: &sampling,
                    model: "test-model",
                    static_segments: &[],
                },
            )
            .await?;
        let job = store
            .pending_compaction_for_branch(branch_id)
            .await?
            .context("test summary did not queue compaction")?;
        let sequence = store
            .begin_sequence(store.world_id_for_agent(agent_id).await?, "test summary")
            .await?;
        let invocation_id = store
            .start_invocation(Some(sequence.id), agent_id, Some(branch_id), None, None)
            .await?;
        let inference_id = store
            .start_inference(InferenceInput {
                agent_id,
                sequence_id: Some(sequence.id),
                invocation_id,
                segments,
                request,
                model: "test-model",
            })
            .await?;
        store
            .finish_summary_inference(
                inference_id,
                &job,
                InferenceOutcome::Response(Response {
                    content: Content::Text(content.to_owned()),
                    ..response()
                }),
                1,
                Some(&[PositionRange {
                    branch_id,
                    first_position: 0,
                    last_position: covers_through_position,
                }]),
                ("threshold", 2),
            )
            .await?;
        store
            .latest_summary_for_branch(branch_id)
            .await?
            .context("completed test summary was not activated")
            .map(|summary| summary.id)
    }

    fn database() -> super::TestDatabase {
        super::TestDatabase::new("store-test")
    }

    #[tokio::test]
    async fn compaction_consolidation_migration_replaces_legacy_notice_schema() {
        let db = database();
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(db.path())
                    .create_if_missing(true)
                    .foreign_keys(true),
            )
            .await
            .unwrap();
        sqlx::raw_sql(include_str!("../migrations/0001_recording.sql"))
            .execute(&pool)
            .await
            .unwrap();

        sqlx::raw_sql(include_str!(
            "../migrations/0002_consolidate_compaction.sql"
        ))
        .execute(&pool)
        .await
        .unwrap();

        let legacy_table: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'chat_notice'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(
            legacy_table, 0,
            "the migration removes the second event table"
        );
        let summary_columns: Vec<String> =
            sqlx::query_scalar("SELECT name FROM pragma_table_info('summary') ORDER BY cid")
                .fetch_all(&pool)
                .await
                .unwrap();
        assert!(
            summary_columns.ends_with(&["reason".to_owned(), "retained_messages".to_owned()]),
            "the one summary record owns compaction metadata: {summary_columns:?}"
        );
        let view_columns: Vec<String> = sqlx::query_scalar(
            "SELECT name FROM pragma_table_info('summary_content') ORDER BY cid",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        assert!(
            view_columns.contains(&"reason".to_owned())
                && view_columns.contains(&"retained_messages".to_owned()),
            "summary readers must see the migrated metadata: {view_columns:?}"
        );
    }

    fn response() -> Response {
        Response {
            content: Content::Text("A locked chest.".to_string()),
            reasoning: String::new(),
            usage: Usage {
                input_tokens: 12,
                output_tokens: 5,
            },
        }
    }

    fn test_scenario(name: &str) -> Scenario {
        Scenario {
            name: name.into(),
            initial_prompt: String::new(),
            storyteller_summary: String::new(),
            notes: Notes::default(),
            starting_location: "start".into(),
            starting_group: "start".into(),
            locations: vec![crate::scenario::Location {
                name: "start".into(),
                kind: "test".into(),
                description: String::new(),
                notes: Notes::default(),
            }],
            connections: vec![],
            groups: vec![crate::scenario::Group {
                name: "start".into(),
                location: "start".into(),
                description: String::new(),
                notes: Notes::default(),
            }],
            npcs: vec![],
            item_types: vec![],
            spell_types: vec![],
            items: vec![],
        }
    }

    async fn store_with_history() -> (Store, super::TestDatabase, i64, Vec<Segment>, Request) {
        let db = database();
        let store = Store::open(db.path()).await.expect("store should open");
        let world = store
            .create_world("test world")
            .await
            .expect("world should persist");
        let agent = store
            .create_agent(world)
            .await
            .expect("agent should persist");
        store
            .append_message(
                agent,
                &Message::text(Role::User, "What is beneath the floorboards?"),
            )
            .await
            .expect("user message should persist");
        store
            .append_message(
                agent,
                &Message::assistant(
                    Content::Text("A locked chest.".to_string()),
                    "scratch work".to_string(),
                ),
            )
            .await
            .expect("assistant message should persist");
        let prompt = store
            .store_prompt_text("You are a careful guide.")
            .await
            .expect("prompt should persist");
        let segments = vec![
            Segment::Text {
                text: prompt,
                role: Role::System,
            },
            store
                .live_segments(store.agent_branch(agent).await.unwrap())
                .await
                .unwrap()
                .pop()
                .unwrap(),
        ];
        let request = store
            .request_for_segments(
                agent,
                &segments,
                Sampling {
                    temperature: 0.7,
                    ..Default::default()
                },
            )
            .await
            .expect("request should assemble");
        let branch_id = store.branch_id_for_agent(agent).await.unwrap();
        let stored_reasoning: String = sqlx::query_scalar(
            "SELECT json_extract(recorded_content.body, '$.reasoning') FROM message \
             JOIN history_entry ON history_entry.id = message.id \
             JOIN recorded_content ON recorded_content.id = message.content \
             WHERE history_entry.branch_id = ? AND history_entry.position = 1",
        )
        .bind(branch_id)
        .fetch_one(&store.pool)
        .await
        .expect("assistant reasoning should persist");
        assert_eq!(stored_reasoning, "scratch work");
        assert!(request.messages[1].reasoning.is_empty());
        (store, db, agent, segments, request)
    }

    #[tokio::test]
    async fn membership_resolves_only_the_authenticated_users_player_history() {
        let db = database();
        let store = Store::open(db.path()).await.unwrap();
        let alex_one = store
            .find_or_create_user("alex.one@example.test", "Alex")
            .await
            .unwrap();
        let alex_two = store
            .find_or_create_user("alex.two@example.test", "Alex")
            .await
            .unwrap();
        let alex_one_returning = store
            .find_or_create_user("alex.one@example.test", "Not Alex")
            .await
            .unwrap();
        assert_ne!(
            alex_one.id, alex_two.id,
            "email, not display name, is identity"
        );
        assert_eq!(
            alex_one_returning.display_name, "Alex",
            "a later login must not overwrite a player-selected display name"
        );
        let renamed = store
            .rename_user(alex_one.id, "Alex the Bold")
            .await
            .unwrap();
        assert_eq!(renamed.email, "alex.one@example.test");
        assert_eq!(renamed.display_name, "Alex the Bold");
        assert_eq!(
            store
                .find_or_create_user("alex.one@example.test", "Different Google name")
                .await
                .unwrap()
                .display_name,
            "Alex the Bold",
            "a player-selected display name survives later OAuth logins"
        );

        let first = store
            .install_scenario(&alex_one, &test_scenario("first world"))
            .await
            .unwrap();
        let second = store
            .install_scenario(&alex_two, &test_scenario("second world"))
            .await
            .unwrap();
        let other_world_id = second.world_id;

        assert_eq!(
            store
                .active_player_character(alex_one.id, first.world_id, first.member.character_id)
                .await
                .unwrap(),
            Some(first.member.clone())
        );
        assert_eq!(
            store
                .active_player_character(alex_two.id, second.world_id, second.member.character_id)
                .await
                .unwrap(),
            Some(second.member.clone())
        );
        assert_eq!(
            store
                .active_player_character(alex_one.id, other_world_id, second.member.character_id)
                .await
                .unwrap(),
            None,
            "a world id alone must never select another user's player agent"
        );
    }

    #[tokio::test]
    async fn active_member_can_create_independent_player_characters() {
        let db = database();
        let store = Store::open(db.path()).await.unwrap();
        let owner = store
            .find_or_create_user("owner@example.test", "Owner")
            .await
            .unwrap();
        let installed = store
            .install_scenario(&owner, &test_scenario("two characters"))
            .await
            .unwrap();
        let second = store
            .create_player_character(owner.id, installed.world_id)
            .await
            .unwrap();
        assert_ne!(installed.member.character_id, second.character_id);
        assert_ne!(installed.member.agent_id, second.agent_id);
        let characters: Vec<(String, i64)> = sqlx::query_as(
            "SELECT character.name, character.tool_id FROM player_character \
             JOIN character ON character.id = player_character.character_id \
             WHERE player_character.member_id = ? ORDER BY character.id",
        )
        .bind(installed.member.member_id)
        .fetch_all(&store.pool)
        .await
        .unwrap();
        assert_eq!(characters.len(), 2);
        assert!(
            characters
                .iter()
                .all(|(name, tool_id)| { name == &format!("Adventurer{tool_id}") })
        );
        assert_eq!(
            store.player_chat(&second).await.unwrap(),
            vec![],
            "a new character never inherits another character's history"
        );
        store
            .append_message(
                installed.member.agent_id,
                &Message::text(Role::User, "First character speaks."),
            )
            .await
            .unwrap();
        assert_eq!(store.player_chat(&second).await.unwrap(), vec![]);
        let members = store.world_members(installed.world_id).await.unwrap();
        assert_eq!(members.len(), 1);
        assert_eq!(members[0].characters.len(), 2);
        let gm_history = store.player_group_history(&installed.member).await.unwrap();
        let gm_scene = store
            .gm_scene(installed.world_id, gm_history.branch_id)
            .await
            .unwrap();
        assert_eq!(
            gm_scene
                .players
                .iter()
                .map(|player| player.name.as_str())
                .collect::<Vec<_>>(),
            characters
                .iter()
                .map(|(name, _)| name.as_str())
                .collect::<Vec<_>>(),
            "the GM packet must identify every player character at its location"
        );
        assert!(
            gm_scene
                .players
                .iter()
                .all(|player| player.sheet == serde_json::json!({}))
        );
        assert_eq!(
            store
                .active_player_character(owner.id, installed.world_id, second.character_id)
                .await
                .unwrap(),
            Some(second)
        );
    }

    #[tokio::test]
    async fn player_chat_reloads_only_renderable_text_from_its_active_history() {
        let db = database();
        let store = Store::open(db.path()).await.unwrap();
        let owner = store
            .find_or_create_user("history@example.test", "History")
            .await
            .unwrap();
        let installed = store
            .install_scenario(&owner, &test_scenario("history"))
            .await
            .unwrap();
        store
            .append_message(
                installed.member.agent_id,
                &Message::text(Role::User, "Look around."),
            )
            .await
            .unwrap();
        let branch = store.agent_branch(installed.member.agent_id).await.unwrap();
        let sequence = store
            .begin_sequence(installed.world_id, "internal instruction")
            .await
            .unwrap();
        store
            .append_instruction(
                branch,
                &Message::text(Role::User, "Speak directly to the player."),
                sequence.id,
            )
            .await
            .unwrap();
        store
            .append_message(
                installed.member.agent_id,
                &Message::assistant(
                    Content::ToolCalls(vec![ToolCall {
                        id: "call-1".into(),
                        name: "look".into(),
                        arguments: r#"{"description":"around"}"#.into(),
                    }]),
                    String::new(),
                ),
            )
            .await
            .unwrap();
        store
            .append_message(
                installed.member.agent_id,
                &Message::tool_result("call-1".into(), "GM result".into()),
            )
            .await
            .unwrap();
        store
            .append_message(
                installed.member.agent_id,
                &Message::assistant(Content::Text("The hut is quiet.".into()), String::new()),
            )
            .await
            .unwrap();
        store
            .append_message(
                installed.member.agent_id,
                &Message::text(Role::Narration, "Toma watches from the doorway."),
            )
            .await
            .unwrap();

        let player_chat = store.player_chat(&installed.member).await.unwrap();
        assert_eq!(
            player_chat
                .iter()
                .map(|entry| (&entry.role, entry.text.as_str()))
                .collect::<Vec<_>>(),
            vec![
                (&Role::User, "Look around."),
                (&Role::Assistant, "The hut is quiet."),
                (&Role::Narration, "Toma watches from the doorway."),
            ],
            "reloadable player chat must not leak raw tool syntax"
        );
        let delta = store
            .player_chat_after(&installed.member, Some(player_chat[0].id))
            .await
            .unwrap();
        assert_eq!(
            delta
                .iter()
                .map(|entry| (&entry.role, entry.text.as_str()))
                .collect::<Vec<_>>(),
            vec![
                (&Role::Assistant, "The hut is quiet."),
                (&Role::Narration, "Toma watches from the doorway."),
            ],
            "a page snapshot cursor must receive each later visible entry once"
        );
    }

    #[tokio::test]
    async fn invitations_only_grant_the_invited_world_and_preserve_one_player_history() {
        let db = database();
        let store = Store::open(db.path()).await.unwrap();
        let owner = store
            .find_or_create_user("owner@example.test", "Owner")
            .await
            .unwrap();
        let invited = store
            .find_or_create_user("invited@example.test", "Invited")
            .await
            .unwrap();
        let outsider = store
            .find_or_create_user("outsider@example.test", "Outsider")
            .await
            .unwrap();
        let world = store
            .install_scenario(&owner, &test_scenario("invited world"))
            .await
            .unwrap();
        let other_world = store
            .install_scenario(&owner, &test_scenario("other world"))
            .await
            .unwrap();
        let invite = store
            .create_invitation(owner.id, world.world_id, Some(1))
            .await
            .unwrap();
        let member = store
            .accept_invitation(&invited, &invite.token)
            .await
            .unwrap();
        assert_eq!(member.world_id, world.world_id);
        assert_eq!(
            store
                .active_player_character(
                    invited.id,
                    other_world.world_id,
                    other_world.member.character_id
                )
                .await
                .unwrap(),
            None,
            "an invite must not select or expose another world"
        );
        assert!(
            store
                .accept_invitation(&outsider, &invite.token)
                .await
                .is_err(),
            "the configured slot limit must be enforced when two people race to join"
        );
        store
            .remove_member(owner.id, world.world_id, invited.id)
            .await
            .unwrap();
        assert_eq!(
            store
                .active_player_character(invited.id, world.world_id, member.character_id)
                .await
                .unwrap(),
            None,
            "a removed member must no longer resolve to a playable history"
        );
        let members = store.world_members(world.world_id).await.unwrap();
        assert_eq!(
            members
                .iter()
                .map(|member| (
                    member.user_id,
                    member.display_name.as_str(),
                    member.access.as_str(),
                ))
                .collect::<Vec<_>>(),
            vec![
                (owner.id, "Owner", "active"),
                (invited.id, "Invited", "removed")
            ],
            "the detail page must retain the removed membership and its character"
        );
        let names = members
            .iter()
            .map(|member| member.characters[0].name.as_str())
            .collect::<Vec<_>>();
        assert!(names.iter().all(|name| {
            name.strip_prefix("Adventurer")
                .is_some_and(|suffix| suffix.parse::<i64>().is_ok())
        }));
        assert_ne!(names[0], names[1]);
        let restored = store
            .accept_invitation(&invited, &invite.token)
            .await
            .unwrap();
        assert_eq!(
            restored, member,
            "rejoining restores the original player history"
        );
        assert!(
            store
                .remove_member(owner.id, world.world_id, owner.id)
                .await
                .is_err(),
            "the owner relationship cannot be converted into a removed membership"
        );
        store
            .revoke_invitation(owner.id, world.world_id, &invite.token)
            .await
            .unwrap();
        assert!(
            store
                .accept_invitation(&outsider, &invite.token)
                .await
                .is_err(),
            "a revoked link must not grant access"
        );
    }

    #[tokio::test]
    async fn character_creation_rolls_are_stored_and_ready_requires_them() {
        let db = database();
        let store = Store::open(db.path()).await.unwrap();
        let owner = store
            .find_or_create_user("roller@example.test", "Roller")
            .await
            .unwrap();
        let installed = store
            .install_scenario(&owner, &test_scenario("creation"))
            .await
            .unwrap();
        let sequence = store
            .begin_sequence(installed.world_id, "creation test")
            .await
            .unwrap();
        assert!(
            store
                .ready_to_begin(&installed.member, sequence.id, None)
                .await
                .is_err(),
            "a blank Adventurer must not skip the required rolls"
        );
        let hp = store
            .roll_hit_protection(&installed.member, sequence.id, None)
            .await
            .unwrap();
        assert!((1..=6).contains(&hp));
        assert_eq!(
            store
                .roll_hit_protection(&installed.member, sequence.id, None)
                .await
                .unwrap(),
            hp,
            "repeating a tool call must not create a player-selectable reroll"
        );
        let attributes = store
            .roll_attributes(&installed.member, sequence.id, None)
            .await
            .unwrap();
        assert!(
            [attributes.0, attributes.1, attributes.2]
                .iter()
                .all(|value| (3..=18).contains(value))
        );
        assert_eq!(
            store
                .roll_attributes(&installed.member, sequence.id, None)
                .await
                .unwrap(),
            attributes,
            "a second attribute call returns the same character sheet"
        );
        store
            .ready_to_begin(&installed.member, sequence.id, None)
            .await
            .unwrap();
        assert!(store.is_ready_to_begin(&installed.member).await.unwrap());
    }

    #[tokio::test]
    async fn installs_bread_thief_as_a_complete_playable_relationship_graph() {
        let db = database();
        let store = Store::open(db.path()).await.unwrap();
        let owner = store
            .find_or_create_user("warden@example.test", "Warden")
            .await
            .unwrap();
        let scenario = Scenario::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/scenarios/bread_thief.json"
        ))
        .unwrap();
        let installed = store.install_scenario(&owner, &scenario).await.unwrap();

        assert!(installed.character_handle.starts_with("char"));
        assert_eq!(installed.character_handle.len(), 6);
        let (active_character, active_tool_id): (String, i64) = sqlx::query_as(
            "SELECT character.name, character.tool_id FROM player_character \
             JOIN character ON character.id = player_character.character_id \
             WHERE player_character.member_id = ?",
        )
        .bind(installed.member.member_id)
        .fetch_one(&store.pool)
        .await
        .unwrap();
        assert_eq!(active_character, format!("Adventurer{active_tool_id}"));
        let groups: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM \"group\" WHERE world_id = ?")
            .bind(installed.world_id)
            .fetch_one(&store.pool)
            .await
            .unwrap();
        let npc_agents: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM npc_agent \
             JOIN character ON character.id = npc_agent.character_id WHERE character.world_id = ?",
        )
        .bind(installed.world_id)
        .fetch_one(&store.pool)
        .await
        .unwrap();
        let placed_items: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM item_location \
             JOIN item ON item.id = item_location.item_id WHERE item.world_id = ?",
        )
        .bind(installed.world_id)
        .fetch_one(&store.pool)
        .await
        .unwrap();
        assert_eq!(groups, 1, "Bread Thief declares one starting group");
        assert_eq!(npc_agents, 2, "Mara and Toma have distinct agent histories");
        assert_eq!(
            placed_items, 2,
            "the flour and cache can be found in the world"
        );
        let player_scene = store.player_scene(&installed.member).await.unwrap();
        assert_eq!(player_scene.character_name, active_character);
        assert_eq!(player_scene.location_name, "charcoal hut");
        assert!(player_scene.inventory.is_empty());
        assert_eq!(
            player_scene
                .items
                .iter()
                .map(|item| item.name.as_str())
                .collect::<Vec<_>>(),
            ["flour sack", "emergency cache"],
            "the player context must be derived from their actual location"
        );
        assert_eq!(
            player_scene
                .npcs
                .iter()
                .map(|npc| npc.name.as_str())
                .collect::<Vec<_>>(),
            ["Mara", "Toma"]
        );
        let gm_branch_id: i64 =
            sqlx::query_scalar("SELECT branch_id FROM \"group\" WHERE world_id = ?")
                .bind(installed.world_id)
                .fetch_one(&store.pool)
                .await
                .unwrap();
        let gm_scene = store
            .gm_scene(installed.world_id, gm_branch_id)
            .await
            .unwrap();
        assert_eq!(gm_scene.location_name, player_scene.location_name);
        assert!(gm_scene.gm_notes.get("interior").is_some());
        assert_eq!(
            gm_scene.group_description,
            "Everyone present at the charcoal hut: the Adventurer, Mara and Toma."
        );
        assert!(
            gm_scene.group_gm_notes.get("grouping").is_some(),
            "the group's own GM notes must be distinct from the location's"
        );
        let toma = gm_scene.npcs.iter().find(|npc| npc.name == "Toma").unwrap();
        assert!(toma.motive.contains("imprisonment"));
        assert!(toma.gm_notes.get("beliefs").is_some());
        assert_eq!(
            store.export_scenario(installed.world_id).await.unwrap(),
            scenario,
            "exported JSON must recreate the same scenario graph in a fresh world"
        );
    }

    #[tokio::test]
    async fn pending_actions_keep_the_validated_call_for_its_group_gm() {
        let db = database();
        let store = Store::open(db.path()).await.unwrap();
        let owner = store
            .find_or_create_user("warden@example.test", "Warden")
            .await
            .unwrap();
        let scenario = Scenario::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/scenarios/bread_thief.json"
        ))
        .unwrap();
        let installed = store.install_scenario(&owner, &scenario).await.unwrap();
        let sequence = store
            .begin_sequence(installed.world_id, "player message")
            .await
            .unwrap();
        let args = r#"{"description":"look through the rear window"}"#;
        let requesting_tool_execution_id = record_tool_call_for_test(
            &store,
            installed.member.agent_id,
            None,
            Some(sequence.id),
            "call-1",
            "look",
            args,
        )
        .await;
        let action = store
            .create_pending_action(
                &installed.member,
                sequence.id,
                requesting_tool_execution_id,
                "look",
                args,
            )
            .await
            .unwrap();
        assert_eq!(action.id, 1);
        assert_eq!(action.tool, "look");
        assert_eq!(action.args, args);
        assert_eq!(action.sequence_id, sequence.id);
        let group_gm: i64 = sqlx::query_scalar(
            "SELECT world_gm.agent_id FROM player_character \
             JOIN character_group ON character_group.character_id = player_character.character_id \
             JOIN \"group\" ON \"group\".id = character_group.group_id \
             JOIN world_gm ON world_gm.world_id = \"group\".world_id \
             WHERE player_character.member_id = ?",
        )
        .bind(installed.member.member_id)
        .fetch_one(&store.pool)
        .await
        .unwrap();
        assert_eq!(action.gm_agent_id, group_gm);

        let approving_tool_execution_id = record_tool_call_for_test(
            &store,
            group_gm,
            Some(action.gm_branch_id),
            Some(sequence.id),
            "approve-1",
            "approve_action",
            "{}",
        )
        .await;
        let error = store
            .resolve_pending_action(
                installed.member.agent_id,
                installed.world_id,
                action.id,
                approving_tool_execution_id,
                "approved",
            )
            .await
            .expect_err("a player agent cannot resolve its own action");
        assert!(format!("{error:#}").contains("is not pending for group GM"));
        let resolved = store
            .resolve_pending_action(
                group_gm,
                installed.world_id,
                action.id,
                approving_tool_execution_id,
                "approved",
            )
            .await
            .unwrap();
        assert_eq!(resolved.id, action.id);
        assert!(resolved.approved);
        let recorded_result: String = sqlx::query_scalar(
            "SELECT json_extract(recorded_content.body, '$.content.value') \
             FROM action JOIN recorded_content ON recorded_content.id = action.result_content_id \
             WHERE action.sequence_id = ?",
        )
        .bind(sequence.id)
        .fetch_one(&store.pool)
        .await
        .unwrap();
        assert_eq!(recorded_result, "approved");

        // A repeated resolve for the same action returns the recorded
        // outcome without re-inserting or re-executing anything (design.md:
        // "cannot be approved twice").
        let actions_before: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM action")
            .fetch_one(&store.pool)
            .await
            .unwrap();
        let resolved_again = store
            .resolve_pending_action(
                group_gm,
                installed.world_id,
                action.id,
                approving_tool_execution_id,
                "approved",
            )
            .await
            .unwrap();
        assert_eq!(resolved_again, resolved);
        let actions_after: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM action")
            .fetch_one(&store.pool)
            .await
            .unwrap();
        assert_eq!(
            actions_before, actions_after,
            "a repeated resolve must not insert a second action row"
        );

        let other_world = store.create_world("other").await.unwrap();
        let other_sequence = store
            .begin_sequence(other_world, "player message")
            .await
            .unwrap();
        let error = store
            .create_pending_action(
                &installed.member,
                other_sequence.id,
                requesting_tool_execution_id,
                "look",
                args,
            )
            .await
            .expect_err("an action cannot borrow a sequence from another world");
        assert!(format!("{error:#}").contains("is not in world"));
        let next_action_id: i64 =
            sqlx::query_scalar("SELECT next_action_id FROM world WHERE id = ?")
                .bind(installed.world_id)
                .fetch_one(&store.pool)
                .await
                .unwrap();
        assert_eq!(
            next_action_id, 1,
            "a rejected request consumes no action id"
        );
    }

    #[tokio::test]
    async fn approved_take_transfers_one_available_item_exactly_once() {
        let db = database();
        let store = Store::open(db.path()).await.unwrap();
        let owner = store
            .find_or_create_user("taker@example.test", "Taker")
            .await
            .unwrap();
        let scenario = Scenario::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/scenarios/bread_thief.json"
        ))
        .unwrap();
        let installed = store.install_scenario(&owner, &scenario).await.unwrap();
        let sequence = store
            .begin_sequence(installed.world_id, "player message")
            .await
            .unwrap();
        let requesting_tool_execution_id = record_tool_call_for_test(
            &store,
            installed.member.agent_id,
            None,
            Some(sequence.id),
            "call-1",
            "take",
            r#"{"item":"flour sack"}"#,
        )
        .await;
        let action = store
            .create_pending_action(
                &installed.member,
                sequence.id,
                requesting_tool_execution_id,
                "take",
                r#"{"item":"flour sack"}"#,
            )
            .await
            .unwrap();
        let approving_tool_execution_id = record_tool_call_for_test(
            &store,
            action.gm_agent_id,
            Some(action.gm_branch_id),
            Some(sequence.id),
            "approve-1",
            "approve_action",
            "{}",
        )
        .await;
        store
            .resolve_pending_action(
                action.gm_agent_id,
                installed.world_id,
                action.id,
                approving_tool_execution_id,
                "approved",
            )
            .await
            .unwrap();
        let locations: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM item_location JOIN item ON item.id = item_location.item_id \
             WHERE item.world_id = ? AND item.name = 'flour sack'",
        )
        .bind(installed.world_id)
        .fetch_one(&store.pool)
        .await
        .unwrap();
        let holders: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM item_character JOIN item ON item.id = item_character.item_id \
             WHERE item.world_id = ? AND item.name = 'flour sack'",
        )
        .bind(installed.world_id)
        .fetch_one(&store.pool)
        .await
        .unwrap();
        assert_eq!(locations, 0);
        assert_eq!(holders, 1, "approval must transfer, not duplicate, flour");
        assert_eq!(
            store
                .player_scene(&installed.member)
                .await
                .unwrap()
                .inventory
                .iter()
                .map(|item| item.name.as_str())
                .collect::<Vec<_>>(),
            ["flour sack"]
        );
        // A repeated resolve for the same action returns the recorded outcome
        // instead of erroring, and must not execute the transfer again
        // (design.md: "cannot be approved twice").
        let resolved_again = store
            .resolve_pending_action(
                action.gm_agent_id,
                installed.world_id,
                action.id,
                approving_tool_execution_id,
                "approved",
            )
            .await
            .expect("a repeated resolve must return the recorded outcome, not error");
        assert_eq!(resolved_again.id, action.id);
        assert!(resolved_again.approved);
        let holders_after_retry: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM item_character JOIN item ON item.id = item_character.item_id \
             WHERE item.world_id = ? AND item.name = 'flour sack'",
        )
        .bind(installed.world_id)
        .fetch_one(&store.pool)
        .await
        .unwrap();
        assert_eq!(
            holders_after_retry, 1,
            "a resolved action must not execute its transfer twice"
        );
    }

    /// design.md: "They cannot be approved twice" - a second resolve with a
    /// *different* outcome than the first (e.g. a GM calling approve_action
    /// then reject_action for the same action_id in one turn) is a real
    /// conflict, not a harmless retry. The store is the sole authority: it
    /// never re-executes the mutation and never reports the newly-requested
    /// outcome - it returns the original recorded decision verbatim, so the
    /// caller (the tool closure) can compare and reject the conflicting
    /// request itself rather than the store raising a generic error.
    #[tokio::test]
    async fn resolving_an_action_with_a_conflicting_outcome_returns_the_original_decision() {
        let db = database();
        let store = Store::open(db.path()).await.unwrap();
        let owner = store
            .find_or_create_user("taker@example.test", "Taker")
            .await
            .unwrap();
        let scenario = Scenario::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/scenarios/bread_thief.json"
        ))
        .unwrap();
        let installed = store.install_scenario(&owner, &scenario).await.unwrap();
        let sequence = store
            .begin_sequence(installed.world_id, "player message")
            .await
            .unwrap();
        let requesting_tool_execution_id = record_tool_call_for_test(
            &store,
            installed.member.agent_id,
            None,
            Some(sequence.id),
            "call-1",
            "take",
            r#"{"item":"flour sack"}"#,
        )
        .await;
        let action = store
            .create_pending_action(
                &installed.member,
                sequence.id,
                requesting_tool_execution_id,
                "take",
                r#"{"item":"flour sack"}"#,
            )
            .await
            .unwrap();
        let approving_tool_execution_id = record_tool_call_for_test(
            &store,
            action.gm_agent_id,
            Some(action.gm_branch_id),
            Some(sequence.id),
            "approve-1",
            "approve_action",
            "{}",
        )
        .await;
        store
            .resolve_pending_action(
                action.gm_agent_id,
                installed.world_id,
                action.id,
                approving_tool_execution_id,
                "approved",
            )
            .await
            .unwrap();
        let reject_tool_execution_id = record_tool_call_for_test(
            &store,
            action.gm_agent_id,
            Some(action.gm_branch_id),
            Some(sequence.id),
            "reject-1",
            "reject_action",
            "{}",
        )
        .await;
        let resolved = store
            .resolve_pending_action(
                action.gm_agent_id,
                installed.world_id,
                action.id,
                reject_tool_execution_id,
                "rejected: changed my mind",
            )
            .await
            .expect("a conflicting resolve returns the original decision, it does not error");
        assert!(
            resolved.approved,
            "the store must report the original approval, not the newly-requested rejection"
        );
        let holders: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM item_character JOIN item ON item.id = item_character.item_id \
             WHERE item.world_id = ? AND item.name = 'flour sack'",
        )
        .bind(installed.world_id)
        .fetch_one(&store.pool)
        .await
        .unwrap();
        assert_eq!(
            holders, 1,
            "the original approval's transfer must remain committed, not undone or repeated by the conflicting resolve"
        );
    }

    #[tokio::test]
    async fn character_handles_are_unique_and_widen_only_after_two_digits_are_full() {
        let db = database();
        let store = Store::open(db.path()).await.unwrap();
        let owner = store
            .find_or_create_user("warden@example.test", "Warden")
            .await
            .unwrap();
        let scenario = test_scenario("handle allocation");
        let mut handles = std::collections::HashSet::new();
        for _ in 0..90 {
            let installed = store.install_scenario(&owner, &scenario).await.unwrap();
            assert_eq!(installed.character_handle.len(), 6);
            assert!(handles.insert(installed.character_handle));
        }
        let widened = store.install_scenario(&owner, &scenario).await.unwrap();
        assert_eq!(widened.character_handle.len(), 7);
    }

    #[tokio::test]
    async fn inference_identity_survives_execution_failure_and_restart() {
        let (store, db, agent, segments, request) = store_with_history().await;
        let invocation_id = store
            .start_invocation(None, agent, None, None, None)
            .await
            .unwrap();
        let id = store
            .start_inference(InferenceInput {
                agent_id: agent,
                sequence_id: None,
                invocation_id,
                segments: &segments,
                request: &request,
                model: "test-model",
            })
            .await
            .unwrap();
        let running = store.reconstruct_inference(id).await.unwrap();
        assert_eq!(running.outcome, RecordedOutcome::Running);
        assert_eq!(running.request, request);
        assert_eq!(running.prompt_evidence, None);
        assert_eq!(running.duration_ms, None);
        let reopened = Store::open(db.path()).await.unwrap();
        assert_eq!(
            reopened.reconstruct_inference(id).await.unwrap().outcome,
            RecordedOutcome::Running,
            "opening a database for inspection must not interrupt live work"
        );
        reopened.interrupt_open_inferences().await.unwrap();
        let interrupted = reopened.reconstruct_inference(id).await.unwrap();
        assert_eq!(interrupted.request, request);
        assert_eq!(interrupted.duration_ms, None);
        assert_eq!(
            interrupted.outcome,
            RecordedOutcome::Error("Inference interrupted by process restart".into())
        );
        assert!(
            reopened
                .finish_inference(id, InferenceOutcome::Response(response()), 1)
                .await
                .is_err(),
            "late completion cannot rewrite a terminal record"
        );
    }

    /// design.md's Chat schema deliberately drops the old content-value
    /// uniqueness constraint: two independently produced events with equal
    /// text are still distinct events, not deduplicated rows. This replaces
    /// the old test asserting the opposite (text/reasoning interning), which
    /// was the pre-slice-B behavior this schema intentionally changed.
    #[tokio::test]
    async fn repeated_equal_text_stays_distinct_as_events() {
        let (store, _db, agent, segments, request) = store_with_history().await;
        let response = response();
        let id = record_inference(
            &store,
            agent,
            &segments,
            &request,
            InferenceOutcome::Response(response.clone()),
            "test-model",
            1,
        )
        .await
        .unwrap();
        let (output,): (i64,) = sqlx::query_as("SELECT output FROM inference WHERE id = ?")
            .bind(id)
            .fetch_one(&store.pool)
            .await
            .unwrap();

        // A second, independently produced event with the exact same text
        // and reasoning gets its own recorded_content row, not the inference
        // output's row.
        let assistant = Message::assistant(response.content.clone(), response.reasoning.clone());
        let message = store.append_message(agent, &assistant).await.unwrap();
        let (history,): (i64,) = sqlx::query_as("SELECT content FROM message WHERE id = ?")
            .bind(message)
            .fetch_one(&store.pool)
            .await
            .unwrap();
        assert_ne!(
            output, history,
            "an independently produced event with equal text/reasoning must be a distinct recorded_content row"
        );
        assert_eq!(
            store.recorded_content(output).await.unwrap().1,
            response.reasoning
        );
        assert_eq!(
            store.recorded_content(history).await.unwrap().1,
            response.reasoning
        );

        // A third event, again with the same text, also gets its own row -
        // repetition alone never collapses events onto one another.
        let MessageContent::Text(text) = &assistant.content else {
            panic!("text fixture required")
        };
        let narration = store
            .append_message(agent, &Message::text(Role::Narration, text.clone()))
            .await
            .unwrap();
        let (narration_content,): (i64,) =
            sqlx::query_as("SELECT content FROM message WHERE id = ?")
                .bind(narration)
                .fetch_one(&store.pool)
                .await
                .unwrap();
        assert_ne!(narration_content, output);
        assert_ne!(narration_content, history);

        assert_eq!(
            store.reconstruct_inference(id).await.unwrap().outcome,
            RecordedOutcome::Response(response)
        );
    }

    /// design.md's Inference records: "The same output reference supplies
    /// any assistant message, summary or narration delivery derived from
    /// that inference" - finish_chat_inference's own assistant-message row
    /// must reference the same recorded_content the inference's `output`
    /// column already points to, not a second, freshly-interned copy of the
    /// same reply.
    #[tokio::test]
    async fn a_settled_reply_shares_its_content_row_with_the_inference_output() {
        let db = database();
        let store = Store::open(db.path()).await.unwrap();
        let world = store.create_world("test world").await.unwrap();
        let agent = store.create_agent(world).await.unwrap();
        let invocation = store
            .start_invocation(None, agent, None, None, None)
            .await
            .unwrap();
        let inference_id = store
            .start_inference(InferenceInput {
                agent_id: agent,
                sequence_id: None,
                invocation_id: invocation,
                segments: &[],
                request: &Request {
                    messages: vec![Message::text(Role::User, "hello")],
                    tools: Vec::new(),
                    sampling: Sampling::default(),
                },
                model: "test-model",
            })
            .await
            .unwrap();
        store
            .finish_chat_inference(
                inference_id,
                agent,
                InferenceOutcome::Response(Response {
                    content: Content::Text("a distinctive settled reply".into()),
                    ..response()
                }),
                1,
                CompactionPolicy {
                    sequence_id: None,
                    compact_before_next_input_tokens: usize::MAX,
                    sampling: &Sampling::default(),
                    model: "test-model",
                    static_segments: &[],
                },
            )
            .await
            .unwrap();
        let output: i64 = sqlx::query_scalar("SELECT output FROM inference WHERE id = ?")
            .bind(inference_id)
            .fetch_one(&store.pool)
            .await
            .unwrap();
        let message_content: i64 =
            sqlx::query_scalar("SELECT content FROM message WHERE source_inference_id = ?")
                .bind(inference_id)
                .fetch_one(&store.pool)
                .await
                .unwrap();
        assert_eq!(
            output, message_content,
            "the settled reply's own message must reference the same recorded_content \
             row as inference.output, not a second copy"
        );
        // Two independent equal outputs must still get distinct rows
        // (design.md's Chat schema: "No uniqueness constraint on body") -
        // sharing must be an explicit reference, not accidental interning.
        let other_invocation = store
            .start_invocation(None, agent, None, None, None)
            .await
            .unwrap();
        let other_inference_id = store
            .start_inference(InferenceInput {
                agent_id: agent,
                sequence_id: None,
                invocation_id: other_invocation,
                segments: &[],
                request: &Request {
                    messages: vec![Message::text(Role::User, "hello again")],
                    tools: Vec::new(),
                    sampling: Sampling::default(),
                },
                model: "test-model",
            })
            .await
            .unwrap();
        store
            .finish_chat_inference(
                other_inference_id,
                agent,
                InferenceOutcome::Response(Response {
                    content: Content::Text("a distinctive settled reply".into()),
                    ..response()
                }),
                1,
                CompactionPolicy {
                    sequence_id: None,
                    compact_before_next_input_tokens: usize::MAX,
                    sampling: &Sampling::default(),
                    model: "test-model",
                    static_segments: &[],
                },
            )
            .await
            .unwrap();
        let other_output: i64 = sqlx::query_scalar("SELECT output FROM inference WHERE id = ?")
            .bind(other_inference_id)
            .fetch_one(&store.pool)
            .await
            .unwrap();
        assert_ne!(
            output, other_output,
            "an independently produced equal-text reply must still get its own content row"
        );
    }

    /// design.md's Chat schema: "every recipient references that same
    /// selection" - GM narration delivered to several present characters
    /// shares one original payload rather than a fresh copy per recipient.
    #[tokio::test]
    async fn narration_delivered_to_several_recipients_shares_one_content_row() {
        let db = database();
        let store = Store::open(db.path()).await.unwrap();
        let owner = store
            .find_or_create_user("owner@example.test", "Owner")
            .await
            .unwrap();
        let installed = store
            .install_scenario(&owner, &test_scenario("narration sharing"))
            .await
            .unwrap();
        let second = store
            .create_player_character(owner.id, installed.world_id)
            .await
            .unwrap();
        let location_id = store.player_location_id(&installed.member).await.unwrap();
        assert_eq!(
            store.player_location_id(&second).await.unwrap(),
            location_id,
            "both characters must start at the same location for this test"
        );
        // Narration recipients are connected PCs (design.md's Membership and
        // presence); both characters must be online to receive it.
        store
            .mark_character_present(installed.member.character_id)
            .await
            .unwrap();
        store
            .mark_character_present(second.character_id)
            .await
            .unwrap();

        // The narrating GM's own reply, whose recorded_content row delivery
        // must reference rather than copy.
        let mut transaction = store.pool.begin().await.unwrap();
        let content_id = Store::content_in(
            &mut transaction,
            &MessageContent::Text("A shadow crosses the doorway.".to_string()),
            "",
        )
        .await
        .unwrap();
        transaction.commit().await.unwrap();

        store
            .append_group_narration(
                installed.world_id,
                store
                    .character_group_id(installed.member.character_id)
                    .await
                    .unwrap(),
                content_id,
            )
            .await
            .unwrap();

        let contents: Vec<(i64,)> = sqlx::query_as(
            "SELECT message.content FROM message \
             JOIN history_entry ON history_entry.id = message.id \
             JOIN agent_history ON agent_history.branch_id = history_entry.branch_id \
             WHERE agent_history.agent_id IN (?, ?) AND message.role = ? \
             ORDER BY agent_history.agent_id",
        )
        .bind(installed.member.agent_id)
        .bind(second.agent_id)
        .bind(serde_json::to_string(&Role::Narration).unwrap())
        .fetch_all(&store.pool)
        .await
        .unwrap();
        assert_eq!(
            contents.len(),
            2,
            "both present characters must receive the narration"
        );
        assert_eq!(
            contents[0].0, content_id,
            "delivered narration must reference the GM's own content row"
        );
        assert_eq!(
            contents[0].0, contents[1].0,
            "every recipient must reference the same content row, not a copy"
        );
    }

    #[tokio::test]
    async fn reconstructs_history_recipe_without_copying_messages() {
        let (store, _db, agent, segments, request) = store_with_history().await;
        let first = record_inference(
            &store,
            agent,
            &segments,
            &request,
            InferenceOutcome::Response(response()),
            "test-model",
            14,
        )
        .await
        .expect("record should persist");
        let second = record_inference(
            &store,
            agent,
            &segments,
            &request,
            InferenceOutcome::Response(response()),
            "test-model",
            15,
        )
        .await
        .expect("repeat record should persist");

        let recorded = store
            .reconstruct_inference(first)
            .await
            .expect("recorded request should reconstruct");
        assert_eq!(recorded.request, request);
        assert_eq!(recorded.outcome, RecordedOutcome::Response(response()));
        assert_eq!(store.inference_count().await.unwrap(), 2);
        assert_ne!(first, second);
        let messages: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM message")
            .fetch_one(&store.pool)
            .await
            .unwrap();
        assert_eq!(messages, 2);
    }

    #[test]
    fn inference_hash_preserves_legacy_input_shape_and_ignores_provenance() {
        let mut request = Request {
            messages: vec![Message::text(Role::User, "Look.")],
            tools: vec![],
            sampling: Sampling::default(),
        };
        let legacy = br#"{"messages":[{"role":"User","content":{"type":"Text","value":"Look."},"reasoning":""}],"tools":[],"sampling":{"temperature":0.0,"top_p":null,"top_k":null,"min_p":null,"presence_penalty":null,"enable_thinking":false}}"#;
        let expected = blake3::hash(legacy).to_hex().to_string();
        assert_eq!(inference_input_hash(&request).unwrap(), expected);
        request.messages[0].origin = MessageOrigin::Instruction;
        assert_eq!(inference_input_hash(&request).unwrap(), expected);
    }

    #[tokio::test]
    async fn reconstruction_rejects_corrupt_or_invalid_references() {
        let (store, _db, agent, segments, request) = store_with_history().await;
        let id = record_inference(
            &store,
            agent,
            &segments,
            &request,
            InferenceOutcome::Response(response()),
            "test-model",
            14,
        )
        .await
        .expect("record should persist");
        let text = match &segments[0] {
            Segment::Text { text, .. } => text,
            _ => unreachable!(),
        };
        // Prompt rows are written once and never updated. Rewriting one is
        // therefore corruption, and `input_hash` over the whole reassembled
        // request is what catches it.
        sqlx::query("UPDATE text SET content = 'corrupt' WHERE id = ?")
            .bind(text)
            .execute(&store.pool)
            .await
            .unwrap();
        assert!(
            format!("{:#}", store.reconstruct_inference(id).await.unwrap_err())
                .contains("does not match")
        );

        sqlx::query("UPDATE text SET content = ? WHERE id = ?")
            .bind("You are a careful guide.")
            .bind(text)
            .execute(&store.pool)
            .await
            .unwrap();
        let branch_id = store.branch_id_for_agent(agent).await.unwrap();
        sqlx::query(
            "DELETE FROM message WHERE id = (SELECT id FROM history_entry WHERE branch_id = ? AND position = 1)",
        )
        .bind(branch_id)
        .execute(&store.pool)
        .await
        .unwrap();
        assert!(
            format!("{:#}", store.reconstruct_inference(id).await.unwrap_err())
                .contains("missing rows")
        );

        // A corrupted recorded_content row (unparseable body) must be named
        // by its own id, not a generic decode failure.
        let content_id: i64 = sqlx::query_scalar("SELECT output FROM inference WHERE id = ?")
            .bind(id)
            .fetch_one(&store.pool)
            .await
            .unwrap();
        sqlx::query("UPDATE recorded_content SET body = 'not json' WHERE id = ?")
            .bind(content_id)
            .execute(&store.pool)
            .await
            .unwrap();
        let error = format!(
            "{:#}",
            store.recorded_content(content_id).await.unwrap_err()
        );
        assert!(
            error.contains(&content_id.to_string()),
            "corruption error must name the failing content id: {error}"
        );
    }

    #[tokio::test]
    async fn reconstruction_rejects_cross_agent_references_and_preserves_failures() {
        let (store, _db, agent, mut segments, request) = store_with_history().await;
        let world: i64 = sqlx::query_scalar("SELECT world_id FROM agent WHERE id = ?")
            .bind(agent)
            .fetch_one(&store.pool)
            .await
            .unwrap();
        let other = store.create_agent(world).await.unwrap();
        let other_branch = store.branch_id_for_agent(other).await.unwrap();
        let own_branch = store.branch_id_for_agent(agent).await.unwrap();
        if let Segment::Messages { messages } = &mut segments[1] {
            messages.branch_id = other_branch;
        }
        let id = record_inference(
            &store,
            agent,
            &segments,
            &request,
            InferenceOutcome::Error("backend disconnected".to_string()),
            "test-model",
            14,
        )
        .await
        .expect("failed inference should persist");
        assert!(
            format!("{:#}", store.reconstruct_inference(id).await.unwrap_err())
                .contains("references messages from branch")
        );

        if let Segment::Messages { messages } = &mut segments[1] {
            messages.branch_id = own_branch;
        }
        sqlx::query("UPDATE inference SET segments = ? WHERE id = ?")
            .bind(serde_json::to_string(&segments).unwrap())
            .bind(id)
            .execute(&store.pool)
            .await
            .unwrap();
        let recorded = store.reconstruct_inference(id).await.unwrap();
        assert_eq!(
            recorded.outcome,
            RecordedOutcome::Error("backend disconnected".to_string())
        );
    }

    /// `check_message_range_ownership` validates every `Segment::Messages`
    /// branch in one batched query (design.md/plan: "one query over all
    /// message ranges, not once per segment") - a merge's recipe can carry
    /// several. This proves it actually checks every one, not only the first.
    #[tokio::test]
    async fn request_for_segments_rejects_a_foreign_branch_among_several_message_ranges() {
        let (store, _db, agent, segments, request) = store_with_history().await;
        record_inference(
            &store,
            agent,
            &segments,
            &request,
            InferenceOutcome::Response(response()),
            "test-model",
            14,
        )
        .await
        .unwrap();
        let world: i64 = sqlx::query_scalar("SELECT world_id FROM agent WHERE id = ?")
            .bind(agent)
            .fetch_one(&store.pool)
            .await
            .unwrap();
        let other = store.create_agent(world).await.unwrap();
        let other_branch = store.branch_id_for_agent(other).await.unwrap();
        let Segment::Messages {
            messages: own_range,
        } = segments[1].clone()
        else {
            panic!("expected a message range segment");
        };
        let recipe = vec![
            Segment::Messages {
                messages: own_range,
            },
            Segment::Messages {
                messages: PositionRange {
                    branch_id: other_branch,
                    first_position: 0,
                    last_position: 0,
                },
            },
        ];
        let error = store
            .request_for_segments(agent, &recipe, Sampling::default())
            .await
            .unwrap_err();
        assert!(
            format!("{error:#}").contains(&format!("branch {other_branch}")),
            "the second, foreign range must be caught even though the first is agent's own: {error:#}"
        );
        // The valid range alone must still succeed, confirming the rejection
        // above is really about the foreign branch, not something else in
        // the recipe.
        store
            .request_for_segments(
                agent,
                &[Segment::Messages {
                    messages: own_range,
                }],
                Sampling::default(),
            )
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn latest_summary_replaces_only_the_history_it_covers() {
        let (store, _db, agent, segments, request) = store_with_history().await;
        let summary = complete_summary(
            &store,
            agent,
            &segments,
            &request,
            "The floorboards hide a locked chest.",
            0,
        )
        .await
        .unwrap();
        // A fresh event follows the summary, as one always does in production.
        store
            .append_message(agent, &Message::text(Role::User, "And the walls?"))
            .await
            .unwrap();
        let history = store
            .live_segments(store.agent_branch(agent).await.unwrap())
            .await
            .unwrap();
        // The summary covers only position 0 ("What is beneath the
        // floorboards?"), so the live range spans from the still-uncovered
        // assistant reply (position 1) through the newest message - even
        // though the summary's own activation entry sits at a later position
        // in the unified index (it was recorded after position 1).
        assert!(matches!(
            history.as_slice(),
            [
                Segment::Summary { summary: stored },
                Segment::Messages { messages: PositionRange { first_position: 1, .. } },
            ] if *stored == summary
        ));
        let prompt = store
            .store_prompt_text("You are a careful guide.")
            .await
            .unwrap();
        let mut recipe = vec![Segment::Text {
            text: prompt,
            role: Role::System,
        }];
        recipe.extend(history);
        let assembled = store
            .request_for_segments(
                agent,
                &recipe,
                Sampling {
                    temperature: 0.7,
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        // The summary is folded into the single leading system message, not a
        // second system turn, so strict chat templates accept it.
        assert!(matches!(assembled.messages.as_slice(), [
            Message { role: Role::System, content: MessageContent::Text(system), .. },
            Message { role: Role::Assistant, .. },
            Message { role: Role::User, .. },
        ] if system.starts_with("You are a careful guide.")
            && system.contains("The floorboards hide a locked chest.")));
    }

    #[tokio::test]
    async fn deferred_compaction_links_its_summary_to_the_triggering_sequence() {
        let db = database();
        let store = Store::open(db.path()).await.unwrap();
        let world = store.create_world("test world").await.unwrap();
        let agent = store.create_agent(world).await.unwrap();
        for turn in 0..3 {
            store
                .append_message(agent, &Message::text(Role::User, format!("q{turn}")))
                .await
                .unwrap();
            store
                .append_message(
                    agent,
                    &Message::assistant(Content::Text(format!("a{turn}")), String::new()),
                )
                .await
                .unwrap();
        }
        // The reply that crossed the threshold served this sequence.
        let trigger = store.begin_sequence(world, "player message").await.unwrap();
        let reply_invocation = store
            .start_invocation(Some(trigger.id), agent, None, None, None)
            .await
            .unwrap();
        let reply_inference = store
            .start_inference(InferenceInput {
                agent_id: agent,
                sequence_id: Some(trigger.id),
                invocation_id: reply_invocation,
                segments: &[],
                request: &Request {
                    messages: vec![Message::text(Role::User, "q2")],
                    tools: Vec::new(),
                    sampling: Sampling::default(),
                },
                model: "test-model",
            })
            .await
            .unwrap();
        store
            .finish_chat_inference(
                reply_inference,
                agent,
                InferenceOutcome::Response(Response {
                    content: Content::Text("final".into()),
                    ..response()
                }),
                1,
                CompactionPolicy {
                    sequence_id: Some(trigger.id),
                    compact_before_next_input_tokens: 1,
                    sampling: &Sampling::default(),
                    model: "test-model",
                    static_segments: &[],
                },
            )
            .await
            .unwrap();
        let job = store
            .pending_compaction(agent)
            .await
            .unwrap()
            .expect("the over-threshold reply queued compaction");
        assert_eq!(
            job.sequence_id,
            Some(trigger.id),
            "the queued job records the sequence whose reply triggered it"
        );

        // Resolve the job through a real compaction inference that carries the
        // job's sequence, as compaction::run does. Deferred compaction's
        // invocation has no parent tool call; it records its triggering
        // inference as cause_inference_id instead (design.md).
        let compaction_invocation = store
            .start_invocation(job.sequence_id, agent, None, None, Some(reply_inference))
            .await
            .unwrap();
        let summary_inference = store
            .start_inference(InferenceInput {
                agent_id: agent,
                sequence_id: job.sequence_id,
                invocation_id: compaction_invocation,
                segments: &[],
                request: &Request {
                    messages: vec![Message::text(Role::User, "summarise")],
                    tools: Vec::new(),
                    sampling: Sampling::default(),
                },
                model: "test-model",
            })
            .await
            .unwrap();
        let branch_id = store.branch_id_for_agent(agent).await.unwrap();
        store
            .finish_summary_inference(
                summary_inference,
                &job,
                InferenceOutcome::Response(Response {
                    content: Content::Text("Earlier: q0..q2.".into()),
                    ..response()
                }),
                1,
                Some(&[PositionRange {
                    branch_id,
                    first_position: 0,
                    last_position: job.position,
                }]),
                ("threshold", 2),
            )
            .await
            .unwrap();

        assert_eq!(
            store.sequence_inference_ids(trigger.id).await.unwrap(),
            vec![reply_inference, summary_inference],
            "the sequence owns both the reply and the compaction it triggered"
        );
        assert_eq!(
            store
                .latest_summary_for_branch(branch_id)
                .await
                .unwrap()
                .unwrap()
                .inference_id,
            summary_inference
        );
    }

    /// A world_gm agent has no `agent_history` row - it has one branch per
    /// group instead (design.md's `GroupHistory`) - so every function this
    /// exercises must resolve compaction state through the group's own
    /// branch, never a plain `agent_id` lookup. Drives two real over-
    /// threshold replies on the same group branch and confirms: the job is
    /// visible to both `pending_compaction_for_branch` and the unfiltered
    /// `pending_compactions()` (which a bare `agent_history` JOIN would
    /// silently exclude); a second over-threshold reply while the first
    /// job is still pending does not collide (`ON CONFLICT DO NOTHING`); and
    /// resolving the job through `compaction::run`'s real shape (an
    /// invocation started with the branch explicit, since `start_invocation`
    /// falling back to `branch_for_agent` would fail for a world_gm) settles
    /// cleanly.
    #[tokio::test]
    async fn a_world_gm_groups_branch_compacts_without_an_agent_history_row() {
        let db = database();
        let store = Store::open(db.path()).await.unwrap();
        let owner = store
            .find_or_create_user("warden@example.test", "Warden")
            .await
            .unwrap();
        let installed = store
            .install_scenario(&owner, &test_scenario("world gm compaction"))
            .await
            .unwrap();
        let group_id = store
            .character_group_id(installed.member.character_id)
            .await
            .unwrap();
        let gm_history = store.group_gm_history(group_id).await.unwrap();

        assert!(
            store
                .pending_compaction_for_branch(gm_history.branch_id)
                .await
                .unwrap()
                .is_none(),
            "no compaction is pending before any over-threshold reply"
        );

        let trigger = store
            .begin_sequence(installed.world_id, "gm turn")
            .await
            .unwrap();
        let reply_invocation = store
            .start_invocation(
                Some(trigger.id),
                gm_history.agent_id,
                Some(gm_history.branch_id),
                None,
                None,
            )
            .await
            .unwrap();
        let reply_inference = store
            .start_inference(InferenceInput {
                agent_id: gm_history.agent_id,
                sequence_id: Some(trigger.id),
                invocation_id: reply_invocation,
                segments: &[],
                request: &Request {
                    messages: vec![Message::text(Role::User, "narrate")],
                    tools: Vec::new(),
                    sampling: Sampling::default(),
                },
                model: "test-model",
            })
            .await
            .unwrap();
        store
            .finish_chat_inference(
                reply_inference,
                gm_history.agent_id,
                InferenceOutcome::Response(Response {
                    content: Content::Text("A shadow crosses the doorway.".into()),
                    ..response()
                }),
                1,
                CompactionPolicy {
                    sequence_id: Some(trigger.id),
                    compact_before_next_input_tokens: 1,
                    sampling: &Sampling::default(),
                    model: "test-model",
                    static_segments: &[],
                },
            )
            .await
            .unwrap();

        let job = store
            .pending_compaction_for_branch(gm_history.branch_id)
            .await
            .unwrap()
            .expect("the over-threshold GM reply queued compaction on its own branch");
        assert_eq!(job.agent_id, gm_history.agent_id);
        assert!(
            store
                .pending_compactions()
                .await
                .unwrap()
                .iter()
                .any(|job| job.branch_id == gm_history.branch_id),
            "the unfiltered listing (used at startup recovery) must also see a world_gm's job, \
             not silently exclude it the way a bare agent_history JOIN would"
        );

        // A second over-threshold reply on the same branch while the first
        // job is still pending must not collide (finish_chat_inference's own
        // INSERT has ON CONFLICT(branch_id) DO NOTHING, matching
        // enqueue_capacity_compaction's sibling).
        let second_invocation = store
            .start_invocation(
                Some(trigger.id),
                gm_history.agent_id,
                Some(gm_history.branch_id),
                None,
                None,
            )
            .await
            .unwrap();
        let second_inference = store
            .start_inference(InferenceInput {
                agent_id: gm_history.agent_id,
                sequence_id: Some(trigger.id),
                invocation_id: second_invocation,
                segments: &[],
                request: &Request {
                    messages: vec![Message::text(Role::User, "narrate more")],
                    tools: Vec::new(),
                    sampling: Sampling::default(),
                },
                model: "test-model",
            })
            .await
            .unwrap();
        store
            .finish_chat_inference(
                second_inference,
                gm_history.agent_id,
                InferenceOutcome::Response(Response {
                    content: Content::Text("The floorboards creak.".into()),
                    ..response()
                }),
                1,
                CompactionPolicy {
                    sequence_id: Some(trigger.id),
                    compact_before_next_input_tokens: 1,
                    sampling: &Sampling::default(),
                    model: "test-model",
                    static_segments: &[],
                },
            )
            .await
            .expect("a second over-threshold reply must not collide with the still-pending job");
        assert_eq!(
            store
                .pending_compaction_for_branch(gm_history.branch_id)
                .await
                .unwrap()
                .unwrap()
                .position,
            job.position,
            "the first job's row must stand unchanged, not be silently replaced"
        );

        // Resolve the job exactly as compaction::run does: an explicit
        // branch on invocation start, since start_invocation falling back to
        // branch_for_agent would fail for a world_gm agent.
        let compaction_invocation = store
            .start_invocation(
                job.sequence_id,
                job.agent_id,
                Some(job.branch_id),
                None,
                Some(reply_inference),
            )
            .await
            .unwrap();
        let summary_inference = store
            .start_inference(InferenceInput {
                agent_id: job.agent_id,
                sequence_id: job.sequence_id,
                invocation_id: compaction_invocation,
                segments: &[],
                request: &Request {
                    messages: vec![Message::text(Role::User, "summarise")],
                    tools: Vec::new(),
                    sampling: Sampling::default(),
                },
                model: "test-model",
            })
            .await
            .unwrap();
        store
            .finish_summary_inference(
                summary_inference,
                &job,
                InferenceOutcome::Response(Response {
                    content: Content::Text("Earlier: the doorway and floorboards.".into()),
                    ..response()
                }),
                1,
                Some(&[PositionRange {
                    branch_id: job.branch_id,
                    first_position: 0,
                    last_position: job.position,
                }]),
                ("threshold", 2),
            )
            .await
            .unwrap();

        assert!(
            store
                .pending_compaction_for_branch(gm_history.branch_id)
                .await
                .unwrap()
                .is_none(),
            "resolving the job must clear it"
        );
        assert_eq!(
            store
                .latest_summary_for_branch(gm_history.branch_id)
                .await
                .unwrap()
                .unwrap()
                .inference_id,
            summary_inference
        );
    }

    /// After a merge, the merged branch's parents each carry their own
    /// independent summary over their own disjoint range. `live_segments`
    /// must surface both - not just the single newest one across the whole
    /// traversal - or the model silently loses one parent's compaction and
    /// pays full raw-message cost for it (plans/developer-inspection.md's
    /// D5 "longest-disjoint-summary selector").
    #[tokio::test]
    async fn live_segments_after_a_merge_carries_each_parent_branchs_own_summary() {
        let db = database();
        let store = Store::open(db.path()).await.unwrap();
        let owner = store
            .find_or_create_user("warden@example.test", "Warden")
            .await
            .unwrap();
        let installed = store
            .install_scenario(&owner, &test_scenario("merge live segments"))
            .await
            .unwrap();
        let starting_group_id = store
            .character_group_id(installed.member.character_id)
            .await
            .unwrap();
        let world_gm_agent_id = store
            .group_gm_history(starting_group_id)
            .await
            .unwrap()
            .agent_id;
        let location_id: i64 = sqlx::query_scalar("SELECT location_id FROM \"group\" WHERE id = ?")
            .bind(starting_group_id)
            .fetch_one(&store.pool())
            .await
            .unwrap();

        // Two independent group branches, each with one settled message and
        // its own summary covering it - mirroring what a real compaction
        // would leave behind on each side before they ever merge.
        async fn seed_branch(
            store: &Store,
            world_id: i64,
            agent_id: i64,
            branch_id: i64,
            text: &str,
            summary_text: &str,
        ) {
            let trigger = store.begin_sequence(world_id, "seed").await.unwrap();
            let invocation = store
                .start_invocation(Some(trigger.id), agent_id, Some(branch_id), None, None)
                .await
                .unwrap();
            let inference = store
                .start_inference(InferenceInput {
                    agent_id,
                    sequence_id: Some(trigger.id),
                    invocation_id: invocation,
                    segments: &[],
                    request: &Request {
                        messages: vec![Message::text(Role::User, "narrate")],
                        tools: Vec::new(),
                        sampling: Sampling::default(),
                    },
                    model: "test-model",
                })
                .await
                .unwrap();
            store
                .finish_chat_inference(
                    inference,
                    agent_id,
                    InferenceOutcome::Response(Response {
                        content: Content::Text(text.to_string()),
                        ..response()
                    }),
                    1,
                    CompactionPolicy {
                        sequence_id: Some(trigger.id),
                        compact_before_next_input_tokens: 1,
                        sampling: &Sampling::default(),
                        model: "test-model",
                        static_segments: &[],
                    },
                )
                .await
                .unwrap();
            let job = store
                .pending_compaction_for_branch(branch_id)
                .await
                .unwrap()
                .expect("the settled reply must have queued compaction on its own branch");
            let summary_invocation = store
                .start_invocation(
                    job.sequence_id,
                    job.agent_id,
                    Some(job.branch_id),
                    None,
                    Some(inference),
                )
                .await
                .unwrap();
            let summary_inference = store
                .start_inference(InferenceInput {
                    agent_id,
                    sequence_id: Some(trigger.id),
                    invocation_id: summary_invocation,
                    segments: &[],
                    request: &Request {
                        messages: vec![Message::text(Role::User, "summarise")],
                        tools: Vec::new(),
                        sampling: Sampling::default(),
                    },
                    model: "test-model",
                })
                .await
                .unwrap();
            store
                .finish_summary_inference(
                    summary_inference,
                    &job,
                    InferenceOutcome::Response(Response {
                        content: Content::Text(summary_text.to_string()),
                        ..response()
                    }),
                    1,
                    Some(&[PositionRange {
                        branch_id,
                        first_position: 0,
                        last_position: job.position,
                    }]),
                    ("threshold", 2),
                )
                .await
                .unwrap();
        }

        let mut left_setup = store.pool().begin().await.unwrap();
        let (_left_group_id, left_branch_id) = Store::spawn_group_branch(
            &mut left_setup,
            installed.world_id,
            world_gm_agent_id,
            location_id,
            "left",
            "left side",
            &[],
        )
        .await
        .unwrap();
        left_setup.commit().await.unwrap();
        let mut right_setup = store.pool().begin().await.unwrap();
        let (_right_group_id, right_branch_id) = Store::spawn_group_branch(
            &mut right_setup,
            installed.world_id,
            world_gm_agent_id,
            location_id,
            "right",
            "right side",
            &[],
        )
        .await
        .unwrap();
        right_setup.commit().await.unwrap();

        seed_branch(
            &store,
            installed.world_id,
            world_gm_agent_id,
            left_branch_id,
            "The left group settles in.",
            "Earlier, the left group arrived.",
        )
        .await;
        seed_branch(
            &store,
            installed.world_id,
            world_gm_agent_id,
            right_branch_id,
            "The right group settles in.",
            "Earlier, the right group arrived.",
        )
        .await;

        let mut transaction = store.pool().begin().await.unwrap();
        let left_through: i64 =
            sqlx::query_scalar("SELECT next_position - 1 FROM history_branch WHERE id = ?")
                .bind(left_branch_id)
                .fetch_one(&mut *transaction)
                .await
                .unwrap();
        let right_through: i64 =
            sqlx::query_scalar("SELECT next_position - 1 FROM history_branch WHERE id = ?")
                .bind(right_branch_id)
                .fetch_one(&mut *transaction)
                .await
                .unwrap();
        let (merged_group_id, _merged_branch_id) = Store::spawn_group_branch(
            &mut transaction,
            installed.world_id,
            world_gm_agent_id,
            location_id,
            "merged",
            "reunited",
            &[
                (left_branch_id, left_through),
                (right_branch_id, right_through),
            ],
        )
        .await
        .unwrap();
        transaction.commit().await.unwrap();

        let segments = store
            .live_segments(store.group_branch(merged_group_id).await.unwrap())
            .await
            .unwrap();
        let summary_count = segments
            .iter()
            .filter(|segment| matches!(segment, Segment::Summary { .. }))
            .count();
        assert_eq!(
            summary_count, 2,
            "both parent branches' independent summaries must survive the merge, not just the newest one: {segments:?}"
        );
        let message_segments: Vec<&PositionRange> = segments
            .iter()
            .filter_map(|segment| match segment {
                Segment::Messages { messages } => Some(messages),
                _ => None,
            })
            .collect();
        assert!(
            message_segments.is_empty(),
            "every message on both branches is already covered by its own branch's summary: {segments:?}"
        );
    }

    /// A plain player agent's branch graph is its one branch, no parents, no
    /// group head - the trivial case every non-GM agent must keep rendering
    /// as today's single linear view.
    #[tokio::test]
    async fn gm_branch_graph_is_one_parentless_headless_branch_for_a_plain_agent() {
        let db = database();
        let store = Store::open(db.path()).await.unwrap();
        let owner = store
            .find_or_create_user("warden@example.test", "Warden")
            .await
            .unwrap();
        let installed = store
            .install_scenario(&owner, &test_scenario("plain agent branch graph"))
            .await
            .unwrap();
        let world_id = installed.world_id;
        let player_agent_id = installed.member.agent_id;
        store
            .set_developer_mode(owner.id, world_id, true)
            .await
            .unwrap();

        let nodes = store
            .gm_branch_graph(owner.id, world_id, player_agent_id)
            .await
            .unwrap();

        assert_eq!(
            nodes.len(),
            1,
            "a plain agent has exactly one branch: {nodes:?}"
        );
        assert!(
            nodes[0].parents.is_empty(),
            "a plain agent's branch has no parents: {nodes:?}"
        );
        assert!(
            nodes[0].head.is_none(),
            "a plain agent's head is its own agent_history row, not a group: {nodes:?}"
        );
    }

    /// After a split, the world_gm's branch graph must show the whole DAG:
    /// the retired source branch (still present, no current head) and both
    /// resulting branches (each a current group head), with the split's
    /// exact parent/through_position edge - user_declarations.md: "the GM's
    /// chat history view needs to authentically display the DAG, including
    /// multiple heads - one per Group".
    #[tokio::test]
    async fn gm_branch_graph_shows_a_split_as_a_retired_source_and_two_new_heads() {
        let db = database();
        let store = Store::open(db.path()).await.unwrap();
        let owner = store
            .find_or_create_user("warden@example.test", "Warden")
            .await
            .unwrap();
        let scenario = Scenario::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/scenarios/bread_thief.json"
        ))
        .unwrap();
        let installed = store.install_scenario(&owner, &scenario).await.unwrap();
        let original_group_id = store
            .character_group_id(installed.member.character_id)
            .await
            .unwrap();
        let toma_id: i64 = sqlx::query_scalar("SELECT id FROM character WHERE name = 'Toma'")
            .fetch_one(&store.pool())
            .await
            .unwrap();
        let mara_id: i64 = sqlx::query_scalar("SELECT id FROM character WHERE name = 'Mara'")
            .fetch_one(&store.pool())
            .await
            .unwrap();
        let gm_history = store.group_gm_history(original_group_id).await.unwrap();
        store
            .set_developer_mode(owner.id, installed.world_id, true)
            .await
            .unwrap();

        let trigger = store
            .begin_sequence(installed.world_id, "split")
            .await
            .unwrap();
        let invocation = store
            .start_invocation(
                Some(trigger.id),
                gm_history.agent_id,
                Some(gm_history.branch_id),
                None,
                None,
            )
            .await
            .unwrap();
        let inference = store
            .start_inference(InferenceInput {
                agent_id: gm_history.agent_id,
                sequence_id: Some(trigger.id),
                invocation_id: invocation,
                segments: &[],
                request: &Request {
                    messages: vec![Message::text(Role::User, "split")],
                    tools: Vec::new(),
                    sampling: Sampling::default(),
                },
                model: "test-model",
            })
            .await
            .unwrap();
        let executions = store.create_tool_executions(inference, 1).await.unwrap();
        let source_through_position: i64 =
            sqlx::query_scalar("SELECT next_position - 1 FROM history_branch WHERE id = ?")
                .bind(gm_history.branch_id)
                .fetch_one(&store.pool())
                .await
                .unwrap();
        store
            .apply_topology_changes(
                inference,
                vec![(
                    "split-1".to_string(),
                    executions[0],
                    tools::TopologyChange::Split {
                        group_id: original_group_id,
                        partitions: vec![
                            tools::GroupPartition {
                                character_ids: vec![installed.member.character_id, mara_id],
                                name: "player side".into(),
                                description: "outside".into(),
                            },
                            tools::GroupPartition {
                                character_ids: vec![toma_id],
                                name: "Toma side".into(),
                                description: "inside".into(),
                            },
                        ],
                        continue_with_character_id: Some(installed.member.character_id),
                    },
                )],
            )
            .await
            .unwrap();

        let player_group_id = store
            .character_group_id(installed.member.character_id)
            .await
            .unwrap();
        let toma_group_id = store.character_group_id(toma_id).await.unwrap();
        let player_branch_id = store
            .group_gm_history(player_group_id)
            .await
            .unwrap()
            .branch_id;
        let toma_branch_id = store
            .group_gm_history(toma_group_id)
            .await
            .unwrap()
            .branch_id;

        let nodes = store
            .gm_branch_graph(owner.id, installed.world_id, gm_history.agent_id)
            .await
            .unwrap();
        assert_eq!(
            nodes.len(),
            3,
            "the retired source branch plus its two split children: {nodes:?}"
        );

        let source_node = nodes
            .iter()
            .find(|node| node.branch_id == gm_history.branch_id)
            .expect("the source branch must still appear in the graph");
        assert!(
            source_node.parents.is_empty(),
            "the root branch has no parents: {nodes:?}"
        );
        assert!(
            source_node.head.is_none(),
            "the split's source group is retired, so it has no current head: {nodes:?}"
        );

        for (branch_id, expected_group_id) in [
            (player_branch_id, player_group_id),
            (toma_branch_id, toma_group_id),
        ] {
            let node = nodes
                .iter()
                .find(|node| node.branch_id == branch_id)
                .unwrap_or_else(|| panic!("split child branch {branch_id} must appear: {nodes:?}"));
            assert_eq!(
                node.parents,
                vec![BranchParent {
                    branch_id: gm_history.branch_id,
                    through_position: source_through_position
                }],
                "each split child's one parent is the exact source cut"
            );
            let head = node
                .head
                .as_ref()
                .expect("each split child is a current group head");
            assert_eq!(head.group_id, expected_group_id);
        }
    }

    #[tokio::test]
    async fn a_summary_without_a_role_prompt_becomes_the_leading_system_message() {
        let (store, _db, agent, segments, request) = store_with_history().await;
        complete_summary(&store, agent, &segments, &request, "Earlier events.", 0)
            .await
            .unwrap();
        // A fresh event follows the summary, as one always does in production.
        store
            .append_message(agent, &Message::text(Role::User, "And then?"))
            .await
            .unwrap();
        let request = store
            .request_for_segments(
                agent,
                &store
                    .live_segments(store.agent_branch(agent).await.unwrap())
                    .await
                    .unwrap(),
                Sampling {
                    temperature: 0.7,
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        // One system message, first, carrying the marked summary.
        assert!(matches!(
            &request.messages[0],
            Message { role: Role::System, content: MessageContent::Text(text), .. }
                if text.contains("Earlier events.")
        ));
        assert!(
            request.messages[1..]
                .iter()
                .all(|message| message.role != Role::System)
        );
    }

    #[tokio::test]
    async fn summary_recipe_reconstructs_and_rejects_cross_agent_summary() {
        let (store, _db, agent, segments, request) = store_with_history().await;
        let summary = complete_summary(&store, agent, &segments, &request, "Earlier events.", 0)
            .await
            .unwrap();
        let new_message = store
            .append_message(agent, &Message::text(Role::User, "And then?"))
            .await
            .unwrap();
        let branch_id = store.branch_id_for_agent(agent).await.unwrap();
        let new_position: i64 =
            sqlx::query_scalar("SELECT position FROM history_entry WHERE id = ?")
                .bind(new_message)
                .fetch_one(&store.pool)
                .await
                .unwrap();
        let recipe = vec![
            Segment::Summary { summary },
            Segment::Messages {
                messages: PositionRange {
                    branch_id,
                    first_position: new_position,
                    last_position: new_position,
                },
            },
        ];
        let summary_request = store
            .request_for_segments(
                agent,
                &recipe,
                Sampling {
                    temperature: 0.7,
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        let recorded = record_inference(
            &store,
            agent,
            &recipe,
            &summary_request,
            InferenceOutcome::Response(response()),
            "test-model",
            14,
        )
        .await
        .unwrap();
        assert_eq!(
            store.reconstruct_inference(recorded).await.unwrap().request,
            summary_request
        );

        let world: i64 = sqlx::query_scalar("SELECT world_id FROM agent WHERE id = ?")
            .bind(agent)
            .fetch_one(&store.pool)
            .await
            .unwrap();
        let other = store.create_agent(world).await.unwrap();
        // Isolate the summary-ownership check from check_message_range_ownership's
        // now-batched, up-front message-range check (which would otherwise
        // reject this recipe first, for the unrelated reason that its
        // message range also belongs to `agent`, not `other`): reference
        // only the cross-agent summary here.
        let summary_only_recipe = vec![Segment::Summary { summary }];
        let error = store
            .request_for_segments(
                other,
                &summary_only_recipe,
                Sampling {
                    temperature: 0.7,
                    ..Default::default()
                },
            )
            .await
            .unwrap_err();
        assert!(format!("{error:#}").contains("references summary"));
    }

    #[tokio::test]
    async fn an_assembled_request_must_be_a_well_formed_conversation() {
        let (store, _db, agent, ..) = store_with_history().await;
        let branch_id = store.branch_id_for_agent(agent).await.unwrap();
        let prompt = store.store_prompt_text("Guide.").await.unwrap();
        let system = Segment::Text {
            text: prompt,
            role: Role::System,
        };
        let history = Segment::Messages {
            messages: PositionRange {
                branch_id,
                first_position: 0,
                last_position: 1,
            },
        };
        let sampling = Sampling {
            temperature: 0.0,
            ..Default::default()
        };

        // A second system message anywhere but first is rejected.
        let error = store
            .request_for_segments(
                agent,
                &[system.clone(), history.clone(), system.clone()],
                sampling.clone(),
            )
            .await
            .unwrap_err();
        assert!(format!("{error:#}").contains("not only first"));

        // A request with no user turn is rejected.
        let assistant_only = Segment::Messages {
            messages: PositionRange {
                branch_id,
                first_position: 1,
                last_position: 1,
            },
        };
        let error = store
            .request_for_segments(agent, &[system.clone(), assistant_only], sampling.clone())
            .await
            .unwrap_err();
        assert!(format!("{error:#}").contains("no user turn"));

        // A leading system message plus a user turn is accepted.
        store
            .request_for_segments(agent, &[system, history], sampling)
            .await
            .unwrap();
    }

    // --- Developer Mode inspection ------------------------------------------

    /// An installed Bread Thief world with the owner and one other active
    /// member, plus their user records.
    async fn inspectable_world() -> (Store, super::TestDatabase, i64, User, User) {
        let db = database();
        let store = Store::open(db.path()).await.unwrap();
        let owner = store
            .find_or_create_user("owner@example.test", "Owner")
            .await
            .unwrap();
        let scenario = Scenario::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/scenarios/bread_thief.json"
        ))
        .unwrap();
        let installed = store.install_scenario(&owner, &scenario).await.unwrap();
        let player = store
            .find_or_create_user("player@example.test", "Player")
            .await
            .unwrap();
        let invitation = store
            .create_invitation(owner.id, installed.world_id, None)
            .await
            .unwrap();
        store
            .accept_invitation(&player, &invitation.token)
            .await
            .unwrap();
        (store, db, installed.world_id, owner, player)
    }

    #[tokio::test]
    async fn only_the_owner_toggles_developer_mode_in_either_direction() {
        let (store, _db, world_id, owner, player) = inspectable_world().await;
        assert!(!store.world(world_id).await.unwrap().unwrap().developer_mode);

        store
            .set_developer_mode(player.id, world_id, true)
            .await
            .expect_err("a non-owner cannot toggle developer mode");
        assert!(!store.world(world_id).await.unwrap().unwrap().developer_mode);

        store
            .set_developer_mode(owner.id, world_id, true)
            .await
            .unwrap();
        assert!(store.world(world_id).await.unwrap().unwrap().developer_mode);
        // Idempotent, and reversible - it is a toggle, not one-way.
        store
            .set_developer_mode(owner.id, world_id, true)
            .await
            .unwrap();
        store
            .set_developer_mode(owner.id, world_id, false)
            .await
            .unwrap();
        assert!(!store.world(world_id).await.unwrap().unwrap().developer_mode);
    }

    #[tokio::test]
    async fn inspection_requires_active_membership_and_enabled_mode() {
        let (store, _db, world_id, owner, player) = inspectable_world().await;
        let stranger = store
            .find_or_create_user("stranger@example.test", "Stranger")
            .await
            .unwrap();

        // Disabled: even an active member cannot inspect.
        store
            .world_sequences(player.id, world_id, None, 50)
            .await
            .expect_err("developer mode is off");

        store
            .set_developer_mode(owner.id, world_id, true)
            .await
            .unwrap();

        // Enabled: every active member can, owner and player alike.
        store
            .world_sequences(owner.id, world_id, None, 50)
            .await
            .unwrap();
        store
            .world_sequences(player.id, world_id, None, 50)
            .await
            .unwrap();

        // A non-member cannot, enabled or not.
        store
            .world_sequences(stranger.id, world_id, None, 50)
            .await
            .expect_err("a non-member cannot inspect");

        // An inactive (removed) member cannot, even with developer mode on.
        sqlx::query(
            "UPDATE world_member SET access = 'removed' WHERE user_id = ? AND world_id = ?",
        )
        .bind(player.id)
        .bind(world_id)
        .execute(&store.pool)
        .await
        .unwrap();
        store
            .world_agents(player.id, world_id)
            .await
            .expect_err("a removed member loses inspection access");
    }

    #[tokio::test]
    async fn inspection_rejects_a_record_from_another_world() {
        let (store, _db, world_id, owner, _player) = inspectable_world().await;
        store
            .set_developer_mode(owner.id, world_id, true)
            .await
            .unwrap();
        // A second world with its own sequence and inference.
        let other_world = store.create_world("other").await.unwrap();
        let other_agent = store.create_agent(other_world).await.unwrap();
        let other_sequence = store
            .begin_sequence(other_world, "player message")
            .await
            .unwrap();
        let other_invocation = store
            .start_invocation(Some(other_sequence.id), other_agent, None, None, None)
            .await
            .unwrap();
        let other_inference = store
            .start_inference(InferenceInput {
                agent_id: other_agent,
                sequence_id: Some(other_sequence.id),
                invocation_id: other_invocation,
                segments: &[],
                request: &Request {
                    messages: vec![Message::text(Role::User, "hi")],
                    tools: Vec::new(),
                    sampling: Sampling::default(),
                },
                model: "test",
            })
            .await
            .unwrap();

        let sequence_error = store
            .sequence_tree(owner.id, world_id, other_sequence.id)
            .await
            .unwrap_err();
        assert!(format!("{sequence_error:#}").contains("is not in world"));
        let inference_error = store
            .inference_evidence(owner.id, world_id, other_inference)
            .await
            .unwrap_err();
        assert!(format!("{inference_error:#}").contains("is not in world"));
        let agent_error = store
            .first_raw_history_cursor(owner.id, world_id, other_agent, None)
            .await
            .unwrap_err();
        assert!(format!("{agent_error:#}").contains("is not in world"));

        let local_agent = store.create_agent(world_id).await.unwrap();
        let foreign_branch = Store::branch_for_agent(&store, other_agent).await.unwrap();
        let branch_error = store
            .branch_for_viewer(owner.id, world_id, local_agent, foreign_branch)
            .await
            .unwrap_err();
        assert!(
            format!("{branch_error:#}").contains("does not belong to agent"),
            "a browser cannot select another agent's branch: {branch_error:#}"
        );
    }

    #[tokio::test]
    async fn sequence_tree_resolves_a_recorded_turn() {
        let (store, _db, world_id, owner, _player) = inspectable_world().await;
        store
            .set_developer_mode(owner.id, world_id, true)
            .await
            .unwrap();
        let agent = store.create_agent(world_id).await.unwrap();
        store
            .append_message(agent, &Message::text(Role::User, "I search the room."))
            .await
            .unwrap();
        let prompt = store
            .store_prompt_text("You guide the players.")
            .await
            .unwrap();
        let segments = vec![
            Segment::Text {
                text: prompt,
                role: Role::System,
            },
            store
                .live_segments(store.agent_branch(agent).await.unwrap())
                .await
                .unwrap()
                .pop()
                .unwrap(),
        ];
        let recipe_request = store
            .request_for_segments(agent, &segments, Sampling::default())
            .await
            .unwrap();
        let sequence = store
            .begin_sequence(world_id, "player message")
            .await
            .unwrap();
        let root_invocation = store
            .start_invocation(Some(sequence.id), agent, None, None, None)
            .await
            .unwrap();
        let parent = store
            .start_inference(InferenceInput {
                agent_id: agent,
                sequence_id: Some(sequence.id),
                invocation_id: root_invocation,
                segments: &segments,
                request: &recipe_request,
                model: "test",
            })
            .await
            .unwrap();
        // The parent's own output is a tool call; a nested invocation records
        // the exact tool_execution that spawned it (design.md).
        store
            .finish_inference(
                parent,
                InferenceOutcome::Response(Response {
                    content: Content::ToolCalls(vec![ToolCall {
                        id: "call-1".to_string(),
                        name: "search".to_string(),
                        arguments: "{}".to_string(),
                    }]),
                    ..response()
                }),
                1,
            )
            .await
            .unwrap();
        let parent_tool_execution_id = store.create_tool_executions(parent, 1).await.unwrap()[0];
        let child_invocation = store
            .start_invocation(
                Some(sequence.id),
                agent,
                None,
                Some(parent_tool_execution_id),
                None,
            )
            .await
            .unwrap();
        let child = store
            .start_inference(InferenceInput {
                agent_id: agent,
                sequence_id: Some(sequence.id),
                invocation_id: child_invocation,
                segments: &segments,
                request: &recipe_request,
                model: "test",
            })
            .await
            .unwrap();
        store
            .finish_inference(
                child,
                InferenceOutcome::Response(Response {
                    content: Content::Text("The key is iron.".into()),
                    ..response()
                }),
                1,
            )
            .await
            .unwrap();

        let tree = store
            .sequence_tree(owner.id, world_id, sequence.id)
            .await
            .unwrap();
        assert_eq!(tree.len(), 2);
        assert_eq!(tree[0].inference.id, parent);
        assert_eq!(tree[0].inference.parent_inference_id, None);
        assert_eq!(tree[1].inference.id, child);
        assert_eq!(
            tree[1].inference.parent_inference_id,
            Some(parent),
            "the child inference's parent resolves through its invocation's parent_tool_execution_id"
        );
    }

    #[tokio::test]
    async fn raw_history_interleaves_summaries_and_failures_in_order() {
        let (store, _db, world_id, owner, _player) = inspectable_world().await;
        store
            .set_developer_mode(owner.id, world_id, true)
            .await
            .unwrap();
        let agent = store.create_agent(world_id).await.unwrap();
        for turn in 0..4 {
            store
                .append_message(agent, &Message::text(Role::User, format!("q{turn}")))
                .await
                .unwrap();
        }
        // A failed inference for this agent.
        let failed_invocation = store
            .start_invocation(None, agent, None, None, None)
            .await
            .unwrap();
        let failed = store
            .start_inference(InferenceInput {
                agent_id: agent,
                sequence_id: None,
                invocation_id: failed_invocation,
                segments: &[],
                request: &Request {
                    messages: vec![Message::text(Role::User, "q1")],
                    tools: Vec::new(),
                    sampling: Sampling::default(),
                },
                model: "test",
            })
            .await
            .unwrap();
        store
            .finish_inference(failed, InferenceOutcome::Error("model exploded".into()), 1)
            .await
            .unwrap();
        // A compaction covering the first two messages.
        let summary_invocation = store
            .start_invocation(None, agent, None, None, None)
            .await
            .unwrap();
        let summary_inference = store
            .start_inference(InferenceInput {
                agent_id: agent,
                sequence_id: None,
                invocation_id: summary_invocation,
                segments: &[],
                request: &Request {
                    messages: vec![Message::text(Role::User, "summarise")],
                    tools: Vec::new(),
                    sampling: Sampling::default(),
                },
                model: "test",
            })
            .await
            .unwrap();
        let branch_id = store.branch_id_for_agent(agent).await.unwrap();
        store
            .enqueue_capacity_compaction(
                agent,
                Some(branch_id),
                1,
                CompactionPolicy {
                    sequence_id: None,
                    compact_before_next_input_tokens: 1,
                    sampling: &Sampling::default(),
                    model: "test",
                    static_segments: &[],
                },
            )
            .await
            .unwrap();
        let job = store
            .pending_compaction_for_branch(branch_id)
            .await
            .unwrap()
            .unwrap();
        store
            .finish_summary_inference(
                summary_inference,
                &job,
                InferenceOutcome::Response(Response {
                    content: Content::Text("q0 and q1 happened.".into()),
                    ..response()
                }),
                1,
                Some(&[PositionRange {
                    branch_id,
                    first_position: 0,
                    last_position: 1,
                }]),
                ("threshold", 2),
            )
            .await
            .unwrap();

        let (page, cursor) = store
            .agent_raw_history(
                owner.id,
                store
                    .first_raw_history_cursor(owner.id, world_id, agent, None)
                    .await
                    .unwrap(),
                50,
            )
            .await
            .unwrap();
        assert_eq!(cursor, None, "a full page has nothing left to fetch");
        // Every raw message is present - compaction does not remove them.
        let messages: Vec<i64> = page
            .iter()
            .filter_map(|entry| match entry {
                RawHistoryEntry::Message { id, .. } => Some(*id),
                _ => None,
            })
            .collect();
        assert_eq!(messages.len(), 4, "raw history keeps every message");
        assert!(
            page.iter().any(|entry| matches!(
                entry,
                RawHistoryEntry::Summary { covered_ranges, .. }
                    if covered_ranges == &[PositionRange { branch_id, first_position: 0, last_position: 1 }]
            )),
            "the summary marker sits at its compaction point: {page:?}"
        );
        assert!(
            page.iter().any(|entry| matches!(entry, RawHistoryEntry::Inference { error: Some(error), .. } if error == "model exploded")),
            "the failed inference appears inline"
        );
        // Cursor paging returns the same total set in slices, with nothing
        // skipped or duplicated across a boundary that lands exactly on a
        // failure/summary entry, and the cursor round-trips.
        let (first_two, mid_cursor) = store
            .agent_raw_history(
                owner.id,
                store
                    .first_raw_history_cursor(owner.id, world_id, agent, None)
                    .await
                    .unwrap(),
                2,
            )
            .await
            .unwrap();
        assert_eq!(first_two.len(), 2);
        let mid_cursor = mid_cursor.expect("more entries remain after the first two");
        let mut paged = first_two;
        let mut cursor = Some(mid_cursor);
        loop {
            let (page, next) = store
                .agent_raw_history(
                    owner.id,
                    store
                        .resume_raw_history_cursor(owner.id, world_id, agent, cursor.unwrap())
                        .await
                        .unwrap(),
                    2,
                )
                .await
                .unwrap();
            if page.is_empty() {
                break;
            }
            paged.extend(page);
            cursor = next;
            if cursor.is_none() {
                break;
            }
        }
        let (whole, _) = store
            .agent_raw_history(
                owner.id,
                store
                    .first_raw_history_cursor(owner.id, world_id, agent, None)
                    .await
                    .unwrap(),
                50,
            )
            .await
            .unwrap();
        assert_eq!(
            paged, whole,
            "paging through the cursor must see the exact same entries, in the same order, as one page"
        );
    }

    /// Two agents with very different message/inference counts must page
    /// completely independently: pagination for one never leaks into, skips
    /// against, or is bounded by the other's history.
    #[tokio::test]
    async fn diverging_agent_histories_page_independently() {
        let (store, _db, world_id, owner, _player) = inspectable_world().await;
        store
            .set_developer_mode(owner.id, world_id, true)
            .await
            .unwrap();
        let quiet = store.create_agent(world_id).await.unwrap();
        let busy = store.create_agent(world_id).await.unwrap();
        store
            .append_message(quiet, &Message::text(Role::User, "only message"))
            .await
            .unwrap();
        for turn in 0..9 {
            store
                .append_message(busy, &Message::text(Role::User, format!("q{turn}")))
                .await
                .unwrap();
        }

        let (quiet_page, quiet_cursor) = store
            .agent_raw_history(
                owner.id,
                store
                    .first_raw_history_cursor(owner.id, world_id, quiet, None)
                    .await
                    .unwrap(),
                50,
            )
            .await
            .unwrap();
        assert_eq!(quiet_page.len(), 1, "the quiet agent has exactly one entry");
        assert_eq!(quiet_cursor, None);

        let (busy_first, busy_cursor) = store
            .agent_raw_history(
                owner.id,
                store
                    .first_raw_history_cursor(owner.id, world_id, busy, None)
                    .await
                    .unwrap(),
                4,
            )
            .await
            .unwrap();
        assert_eq!(busy_first.len(), 4);
        let busy_cursor = busy_cursor.expect("more of the busy agent's history remains");
        let (busy_rest, busy_cursor) = store
            .agent_raw_history(
                owner.id,
                store
                    .resume_raw_history_cursor(owner.id, world_id, busy, busy_cursor)
                    .await
                    .unwrap(),
                50,
            )
            .await
            .unwrap();
        assert_eq!(busy_first.len() + busy_rest.len(), 9);
        assert_eq!(
            busy_cursor, None,
            "the busy agent's page must end on its own count, unaffected by the quiet agent"
        );
        assert!(
            busy_first.iter().chain(busy_rest.iter()).all(
                |entry| matches!(entry, RawHistoryEntry::Message { content, .. }
                    if matches!(content, MessageContent::Text(text) if text != "only message"))
            ),
            "the busy agent's page must never contain the quiet agent's entry"
        );
    }

    /// design.md's paging contract: an unknown cursor position is an error,
    /// never silently treated as end-of-history (the previous
    /// `unwrap_or(all_entries.len())` behavior this replaced).
    #[tokio::test]
    async fn agent_raw_history_rejects_a_cursor_position_outside_the_reach() {
        let (store, _db, world_id, owner, _player) = inspectable_world().await;
        store
            .set_developer_mode(owner.id, world_id, true)
            .await
            .unwrap();
        let agent = store.create_agent(world_id).await.unwrap();
        store
            .append_message(agent, &Message::text(Role::User, "only message"))
            .await
            .unwrap();
        let branch_id = Store::branch_for_agent(&store, agent).await.unwrap();
        // A position well past this agent's one real entry, on its real branch.
        let bogus_cursor = RawHistoryCursor {
            branch_id,
            cut: 0,
            next_branch_id: branch_id,
            next_position: 999,
        };
        let error = store
            .resume_raw_history_cursor(owner.id, world_id, agent, bogus_cursor)
            .await
            .unwrap_err();
        assert!(
            format!("{error:#}").contains("outside this history"),
            "{error:#}"
        );
    }

    #[tokio::test]
    async fn world_objects_page_stably_filters_and_links_current_state() {
        let (store, _db, world_id, owner, _player) = inspectable_world().await;
        store
            .set_developer_mode(owner.id, world_id, true)
            .await
            .unwrap();
        let objects = store
            .world_objects(
                owner.id,
                world_id,
                WorldObjectsQuery {
                    object_type: None,
                    object_id: None,
                    search: None,
                    after: None,
                    limit: 50,
                },
            )
            .await
            .unwrap();
        assert!(
            objects
                .objects
                .iter()
                .any(|object| object.object_type == ObjectType::Character),
            "the scenario has characters"
        );
        assert!(
            objects
                .objects
                .iter()
                .any(|object| object.object_type == ObjectType::Location),
            "the scenario has locations"
        );
        assert!(
            objects
                .objects
                .iter()
                .any(|object| object.object_type == ObjectType::Item),
            "the scenario has items"
        );
        assert!(
            !objects
                .objects
                .iter()
                .any(|object| object.object_type == ObjectType::Connection),
            "the single-location Bread Thief scenario has no connections"
        );
        // GM and Storyteller notes are exposed - the NPCs carry seeded ones.
        assert!(
            objects
                .objects
                .iter()
                .any(|object| object.gm_notes.contains("dialogue")),
            "a seeded NPC's GM notes are exposed for inspection"
        );
        let item = objects
            .objects
            .iter()
            .find(|object| object.object_type == ObjectType::Item)
            .unwrap();
        assert!(
            item.relationships
                .iter()
                .any(|link| link.object_type == ObjectType::ItemType)
                && item.relationships.iter().any(|link| matches!(
                    link.object_type,
                    ObjectType::Location | ObjectType::Character
                )),
            "an item links to its type and current holder"
        );
        let exact_item = store
            .world_objects(
                owner.id,
                world_id,
                WorldObjectsQuery {
                    object_type: Some(ObjectType::Item),
                    object_id: Some(item.id),
                    search: None,
                    after: None,
                    limit: 50,
                },
            )
            .await
            .unwrap();
        assert_eq!(
            exact_item.objects.len(),
            1,
            "a typed object URL selects one object"
        );
        let first = store
            .world_objects(
                owner.id,
                world_id,
                WorldObjectsQuery {
                    object_type: None,
                    object_id: None,
                    search: None,
                    after: None,
                    limit: 1,
                },
            )
            .await
            .unwrap();
        let cursor = first
            .cursor
            .clone()
            .expect("the scenario spans more than one page");
        let second = store
            .world_objects(
                owner.id,
                world_id,
                WorldObjectsQuery {
                    object_type: None,
                    object_id: None,
                    search: None,
                    after: Some(cursor),
                    limit: 1,
                },
            )
            .await
            .unwrap();
        assert_ne!(
            (first.objects[0].object_type, first.objects[0].id),
            (second.objects[0].object_type, second.objects[0].id),
            "the next cursor advances the stable type/id index"
        );
        // The world's current time is present state, distinct from any turn's
        // recorded segments. `WorldObjects` is not a `Segment`, so the type
        // system already prevents `request_for_segments` from consuming it.
        assert_eq!(objects.time, 0, "a fresh world starts at time 0");
    }

    #[tokio::test]
    async fn world_agents_lists_every_chat_agent_with_its_role() {
        let (store, _db, world_id, owner, _player) = inspectable_world().await;
        store
            .set_developer_mode(owner.id, world_id, true)
            .await
            .unwrap();
        let agents = store.world_agents(owner.id, world_id).await.unwrap();
        assert!(
            agents.iter().any(|agent| agent.role == "player"),
            "the owner's player agent is listed"
        );
        assert!(
            agents.iter().any(|agent| agent.role == "world_gm"),
            "the world GM agent is listed"
        );
        assert!(
            agents.iter().all(|agent| agent.role != "unknown"),
            "every agent resolves to a known game role"
        );
    }
}
