//! Creating a world's objects, for scenario installation and for the
//! Storyteller's authoring tools alike: one insert per kind of object, so both
//! build identical records. The Storyteller refers to objects by the IDs the
//! tools return; names stay distinct within a kind so people and the other
//! agents can still refer to them.

use anyhow::{Context, Result};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sqlx::{Sqlite, Transaction, types::Json};

use super::{CharacterSeed, Store};
use crate::scenario::{Connection, Group, ItemType, Location, Notes, Npc};

// The kinds of top-level object the Questioner reviews. Also the allowed
// values of its tools' `kind` argument.
#[derive(Clone, Copy, Debug, Deserialize, JsonSchema, PartialEq)]
#[serde(rename_all = "snake_case")]
#[schemars(inline)]
pub enum ObjectKind {
    Location,
    Connection,
    Group,
    Npc,
    ItemType,
    Item,
}

impl ObjectKind {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::Location => "location",
            Self::Connection => "connection",
            Self::Group => "group",
            Self::Npc => "npc",
            Self::ItemType => "item_type",
            Self::Item => "item",
        }
    }

    /// The fields an object of this kind has, and whether creating one needs it.
    /// An item needs exactly one of `location_id` and `held_by_id`.
    pub(super) fn fields(self) -> &'static [(&'static str, bool)] {
        match self {
            Self::Location => &[("name", true), ("kind", true), ("description", true)],
            Self::Connection => &[
                ("from_id", true),
                ("to_id", true),
                ("travel_time", true),
                ("description", true),
            ],
            Self::Group => &[("name", true), ("location_id", true), ("description", true)],
            Self::Npc => &[
                ("name", true),
                ("group_id", true),
                ("description", true),
                ("background", true),
                ("motive", true),
                ("ambition", false),
                ("sheet", false),
            ],
            Self::ItemType => &[("name", true), ("description", true), ("stats", false)],
            Self::Item => &[
                ("name", true),
                ("item_type_id", true),
                ("description", true),
                ("location_id", false),
                ("held_by_id", false),
            ],
        }
    }
}

// The fields of any object; which of them an object has depends on its kind.
// Objects are referred to by the IDs the tools return.
#[derive(Clone, Debug, Default, Deserialize, JsonSchema, Serialize)]
pub struct Fields {
    pub name: Option<String>,
    pub kind: Option<String>,
    pub description: Option<String>,
    pub from_id: Option<i64>,
    pub to_id: Option<i64>,
    pub travel_time: Option<i64>,
    pub location_id: Option<i64>,
    pub group_id: Option<i64>,
    pub item_type_id: Option<i64>,
    pub held_by_id: Option<i64>,
    pub background: Option<String>,
    pub motive: Option<String>,
    pub ambition: Option<String>,
    #[schemars(with = "Option<std::collections::BTreeMap<String, serde_json::Value>>")]
    pub sheet: Option<serde_json::Value>,
    #[schemars(with = "Option<std::collections::BTreeMap<String, serde_json::Value>>")]
    pub stats: Option<serde_json::Value>,
}

impl Fields {
    /// The names of the fields that have a value.
    fn given(&self) -> Vec<String> {
        match serde_json::to_value(self) {
            Ok(serde_json::Value::Object(map)) => map
                .into_iter()
                .filter(|(_, value)| !value.is_null())
                .map(|(name, _)| name)
                .collect(),
            _ => Vec::new(),
        }
    }

    /// Refuse a field the kind does not have and, when creating, a missing one.
    pub(super) fn check(&self, kind: ObjectKind, creating: bool) -> Result<()> {
        let given = self.given();
        let has: Vec<&str> = kind.fields().iter().map(|(name, _)| *name).collect();
        if let Some(extra) = given.iter().find(|field| !has.contains(&field.as_str())) {
            return reject(format!(
                "A {} has the fields {}, not `{extra}`.",
                kind.as_str(),
                has.join(", ")
            ));
        }
        if creating {
            let missing: Vec<&str> = kind
                .fields()
                .iter()
                .filter(|(name, needed)| *needed && !given.iter().any(|field| field == name))
                .map(|(name, _)| *name)
                .collect();
            if !missing.is_empty() {
                return reject(format!("A {} needs {}.", kind.as_str(), missing.join(", ")));
            }
        }
        Ok(())
    }
}

/// What to do about what depends on an object being deleted.
#[derive(Clone, Debug, Default)]
pub struct DeleteOptions {
    pub delete_groups: bool,
    pub item_location_id: Option<i64>,
    pub item_description: Option<String>,
    pub delete_items: bool,
}

