use std::{collections::HashMap, path::Path};

use anyhow::{Context, Result, ensure};
use rand::Rng;
use serde::{Deserialize, Serialize};
use sqlx::{
    FromRow, Sqlite, SqlitePool, Transaction,
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions},
};

use crate::game::CharacterSheet;
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

#[derive(Debug, PartialEq)]
pub enum RecordedOutcome {
    Response(Response),
    Error(String),
}

#[derive(Debug, PartialEq)]
pub struct RecordedInference {
    pub id: i64,
    pub agent_id: i64,
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
    pub input_tokens: usize,
    pub sampling: Sampling,
    pub model: String,
}

#[derive(Debug, FromRow)]
struct PendingCompactionRow {
    agent_id: i64,
    after_message_id: i64,
    input_tokens: i64,
    sampling: String,
    model: String,
}

impl TryFrom<PendingCompactionRow> for PendingCompaction {
    type Error = anyhow::Error;

    fn try_from(row: PendingCompactionRow) -> Result<Self> {
        Ok(Self {
            agent_id: row.agent_id,
            after_message_id: row.after_message_id,
            input_tokens: usize::try_from(row.input_tokens)
                .context("pending compaction has a negative token count")?,
            sampling: serde_json::from_str(&row.sampling)
                .context("deserializing pending compaction sampling")?,
            model: row.model,
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
        let sheet = serde_json::to_value(CharacterSheet::roll_adventurer())
            .context("serializing Adventurer character sheet")?;
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
    /// transaction, so a reply that was delivered can never lose the work it
    /// created on process restart.
    pub async fn append_reply_and_enqueue_compaction(
        &self,
        agent_id: i64,
        message: &Message,
        input_tokens: usize,
        compact_at_input_tokens: usize,
        sampling: &Sampling,
        model: &str,
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
        .with_context(|| format!("appending final reply {seq} for agent {agent_id}"))?;
        let message_id = result.last_insert_rowid();
        if input_tokens >= compact_at_input_tokens {
            sqlx::query(
                "INSERT INTO pending_compaction (agent_id, after_message_id, input_tokens, sampling, model) VALUES (?, ?, ?, ?, ?)",
            )
            .bind(agent_id)
            .bind(message_id)
            .bind(i64::try_from(input_tokens).context("input token count is too large")?)
            .bind(serde_json::to_string(sampling).context("serializing compaction sampling")?)
            .bind(model)
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
            "SELECT agent_id, after_message_id, input_tokens, sampling, model FROM pending_compaction ORDER BY after_message_id",
        )
        .fetch_all(&self.pool)
        .await
        .context("loading pending compactions")?;
        rows.into_iter().map(PendingCompaction::try_from).collect()
    }

    pub async fn pending_compaction(&self, agent_id: i64) -> Result<Option<PendingCompaction>> {
        sqlx::query_as::<_, PendingCompactionRow>(
            "SELECT agent_id, after_message_id, input_tokens, sampling, model \
             FROM pending_compaction WHERE agent_id = ?",
        )
        .bind(agent_id)
        .fetch_optional(&self.pool)
        .await
        .with_context(|| format!("loading pending compaction for agent {agent_id}"))?
        .map(PendingCompaction::try_from)
        .transpose()
    }

    #[cfg(test)]
    pub async fn enqueue_compaction_for_test(&self, job: &PendingCompaction) -> Result<()> {
        sqlx::query(
            "INSERT INTO pending_compaction (agent_id, after_message_id, input_tokens, sampling, model) VALUES (?, ?, ?, ?, ?)",
        )
        .bind(job.agent_id)
        .bind(job.after_message_id)
        .bind(i64::try_from(job.input_tokens).context("input token count is too large")?)
        .bind(serde_json::to_string(&job.sampling).context("serializing compaction sampling")?)
        .bind(&job.model)
        .execute(&self.pool)
        .await
        .context("enqueueing test compaction")?;
        Ok(())
    }

    /// Make a completed compaction visible and retire exactly its durable job.
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
        let result = sqlx::query(
            "INSERT INTO summary (agent_id, covers_to_seq, content, inference_id) VALUES (?, ?, ?, ?)",
        )
        .bind(agent_id)
        .bind(covers_to_seq)
        .bind(content)
        .bind(inference_id)
        .execute(&self.pool)
        .await
        .with_context(|| format!("storing summary through message {covers_to_seq} for agent {agent_id}"))?;
        Ok(result.last_insert_rowid())
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

    pub async fn record_inference(
        &self,
        agent_id: i64,
        segments: &[Segment],
        request: &Request,
        outcome: InferenceOutcome,
        model: &str,
        duration_ms: u64,
    ) -> Result<i64> {
        let segments = serde_json::to_string(segments).context("serializing inference segments")?;
        let sampling =
            serde_json::to_string(&request.sampling).context("serializing inference sampling")?;
        let input =
            serde_json::to_vec(request).context("serializing assembled inference request")?;
        let input_hash = blake3::hash(&input).to_hex().to_string();
        let duration_ms =
            i64::try_from(duration_ms).context("inference duration exceeds SQLite range")?;
        let (output, error, input_tokens, output_tokens) = match outcome {
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
             (agent_id, segments, sampling, output, error, input_hash, input_tokens, output_tokens, duration_ms, model) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(agent_id)
        .bind(segments)
        .bind(sampling)
        .bind(output)
        .bind(error)
        .bind(input_hash)
        .bind(input_tokens)
        .bind(output_tokens)
        .bind(duration_ms)
        .bind(model)
        .execute(&self.pool)
        .await
        .context("storing inference record")?;
        Ok(result.last_insert_rowid())
    }

    pub async fn reconstruct_inference(&self, id: i64) -> Result<RecordedInference> {
        let row = sqlx::query_as::<_, InferenceRow>(
            "SELECT id, agent_id, segments, sampling, output, error, input_hash, input_tokens, output_tokens, duration_ms, model \
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
}

#[cfg(test)]
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;
    use crate::llm::Content;

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
        let adventurer_sheet: CharacterSheet = serde_json::from_str(
            &sqlx::query_scalar::<_, String>(
                "SELECT character.sheet FROM player_character \
                 JOIN character ON character.id = player_character.character_id \
                 WHERE player_character.member_id = ?",
            )
            .bind(installed.member.member_id)
            .fetch_one(&store.pool)
            .await
            .unwrap(),
        )
        .unwrap();
        assert!((1..=6).contains(&adventurer_sheet.hp));
        assert!((3..=18).contains(&adventurer_sheet.str));
        assert!((3..=18).contains(&adventurer_sheet.dex));
        assert!((3..=18).contains(&adventurer_sheet.wil));
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
        assert_eq!(
            store.export_scenario(installed.world_id).await.unwrap(),
            scenario,
            "exported JSON must recreate the same scenario graph in a fresh world"
        );

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
