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

// The kinds of top-level object the Questioner reviews, and the allowed values
// of the Storyteller and Questioner tools' `object` argument.
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
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Location => "location",
            Self::Connection => "connection",
            Self::Group => "group",
            Self::Npc => "npc",
            Self::ItemType => "item_type",
            Self::Item => "item",
        }
    }

    /// The fields creating or changing an object of this kind takes, and whether
    /// creating one needs it. They never overlap or contradict, so a call with
    /// valid fields is valid. An item is created lying at a location; it is
    /// picked up with `give_item`.
    pub fn fields(self, creating: bool) -> &'static [(&'static str, bool)] {
        match (self, creating) {
            (Self::Location, _) => &[
                ("name", true),
                ("location_kind", true),
                ("description", true),
            ],
            (Self::Connection, true) => &[
                ("name", true),
                ("from_id", true),
                ("to_id", true),
                ("travel_time", true),
                ("description", true),
            ],
            (Self::Connection, false) => &[
                ("name", false),
                ("from_id", false),
                ("to_id", false),
                ("travel_time", false),
                ("description", false),
            ],
            // A group is never created or changed by itself: every NPC starts in
            // a group of its own, `group_npcs` puts NPCs together, and a group
            // goes with its last NPC.
            (Self::Group, _) => &[],
            (Self::Npc, true) => &[
                ("name", true),
                ("location_id", true),
                ("description", true),
                ("background", true),
                ("motive", true),
                ("ambition", false),
                ("sheet", false),
            ],
            (Self::Npc, false) => &[
                ("name", false),
                ("description", false),
                ("background", false),
                ("motive", false),
                ("ambition", false),
                ("sheet", false),
            ],
            (Self::ItemType, _) => &[("name", true), ("description", true), ("stats", false)],
            (Self::Item, true) => &[
                ("name", true),
                ("item_type_id", true),
                ("description", true),
                ("location_id", true),
            ],
            (Self::Item, false) => &[
                ("name", false),
                ("item_type_id", false),
                ("description", false),
                ("location_id", false),
            ],
        }
    }
}

// The fields of any object; which of them an object has depends on its kind.
// Objects are referred to by the IDs the tools return.
#[derive(Clone, Debug, Default, Deserialize, JsonSchema, Serialize)]
pub struct Fields {
    pub name: Option<String>,
    pub location_kind: Option<String>,
    pub description: Option<String>,
    pub from_id: Option<i64>,
    pub to_id: Option<i64>,
    pub travel_time: Option<i64>,
    pub location_id: Option<i64>,
    pub item_type_id: Option<i64>,
    pub background: Option<String>,
    pub motive: Option<String>,
    pub ambition: Option<String>,
    #[schemars(with = "Option<std::collections::BTreeMap<String, serde_json::Value>>")]
    pub sheet: Option<serde_json::Value>,
    #[schemars(with = "Option<std::collections::BTreeMap<String, serde_json::Value>>")]
    pub stats: Option<serde_json::Value>,
}