/// The object a change created or acted on, and what the world holds after it.
#[derive(Debug, Serialize)]
pub struct Changed {
    pub id: i64,
    pub counts: Counts,
}

/// Where an item is: exactly one of these.
#[derive(Clone, Copy)]
pub(super) enum Holder {
    Location(i64),
    Character(i64),
}

/// A request the world's rules refuse, to be shown to the agent that made it
/// as a rejected tool result so it can correct itself. Anything else that goes
/// wrong is a failure of the game.
#[derive(Debug)]
pub struct Rejected(pub String);

impl std::fmt::Display for Rejected {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for Rejected {}

/// A field `Fields::check` has already required.
fn need<T: Clone>(field: &Option<T>, name: &str) -> Result<T> {
    match field {
        Some(value) => Ok(value.clone()),
        None => reject(format!("`{name}` is needed.")),
    }
}

pub(super) fn reject<T>(message: String) -> Result<T> {
    Err(Rejected(message).into())
}

/// How much of each kind the world holds, returned after every change so the
/// Storyteller can see what might be missing.
#[derive(Debug, PartialEq, Serialize)]
pub struct Counts {
    pub locations: i64,
    pub connections: i64,
    pub groups: i64,
    pub npcs: i64,
    pub item_types: i64,
    pub items: i64,
}

impl Store {
    pub(super) async fn insert_location(
        transaction: &mut Transaction<'_, Sqlite>,
        world_id: i64,
        location: &Location,
    ) -> Result<i64> {
        let gm_notes = Json(&location.notes.gm);
        let storyteller_notes = Json(&location.notes.storyteller);
        Ok(sqlx::query!(
            "INSERT INTO location (world_id, name, kind, description, gm_notes, storyteller_notes) \
             VALUES (?, ?, ?, ?, ?, ?)",
            world_id,
            location.name,
            location.kind,
            location.description,
            gm_notes,
            storyteller_notes,
        )
        .execute(&mut **transaction)
        .await
        .with_context(|| format!("creating location {}", location.name))?
        .last_insert_rowid())
    }

    pub(super) async fn insert_connection(
        transaction: &mut Transaction<'_, Sqlite>,
        world_id: i64,
        from_location_id: i64,
        to_location_id: i64,
        connection: &Connection,
    ) -> Result<i64> {
        let gm_notes = Json(&connection.notes.gm);
        let storyteller_notes = Json(&connection.notes.storyteller);
        Ok(sqlx::query!(
            "INSERT INTO connection (world_id, from_location_id, to_location_id, travel_time, description, gm_notes, storyteller_notes) \
             VALUES (?, ?, ?, ?, ?, ?, ?)",
            world_id,
            from_location_id,
            to_location_id,
            connection.travel_time,
            connection.description,
            gm_notes,
            storyteller_notes,
        )
        .execute(&mut **transaction)
        .await
        .context("creating connection")?
        .last_insert_rowid())
    }

    /// The world's GM agent, created with its first group.
    async fn world_gm_agent(
        transaction: &mut Transaction<'_, Sqlite>,
        world_id: i64,
    ) -> Result<i64> {
        if let Some(agent_id) =
            sqlx::query_scalar!("SELECT agent_id FROM world_gm WHERE world_id = ?", world_id,)
                .fetch_optional(&mut **transaction)
                .await
                .with_context(|| format!("loading the GM of world {world_id}"))?
        {
            return Ok(agent_id);
        }
        let agent_id = sqlx::query!("INSERT INTO agent (world_id) VALUES (?)", world_id)
            .execute(&mut **transaction)
            .await
            .with_context(|| format!("creating world GM for world {world_id}"))?
            .last_insert_rowid();
        sqlx::query!(
            "INSERT INTO world_gm (world_id, agent_id) VALUES (?, ?)",
            world_id,
            agent_id,
        )
        .execute(&mut **transaction)
        .await
        .with_context(|| format!("linking world GM for world {world_id}"))?;
        Ok(agent_id)
    }

