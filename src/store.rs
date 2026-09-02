use std::{collections::HashMap, path::Path};

use anyhow::{Context, Result, ensure};
use rand::{Rng, distr::Alphanumeric};
use serde::{Deserialize, Serialize};
use sqlx::{
    FromRow, Sqlite, SqlitePool, Transaction,
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions},
};

use crate::llm::{
    Message, MessageContent, Request, Response, Role, Sampling, ToolDefinition, Usage,
};
use crate::scenario::{Item, ItemType, Location, Notes, Npc, Path as ScenarioPath, Scenario};

#[derive(Clone)]
pub struct Store {
    pool: SqlitePool,
}

#[derive(Clone, Debug, FromRow, PartialEq)]
pub struct User {
    pub id: i64,
    pub email: String,
    pub display_name: String,
}

#[derive(Clone, Debug, FromRow, PartialEq)]
pub struct MemberAgent {
    pub member_id: i64,
    pub world_id: i64,
    pub user_id: i64,
    pub agent_id: i64,
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
}

#[derive(Clone, Debug, FromRow, PartialEq)]
pub struct WorldMember {
    pub user_id: i64,
    pub display_name: String,
    pub access: String,
    pub character_name: String,
}

/// A text entry safe to render in the player-facing chat. Tool calls and tool
/// results remain in the stored agent history but never cross this boundary.
#[derive(Clone, Debug, PartialEq)]
pub struct PlayerChatEntry {
    pub id: i64,
    pub role: Role,
    pub text: String,
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
    pub inference_id: i64,
    pub character_id: i64,
    pub location_gm_agent_id: i64,
    pub tool: String,
    pub args: String,
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
    pub items: Vec<SceneItem>,
    pub npcs: Vec<GmSceneNpc>,
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

#[derive(FromRow)]
struct GmSceneNpcRow {
    name: String,
    description: String,
    background: String,
    motive: String,
    ambition: String,
    gm_notes: String,
}

#[derive(Debug, FromRow)]
struct PendingActionRow {
    world_id: i64,
    id: i64,
    sequence_id: i64,
    inference_id: i64,
    character_id: i64,
    location_gm_agent_id: i64,
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
            inference_id: row.inference_id,
            character_id: row.character_id,
            location_gm_agent_id: row.location_gm_agent_id,
            tool: row.tool,
            args: row.args,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct InstalledWorld {
    pub world_id: i64,
    pub member: MemberAgent,
    pub character_handle: String,
}

struct JoinedWorld {
    member_id: i64,
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

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(untagged)]
pub enum Segment {
    Text { text: i64, role: Role },
    Tools { text: i64 },
    Summary { summary: i64 },
    Messages { messages: MessageRange },
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct MessageRange {
    pub agent_id: i64,
    pub first_seq: i64,
    pub last_seq: i64,
}

#[derive(Debug)]
pub enum InferenceOutcome {
    Response(Response),
    Error(String),
}

/// The complete stored record of one model invocation.
pub struct InferenceRecord<'a> {
    pub agent_id: i64,
    pub sequence_id: Option<i64>,
    pub parent_inference_id: Option<i64>,
    pub segments: &'a [Segment],
    pub request: &'a Request,
    pub outcome: InferenceOutcome,
    pub model: &'a str,
    pub duration_ms: u64,
}

#[derive(Debug, PartialEq)]
pub enum RecordedOutcome {
    Response(Response),
    Error(String),
}

#[derive(Debug, PartialEq)]
pub struct RecordedInference {
    pub id: i64,
    pub agent_id: i64,
    pub sequence_id: Option<i64>,
    pub parent_inference_id: Option<i64>,
    pub segments: Vec<Segment>,
    pub request: Request,
    pub outcome: RecordedOutcome,
    pub model: String,
    pub duration_ms: u64,
}

#[derive(Debug, FromRow)]
struct InferenceRow {
    id: i64,
    agent_id: i64,
    sequence_id: Option<i64>,
    parent_inference_id: Option<i64>,
    segments: String,
    sampling: String,
    output: Option<String>,
    error: Option<String>,
    input_hash: String,
    input_tokens: Option<i64>,
    output_tokens: Option<i64>,
    duration_ms: i64,
    model: String,
}

#[derive(Debug, FromRow)]
struct MessageRow {
    seq: i64,
    role: String,
    content: String,
}

#[derive(Debug, FromRow)]
struct SummaryRow {
    id: i64,
    agent_id: i64,
    covers_to_seq: i64,
    content: String,
    inference_id: i64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Summary {
    pub id: i64,
    pub agent_id: i64,
    pub covers_to_seq: i64,
    pub content: String,
    pub inference_id: i64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PendingCompaction {
    pub agent_id: i64,
    pub after_message_id: i64,
    pub next_input_tokens: usize,
    pub sampling: Sampling,
    pub model: String,
    /// Static prompt and tool segments from the normal request that queued
    /// this job. Raw history is always rebuilt from its current stored form.
    pub static_segments: Vec<Segment>,
}

/// A completed agent reply and the factual usage that determines whether its
/// successor needs deferred compaction.
pub struct CompletedReply<'a> {
    pub message: &'a Message,
    /// Exact next-context size reported by the completed inference: its input
    /// plus generated output. This drives the normal, zero-tokenizer
    /// compaction path.
    pub next_input_tokens: usize,
    pub compact_before_next_input_tokens: usize,
    pub sampling: &'a Sampling,
    pub model: &'a str,
    pub static_segments: &'a [Segment],
}

#[derive(Debug, FromRow)]
struct PendingCompactionRow {
    agent_id: i64,
    after_message_id: i64,
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
            after_message_id: row.after_message_id,
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
        Ok(Self { pool })
    }

