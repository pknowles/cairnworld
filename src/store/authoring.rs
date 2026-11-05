//! Creating a world's objects, for scenario installation and for the
//! Storyteller's authoring tools alike: one insert per kind of object, so both
//! build identical records. The Storyteller refers to objects by name, as a
//! scenario does, so authoring keeps the names of one kind distinct.

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
}

// An item to create, held by an NPC or lying at a location.
#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NewItem {
    pub name: String,
    pub item_type: String,
    pub description: String,
    pub location: Option<String>,
    pub held_by: Option<String>,
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

pub(super) fn reject<T>(message: String) -> Result<T> {
    Err(Rejected(message).into())
}

/// An object for the Storyteller to add to the world being built.
pub enum NewObject<'a> {
    Location(&'a Location),
    Connection(&'a Connection),
    Group(&'a Group),
    Npc(&'a Npc),
    ItemType(&'a ItemType),
    Item(&'a NewItem),
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
    ) -> Result<()> {
        let gm_notes = Json(&connection.notes.gm);
        let storyteller_notes = Json(&connection.notes.storyteller);
        sqlx::query!(
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
        .context("creating connection")?;
        Ok(())
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
    ) -> Result<()> {
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
        Ok(())
    }

    pub(super) async fn insert_npc(
        transaction: &mut Transaction<'_, Sqlite>,
        world_id: i64,
        group_id: i64,
        npc: &Npc,
    ) -> Result<()> {
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
        Ok(())
    }

    /// Add one object to a world whose Storyteller is building it, and report
    /// what the world now holds. The references are resolved by name inside
    /// the world; a missing one or a name already taken changes nothing.
    pub async fn create_object(&self, world_id: i64, object: NewObject<'_>) -> Result<Counts> {
        let mut transaction = self
            .pool
            .begin()
            .await
            .with_context(|| format!("creating an object in world {world_id}"))?;
        Self::ensure_stage(&mut transaction, world_id, "build").await?;
        match object {
            NewObject::Location(location) => {
                Self::ensure_unused(
                    &mut transaction,
                    world_id,
                    ObjectKind::Location,
                    &location.name,
                )
                .await?;
                Self::insert_location(&mut transaction, world_id, location).await?;
            }
            NewObject::Connection(connection) => {
                if connection.travel_time < 0 {
                    return reject("A connection's travel time cannot be negative.".into());
                }
                if connection.from == connection.to {
                    return reject("A connection cannot join a location to itself.".into());
                }
                let from = Self::named_id(
                    &mut transaction,
                    world_id,
                    ObjectKind::Location,
                    &connection.from,
                )
                .await?;
                let to = Self::named_id(
                    &mut transaction,
                    world_id,
                    ObjectKind::Location,
                    &connection.to,
                )
                .await?;
                Self::insert_connection(&mut transaction, world_id, from, to, connection).await?;
            }
            NewObject::Group(group) => {
                Self::ensure_unused(&mut transaction, world_id, ObjectKind::Group, &group.name)
                    .await?;
                let location = Self::named_id(
                    &mut transaction,
                    world_id,
                    ObjectKind::Location,
                    &group.location,
                )
                .await?;
                Self::insert_group(&mut transaction, world_id, location, group).await?;
            }
            NewObject::Npc(npc) => {
                Self::ensure_unused(&mut transaction, world_id, ObjectKind::Npc, &npc.name).await?;
                let group =
                    Self::named_id(&mut transaction, world_id, ObjectKind::Group, &npc.group)
                        .await?;
                Self::insert_npc(&mut transaction, world_id, group, npc).await?;
            }
            NewObject::ItemType(item_type) => {
                Self::ensure_unused(
                    &mut transaction,
                    world_id,
                    ObjectKind::ItemType,
                    &item_type.name,
                )
                .await?;
                Self::insert_item_type(&mut transaction, world_id, item_type).await?;
            }
            NewObject::Item(item) => {
                Self::ensure_unused(&mut transaction, world_id, ObjectKind::Item, &item.name)
                    .await?;
                let item_type = Self::named_id(
                    &mut transaction,
                    world_id,
                    ObjectKind::ItemType,
                    &item.item_type,
                )
                .await?;
                let holder = match (&item.location, &item.held_by) {
                    (Some(location), None) => Holder::Location(
                        Self::named_id(&mut transaction, world_id, ObjectKind::Location, location)
                            .await?,
                    ),
                    (None, Some(npc)) => {
                        let npc = Self::named_id(&mut transaction, world_id, ObjectKind::Npc, npc)
                            .await?;
                        // What an NPC carries is part of the NPC.
                        Self::clear_review(&mut transaction, ObjectKind::Npc, npc).await?;
                        Holder::Character(npc)
                    }
                    _ => {
                        return reject("Give either a location or an NPC to hold the item.".into());
                    }
                };
                Self::insert_item(
                    &mut transaction,
                    world_id,
                    item_type,
                    holder,
                    &item.name,
                    &item.description,
                    &Notes::default(),
                )
                .await?;
            }
        }
        let counts = Self::counts(&mut transaction, world_id).await?;
        transaction
            .commit()
            .await
            .with_context(|| format!("committing an object of world {world_id}"))?;
        Ok(counts)
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
            ObjectKind::Connection => {
                let Some((from, to)) = name.split_once(" -> ") else {
                    return Ok(None);
                };
                sqlx::query_scalar!(
                    "SELECT connection.id FROM connection                      JOIN location AS origin ON origin.id = connection.from_location_id                      JOIN location AS target ON target.id = connection.to_location_id                      WHERE connection.world_id = ? AND origin.name = ? AND target.name = ? \
                       AND (connection.deleted = 0 OR ?)",
                    world_id,
                    from,
                    to,
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

    pub(super) async fn named_id(
        transaction: &mut Transaction<'_, Sqlite>,
        world_id: i64,
        kind: ObjectKind,
        name: &str,
    ) -> Result<i64> {
        match Self::find_by_name(transaction, world_id, kind, name, false).await? {
            Some(id) => Ok(id),
            None => reject(format!(
                "There is no {} called `{name}` in the world.",
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
}