    pub(super) async fn insert_group(
        transaction: &mut Transaction<'_, Sqlite>,
        world_id: i64,
        location_id: i64,
        group: &Group,
    ) -> Result<i64> {
        let gm_agent_id = Self::world_gm_agent(transaction, world_id).await?;
        let branch_id = sqlx::query!(
            "INSERT INTO history_branch (agent_id) VALUES (?)",
            gm_agent_id,
        )
        .execute(&mut **transaction)
        .await
        .with_context(|| format!("creating history branch for group {}", group.name))?
        .last_insert_rowid();
        let gm_notes = Json(&group.notes.gm);
        let storyteller_notes = Json(&group.notes.storyteller);
        Ok(sqlx::query!(
            "INSERT INTO \"group\" (world_id, name, location_id, description, branch_id, gm_notes, storyteller_notes, scene_narrated) \
             VALUES (?, ?, ?, ?, ?, ?, ?, 0)",
            world_id,
            group.name,
            location_id,
            group.description,
            branch_id,
            gm_notes,
            storyteller_notes,
        )
        .execute(&mut **transaction)
        .await
        .with_context(|| format!("creating group {}", group.name))?
        .last_insert_rowid())
    }

    pub(super) async fn insert_item_type(
        transaction: &mut Transaction<'_, Sqlite>,
        world_id: i64,
        item_type: &ItemType,
    ) -> Result<i64> {
        let stats = Json(&item_type.stats);
        let gm_notes = Json(&item_type.notes.gm);
        let storyteller_notes = Json(&item_type.notes.storyteller);
        Ok(sqlx::query!(
            "INSERT INTO item_type (world_id, name, description, stats, gm_notes, storyteller_notes) VALUES (?, ?, ?, ?, ?, ?)",
            world_id,
            item_type.name,
            item_type.description,
            stats,
            gm_notes,
            storyteller_notes,
        )
        .execute(&mut **transaction)
        .await
        .with_context(|| format!("creating item type {}", item_type.name))?
        .last_insert_rowid())
    }

    pub(super) async fn insert_item(
        transaction: &mut Transaction<'_, Sqlite>,
        world_id: i64,
        item_type_id: i64,
        holder: Holder,
        name: &str,
        description: &str,
        notes: &Notes,
    ) -> Result<i64> {
        let gm_notes = Json(&notes.gm);
        let storyteller_notes = Json(&notes.storyteller);
        let item_id = sqlx::query!(
            "INSERT INTO item (world_id, item_type_id, name, description, gm_notes, storyteller_notes) \
             VALUES (?, ?, ?, ?, ?, ?)",
            world_id,
            item_type_id,
            name,
            description,
            gm_notes,
            storyteller_notes,
        )
        .execute(&mut **transaction)
        .await
        .with_context(|| format!("creating item {name}"))?
        .last_insert_rowid();
        Self::move_item(transaction, item_id, holder)
            .await
            .with_context(|| format!("placing item {name}"))?;
        Ok(item_id)
    }