    /// Sessions and game records intentionally share SQLite's durability and
    /// process lifetime. The session crate owns its session tables; Store owns
    /// every Cairnworld table.
    pub fn pool(&self) -> SqlitePool {
        self.pool.clone()
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
        let result = sqlx::query("INSERT INTO agent (world_id) VALUES (?)")
            .bind(world_id)
            .execute(&self.pool)
            .await
            .with_context(|| format!("creating agent in world {world_id}"))?;
        Ok(result.last_insert_rowid())
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
    pub async fn accept_invitation(&self, user: &User, token: &str) -> Result<MemberAgent> {
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
        let existing: Option<MemberAgent> = sqlx::query_as(
            "SELECT member.id AS member_id, member.world_id, member.user_id, player.agent_id \
             FROM world_member AS member \
             JOIN member_player_agent AS player ON player.member_id = member.id \
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
        let starting_location_id: i64 = sqlx::query_scalar(
            "SELECT character_location.location_id FROM world_owner \
             JOIN world_member ON world_member.world_id = world_owner.world_id \
               AND world_member.user_id = world_owner.user_id \
             JOIN player_character ON player_character.member_id = world_member.id \
             JOIN character_location ON character_location.character_id = player_character.character_id \
             WHERE world_owner.world_id = ?",
        )
        .bind(invitation.world_id)
        .fetch_optional(&mut *transaction)
        .await
        .context("finding invitation world starting location")?
        .with_context(|| format!("world {} has no owner Adventurer location", invitation.world_id))?;
        let joined = Self::join_world(
            &mut transaction,
            user,
            invitation.world_id,
            starting_location_id,
        )
        .await
        .context("creating invited player membership")?;
        transaction
            .commit()
            .await
            .context("committing invitation acceptance")?;
        Ok(MemberAgent {
            member_id: joined.member_id,
            world_id: invitation.world_id,
            user_id: user.id,
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
            "SELECT world.id, world.name, world_owner.user_id AS owner_id FROM world \
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
            "SELECT world.id, world.name, world_owner.user_id AS owner_id FROM world \
             JOIN world_owner ON world_owner.world_id = world.id WHERE world.id = ?",
        )
        .bind(world_id)
        .fetch_optional(&self.pool)
        .await
        .with_context(|| format!("loading world {world_id}"))
    }

    pub async fn world_members(&self, world_id: i64) -> Result<Vec<WorldMember>> {
        sqlx::query_as(
            "SELECT member.user_id, user.display_name, member.access, character.name AS character_name \
             FROM world_member AS member \
             JOIN user ON user.id = member.user_id \
             JOIN player_character ON player_character.member_id = member.id \
             JOIN character ON character.id = player_character.character_id \
             WHERE member.world_id = ? ORDER BY member.id",
        )
        .bind(world_id)
        .fetch_all(&self.pool)
        .await
        .with_context(|| format!("loading members of world {world_id}"))
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
            let gm_agent_id = sqlx::query("INSERT INTO agent (world_id) VALUES (?)")
                .bind(world_id)
                .execute(&mut *transaction)
                .await
                .with_context(|| format!("creating GM for location {}", location.name))?
                .last_insert_rowid();
            sqlx::query("INSERT INTO location_gm (location_id, agent_id) VALUES (?, ?)")
                .bind(location_id)
                .bind(gm_agent_id)
                .execute(&mut *transaction)
                .await
                .with_context(|| format!("linking GM for location {}", location.name))?;
            locations.insert(location.name.as_str(), location_id);
        }

        for path in &scenario.paths {
            sqlx::query(
                "INSERT INTO path (world_id, from_location_id, to_location_id, travel_time, description, gm_notes, storyteller_notes) \
                 VALUES (?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(world_id)
            .bind(locations[path.from.as_str()])
            .bind(locations[path.to.as_str()])
            .bind(path.travel_time)
            .bind(&path.description)
            .bind(serde_json::to_string(&path.notes.gm)?)
            .bind(serde_json::to_string(&path.notes.storyteller)?)
            .execute(&mut *transaction)
            .await
            .context("creating scenario path")?;
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
                "INSERT INTO character_location (character_id, location_id, description) VALUES (?, ?, '')",
            )
            .bind(character_id)
            .bind(locations[npc.location.as_str()])
            .execute(&mut *transaction)
            .await
            .with_context(|| format!("placing NPC {}", npc.name))?;
            let agent_id = sqlx::query("INSERT INTO agent (world_id) VALUES (?)")
                .bind(world_id)
                .execute(&mut *transaction)
                .await
                .with_context(|| format!("creating NPC agent for {}", npc.name))?
                .last_insert_rowid();
            sqlx::query("INSERT INTO npc_agent (character_id, agent_id) VALUES (?, ?)")
                .bind(character_id)
                .bind(agent_id)
                .execute(&mut *transaction)
                .await
                .with_context(|| format!("linking NPC agent for {}", npc.name))?;
        }

        let member = Self::join_world(
            &mut transaction,
            owner,
            world_id,
            locations[scenario.starting_location.as_str()],
        )
        .await?;
        transaction
            .commit()
            .await
            .with_context(|| format!("committing scenario world {world_id}"))?;
        Ok(InstalledWorld {
            world_id,
            character_handle: format!("char{}", member.character_tool_id),
            member: MemberAgent {
                member_id: member.member_id,
                world_id,
                user_id: owner.id,
                agent_id: member.agent_id,
            },
        })
    }

    /// The single access lookup used before a member may read or affect a world.
    pub async fn active_member_agent(
        &self,
        user_id: i64,
        world_id: i64,
    ) -> Result<Option<MemberAgent>> {
        sqlx::query_as(
            "SELECT member.id AS member_id, member.world_id, member.user_id, player.agent_id \
             FROM world_member AS member \
             JOIN member_player_agent AS player ON player.member_id = member.id \
             WHERE member.user_id = ? AND member.world_id = ? AND member.access = 'active'",
        )
        .bind(user_id)
        .bind(world_id)
        .fetch_optional(&self.pool)
        .await
        .with_context(|| {
            format!("resolving active membership for user {user_id} in world {world_id}")
        })
    }

    /// Load exactly the player-visible state for the member's current location.
    /// It rechecks the membership relation so a stale browser connection cannot
    /// retain a scene after its access has been removed.
    pub async fn player_scene(&self, member: &MemberAgent) -> Result<PlayerScene> {
        let (character_id, character_name, character_description, sheet, location_id, location_name, location_description):
            (i64, String, String, String, i64, String, String) = sqlx::query_as(
            "SELECT character.id, character.name, character.description, character.sheet, location.id, location.name, location.description \
             FROM world_member AS member \
             JOIN player_character ON player_character.member_id = member.id \
             JOIN character ON character.id = player_character.character_id \
             JOIN character_location ON character_location.character_id = character.id \
             JOIN location ON location.id = character_location.location_id \
             WHERE member.id = ? AND member.world_id = ? AND member.user_id = ? AND member.access = 'active'",
        )
        .bind(member.member_id)
        .bind(member.world_id)
        .bind(member.user_id)
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

    /// Resolve the active member's current location without exposing an
    /// internal database id in the model-facing scene packet.
    pub async fn member_location_id(&self, member: &MemberAgent) -> Result<i64> {
        sqlx::query_scalar(
            "SELECT character_location.location_id FROM world_member AS member \
             JOIN player_character ON player_character.member_id = member.id \
             JOIN character_location ON character_location.character_id = player_character.character_id \
             WHERE member.id = ? AND member.world_id = ? AND member.user_id = ? AND member.access = 'active'",
        )
        .bind(member.member_id)
        .bind(member.world_id)
        .bind(member.user_id)
        .fetch_optional(&self.pool)
        .await
        .context("loading member location")?
        .with_context(|| format!("member {} has no active location", member.member_id))
    }

    /// Resolve the current location's GM from the authenticated membership.
    /// The player agent never chooses which GM receives the opening request.
    pub async fn member_location_gm_agent_id(&self, member: &MemberAgent) -> Result<i64> {
        sqlx::query_scalar(
            "SELECT location_gm.agent_id FROM world_member AS member \
             JOIN player_character ON player_character.member_id = member.id \
             JOIN character_location ON character_location.character_id = player_character.character_id \
             JOIN location_gm ON location_gm.location_id = character_location.location_id \
             WHERE member.id = ? AND member.world_id = ? AND member.user_id = ? AND member.access = 'active'",
        )
        .bind(member.member_id)
        .bind(member.world_id)
        .bind(member.user_id)
        .fetch_optional(&self.pool)
        .await
        .context("loading member location GM")?
        .with_context(|| format!("member {} has no active location GM", member.member_id))
    }

    /// Load the text-only portion of one active membership's player-agent
    /// history in stored message order. This is used by the game page on
    /// every reload; it does not rely on a websocket's in-memory lifetime.
    pub async fn player_chat(&self, member: &MemberAgent) -> Result<Vec<PlayerChatEntry>> {
        self.player_chat_after(member, None).await
    }

    /// Load player-visible chat entries newer than a stored message cursor.
    /// A browser renders a snapshot, then asks the websocket for this delta so
    /// work completed between rendering and connection is neither lost nor
    /// duplicated.
    pub async fn player_chat_after(
        &self,
        member: &MemberAgent,
        after_message_id: Option<i64>,
    ) -> Result<Vec<PlayerChatEntry>> {
        let active: Option<i64> = sqlx::query_scalar(
            "SELECT 1 FROM world_member AS member \
             JOIN member_player_agent ON member_player_agent.member_id = member.id \
             WHERE member.id = ? AND member.world_id = ? AND member.user_id = ? \
             AND member_player_agent.agent_id = ? AND member.access = 'active'",
        )
        .bind(member.member_id)
        .bind(member.world_id)
        .bind(member.user_id)
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
        let rows: Vec<(i64, String, String)> = sqlx::query_as(
            "SELECT message.id, message.role, message.content FROM world_member AS member \
             JOIN member_player_agent ON member_player_agent.member_id = member.id \
             JOIN message ON message.agent_id = member_player_agent.agent_id \
             WHERE member.id = ? AND member.world_id = ? AND member.user_id = ? \
             AND member.access = 'active' AND message.id > ? ORDER BY message.id",
        )
        .bind(member.member_id)
        .bind(member.world_id)
        .bind(member.user_id)
        .bind(after_message_id.unwrap_or_default())
        .fetch_all(&self.pool)
        .await
        .context("loading player chat history")?;
        let entries: Vec<Option<PlayerChatEntry>> = rows
            .into_iter()
            .map(|(id, role, content)| {
                let role = serde_json::from_str(&role).context("decoding player chat role")?;
                let content =
                    serde_json::from_str(&content).context("decoding player chat content")?;
                let MessageContent::Text(text) = content else {
                    return Ok(None);
                };
                if role == Role::Assistant && text.is_empty() {
                    return Ok(None);
                }
                Ok(matches!(role, Role::User | Role::Assistant | Role::System)
                    .then_some(PlayerChatEntry { id, role, text }))
            })
            .collect::<Result<_>>()?;
        Ok(entries.into_iter().flatten().collect())
    }

    /// Persist a resolved location narration in every active player-agent
    /// history currently at that location. Call this only after the triggering
    /// player agent has settled, so its tool-call/result sequence stays intact.
    pub async fn append_location_narration(
        &self,
        world_id: i64,
        location_id: i64,
        narration: &str,
    ) -> Result<()> {
        let agents: Vec<i64> = sqlx::query_scalar(
            "SELECT member_player_agent.agent_id FROM world_member \
             JOIN member_player_agent ON member_player_agent.member_id = world_member.id \
             JOIN player_character ON player_character.member_id = world_member.id \
             JOIN character_location ON character_location.character_id = player_character.character_id \
             WHERE world_member.world_id = ? AND world_member.access = 'active' \
             AND character_location.location_id = ? ORDER BY member_player_agent.agent_id",
        )
        .bind(world_id)
        .bind(location_id)
        .fetch_all(&self.pool)
        .await
        .context("finding active player agents at narrated location")?;
        for agent_id in agents {
            self.append_message(agent_id, &Message::text(Role::System, narration))
                .await
                .with_context(|| {
                    format!("storing location narration for player agent {agent_id}")
                })?;
        }
        Ok(())
    }

    /// Load the context a location GM may use to judge one action. This is
    /// addressed through the location-GM relation, never a model-provided
    /// location id.
    pub async fn gm_scene(&self, world_id: i64, agent_id: i64) -> Result<GmScene> {
        let (location_id, location_name, location_description, gm_notes): (
            i64,
            String,
            String,
            String,
        ) = sqlx::query_as(
            "SELECT location.id, location.name, location.description, location.gm_notes \
             FROM location_gm JOIN location ON location.id = location_gm.location_id \
             WHERE location_gm.agent_id = ? AND location.world_id = ?",
        )
        .bind(agent_id)
        .bind(world_id)
        .fetch_optional(&self.pool)
        .await
        .context("loading location GM scene")?
        .with_context(|| format!("agent {agent_id} is not a GM in world {world_id}"))?;
        Ok(GmScene {
            location_name,
            location_description,
            gm_notes: serde_json::from_str(&gm_notes).context("decoding location GM notes")?,
            items: Self::location_items(&self.pool, location_id).await?,
            npcs: Self::location_gm_npcs(&self.pool, location_id).await?,
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
            "SELECT character.name, character.description FROM character_location \
             JOIN character ON character.id = character_location.character_id \
             WHERE character_location.location_id = ? AND character.role = 'npc' ORDER BY character.id",
        )
        .bind(location_id)
        .fetch_all(pool)
        .await
        .context("loading location NPCs")
    }

    async fn location_gm_npcs(pool: &SqlitePool, location_id: i64) -> Result<Vec<GmSceneNpc>> {
        let rows: Vec<GmSceneNpcRow> = sqlx::query_as(
            "SELECT character.name, character.description, character.background, character.motive, character.ambition, character.gm_notes \
             FROM character_location JOIN character ON character.id = character_location.character_id \
             WHERE character_location.location_id = ? AND character.role = 'npc' ORDER BY character.id",
        )
        .bind(location_id)
        .fetch_all(pool)
        .await
        .context("loading location GM NPCs")?;
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

    /// Roll the Adventurer's starting Hit Protection once. Repeating a tool
    /// call returns the stored result instead of offering a reroll.
    pub async fn roll_hit_protection(
        &self,
        member: &MemberAgent,
        sequence_id: i64,
        inference_id: Option<i64>,
    ) -> Result<i64> {
        let mut transaction = self.pool.begin().await.context("starting HP roll")?;
        let (character_id, sheet): (i64, String) = Self::member_sheet(&mut transaction, member)
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
        Self::record_action(
            &mut transaction,
            sequence_id,
            inference_id,
            "roll_hit_protection",
            "{}",
            &format!(r#"{{"hp":{hp}}}"#),
        )
        .await?;
        transaction.commit().await.context("committing HP roll")?;
        Ok(hp)
    }

    /// Roll STR, DEX, and WIL in order, once, using Cairn's 3d6 rule.
    pub async fn roll_attributes(
        &self,
        member: &MemberAgent,
        sequence_id: i64,
        inference_id: Option<i64>,
    ) -> Result<(i64, i64, i64)> {
        let mut transaction = self
            .pool
            .begin()
            .await
            .context("starting attribute rolls")?;
        let (character_id, sheet): (i64, String) = Self::member_sheet(&mut transaction, member)
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
        Self::record_action(
            &mut transaction,
            sequence_id,
            inference_id,
            "roll_attributes",
            "{}",
            &format!(r#"{{"str":{strength},"dex":{dexterity},"wil":{will}}}"#),
        )
        .await?;
        transaction
            .commit()
            .await
            .context("committing attribute rolls")?;
        Ok((strength, dexterity, will))
    }

    pub async fn ready_to_begin(
        &self,
        member: &MemberAgent,
        sequence_id: i64,
        inference_id: Option<i64>,
    ) -> Result<()> {
        let mut transaction = self.pool.begin().await.context("starting ready-to-begin")?;
        let (character_id, sheet): (i64, String) = Self::member_sheet(&mut transaction, member)
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
        Self::record_action(
            &mut transaction,
            sequence_id,
            inference_id,
            "ready_to_begin",
            "{}",
            r#"{"ready":true}"#,
        )
        .await?;
        transaction
            .commit()
            .await
            .context("committing ready-to-begin")
    }

    pub async fn is_ready_to_begin(&self, member: &MemberAgent) -> Result<bool> {
        let mut transaction = self
            .pool
            .begin()
            .await
            .context("checking Adventurer readiness")?;
        let (_, sheet) = Self::member_sheet(&mut transaction, member).await?;
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

    /// Retain a validated character action and assign the next world-local id
    /// before forwarding it to the location GM. The caller supplies only an
    /// already-resolved membership; this lookup determines its character and
    /// GM, so neither can be selected by model or browser input.
    pub async fn create_pending_action(
        &self,
        member: &MemberAgent,
        sequence_id: i64,
        inference_id: i64,
        tool: &str,
        args: &str,
    ) -> Result<PendingAction> {
        let mut transaction = self.pool.begin().await.with_context(|| {
            format!(
                "starting pending {tool} action for member {}",
                member.member_id
            )
        })?;
        let (character_id, location_gm_agent_id): (i64, i64) = sqlx::query_as(
            "SELECT character.id, location_gm.agent_id \
             FROM world_member AS member \
             JOIN member_player_agent AS player ON player.member_id = member.id \
             JOIN player_character ON player_character.member_id = member.id \
             JOIN character ON character.id = player_character.character_id \
             JOIN character_location ON character_location.character_id = character.id \
             JOIN location_gm ON location_gm.location_id = character_location.location_id \
             WHERE member.id = ? AND member.world_id = ? AND member.user_id = ? \
               AND member.access = 'active' AND player.agent_id = ?",
        )
        .bind(member.member_id)
        .bind(member.world_id)
        .bind(member.user_id)
        .bind(member.agent_id)
        .fetch_optional(&mut *transaction)
        .await
        .context("resolving action character and location GM")?
        .context("active membership has no playable character and location GM")?;
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
             (world_id, id, sequence_id, inference_id, character_id, location_gm_agent_id, tool, args) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(member.world_id)
        .bind(action_id)
        .bind(sequence_id)
        .bind(inference_id)
        .bind(character_id)
        .bind(location_gm_agent_id)
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
            inference_id,
            character_id,
            location_gm_agent_id,
            tool: tool.to_string(),
            args: args.to_string(),
        })
    }

    /// Resolve an action only from the GM relationship it was assigned to.
    /// Moving it into the immutable action log and retiring the pending row are
    /// one transaction, so a later call cannot approve it twice.
    pub async fn resolve_pending_action(
        &self,
        location_gm_agent_id: i64,
        world_id: i64,
        action_id: i64,
        inference_id: i64,
        result: &str,
    ) -> Result<PendingAction> {
        let mut transaction = self.pool.begin().await.with_context(|| {
            format!("resolving action {action_id} through GM {location_gm_agent_id}")
        })?;
        let action: PendingAction = sqlx::query_as::<_, PendingActionRow>(
            "SELECT world_id, id, sequence_id, inference_id, character_id, location_gm_agent_id, tool, args \
             FROM pending_action WHERE world_id = ? AND id = ? AND location_gm_agent_id = ?",
        )
        .bind(world_id)
        .bind(action_id)
        .bind(location_gm_agent_id)
        .fetch_optional(&mut *transaction)
        .await
        .context("loading pending action for location GM")?
        .map(Into::into)
        .with_context(|| {
            format!(
                "action {action_id} is not pending for location GM {location_gm_agent_id} in world {world_id}"
            )
        })?;
        let result = if result == "approved" {
            Self::execute_approved_action(&mut transaction, &action).await?
        } else {
            result.to_string()
        };
        sqlx::query(
            "INSERT INTO action (sequence_id, inference_id, tool, args, result) VALUES (?, ?, ?, ?, ?)",
        )
        .bind(action.sequence_id)
        .bind(inference_id)
        .bind(&action.tool)
        .bind(&action.args)
        .bind(&result)
        .execute(&mut *transaction)
        .await
        .with_context(|| format!("recording resolved action {action_id}"))?;
        let deleted = sqlx::query(
            "DELETE FROM pending_action WHERE world_id = ? AND id = ? AND location_gm_agent_id = ?",
        )
        .bind(world_id)
        .bind(action_id)
        .bind(location_gm_agent_id)
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
        Ok(action)
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
                     JOIN character_location ON character_location.location_id = item_location.location_id \
                     WHERE item.world_id = ? AND item.name = ? AND character_location.character_id = ?",
                )
                .bind(action.world_id)
                .bind(&arguments.item)
                .bind(action.character_id)
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
             JOIN character_location ON character_location.character_id = player_character.character_id \
             JOIN location ON location.id = character_location.location_id \
             JOIN world_member ON world_member.id = player_character.member_id \
             JOIN world_owner ON world_owner.world_id = world_member.world_id AND world_owner.user_id = world_member.user_id \
             WHERE world_member.world_id = ? ORDER BY player_character.member_id LIMIT 1",
        )
        .bind(world_id)
        .fetch_optional(&self.pool)
        .await
        .context("loading scenario start location")?
        .context("scenario world has no owner Adventurer location")?;
        let paths: Vec<ScenarioPath> = sqlx::query_as::<_, (String, String, i64, String, String, String)>(
            "SELECT from_location.name, to_location.name, path.travel_time, path.description, path.gm_notes, path.storyteller_notes \
             FROM path JOIN location AS from_location ON from_location.id = path.from_location_id \
             JOIN location AS to_location ON to_location.id = path.to_location_id \
             WHERE path.world_id = ? ORDER BY path.id",
        )
        .bind(world_id)
        .fetch_all(&self.pool)
        .await
        .context("loading scenario paths")?
        .into_iter()
        .map(|(from, to, travel_time, description, gm, storyteller)| {
            Ok(ScenarioPath { from, to, travel_time, description, notes: decode_notes(&gm, &storyteller)? })
        })
        .collect::<Result<_>>()?;
        let npcs: Vec<Npc> = sqlx::query_as::<_, (String, String, String, String, String, String, String, String, String)>(
            "SELECT character.name, location.name, character.description, character.background, character.motive, character.ambition, character.sheet, character.gm_notes, character.storyteller_notes \
             FROM character JOIN character_location ON character_location.character_id = character.id \
             JOIN location ON location.id = character_location.location_id \
             WHERE character.world_id = ? AND character.role = 'npc' ORDER BY character.id",
        )
        .bind(world_id)
        .fetch_all(&self.pool)
        .await
        .context("loading scenario NPCs")?
        .into_iter()
        .map(|(name, location, description, background, motive, ambition, sheet, gm, storyteller)| {
            Ok(Npc { name, location, description, background, motive, ambition, sheet: serde_json::from_str(&sheet).context("decoding NPC sheet")?, notes: decode_notes(&gm, &storyteller)? })
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
            locations,
            paths,
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
        starting_location_id: i64,
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
        let agent_id = sqlx::query("INSERT INTO agent (world_id) VALUES (?)")
            .bind(world_id)
            .execute(&mut **transaction)
            .await
            .with_context(|| format!("creating player agent for world {world_id}"))?
            .last_insert_rowid();
        sqlx::query("INSERT INTO member_player_agent (member_id, agent_id) VALUES (?, ?)")
            .bind(member_id)
            .bind(agent_id)
            .execute(&mut **transaction)
            .await
            .with_context(|| format!("linking player agent for world {world_id}"))?;
        let sheet = serde_json::json!({});
        let notes = Notes::default();
        let character_id = Self::insert_character(
            transaction,
            world_id,
            CharacterSeed {
                role: "pc",
                name: "Adventurer",
                description: "",
                background: "",
                motive: "",
                ambition: "",
                sheet: &sheet,
                notes: &notes,
            },
        )
        .await?;
        sqlx::query("INSERT INTO player_character (member_id, character_id) VALUES (?, ?)")
            .bind(member_id)
            .bind(character_id)
            .execute(&mut **transaction)
            .await
            .with_context(|| format!("linking Adventurer for world {world_id}"))?;
        sqlx::query(
            "INSERT INTO character_location (character_id, location_id, description) VALUES (?, ?, '')",
        )
        .bind(character_id)
        .bind(starting_location_id)
        .execute(&mut **transaction)
        .await
        .with_context(|| format!("placing Adventurer in world {world_id}"))?;
        let character_tool_id = sqlx::query_scalar("SELECT tool_id FROM character WHERE id = ?")
            .bind(character_id)
            .fetch_one(&mut **transaction)
            .await
            .context("loading Adventurer handle")?;
        Ok(JoinedWorld {
            member_id,
            agent_id,
            character_tool_id,
        })
    }

    async fn member_sheet(
        transaction: &mut Transaction<'_, Sqlite>,
        member: &MemberAgent,
    ) -> Result<(i64, String)> {
        sqlx::query_as(
            "SELECT character.id, character.sheet FROM world_member AS member \
             JOIN member_player_agent AS player ON player.member_id = member.id \
             JOIN player_character ON player_character.member_id = member.id \
             JOIN character ON character.id = player_character.character_id \
             WHERE member.id = ? AND member.world_id = ? AND member.user_id = ? \
               AND member.access = 'active' AND player.agent_id = ?",
        )
        .bind(member.member_id)
        .bind(member.world_id)
        .bind(member.user_id)
        .bind(member.agent_id)
        .fetch_optional(&mut **transaction)
        .await
        .context("resolving active Adventurer sheet")?
        .context("active membership has no Adventurer")
    }

    async fn record_action(
        transaction: &mut Transaction<'_, Sqlite>,
        sequence_id: i64,
        inference_id: Option<i64>,
        tool: &str,
        args: &str,
        result: &str,
    ) -> Result<()> {
        sqlx::query("INSERT INTO action (sequence_id, inference_id, tool, args, result) VALUES (?, ?, ?, ?, ?)")
            .bind(sequence_id).bind(inference_id).bind(tool).bind(args).bind(result)
            .execute(&mut **transaction).await
            .with_context(|| format!("recording {tool} action"))?;
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

    pub async fn append_message(&self, agent_id: i64, message: &Message) -> Result<i64> {
        let mut transaction = self
            .pool
            .begin()
            .await
            .with_context(|| format!("starting message transaction for agent {agent_id}"))?;
        let seq: i64 =
            sqlx::query_scalar("SELECT COALESCE(MAX(seq) + 1, 0) FROM message WHERE agent_id = ?")
                .bind(agent_id)
                .fetch_one(&mut *transaction)
                .await
                .with_context(|| format!("finding next message sequence for agent {agent_id}"))?;
        let role = serde_json::to_string(&message.role).context("serializing message role")?;
        let content =
            serde_json::to_string(&message.content).context("serializing message content")?;
        let result = sqlx::query(
            "INSERT INTO message (agent_id, seq, role, content, reasoning) VALUES (?, ?, ?, ?, ?)",
        )
        .bind(agent_id)
        .bind(seq)
        .bind(role)
        .bind(content)
        .bind(&message.reasoning)
        .execute(&mut *transaction)
        .await
        .with_context(|| format!("appending message {seq} for agent {agent_id}"))?;
        transaction
            .commit()
            .await
            .with_context(|| format!("committing message {seq} for agent {agent_id}"))?;
        Ok(result.last_insert_rowid())
    }

    /// Append a final assistant reply and its due compaction in one
    /// transaction. Its completed next-context token count decides whether the
    /// reply creates a compaction
    /// obligation. The reply remains immediately usable while that work runs;
    /// persistence lets the scheduler prevent this agent's next inference from
    /// seeing the un-compacted history, including after a server restart.
    pub async fn append_reply_and_enqueue_compaction(
        &self,
        agent_id: i64,
        reply: CompletedReply<'_>,
    ) -> Result<i64> {
        let mut transaction = self.pool.begin().await.with_context(|| {
            format!("starting reply-and-compaction transaction for agent {agent_id}")
        })?;
        let seq: i64 =
            sqlx::query_scalar("SELECT COALESCE(MAX(seq) + 1, 0) FROM message WHERE agent_id = ?")
                .bind(agent_id)
                .fetch_one(&mut *transaction)
                .await
                .with_context(|| format!("finding next message sequence for agent {agent_id}"))?;
        let role =
            serde_json::to_string(&reply.message.role).context("serializing message role")?;
        let content =
            serde_json::to_string(&reply.message.content).context("serializing message content")?;
        let result = sqlx::query(
            "INSERT INTO message (agent_id, seq, role, content, reasoning) VALUES (?, ?, ?, ?, ?)",
        )
        .bind(agent_id)
        .bind(seq)
        .bind(role)
        .bind(content)
        .bind(&reply.message.reasoning)
        .execute(&mut *transaction)
        .await
        .with_context(|| format!("appending final reply {seq} for agent {agent_id}"))?;
        let message_id = result.last_insert_rowid();
        let next_input_tokens = reply.next_input_tokens;
        if next_input_tokens >= reply.compact_before_next_input_tokens {
            sqlx::query(
                "INSERT INTO pending_compaction (agent_id, after_message_id, next_input_tokens, sampling, model, static_segments) VALUES (?, ?, ?, ?, ?, ?)",
            )
            .bind(agent_id)
            .bind(message_id)
            .bind(i64::try_from(next_input_tokens).context("next-input token count is too large")?)
            .bind(serde_json::to_string(reply.sampling).context("serializing compaction sampling")?)
            .bind(reply.model)
            .bind(serde_json::to_string(reply.static_segments)
                .context("serializing static compaction context")?)
            .execute(&mut *transaction)
            .await
            .with_context(|| format!("enqueueing compaction for agent {agent_id}"))?;
        }
        transaction.commit().await.with_context(|| {
            format!("committing final reply and compaction for agent {agent_id}")
        })?;
        Ok(message_id)
    }

    pub async fn pending_compactions(&self) -> Result<Vec<PendingCompaction>> {
        let rows = sqlx::query_as::<_, PendingCompactionRow>(
            "SELECT agent_id, after_message_id, next_input_tokens, sampling, model, static_segments FROM pending_compaction ORDER BY after_message_id",
        )
        .fetch_all(&self.pool)
        .await
        .context("loading pending compactions")?;
        rows.into_iter().map(PendingCompaction::try_from).collect()
    }

    pub async fn pending_compaction(&self, agent_id: i64) -> Result<Option<PendingCompaction>> {
        sqlx::query_as::<_, PendingCompactionRow>(
            "SELECT agent_id, after_message_id, next_input_tokens, sampling, model, static_segments \
             FROM pending_compaction WHERE agent_id = ?",
        )
        .bind(agent_id)
        .fetch_optional(&self.pool)
        .await
        .with_context(|| format!("loading pending compaction for agent {agent_id}"))?
        .map(PendingCompaction::try_from)
        .transpose()
    }

    /// Persist compaction required by a request the fixed KV cache rejected
    /// before it could produce a response. This is the exceptional counterpart
    /// to `append_reply_and_enqueue_compaction`: tool-loop history may have
    /// grown after the last completed inference, so there is no final reply to
    /// attach the obligation to.
    pub async fn enqueue_capacity_compaction(
        &self,
        agent_id: i64,
        requested_tokens: usize,
        sampling: &Sampling,
        model: &str,
        static_segments: &[Segment],
    ) -> Result<()> {
        let after_message_id: i64 = sqlx::query_scalar(
            "SELECT id FROM message WHERE agent_id = ? ORDER BY seq DESC LIMIT 1",
        )
        .bind(agent_id)
        .fetch_optional(&self.pool)
        .await
        .with_context(|| format!("finding history to compact for agent {agent_id}"))?
        .with_context(|| {
            format!(
                "model request for agent {agent_id} exceeded fixed context capacity before any history existed to compact"
            )
        })?;
        sqlx::query(
            "INSERT INTO pending_compaction (agent_id, after_message_id, next_input_tokens, sampling, model, static_segments) VALUES (?, ?, ?, ?, ?, ?) ON CONFLICT(agent_id) DO NOTHING",
        )
        .bind(agent_id)
        .bind(after_message_id)
        .bind(i64::try_from(requested_tokens).context("capacity-rejected input token count is too large")?)
        .bind(serde_json::to_string(sampling).context("serializing capacity compaction sampling")?)
        .bind(model)
        .bind(serde_json::to_string(static_segments)
            .context("serializing capacity compaction static context")?)
        .execute(&self.pool)
        .await
        .with_context(|| format!("enqueueing capacity recovery compaction for agent {agent_id}"))?;
        Ok(())
    }

    #[cfg(test)]
    pub async fn enqueue_compaction_for_test(&self, job: &PendingCompaction) -> Result<()> {
        sqlx::query(
            "INSERT INTO pending_compaction (agent_id, after_message_id, next_input_tokens, sampling, model, static_segments) VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(job.agent_id)
        .bind(job.after_message_id)
        .bind(i64::try_from(job.next_input_tokens).context("next-input token count is too large")?)
        .bind(serde_json::to_string(&job.sampling).context("serializing compaction sampling")?)
        .bind(&job.model)
        .bind(serde_json::to_string(&job.static_segments)
            .context("serializing test static compaction context")?)
        .execute(&self.pool)
        .await
        .context("enqueueing test compaction")?;
        Ok(())
    }

    /// Make a completed compaction visible and retire exactly its stored job.
    pub async fn finish_compaction(
        &self,
        job: &PendingCompaction,
        summary: Option<(&str, i64, i64)>,
        notice: &str,
    ) -> Result<()> {
        let mut transaction = self.pool.begin().await.with_context(|| {
            format!(
                "starting compaction completion transaction for agent {}",
                job.agent_id
            )
        })?;
        if let Some((content, covers_to_seq, inference_id)) = summary {
            sqlx::query(
                "INSERT INTO summary (agent_id, covers_to_seq, content, inference_id) VALUES (?, ?, ?, ?)",
            )
            .bind(job.agent_id)
            .bind(covers_to_seq)
            .bind(content)
            .bind(inference_id)
            .execute(&mut *transaction)
            .await
            .with_context(|| format!("storing compaction result through message {covers_to_seq}"))?;
        }
        sqlx::query(
            "INSERT INTO chat_notice (agent_id, after_message_id, content) VALUES (?, ?, ?)",
        )
        .bind(job.agent_id)
        .bind(job.after_message_id)
        .bind(notice)
        .execute(&mut *transaction)
        .await
        .with_context(|| format!("storing compaction notice for agent {}", job.agent_id))?;
        let result = sqlx::query(
            "DELETE FROM pending_compaction WHERE agent_id = ? AND after_message_id = ?",
        )
        .bind(job.agent_id)
        .bind(job.after_message_id)
        .execute(&mut *transaction)
        .await
        .with_context(|| format!("retiring compaction job for agent {}", job.agent_id))?;
        ensure!(
            result.rows_affected() == 1,
            "compaction job for agent {} changed before it could complete",
            job.agent_id
        );
        transaction.commit().await.with_context(|| {
            format!(
                "committing compaction completion for agent {}",
                job.agent_id
            )
        })
    }

    /// Persist one rolling summary while retaining its compaction obligation.
    /// A fallback may need several linear passes; keeping the job stored
    /// means a restart resumes from this prefix rather than redoing it.
    pub async fn append_summary(
        &self,
        agent_id: i64,
        covers_to_seq: i64,
        content: &str,
        inference_id: i64,
    ) -> Result<i64> {
        let result = sqlx::query(
            "INSERT INTO summary (agent_id, covers_to_seq, content, inference_id) VALUES (?, ?, ?, ?)",
        )
        .bind(agent_id)
        .bind(covers_to_seq)
        .bind(content)
        .bind(inference_id)
        .execute(&self.pool)
        .await
        .with_context(|| format!("storing compaction result through message {covers_to_seq}"))?;
        Ok(result.last_insert_rowid())
    }

    #[cfg(test)]
    pub async fn chat_notice_contents(&self, agent_id: i64) -> Result<Vec<String>> {
        sqlx::query_scalar("SELECT content FROM chat_notice WHERE agent_id = ? ORDER BY id")
            .bind(agent_id)
            .fetch_all(&self.pool)
            .await
            .with_context(|| format!("loading chat notices for agent {agent_id}"))
    }

    /// Store one static prompt piece and return the id a recipe refers to.
    /// Rows are never updated, so the reference stays true to what was sent.
    pub async fn store_prompt_text(&self, content: &str) -> Result<i64> {
        let result = sqlx::query("INSERT INTO text (content) VALUES (?)")
            .bind(content)
            .execute(&self.pool)
            .await
            .context("storing prompt text")?;
        Ok(result.last_insert_rowid())
    }

    async fn text(&self, id: i64) -> Result<String> {
        sqlx::query_scalar("SELECT content FROM text WHERE id = ?")
            .bind(id)
            .fetch_optional(&self.pool)
            .await
            .context("loading prompt text")?
            .with_context(|| format!("inference references missing text {id}"))
    }

    #[cfg(test)]
    pub async fn store_summary(
        &self,
        agent_id: i64,
        covers_to_seq: i64,
        content: &str,
        inference_id: i64,
    ) -> Result<i64> {
        self.append_summary(agent_id, covers_to_seq, content, inference_id)
            .await
    }

    pub async fn latest_summary(&self, agent_id: i64) -> Result<Option<Summary>> {
        let row = sqlx::query_as::<_, SummaryRow>(
            "SELECT id, agent_id, covers_to_seq, content, inference_id FROM summary \
             WHERE agent_id = ? ORDER BY covers_to_seq DESC, id DESC LIMIT 1",
        )
        .bind(agent_id)
        .fetch_optional(&self.pool)
        .await
        .with_context(|| format!("loading latest summary for agent {agent_id}"))?;
        Ok(row.map(|row| Summary {
            id: row.id,
            agent_id: row.agent_id,
            covers_to_seq: row.covers_to_seq,
            content: row.content,
            inference_id: row.inference_id,
        }))
    }

    /// The live history is the newest summary followed by every raw message it
    /// does not cover. Older messages remain stored for replay and debugging.
    pub async fn history_segments(&self, agent_id: i64) -> Result<Vec<Segment>> {
        let summary = self.latest_summary(agent_id).await?;
        let first_seq = summary
            .as_ref()
            .map_or(0, |summary| summary.covers_to_seq + 1);
        let last_seq: Option<i64> =
            sqlx::query_scalar("SELECT MAX(seq) FROM message WHERE agent_id = ? AND seq >= ?")
                .bind(agent_id)
                .bind(first_seq)
                .fetch_one(&self.pool)
                .await
                .with_context(|| format!("finding live message range for agent {agent_id}"))?;
        let mut segments = summary
            .as_ref()
            .map(|summary| {
                vec![Segment::Summary {
                    summary: summary.id,
                }]
            })
            .unwrap_or_default();
        if let Some(last_seq) = last_seq {
            segments.push(Segment::Messages {
                messages: MessageRange {
                    agent_id,
                    first_seq,
                    last_seq,
                },
            });
        }
        Ok(segments)
    }

    /// Find the newest raw suffix containing exactly `keep_tail_messages` rows,
    /// or every available row when there are fewer.
    pub async fn tail_split(&self, agent_id: i64, keep_tail_messages: usize) -> Result<i64> {
        let covered = self
            .latest_summary(agent_id)
            .await?
            .map_or(-1, |summary| summary.covers_to_seq);
        let rows = sqlx::query_as::<_, (i64,)>(
            "SELECT seq FROM message WHERE agent_id = ? AND seq > ? ORDER BY seq DESC LIMIT ?",
        )
        .bind(agent_id)
        .bind(covered)
        .bind(keep_tail_messages as i64)
        .fetch_all(&self.pool)
        .await
        .with_context(|| format!("loading raw tail for agent {agent_id}"))?;
        Ok(rows.last().map_or(covered, |(seq,)| seq - 1))
    }

    pub async fn request_for_segments(
        &self,
        agent_id: i64,
        segments: &[Segment],
        sampling: Sampling,
    ) -> Result<Request> {
        let mut messages = Vec::new();
        let mut tools = Vec::new();
        for segment in segments {
            match segment {
                Segment::Text { text, role } => {
                    messages.push(Message::text(role.clone(), self.text(*text).await?));
                }
                Segment::Tools { text } => {
                    let definitions: Vec<ToolDefinition> =
                        serde_json::from_str(&self.text(*text).await?)
                            .context("deserializing tool definitions")?;
                    tools.extend(definitions);
                }
                Segment::Summary { summary } => {
                    let row = sqlx::query_as::<_, SummaryRow>(
                        "SELECT id, agent_id, covers_to_seq, content, inference_id FROM summary WHERE id = ?",
                    )
                    .bind(summary)
                    .fetch_optional(&self.pool)
                    .await
                    .context("loading summary")?
                    .with_context(|| format!("inference references missing summary {summary}"))?;
                    ensure!(
                        row.agent_id == agent_id,
                        "inference for agent {agent_id} references summary {} from agent {}",
                        row.id,
                        row.agent_id
                    );
                    messages.push(Message::text(Role::System, row.content));
                }
                Segment::Messages { messages: range } => {
                    ensure!(
                        range.agent_id == agent_id,
                        "inference for agent {agent_id} references messages from agent {}",
                        range.agent_id
                    );
                    ensure!(
                        range.first_seq <= range.last_seq,
                        "message range {}..={} is invalid",
                        range.first_seq,
                        range.last_seq
                    );
                    let rows = sqlx::query_as::<_, MessageRow>(
                        "SELECT seq, role, content FROM message \
                         WHERE agent_id = ? AND seq BETWEEN ? AND ? ORDER BY seq",
                    )
                    .bind(range.agent_id)
                    .bind(range.first_seq)
                    .bind(range.last_seq)
                    .fetch_all(&self.pool)
                    .await
                    .with_context(|| {
                        format!(
                            "loading messages {}..={} for agent {}",
                            range.first_seq, range.last_seq, range.agent_id
                        )
                    })?;
                    let expected = usize::try_from(range.last_seq - range.first_seq + 1)
                        .context("message range is too large")?;
                    ensure!(
                        rows.len() == expected,
                        "message range {}..={} for agent {} has missing rows",
                        range.first_seq,
                        range.last_seq,
                        range.agent_id
                    );
                    for (offset, row) in rows.into_iter().enumerate() {
                        let expected_seq = range.first_seq
                            + i64::try_from(offset)
                                .context("message range offset exceeds SQLite range")?;
                        ensure!(
                            row.seq == expected_seq,
                            "message range for agent {} is out of order at sequence {}",
                            range.agent_id,
                            expected_seq
                        );
                        let role = serde_json::from_str(&row.role).with_context(|| {
                            format!(
                                "deserializing role for agent {} message {}",
                                range.agent_id, row.seq
                            )
                        })?;
                        let content: MessageContent = serde_json::from_str(&row.content)
                            .with_context(|| {
                                format!(
                                    "deserializing content for agent {} message {}",
                                    range.agent_id, row.seq
                                )
                            })?;
                        messages.push(Message {
                            role,
                            content,
                            reasoning: String::new(),
                        });
                    }
                }
            }
        }
        Ok(Request {
            messages,
            tools,
            sampling,
        })
    }

    pub async fn record(&self, record: InferenceRecord<'_>) -> Result<i64> {
        let segments =
            serde_json::to_string(record.segments).context("serializing inference segments")?;
        let sampling = serde_json::to_string(&record.request.sampling)
            .context("serializing inference sampling")?;
        let input = serde_json::to_vec(record.request)
            .context("serializing assembled inference request")?;
        let input_hash = blake3::hash(&input).to_hex().to_string();
        let duration_ms =
            i64::try_from(record.duration_ms).context("inference duration exceeds SQLite range")?;
        let (output, error, input_tokens, output_tokens) = match record.outcome {
            InferenceOutcome::Response(response) => (
                Some(serde_json::to_string(&response).context("serializing inference output")?),
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
        };
        let result = sqlx::query(
            "INSERT INTO inference \
             (agent_id, sequence_id, parent_inference_id, segments, sampling, output, error, input_hash, input_tokens, output_tokens, duration_ms, model) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(record.agent_id)
        .bind(record.sequence_id)
        .bind(record.parent_inference_id)
        .bind(segments)
        .bind(sampling)
        .bind(output)
        .bind(error)
        .bind(input_hash)
        .bind(input_tokens)
        .bind(output_tokens)
        .bind(duration_ms)
        .bind(record.model)
        .execute(&self.pool)
        .await
        .context("storing inference record")?;
        Ok(result.last_insert_rowid())
    }

    #[cfg(test)]
    pub async fn record_inference(
        &self,
        agent_id: i64,
        segments: &[Segment],
        request: &Request,
        outcome: InferenceOutcome,
        model: &str,
        duration_ms: u64,
    ) -> Result<i64> {
        self.record(InferenceRecord {
            agent_id,
            sequence_id: None,
            parent_inference_id: None,
            segments,
            request,
            outcome,
            model,
            duration_ms,
        })
        .await
    }

    pub async fn reconstruct_inference(&self, id: i64) -> Result<RecordedInference> {
        let row = sqlx::query_as::<_, InferenceRow>(
            "SELECT id, agent_id, sequence_id, parent_inference_id, segments, sampling, output, error, input_hash, input_tokens, output_tokens, duration_ms, model \
             FROM inference WHERE id = ?",
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
        let input =
            serde_json::to_vec(&request).context("serializing reconstructed inference request")?;
        ensure!(
            blake3::hash(&input).to_hex().as_str() == row.input_hash,
            "inference {} reconstructed input does not match its hash",
            row.id
        );
        let outcome = match (row.output, row.error, row.input_tokens, row.output_tokens) {
            (Some(output), None, Some(input_tokens), Some(output_tokens)) => {
                let response: Response = serde_json::from_str(&output)
                    .context("deserializing recorded inference output")?;
                ensure!(
                    response.usage
                        == Usage {
                            input_tokens: usize::try_from(input_tokens)
                                .context("recorded input token count is negative")?,
                            output_tokens: usize::try_from(output_tokens)
                                .context("recorded output token count is negative")?,
                        },
                    "inference {} usage columns differ from output",
                    row.id
                );
                RecordedOutcome::Response(response)
            }
            (None, Some(error), None, None) => RecordedOutcome::Error(error),
            _ => anyhow::bail!(
                "inference {} has an invalid success or failure outcome",
                row.id
            ),
        };
        Ok(RecordedInference {
            id: row.id,
            agent_id: row.agent_id,
            sequence_id: row.sequence_id,
            parent_inference_id: row.parent_inference_id,
            segments,
            request,
            outcome,
            model: row.model,
            duration_ms: u64::try_from(row.duration_ms)
                .context("recorded inference duration is negative")?,
        })
    }

    /// Rewrite a prompt row that should never change, so tests can prove
    /// tampering is detected rather than silently reconstructed.
    #[cfg(test)]
    pub(crate) async fn corrupt_text_for_test(&self, containing: &str) {
        let rows = sqlx::query("UPDATE text SET content = '[]' WHERE content LIKE ?")
            .bind(format!("%{containing}%"))
            .execute(&self.pool)
            .await
            .expect("corrupting text should succeed")
            .rows_affected();
        assert_eq!(
            rows, 1,
            "expected exactly one text row matching {containing}"
        );
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
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;
    use crate::llm::{Content, ToolCall};

    fn database_path() -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "cairnworld-store-test-{}-{}.sqlite",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system clock before Unix epoch")
                .as_nanos()
        ))
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
            locations: vec![crate::scenario::Location {
                name: "start".into(),
                kind: "test".into(),
                description: String::new(),
                notes: Notes::default(),
            }],
            paths: vec![],
            npcs: vec![],
            item_types: vec![],
            spell_types: vec![],
            items: vec![],
        }
    }

    async fn store_with_history() -> (Store, std::path::PathBuf, i64, Vec<Segment>, Request) {
        let path = database_path();
        let store = Store::open(&path).await.expect("store should open");
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
            store.history_segments(agent).await.unwrap().pop().unwrap(),
        ];
        let request = store
            .request_for_segments(
                agent,
                &segments,
                Sampling {
                    temperature: 0.7,
                    enable_thinking: false,
                },
            )
            .await
            .expect("request should assemble");
        let stored_reasoning: String =
            sqlx::query_scalar("SELECT reasoning FROM message WHERE agent_id = ? AND seq = 1")
                .bind(agent)
                .fetch_one(&store.pool)
                .await
                .expect("assistant reasoning should persist");
        assert_eq!(stored_reasoning, "scratch work");
        assert!(request.messages[1].reasoning.is_empty());
        (store, path, agent, segments, request)
    }

    #[tokio::test]
    async fn membership_resolves_only_the_authenticated_users_player_history() {
        let path = database_path();
        let store = Store::open(&path).await.unwrap();
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
                .active_member_agent(alex_one.id, first.world_id)
                .await
                .unwrap(),
            Some(first.member.clone())
        );
        assert_eq!(
            store
                .active_member_agent(alex_two.id, second.world_id)
                .await
                .unwrap(),
            Some(second.member)
        );
        assert_eq!(
            store
                .active_member_agent(alex_one.id, other_world_id)
                .await
                .unwrap(),
            None,
            "a world id alone must never select another user's player agent"
        );

        drop(store);
        std::fs::remove_file(path).unwrap();
    }

    #[tokio::test]
    async fn player_chat_reloads_only_renderable_text_from_its_active_history() {
        let path = database_path();
        let store = Store::open(&path).await.unwrap();
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
                &Message::text(Role::System, "Toma watches from the doorway."),
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
                (&Role::System, "Toma watches from the doorway."),
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
                (&Role::System, "Toma watches from the doorway."),
            ],
            "a page snapshot cursor must receive each later visible entry once"
        );
        drop(store);
        std::fs::remove_file(path).unwrap();
    }

    #[tokio::test]
    async fn invitations_only_grant_the_invited_world_and_preserve_one_player_history() {
        let path = database_path();
        let store = Store::open(&path).await.unwrap();
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
                .active_member_agent(invited.id, other_world.world_id)
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
                .active_member_agent(invited.id, world.world_id)
                .await
                .unwrap(),
            None,
            "a removed member must no longer resolve to a playable history"
        );
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
        drop(store);
        std::fs::remove_file(path).unwrap();
    }

    #[tokio::test]
    async fn character_creation_rolls_are_stored_and_ready_requires_them() {
        let path = database_path();
        let store = Store::open(&path).await.unwrap();
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
        drop(store);
        std::fs::remove_file(path).unwrap();
    }

    #[tokio::test]
    async fn installs_bread_thief_as_a_complete_playable_relationship_graph() {
        let path = database_path();
        let store = Store::open(&path).await.unwrap();
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
        let active_character: String = sqlx::query_scalar(
            "SELECT character.name FROM player_character \
             JOIN character ON character.id = player_character.character_id \
             WHERE player_character.member_id = ?",
        )
        .bind(installed.member.member_id)
        .fetch_one(&store.pool)
        .await
        .unwrap();
        assert_eq!(active_character, "Adventurer");
        let location_gms: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM location_gm \
             JOIN location ON location.id = location_gm.location_id WHERE location.world_id = ?",
        )
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
        assert_eq!(location_gms, 1, "the shared playable location has one GM");
        assert_eq!(npc_agents, 2, "Mara and Toma have distinct agent histories");
        assert_eq!(
            placed_items, 2,
            "the flour and cache can be found in the world"
        );
        let player_scene = store.player_scene(&installed.member).await.unwrap();
        assert_eq!(player_scene.character_name, "Adventurer");
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
        let gm_agent_id: i64 = sqlx::query_scalar(
            "SELECT location_gm.agent_id FROM location_gm \
             JOIN location ON location.id = location_gm.location_id WHERE location.world_id = ?",
        )
        .bind(installed.world_id)
        .fetch_one(&store.pool)
        .await
        .unwrap();
        let gm_scene = store
            .gm_scene(installed.world_id, gm_agent_id)
            .await
            .unwrap();
        assert_eq!(gm_scene.location_name, player_scene.location_name);
        assert!(gm_scene.gm_notes.get("interior").is_some());
        let toma = gm_scene.npcs.iter().find(|npc| npc.name == "Toma").unwrap();
        assert!(toma.motive.contains("imprisonment"));
        assert!(toma.gm_notes.get("beliefs").is_some());
        assert_eq!(
            store.export_scenario(installed.world_id).await.unwrap(),
            scenario,
            "exported JSON must recreate the same scenario graph in a fresh world"
        );

        drop(store);
        std::fs::remove_file(path).unwrap();
    }

    #[tokio::test]
    async fn pending_actions_keep_the_validated_call_for_its_location_gm() {
        let path = database_path();
        let store = Store::open(&path).await.unwrap();
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
        let request = Request {
            messages: vec![],
            tools: vec![],
            sampling: Sampling {
                temperature: 0.0,
                enable_thinking: false,
            },
        };
        let inference_id = store
            .record(InferenceRecord {
                agent_id: installed.member.agent_id,
                sequence_id: Some(sequence.id),
                parent_inference_id: None,
                segments: &[],
                request: &request,
                outcome: InferenceOutcome::Response(response()),
                model: "scripted",
                duration_ms: 0,
            })
            .await
            .unwrap();
        let args = r#"{"description":"look through the rear window"}"#;
        let action = store
            .create_pending_action(&installed.member, sequence.id, inference_id, "look", args)
            .await
            .unwrap();
        assert_eq!(action.id, 1);
        assert_eq!(action.tool, "look");
        assert_eq!(action.args, args);
        assert_eq!(action.sequence_id, sequence.id);
        let location_gm: i64 = sqlx::query_scalar(
            "SELECT location_gm.agent_id FROM player_character \
             JOIN character_location ON character_location.character_id = player_character.character_id \
             JOIN location_gm ON location_gm.location_id = character_location.location_id \
             WHERE player_character.member_id = ?",
        )
        .bind(installed.member.member_id)
        .fetch_one(&store.pool)
        .await
        .unwrap();
        assert_eq!(action.location_gm_agent_id, location_gm);

        let error = store
            .resolve_pending_action(
                installed.member.agent_id,
                installed.world_id,
                action.id,
                inference_id,
                "approved",
            )
            .await
            .expect_err("a player agent cannot resolve its own action");
        assert!(format!("{error:#}").contains("is not pending for location GM"));
        let resolved = store
            .resolve_pending_action(
                location_gm,
                installed.world_id,
                action.id,
                inference_id,
                "approved",
            )
            .await
            .unwrap();
        assert_eq!(resolved, action);
        let recorded_result: String =
            sqlx::query_scalar("SELECT result FROM action WHERE sequence_id = ? AND tool = 'look'")
                .bind(sequence.id)
                .fetch_one(&store.pool)
                .await
                .unwrap();
        assert_eq!(recorded_result, "approved");

        let other_world = store.create_world("other").await.unwrap();
        let other_sequence = store
            .begin_sequence(other_world, "player message")
            .await
            .unwrap();
        let error = store
            .create_pending_action(
                &installed.member,
                other_sequence.id,
                inference_id,
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

        drop(store);
        std::fs::remove_file(path).unwrap();
    }

    #[tokio::test]
    async fn approved_take_transfers_one_available_item_exactly_once() {
        let path = database_path();
        let store = Store::open(&path).await.unwrap();
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
        let request = Request {
            messages: vec![],
            tools: vec![],
            sampling: Sampling {
                temperature: 0.0,
                enable_thinking: false,
            },
        };
        let inference_id = store
            .record(InferenceRecord {
                agent_id: installed.member.agent_id,
                sequence_id: Some(sequence.id),
                parent_inference_id: None,
                segments: &[],
                request: &request,
                outcome: InferenceOutcome::Response(response()),
                model: "scripted",
                duration_ms: 0,
            })
            .await
            .unwrap();
        let action = store
            .create_pending_action(
                &installed.member,
                sequence.id,
                inference_id,
                "take",
                r#"{"item":"flour sack"}"#,
            )
            .await
            .unwrap();
        store
            .resolve_pending_action(
                action.location_gm_agent_id,
                installed.world_id,
                action.id,
                inference_id,
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
        assert!(
            store
                .resolve_pending_action(
                    action.location_gm_agent_id,
                    installed.world_id,
                    action.id,
                    inference_id,
                    "approved",
                )
                .await
                .is_err(),
            "a resolved action must not execute its transfer twice"
        );
        let holders_after_retry: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM item_character JOIN item ON item.id = item_character.item_id \
             WHERE item.world_id = ? AND item.name = 'flour sack'",
        )
        .bind(installed.world_id)
        .fetch_one(&store.pool)
        .await
        .unwrap();
        assert_eq!(holders_after_retry, 1);

        drop(store);
        std::fs::remove_file(path).unwrap();
    }

    #[tokio::test]
    async fn character_handles_are_unique_and_widen_only_after_two_digits_are_full() {
        let path = database_path();
        let store = Store::open(&path).await.unwrap();
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

        drop(store);
        std::fs::remove_file(path).unwrap();
    }

    #[tokio::test]
    async fn reconstructs_history_recipe_without_copying_messages() {
        let (store, path, agent, segments, request) = store_with_history().await;
        let first = store
            .record_inference(
                agent,
                &segments,
                &request,
                InferenceOutcome::Response(response()),
                "test-model",
                14,
            )
            .await
            .expect("record should persist");
        let second = store
            .record_inference(
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
        drop(store);
        std::fs::remove_file(path).expect("test database should be removable");
    }

    #[tokio::test]
    async fn reconstruction_rejects_corrupt_or_invalid_references() {
        let (store, path, agent, segments, request) = store_with_history().await;
        let id = store
            .record_inference(
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
        sqlx::query("DELETE FROM message WHERE agent_id = ? AND seq = 1")
            .bind(agent)
            .execute(&store.pool)
            .await
            .unwrap();
        assert!(
            format!("{:#}", store.reconstruct_inference(id).await.unwrap_err())
                .contains("missing rows")
        );
        drop(store);
        std::fs::remove_file(path).expect("test database should be removable");
    }

    #[tokio::test]
    async fn reconstruction_rejects_cross_agent_references_and_preserves_failures() {
        let (store, path, agent, mut segments, request) = store_with_history().await;
        let world: i64 = sqlx::query_scalar("SELECT world_id FROM agent WHERE id = ?")
            .bind(agent)
            .fetch_one(&store.pool)
            .await
            .unwrap();
        let other = store.create_agent(world).await.unwrap();
        if let Segment::Messages { messages } = &mut segments[1] {
            messages.agent_id = other;
        }
        let id = store
            .record_inference(
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
                .contains("references messages from agent")
        );

        if let Segment::Messages { messages } = &mut segments[1] {
            messages.agent_id = agent;
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
        drop(store);
        std::fs::remove_file(path).expect("test database should be removable");
    }

    #[tokio::test]
    async fn latest_summary_replaces_only_the_history_it_covers() {
        let (store, path, agent, segments, request) = store_with_history().await;
        let inference = store
            .record_inference(
                agent,
                &segments,
                &request,
                InferenceOutcome::Response(response()),
                "test-model",
                14,
            )
            .await
            .unwrap();
        let summary = store
            .store_summary(agent, 0, "The floorboards hide a locked chest.", inference)
            .await
            .unwrap();
        let history = store.history_segments(agent).await.unwrap();
        assert!(matches!(
            history.as_slice(),
            [
                Segment::Summary { summary: stored },
                Segment::Messages { messages: MessageRange { first_seq: 1, last_seq: 1, .. } },
            ] if *stored == summary
        ));
        let assembled = store
            .request_for_segments(
                agent,
                &history,
                Sampling {
                    temperature: 0.7,
                    enable_thinking: false,
                },
            )
            .await
            .unwrap();
        assert!(matches!(assembled.messages.as_slice(), [
            Message { role: Role::System, content: MessageContent::Text(summary), .. },
            Message { role: Role::Assistant, .. },
        ] if summary == "The floorboards hide a locked chest."));
        drop(store);
        std::fs::remove_file(path).unwrap();
    }

    #[tokio::test]
    async fn summary_recipe_reconstructs_and_rejects_cross_agent_summary() {
        let (store, path, agent, segments, request) = store_with_history().await;
        let source = store
            .record_inference(
                agent,
                &segments,
                &request,
                InferenceOutcome::Response(response()),
                "test-model",
                14,
            )
            .await
            .unwrap();
        let summary = store
            .store_summary(agent, 0, "Earlier events.", source)
            .await
            .unwrap();
        let recipe = vec![Segment::Summary { summary }];
        let summary_request = store
            .request_for_segments(
                agent,
                &recipe,
                Sampling {
                    temperature: 0.7,
                    enable_thinking: false,
                },
            )
            .await
            .unwrap();
        let recorded = store
            .record_inference(
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
        let error = store
            .request_for_segments(
                other,
                &recipe,
                Sampling {
                    temperature: 0.7,
                    enable_thinking: false,
                },
            )
            .await
            .unwrap_err();
        assert!(format!("{error:#}").contains("references summary"));
        drop(store);
        std::fs::remove_file(path).unwrap();
    }
}