impl Fields {
    /// All fields the generic creation and update tools accept.
    pub fn names() -> Vec<String> {
        serde_json::to_value(Self::default())
            .expect("default fields should serialize")
            .as_object()
            .expect("fields should serialize as an object")
            .keys()
            .cloned()
            .collect()
    }

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
        let has: Vec<&str> = kind
            .fields(creating)
            .iter()
            .map(|(name, _)| *name)
            .collect();
        if let Some(extra) = given.iter().find(|field| !has.contains(&field.as_str())) {
            if !creating && kind.fields(true).iter().any(|(name, _)| name == extra) {
                return reject(format!(
                    "A {}'s `{extra}` cannot be changed after it is created.",
                    kind.as_str()
                ));
            }
            return reject(format!(
                "A {} has the fields {}, not `{extra}`.",
                kind.as_str(),
                has.join(", ")
            ));
        }
        if creating {
            let missing: Vec<&str> = kind
                .fields(true)
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

/// The object a change created or acted on.
#[derive(Debug, Serialize)]
pub struct Changed {
    pub kind: &'static str,
    pub id: i64,
    /// The group made for an NPC that started a new one.
    pub group: Option<i64>,
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

    /// Refuse ends that are not two different locations of the world, or that
    /// another connection (not `except`) already joins in that direction.
    pub(super) async fn ensure_ends(
        transaction: &mut Transaction<'_, Sqlite>,
        world_id: i64,
        from: i64,
        to: i64,
        except: Option<i64>,
    ) -> Result<()> {
        if from == to {
            return reject("A connection cannot join a location to itself.".into());
        }
        Self::ensure_exists(transaction, world_id, ObjectKind::Location, from).await?;
        Self::ensure_exists(transaction, world_id, ObjectKind::Location, to).await?;
        let except = except.unwrap_or(0);
        if let Some(existing) = sqlx::query_scalar!(
            "SELECT id FROM connection WHERE world_id = ? AND from_location_id = ? \
             AND to_location_id = ? AND deleted = 0 AND id <> ?",
            world_id,
            from,
            to,
            except,
        )
        .fetch_optional(&mut **transaction)
        .await
        .context("looking for an existing connection")?
        {
            return reject(format!(
                "Connection {existing} already leads from location {from} to location {to}."
            ));
        }
        Ok(())
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
            "INSERT INTO connection (world_id, name, from_location_id, to_location_id, travel_time, description, gm_notes, storyteller_notes) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
            world_id,
            connection.name,
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
            "INSERT INTO \"group\" (world_id, name, location_id, branch_id, gm_notes, storyteller_notes, scene_narrated) \
             VALUES (?, ?, ?, ?, ?, ?, 0)",
            world_id,
            group.name,
            location_id,
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

    /// Add one object to a world whose Storyteller is creating its objects. The fields
    /// are checked against the kind, every reference must be an object of the
    /// world, and a name already taken changes nothing.
    pub async fn create_object(
        &self,
        world_id: i64,
        kind: ObjectKind,
        fields: &Fields,
    ) -> Result<Changed> {
        if kind == ObjectKind::Group {
            return reject(
                "A group is made with an NPC: create the NPC at a location, and put NPCs together with group_npcs."
                    .into(),
            );
        }
        fields.check(kind, true)?;
        let mut transaction = self
            .pool
            .begin()
            .await
            .with_context(|| format!("creating an object in world {world_id}"))?;
        Self::ensure_stage(&mut transaction, world_id, &["create_objects", "review"]).await?;
        let notes = Notes::default();
        let mut made_group = None;
        let id = match kind {
            ObjectKind::Location => {
                let name = need(&fields.name, "name")?;
                Self::ensure_unused(&mut transaction, world_id, kind, &name).await?;
                let location = Location {
                    name,
                    kind: need(&fields.location_kind, "location_kind")?,
                    description: need(&fields.description, "description")?,
                    notes,
                };
                Self::insert_location(&mut transaction, world_id, &location).await?
            }
            ObjectKind::Connection => {
                let name = need(&fields.name, "name")?;
                let from = need(&fields.from_id, "from_id")?;
                let to = need(&fields.to_id, "to_id")?;
                let travel_time = need(&fields.travel_time, "travel_time")?;
                if travel_time < 0 {
                    return reject("A connection's travel time cannot be negative.".into());
                }
                Self::ensure_ends(&mut transaction, world_id, from, to, None).await?;
                Self::ensure_unused(&mut transaction, world_id, kind, &name).await?;
                // The ends are given as IDs, so the names in the record stay empty.
                let connection = Connection {
                    name,
                    from: String::new(),
                    to: String::new(),
                    travel_time,
                    description: need(&fields.description, "description")?,
                    notes,
                };
                Self::insert_connection(&mut transaction, world_id, from, to, &connection).await?
            }
            ObjectKind::Group => unreachable!("a group is refused before it is made"),
            ObjectKind::Npc => {
                let name = need(&fields.name, "name")?;
                Self::ensure_unused(&mut transaction, world_id, kind, &name).await?;
                let location_id = need(&fields.location_id, "location_id")?;
                let group_id =
                    Self::start_group(&mut transaction, world_id, &name, location_id).await?;
                made_group = Some(group_id);
                let npc = Npc {
                    name,
                    group: String::new(),
                    description: need(&fields.description, "description")?,
                    background: need(&fields.background, "background")?,
                    motive: need(&fields.motive, "motive")?,
                    ambition: fields.ambition.clone().unwrap_or_default(),
                    sheet: fields
                        .sheet
                        .clone()
                        .unwrap_or_else(|| serde_json::json!({})),
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
                    stats: fields
                        .stats
                        .clone()
                        .unwrap_or_else(|| serde_json::json!({})),
                    notes,
                };
                Self::insert_item_type(&mut transaction, world_id, &item_type).await?
            }
            ObjectKind::Item => {
                let name = need(&fields.name, "name")?;
                Self::ensure_unused(&mut transaction, world_id, kind, &name).await?;
                let item_type_id = need(&fields.item_type_id, "item_type_id")?;
                Self::ensure_exists(
                    &mut transaction,
                    world_id,
                    ObjectKind::ItemType,
                    item_type_id,
                )
                .await?;
                let location = need(&fields.location_id, "location_id")?;
                Self::ensure_exists(&mut transaction, world_id, ObjectKind::Location, location)
                    .await?;
                let holder = Holder::Location(location);
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
        transaction
            .commit()
            .await
            .with_context(|| format!("committing an object of world {world_id}"))?;
        Ok(Changed {
            kind: kind.as_str(),
            id,
            group: made_group,
        })
    }

    /// The group a new NPC starts in on its own, so a group never exists
    /// without a character. It is named after the NPC until it is renamed.
    async fn start_group(
        transaction: &mut Transaction<'_, Sqlite>,
        world_id: i64,
        npc: &str,
        location_id: i64,
    ) -> Result<i64> {
        let name = format!("{npc}'s group");
        if let Some(existing) =
            Self::find_by_name(transaction, world_id, ObjectKind::Group, &name, true).await?
        {
            return reject(format!(
                "Group {existing} is already called `{name}`; rename it with group_npcs first."
            ));
        }
        Self::ensure_exists(transaction, world_id, ObjectKind::Location, location_id).await?;
        let group = Group {
            name,
            location: String::new(),
            notes: Notes::default(),
        };
        Self::insert_group(transaction, world_id, location_id, &group).await
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
            ObjectKind::Connection => {
                sqlx::query_scalar!(
                    "SELECT id FROM connection WHERE world_id = ? AND name = ? AND (deleted = 0 OR ?)",
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
            None => {
                // Say which ids do exist, so the next attempt can use one.
                let existing: Vec<String> = Self::review_entries(transaction, world_id)
                    .await?
                    .into_iter()
                    .filter(|entry| entry.kind == kind.as_str())
                    .map(|entry| entry.id.to_string())
                    .collect();
                reject(format!(
                    "There is no {} with id {id} in the world. Existing {} ids: {}.",
                    kind.as_str(),
                    kind.as_str(),
                    if existing.is_empty() {
                        "none".to_string()
                    } else {
                        existing.join(", ")
                    }
                ))
            }
        }
    }

    pub(super) async fn ensure_unused(
        transaction: &mut Transaction<'_, Sqlite>,
        world_id: i64,
        kind: ObjectKind,
        name: &str,
    ) -> Result<()> {
        // A deleted object keeps its name, since it still exists.
        match Self::find_by_name(transaction, world_id, kind, name, true).await? {
            Some(id) => reject(format!(
                "The world already has {} {id} called `{name}`; give id {id} to change it.",
                kind.as_str()
            )),
            None => Ok(()),
        }
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
        wanted: &[&str],
    ) -> Result<()> {
        let stage =
            sqlx::query_scalar!("SELECT generation_stage FROM world WHERE id = ?", world_id)
                .fetch_one(&mut **transaction)
                .await
                .with_context(|| format!("loading world {world_id}"))?;
        if !wanted.contains(&stage.as_str()) {
            let wanted = wanted.join("` or `");
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
}

/// Fixtures the tests of the three authoring modules share.
#[cfg(test)]
pub(in crate::store) mod fixtures {
    use crate::store::{Fields, ObjectKind, Store, TestDatabase};

    pub(in crate::store) async fn world_creating_objects() -> (Store, TestDatabase, i64) {
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
            "UPDATE world SET generation_stage = 'create_objects' WHERE id = ?",
            world_id
        )
        .execute(&store.pool)
        .await
        .unwrap();
        (store, db, world_id)
    }

    pub(in crate::store) async fn set_stage(store: &Store, world: i64, stage: &str) {
        sqlx::query("UPDATE world SET generation_stage = ? WHERE id = ?")
            .bind(stage)
            .bind(world)
            .execute(&store.pool)
            .await
            .unwrap();
    }

    pub(in crate::store) async fn create(
        store: &Store,
        world: i64,
        kind: ObjectKind,
        fields: Fields,
    ) -> i64 {
        store.create_object(world, kind, &fields).await.unwrap().id
    }

    pub(in crate::store) fn location_fields(name: &str) -> Fields {
        Fields {
            name: Some(name.into()),
            location_kind: Some("room".into()),
            description: Some(format!("The {name}.")),
            ..Fields::default()
        }
    }

    /// An NPC that starts a new group: the group's and the NPC's IDs.
    pub(in crate::store) async fn group_with_npc(
        store: &Store,
        world: i64,
        group: &str,
        location_id: i64,
        npc: &str,
    ) -> (i64, i64) {
        let changed = store
            .create_object(world, ObjectKind::Npc, &npc_at(npc, location_id))
            .await
            .unwrap();
        let group_id = store
            .group_npcs(world, &[changed.id], group, location_id, "")
            .await
            .unwrap();
        (group_id, changed.id)
    }

    /// An NPC that starts in a group of its own at a location.
    pub(in crate::store) fn npc_at(name: &str, location: i64) -> Fields {
        Fields {
            location_id: Some(location),
            ..npc_fields(name)
        }
    }

    pub(in crate::store) fn npc_fields(name: &str) -> Fields {
        Fields {
            name: Some(name.into()),
            description: Some(String::new()),
            background: Some("Has a past.".into()),
            motive: Some("Has a goal.".into()),
            ..Fields::default()
        }
    }
}

#[cfg(test)]
mod id_tests {
    use super::fixtures::*;
    use super::*;

    #[tokio::test]
    async fn the_storyteller_creates_a_world_one_object_at_a_time() {
        let (store, _db, world) = world_creating_objects().await;
        let hut = create(&store, world, ObjectKind::Location, location_fields("hut")).await;
        let yard = create(&store, world, ObjectKind::Location, location_fields("yard")).await;
        create(
            &store,
            world,
            ObjectKind::Connection,
            Fields {
                name: Some("Hut path".into()),
                from_id: Some(hut),
                to_id: Some(yard),
                travel_time: Some(1),
                description: Some("A short path.".into()),
                ..Fields::default()
            },
        )
        .await;
        let (_, toma) = group_with_npc(&store, world, "family", hut, "Toma").await;
        let loaf = create(
            &store,
            world,
            ObjectKind::ItemType,
            Fields {
                name: Some("loaf".into()),
                description: Some("Bread.".into()),
                ..Fields::default()
            },
        )
        .await;
        let changed = store
            .create_object(
                world,
                ObjectKind::Item,
                &Fields {
                    name: Some("stale loaf".into()),
                    item_type_id: Some(loaf),
                    location_id: Some(hut),
                    description: Some("Hard.".into()),
                    ..Fields::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(changed.id, 1);
        assert!(
            store
                .read_object(world, ObjectKind::Npc, toma)
                .await
                .unwrap()
                .contains("Toma")
        );
    }

    #[tokio::test]
    async fn rejected_creation_changes_nothing_and_creation_requires_the_creating_stage() {
        let (store, _db, world) = world_creating_objects().await;
        create(&store, world, ObjectKind::Location, location_fields("hut")).await;
        let error = store
            .create_object(world, ObjectKind::Location, &location_fields("hut"))
            .await
            .unwrap_err();
        assert!(format!("{error:#}").contains("called `hut`"));
        let error = store
            .create_object(
                world,
                ObjectKind::Npc,
                &Fields {
                    name: Some("Toma".into()),
                    location_id: Some(99),
                    description: Some(String::new()),
                    background: Some(String::new()),
                    motive: Some(String::new()),
                    ..Fields::default()
                },
            )
            .await
            .unwrap_err();
        assert!(format!("{error:#}").contains("no location with id 99"));
        let changed = store
            .create_object(world, ObjectKind::Location, &location_fields("yard"))
            .await
            .unwrap();
        assert_eq!(changed.id, 2);
        set_stage(&store, world, "ask").await;
        let error = store
            .create_object(world, ObjectKind::Location, &location_fields("field"))
            .await
            .unwrap_err();
        assert!(format!("{error:#}").contains("not `create_objects` or `review`"));
    }

    #[tokio::test]
    async fn held_items_are_reviewed_with_their_npc() {
        let (store, _db, world) = world_creating_objects().await;
        let hut = create(&store, world, ObjectKind::Location, location_fields("hut")).await;
        let (_, toma) = group_with_npc(&store, world, "family", hut, "Toma").await;
        let axe_type = create(
            &store,
            world,
            ObjectKind::ItemType,
            Fields {
                name: Some("axe".into()),
                description: Some(String::new()),
                ..Fields::default()
            },
        )
        .await;
        set_stage(&store, world, "review").await;
        store
            .set_reviewed(world, ObjectKind::Npc, toma, true)
            .await
            .unwrap();
        set_stage(&store, world, "create_objects").await;
        let item = create(
            &store,
            world,
            ObjectKind::Item,
            Fields {
                name: Some("wood axe".into()),
                item_type_id: Some(axe_type),
                location_id: Some(hut),
                description: Some(String::new()),
                ..Fields::default()
            },
        )
        .await;
        store.give_item(world, item, toma).await.unwrap();
        assert!(
            store
                .review_list(world)
                .await
                .unwrap()
                .iter()
                .all(|entry| entry.kind != "item")
        );
        assert!(
            store
                .read_object(world, ObjectKind::Item, item)
                .await
                .unwrap()
                .contains("Toma")
        );
        assert!(
            !store
                .review_list(world)
                .await
                .unwrap()
                .into_iter()
                .find(|entry| entry.id == toma)
                .unwrap()
                .reviewed
        );
    }

    #[tokio::test]
    async fn duplicate_connections_are_refused_by_their_ids() {
        let (store, _db, world) = world_creating_objects().await;
        let hut = create(&store, world, ObjectKind::Location, location_fields("hut")).await;
        let yard = create(&store, world, ObjectKind::Location, location_fields("yard")).await;
        let fields = Fields {
            name: Some("Hut path".into()),
            from_id: Some(hut),
            to_id: Some(yard),
            travel_time: Some(1),
            description: Some(String::new()),
            ..Fields::default()
        };
        create(&store, world, ObjectKind::Connection, fields.clone()).await;
        let error = store
            .create_object(world, ObjectKind::Connection, &fields)
            .await
            .unwrap_err();
        assert!(format!("{error:#}").contains("already leads"));
    }

    #[tokio::test]
    async fn a_kind_refuses_fields_it_does_not_have_and_creation_needs_the_ones_it_does() {
        let (store, _db, world) = world_creating_objects().await;
        let hut = create(&store, world, ObjectKind::Location, location_fields("hut")).await;

        // A field of another kind is refused, and the kind's fields are listed.
        let error = store
            .create_object(
                world,
                ObjectKind::Location,
                &Fields {
                    item_type_id: Some(1),
                    ..location_fields("yard")
                },
            )
            .await
            .unwrap_err();
        assert!(
            format!("{error:#}").contains(
                "A location has the fields name, location_kind, description, not `item_type_id`"
            ),
            "{error:#}"
        );

        // A group has no creation of its own, and an NPC needs one.
        let error = store
            .create_object(
                world,
                ObjectKind::Group,
                &Fields {
                    name: Some("family".into()),
                    ..Fields::default()
                },
            )
            .await
            .unwrap_err();
        assert!(
            format!("{error:#}").contains("A group is made with an NPC"),
            "{error:#}"
        );
        let error = store
            .create_object(world, ObjectKind::Npc, &npc_fields("Toma"))
            .await
            .unwrap_err();
        assert!(
            format!("{error:#}").contains("A npc needs location_id"),
            "{error:#}"
        );
        // A group name is unique, though only its ID identifies the group.
        let orchard = create(
            &store,
            world,
            ObjectKind::Location,
            location_fields("orchard"),
        )
        .await;
        let (family, _) = group_with_npc(&store, world, "family", orchard, "Toma").await;
        let mara = create(&store, world, ObjectKind::Npc, npc_at("Mara", orchard)).await;
        let error = store
            .group_npcs(world, &[mara], "family", orchard, "")
            .await
            .unwrap_err();
        assert!(
            format!("{error:#}").contains(&format!("Group {family} is already called `family`")),
            "{error:#}"
        );

        // An item is created at a location.
        let loaf = create(
            &store,
            world,
            ObjectKind::ItemType,
            Fields {
                name: Some("loaf".into()),
                description: Some(String::new()),
                ..Fields::default()
            },
        )
        .await;
        let error = store
            .create_object(
                world,
                ObjectKind::Item,
                &Fields {
                    name: Some("stale loaf".into()),
                    item_type_id: Some(loaf),
                    description: Some(String::new()),
                    ..Fields::default()
                },
            )
            .await
            .unwrap_err();
        assert!(
            format!("{error:#}").contains("needs location_id"),
            "{error:#}"
        );

        // An update is refused the same way, but needs nothing.
        let error = store
            .update_object(
                world,
                ObjectKind::Location,
                hut,
                &Fields {
                    motive: Some("x".into()),
                    ..Fields::default()
                },
            )
            .await
            .unwrap_err();
        assert!(format!("{error:#}").contains("not `motive`"), "{error:#}");
        store
            .update_object(
                world,
                ObjectKind::Location,
                hut,
                &Fields {
                    description: Some("Smaller.".into()),
                    ..Fields::default()
                },
            )
            .await
            .unwrap();
    }
}