    pub(super) async fn insert_npc(
        transaction: &mut Transaction<'_, Sqlite>,
        world_id: i64,
        group_id: i64,
        npc: &Npc,
    ) -> Result<i64> {
        let character_id = Self::insert_character(
            transaction,
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
        sqlx::query!(
            "INSERT INTO character_group (character_id, group_id, description) VALUES (?, ?, '')",
            character_id,
            group_id,
        )
        .execute(&mut **transaction)
        .await
        .with_context(|| format!("placing NPC {}", npc.name))?;
        let agent_id = sqlx::query!("INSERT INTO agent (world_id) VALUES (?)", world_id)
            .execute(&mut **transaction)
            .await
            .with_context(|| format!("creating NPC agent for {}", npc.name))?
            .last_insert_rowid();
        Self::create_agent_history(transaction, agent_id).await?;
        sqlx::query!(
            "INSERT INTO npc_agent (character_id, agent_id) VALUES (?, ?)",
            character_id,
            agent_id,
        )
        .execute(&mut **transaction)
        .await
        .with_context(|| format!("linking NPC agent for {}", npc.name))?;
        Ok(character_id)
    }

    /// Add one object to a world whose Storyteller is building it. The fields
    /// are checked against the kind, every reference must be an object of the
    /// world, and a name already taken changes nothing.
    pub async fn create_object(
        &self,
        world_id: i64,
        kind: ObjectKind,
        fields: &Fields,
    ) -> Result<Changed> {
        fields.check(kind, true)?;
        let mut transaction = self
            .pool
            .begin()
            .await
            .with_context(|| format!("creating an object in world {world_id}"))?;
        Self::ensure_stage(&mut transaction, world_id, "build").await?;
        let notes = Notes::default();
        let id = match kind {
            ObjectKind::Location => {
                let name = need(&fields.name, "name")?;
                Self::ensure_unused(&mut transaction, world_id, kind, &name).await?;
                let location = Location {
                    name,
                    kind: need(&fields.kind, "kind")?,
                    description: need(&fields.description, "description")?,
                    notes,
                };
                Self::insert_location(&mut transaction, world_id, &location).await?
            }
            ObjectKind::Connection => {
                let from = need(&fields.from_id, "from_id")?;
                let to = need(&fields.to_id, "to_id")?;
                let travel_time = need(&fields.travel_time, "travel_time")?;
                if travel_time < 0 {
                    return reject("A connection's travel time cannot be negative.".into());
                }
                if from == to {
                    return reject("A connection cannot join a location to itself.".into());
                }
                Self::ensure_exists(&mut transaction, world_id, ObjectKind::Location, from).await?;
                Self::ensure_exists(&mut transaction, world_id, ObjectKind::Location, to).await?;
                if let Some(existing) = sqlx::query_scalar!(
                    "SELECT id FROM connection WHERE world_id = ? AND from_location_id = ? \
                     AND to_location_id = ? AND deleted = 0",
                    world_id,
                    from,
                    to,
                )
                .fetch_optional(&mut *transaction)
                .await
                .context("looking for an existing connection")?
                {
                    return reject(format!(
                        "Connection {existing} already leads from location {from} to location {to}."
                    ));
                }
                // The ends are given as IDs, so the names in the record stay empty.
                let connection = Connection {
                    from: String::new(),
                    to: String::new(),
                    travel_time,
                    description: need(&fields.description, "description")?,
                    notes,
                };
                Self::insert_connection(&mut transaction, world_id, from, to, &connection).await?
            }
            ObjectKind::Group => {
                let name = need(&fields.name, "name")?;
                Self::ensure_unused(&mut transaction, world_id, kind, &name).await?;
                let location_id = need(&fields.location_id, "location_id")?;
                Self::ensure_exists(&mut transaction, world_id, ObjectKind::Location, location_id)
                    .await?;
                let group = Group {
                    name,
                    location: String::new(),
                    description: need(&fields.description, "description")?,
                    notes,
                };
                Self::insert_group(&mut transaction, world_id, location_id, &group).await?
            }
            ObjectKind::Npc => {
                let name = need(&fields.name, "name")?;
                Self::ensure_unused(&mut transaction, world_id, kind, &name).await?;
                let group_id = need(&fields.group_id, "group_id")?;
                Self::ensure_exists(&mut transaction, world_id, ObjectKind::Group, group_id)
                    .await?;
                let npc = Npc {
                    name,
                    group: String::new(),
                    description: need(&fields.description, "description")?,
                    background: need(&fields.background, "background")?,
                    motive: need(&fields.motive, "motive")?,
                    ambition: fields.ambition.clone().unwrap_or_default(),
                    sheet: fields.sheet.clone().unwrap_or_else(|| serde_json::json!({})),
                    notes,
                };
                Self::insert_npc(&mut transaction, world_id, group_id, &npc).await?
            }
            ObjectKind::ItemType => {
                let name = need(&fields.name, "name")?;
                Self::ensure_unused(&mut transaction, world_id, kind, &name).await?;
                let item_type = ItemType {
                    name,
                    description: need(&fields.description, "description")?,
                    stats: fields.stats.clone().unwrap_or_else(|| serde_json::json!({})),
                    notes,
                };
                Self::insert_item_type(&mut transaction, world_id, &item_type).await?
            }
            ObjectKind::Item => {
                let name = need(&fields.name, "name")?;
                Self::ensure_unused(&mut transaction, world_id, kind, &name).await?;
                let item_type_id = need(&fields.item_type_id, "item_type_id")?;
                Self::ensure_exists(&mut transaction, world_id, ObjectKind::ItemType, item_type_id)
                    .await?;
                let holder = match (fields.location_id, fields.held_by_id) {
                    (Some(location), None) => {
                        Self::ensure_exists(
                            &mut transaction,
                            world_id,
                            ObjectKind::Location,
                            location,
                        )
                        .await?;
                        Holder::Location(location)
                    }
                    (None, Some(npc)) => {
                        Self::ensure_exists(&mut transaction, world_id, ObjectKind::Npc, npc)
                            .await?;
                        // What an NPC carries is part of the NPC.
                        Self::clear_review(&mut transaction, ObjectKind::Npc, npc).await?;
                        Holder::Character(npc)
                    }
                    _ => {
                        return reject(
                            "Give either location_id or held_by_id to say where the item is."
                                .into(),
                        );
                    }
                };
                Self::insert_item(
                    &mut transaction,
                    world_id,
                    item_type_id,
                    holder,
                    &name,
                    &need(&fields.description, "description")?,
                    &notes,
                )
                .await?
            }
        };
        let counts = Self::counts(&mut transaction, world_id).await?;
        transaction
            .commit()
            .await
            .with_context(|| format!("committing an object of world {world_id}"))?;
        Ok(Changed { id, counts })
    }

    /// The object of `kind` called `name` in the world, if there is one.
    pub(super) async fn find_by_name(
        transaction: &mut Transaction<'_, Sqlite>,
        world_id: i64,
        kind: ObjectKind,
        name: &str,
        include_deleted: bool,
    ) -> Result<Option<i64>> {
        match kind {
            ObjectKind::Location => {
                sqlx::query_scalar!(
                    "SELECT id FROM location WHERE world_id = ? AND name = ? AND (deleted = 0 OR ?)",
                    world_id,
                    name,
                    include_deleted,
                )
                .fetch_optional(&mut **transaction)
                .await
            }
            ObjectKind::Group => {
                sqlx::query_scalar!(
                    "SELECT id FROM \"group\" WHERE world_id = ? AND name = ? AND (deleted = 0 OR ?)",
                    world_id,
                    name,
                    include_deleted,
                )
                .fetch_optional(&mut **transaction)
                .await
            }
            ObjectKind::ItemType => {
                sqlx::query_scalar!(
                    "SELECT id FROM item_type WHERE world_id = ? AND name = ? AND (deleted = 0 OR ?)",
                    world_id,
                    name,
                    include_deleted,
                )
                .fetch_optional(&mut **transaction)
                .await
            }
            // A connection has no name; it is told apart by the locations it joins.
            ObjectKind::Connection => Ok(None),
            ObjectKind::Npc => {
                sqlx::query_scalar!(
                    "SELECT id FROM character WHERE world_id = ? AND role = 'npc' AND name = ? AND (deleted = 0 OR ?)",
                    world_id,
                    name,
                    include_deleted,
                )
                .fetch_optional(&mut **transaction)
                .await
            }
            ObjectKind::Item => {
                sqlx::query_scalar!(
                    "SELECT id FROM item WHERE world_id = ? AND name = ? AND (deleted = 0 OR ?)",
                    world_id,
                    name,
                    include_deleted,
                )
                .fetch_optional(&mut **transaction)
                .await
            }
        }
        .with_context(|| format!("looking up {} {name}", kind.as_str()))
    }

    /// Refuse an ID that is not an object of the kind in this world.
    pub(super) async fn ensure_exists(
        transaction: &mut Transaction<'_, Sqlite>,
        world_id: i64,
        kind: ObjectKind,
        id: i64,
    ) -> Result<()> {
        let found = match kind {
            ObjectKind::Location => {
                sqlx::query_scalar!(
                    "SELECT id FROM location WHERE id = ? AND world_id = ? AND deleted = 0",
                    id,
                    world_id,
                )
                .fetch_optional(&mut **transaction)
                .await
            }
            ObjectKind::Connection => {
                sqlx::query_scalar!(
                    "SELECT id FROM connection WHERE id = ? AND world_id = ? AND deleted = 0",
                    id,
                    world_id,
                )
                .fetch_optional(&mut **transaction)
                .await
            }
            ObjectKind::Group => {
                sqlx::query_scalar!(
                    "SELECT id FROM \"group\" WHERE id = ? AND world_id = ? AND deleted = 0",
                    id,
                    world_id,
                )
                .fetch_optional(&mut **transaction)
                .await
            }
            ObjectKind::Npc => {
                sqlx::query_scalar!(
                    "SELECT id FROM character WHERE id = ? AND world_id = ? AND role = 'npc' \
                     AND deleted = 0",
                    id,
                    world_id,
                )
                .fetch_optional(&mut **transaction)
                .await
            }
            ObjectKind::ItemType => {
                sqlx::query_scalar!(
                    "SELECT id FROM item_type WHERE id = ? AND world_id = ? AND deleted = 0",
                    id,
                    world_id,
                )
                .fetch_optional(&mut **transaction)
                .await
            }
            ObjectKind::Item => {
                sqlx::query_scalar!(
                    "SELECT id FROM item WHERE id = ? AND world_id = ? AND deleted = 0",
                    id,
                    world_id,
                )
                .fetch_optional(&mut **transaction)
                .await
            }
        }
        .with_context(|| format!("looking up {} {id}", kind.as_str()))?;
        match found {
            Some(_) => Ok(()),
            None => reject(format!(
                "There is no {} with id {id} in the world.",
                kind.as_str()
            )),
        }
    }

    pub(super) async fn ensure_unused(
        transaction: &mut Transaction<'_, Sqlite>,
        world_id: i64,
        kind: ObjectKind,
        name: &str,
    ) -> Result<()> {
        // A deleted object keeps its name, since it still exists.
        if Self::find_by_name(transaction, world_id, kind, name, true)
            .await?
            .is_some()
        {
            return reject(format!(
                "The world already has a {} called `{name}`.",
                kind.as_str()
            ));
        }
        Ok(())
    }

    /// The Questioner's review no longer holds for an object that changed.
    pub(super) async fn clear_review(
        transaction: &mut Transaction<'_, Sqlite>,
        kind: ObjectKind,
        object_id: i64,
    ) -> Result<()> {
        let kind = kind.as_str();
        sqlx::query!(
            "DELETE FROM object_reviewed WHERE kind = ? AND object_id = ?",
            kind,
            object_id,
        )
        .execute(&mut **transaction)
        .await
        .context("clearing a review")?;
        Ok(())
    }

    pub(super) async fn ensure_stage(
        transaction: &mut Transaction<'_, Sqlite>,
        world_id: i64,
        wanted: &str,
    ) -> Result<()> {
        let stage =
            sqlx::query_scalar!("SELECT generation_stage FROM world WHERE id = ?", world_id)
                .fetch_one(&mut **transaction)
                .await
                .with_context(|| format!("loading world {world_id}"))?;
        if stage != wanted {
            return reject(format!(
                "That does not apply now: the world is at the `{stage}` stage, not `{wanted}`."
            ));
        }
        Ok(())
    }

    pub(super) async fn move_item(
        transaction: &mut Transaction<'_, Sqlite>,
        item_id: i64,
        holder: Holder,
    ) -> Result<()> {
        sqlx::query!("DELETE FROM item_location WHERE item_id = ?", item_id)
            .execute(&mut **transaction)
            .await
            .context("taking an item from its location")?;
        sqlx::query!("DELETE FROM item_character WHERE item_id = ?", item_id)
            .execute(&mut **transaction)
            .await
            .context("taking an item from its holder")?;
        match holder {
            Holder::Location(location_id) => {
                sqlx::query!(
                    "INSERT INTO item_location (item_id, location_id) VALUES (?, ?)",
                    item_id,
                    location_id,
                )
                .execute(&mut **transaction)
                .await
            }
            Holder::Character(character_id) => {
                sqlx::query!(
                    "INSERT INTO item_character (item_id, character_id) VALUES (?, ?)",
                    item_id,
                    character_id,
                )
                .execute(&mut **transaction)
                .await
            }
        }
        .with_context(|| format!("placing item {item_id}"))?;
        Ok(())
    }

    pub(super) async fn counts(
        transaction: &mut Transaction<'_, Sqlite>,
        world_id: i64,
    ) -> Result<Counts> {
        sqlx::query_as!(
            Counts,
            "SELECT \
                (SELECT COUNT(*) FROM location WHERE world_id = ?1 AND deleted = 0) AS \"locations!: i64\", \
                (SELECT COUNT(*) FROM connection WHERE world_id = ?1 AND deleted = 0) AS \"connections!: i64\", \
                (SELECT COUNT(*) FROM \"group\" WHERE world_id = ?1 AND deleted = 0) AS \"groups!: i64\", \
                (SELECT COUNT(*) FROM character WHERE world_id = ?1 AND role = 'npc' AND deleted = 0) AS \"npcs!: i64\", \
                (SELECT COUNT(*) FROM item_type WHERE world_id = ?1 AND deleted = 0) AS \"item_types!: i64\", \
                (SELECT COUNT(*) FROM item WHERE world_id = ?1 AND deleted = 0) AS \"items!: i64\"",
            world_id,
        )
        .fetch_one(&mut **transaction)
        .await
        .with_context(|| format!("counting the objects of world {world_id}"))
    }
}

/// Fixtures the tests of the three authoring modules share.
#[cfg(test)]
pub(in crate::store) mod fixtures {
    use crate::{
        scenario::{Group, Location, Notes, Npc},
        store::{Store, TestDatabase},
    };

    pub(in crate::store) async fn building_world() -> (Store, TestDatabase, i64) {
        let db = TestDatabase::new("authoring");
        let store = Store::open(db.path()).await.unwrap();
        let owner = store
            .find_or_create_user("maker@example.test", "Maker")
            .await
            .unwrap();
        let world_id = store
            .create_world_to_generate(&owner, "a seed")
            .await
            .unwrap()
            .world_id;
        sqlx::query!(
            "UPDATE world SET generation_stage = 'build' WHERE id = ?",
            world_id
        )
        .execute(&store.pool)
        .await
        .unwrap();
        (store, db, world_id)
    }

    pub(in crate::store) fn location(name: &str) -> Location {
        Location {
            name: name.into(),
            kind: "room".into(),
            description: format!("The {name}."),
            notes: Notes::default(),
        }
    }

    pub(in crate::store) fn group_at(name: &str, location: &str) -> Group {
        Group {
            name: name.into(),
            location: location.into(),
            description: String::new(),
            notes: Notes::default(),
        }
    }

    pub(in crate::store) fn npc_in(name: &str, group: &str) -> Npc {
        Npc {
            name: name.into(),
            group: group.into(),
            description: String::new(),
            background: "Has a past.".into(),
            motive: "Has a goal.".into(),
            ambition: String::new(),
            sheet: serde_json::json!({}),
            notes: Notes::default(),
        }
    }

    pub(in crate::store) async fn set_stage(store: &Store, world: i64, stage: &str) {
        sqlx::query("UPDATE world SET generation_stage = ? WHERE id = ?")
            .bind(stage)
            .bind(world)
            .execute(&store.pool)
            .await
            .unwrap();
    }
}

#[cfg(test)]
mod tests {
    use super::fixtures::*;
    use super::*;
    use crate::scenario::{Group, ItemType, Notes, Npc};

    #[tokio::test]
    async fn the_storyteller_builds_a_world_one_object_at_a_time() {
        let (store, _db, world) = building_world().await;
        let hut = location("hut");
        let yard = location("yard");
        store
            .create_object(world, NewObject::Location(&hut))
            .await
            .unwrap();
        store
            .create_object(world, NewObject::Location(&yard))
            .await
            .unwrap();
        let path = Connection {
            from: "hut".into(),
            to: "yard".into(),
            travel_time: 1,
            description: "A short path.".into(),
            notes: Notes::default(),
        };
        store
            .create_object(world, NewObject::Connection(&path))
            .await
            .unwrap();
        let family = Group {
            name: "family".into(),
            location: "hut".into(),
            description: "The household.".into(),
            notes: Notes::default(),
        };
        store
            .create_object(world, NewObject::Group(&family))
            .await
            .unwrap();
        let toma = Npc {
            name: "Toma".into(),
            group: "family".into(),
            description: "A farmer.".into(),
            background: "Owes money.".into(),
            motive: "Keep the farm.".into(),
            ambition: String::new(),
            sheet: serde_json::json!({}),
            notes: Notes::default(),
        };
        store
            .create_object(world, NewObject::Npc(&toma))
            .await
            .unwrap();
        let loaf = ItemType {
            name: "loaf".into(),
            description: "Bread.".into(),
            stats: serde_json::json!({}),
            notes: Notes::default(),
        };
        store
            .create_object(world, NewObject::ItemType(&loaf))
            .await
            .unwrap();
        let counts = store
            .create_object(
                world,
                NewObject::Item(&NewItem {
                    name: "stale loaf".into(),
                    item_type: "loaf".into(),
                    location: Some("hut".into()),
                    held_by: None,
                    description: "Hard.".into(),
                }),
            )
            .await
            .unwrap();
        assert_eq!(
            counts,
            Counts {
                locations: 2,
                connections: 1,
                groups: 1,
                npcs: 1,
                item_types: 1,
                items: 1
            }
        );

        // The NPC is a real one: it has its own agent and chat.
        let npc_agents = sqlx::query_scalar!(
            "SELECT COUNT(*) FROM npc_agent JOIN character ON character.id = npc_agent.character_id \
             WHERE character.world_id = ?",
            world,
        )
        .fetch_one(&store.pool)
        .await
        .unwrap();
        assert_eq!(npc_agents, 1);
    }

    #[tokio::test]
    async fn a_rejected_object_changes_nothing() {
        let (store, _db, world) = building_world().await;
        let hut = location("hut");
        store
            .create_object(world, NewObject::Location(&hut))
            .await
            .unwrap();
        let twice = store
            .create_object(world, NewObject::Location(&hut))
            .await
            .unwrap_err();
        assert!(format!("{twice:#}").contains("already has a location called `hut`"));
        let stranger = Npc {
            name: "Toma".into(),
            group: "nobody".into(),
            description: String::new(),
            background: String::new(),
            motive: String::new(),
            ambition: String::new(),
            sheet: serde_json::json!({}),
            notes: Notes::default(),
        };
        let missing = store
            .create_object(world, NewObject::Npc(&stranger))
            .await
            .unwrap_err();
        assert!(format!("{missing:#}").contains("no group called `nobody`"));
        let yard = location("yard");
        let counts = store
            .create_object(world, NewObject::Location(&yard))
            .await
            .unwrap();
        assert_eq!((counts.locations, counts.npcs), (2, 0));
    }

    #[tokio::test]
    async fn objects_are_only_created_while_the_world_is_being_built() {
        let (store, _db, world) = building_world().await;
        sqlx::query!(
            "UPDATE world SET generation_stage = 'review' WHERE id = ?",
            world
        )
        .execute(&store.pool)
        .await
        .unwrap();
        let hut = location("hut");
        let error = store
            .create_object(world, NewObject::Location(&hut))
            .await
            .unwrap_err();
        assert!(format!("{error:#}").contains("not `build`"));
    }

    #[tokio::test]
    async fn an_item_an_npc_holds_belongs_to_the_npc_and_unreviews_it() {
        let (store, _db, world) = building_world().await;
        store
            .create_object(world, NewObject::Location(&location("hut")))
            .await
            .unwrap();
        let group = Group {
            name: "family".into(),
            location: "hut".into(),
            description: String::new(),
            notes: Notes::default(),
        };
        store
            .create_object(world, NewObject::Group(&group))
            .await
            .unwrap();
        let toma = Npc {
            name: "Toma".into(),
            group: "family".into(),
            description: String::new(),
            background: "b".into(),
            motive: "m".into(),
            ambition: String::new(),
            sheet: serde_json::json!({}),
            notes: Notes::default(),
        };
        store
            .create_object(world, NewObject::Npc(&toma))
            .await
            .unwrap();
        let axe_type = ItemType {
            name: "axe".into(),
            description: String::new(),
            stats: serde_json::json!({}),
            notes: Notes::default(),
        };
        store
            .create_object(world, NewObject::ItemType(&axe_type))
            .await
            .unwrap();
        sqlx::query!(
            "UPDATE world SET generation_stage = 'review' WHERE id = ?",
            world
        )
        .execute(&store.pool)
        .await
        .unwrap();
        store
            .set_reviewed(world, ObjectKind::Npc, "Toma", true)
            .await
            .unwrap();
        sqlx::query!(
            "UPDATE world SET generation_stage = 'build' WHERE id = ?",
            world
        )
        .execute(&store.pool)
        .await
        .unwrap();

        let axe = |holder: (Option<&str>, Option<&str>)| NewItem {
            name: "wood axe".into(),
            item_type: "axe".into(),
            description: String::new(),
            location: holder.0.map(Into::into),
            held_by: holder.1.map(Into::into),
        };
        let both = store
            .create_object(world, NewObject::Item(&axe((Some("hut"), Some("Toma")))))
            .await
            .unwrap_err();
        assert!(format!("{both:#}").contains("either a location or an NPC"));
        store
            .create_object(world, NewObject::Item(&axe((None, Some("Toma")))))
            .await
            .unwrap();

        let listed = store.review_list(world).await.unwrap();
        assert!(
            listed.iter().all(|entry| entry.kind != "item"),
            "{listed:?}"
        );
        let toma = listed.iter().find(|entry| entry.name == "Toma").unwrap();
        assert!(!toma.reviewed);
        assert!(
            store
                .read_object(world, ObjectKind::Item, "wood axe")
                .await
                .unwrap()
                .contains("Toma")
        );
    }

    #[tokio::test]
    async fn a_second_connection_between_the_same_locations_is_refused() {
        let (store, _db, world) = building_world().await;
        for name in ["hut", "yard"] {
            store
                .create_object(world, NewObject::Location(&location(name)))
                .await
                .unwrap();
        }
        let path = Connection {
            from: "hut".into(),
            to: "yard".into(),
            travel_time: 1,
            description: String::new(),
            notes: Notes::default(),
        };
        store
            .create_object(world, NewObject::Connection(&path))
            .await
            .unwrap();
        let twice = store
            .create_object(world, NewObject::Connection(&path))
            .await
            .unwrap_err();
        assert!(format!("{twice:#}").contains("already has a connection called `hut -> yard`"));
        // The other direction is a different connection.
        let back = Connection {
            from: "yard".into(),
            to: "hut".into(),
            ..path
        };
        let counts = store
            .create_object(world, NewObject::Connection(&back))
            .await
            .unwrap();
        assert_eq!(counts.connections, 2);
    }
}
