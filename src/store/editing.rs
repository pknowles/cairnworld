//! Changing the objects of a world being built: replacing fields, and
//! deleting by flag. A delete only flags an object; nothing is removed.

use anyhow::{Context, Result};
use schemars::JsonSchema;
use serde::Deserialize;
use sqlx::{Sqlite, Transaction};

use super::{
    Store,
    authoring::{Counts, Holder, NewObject, ObjectKind, reject},
};

/// One change to the world being built.
pub enum Change<'a> {
    Create(NewObject<'a>),
    Update(Update<'a>),
    Delete(Delete<'a>),
}

// The fields of an existing object to replace; a field left out keeps its
// value. `name` finds the object, `new_name` renames it.
#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LocationUpdate {
    pub name: String,
    pub new_name: Option<String>,
    pub kind: Option<String>,
    pub description: Option<String>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ConnectionUpdate {
    pub from: String,
    pub to: String,
    pub travel_time: Option<i64>,
    pub description: Option<String>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GroupUpdate {
    pub name: String,
    pub new_name: Option<String>,
    pub location: Option<String>,
    pub description: Option<String>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NpcUpdate {
    pub name: String,
    pub new_name: Option<String>,
    pub group: Option<String>,
    pub description: Option<String>,
    pub background: Option<String>,
    pub motive: Option<String>,
    pub ambition: Option<String>,
    #[schemars(with = "Option<std::collections::BTreeMap<String, serde_json::Value>>")]
    pub sheet: Option<serde_json::Value>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ItemTypeUpdate {
    pub name: String,
    pub new_name: Option<String>,
    pub description: Option<String>,
    #[schemars(with = "Option<std::collections::BTreeMap<String, serde_json::Value>>")]
    pub stats: Option<serde_json::Value>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ItemUpdate {
    pub name: String,
    pub new_name: Option<String>,
    pub item_type: Option<String>,
    pub location: Option<String>,
    pub held_by: Option<String>,
    pub description: Option<String>,
}

pub enum Update<'a> {
    Location(&'a LocationUpdate),
    Connection(&'a ConnectionUpdate),
    Group(&'a GroupUpdate),
    Npc(&'a NpcUpdate),
    ItemType(&'a ItemTypeUpdate),
    Item(&'a ItemUpdate),
}

// What to delete and, where something depends on it, what to do about that.
#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LocationDelete {
    pub name: String,
    #[serde(default)]
    pub delete_groups: bool,
}

#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ConnectionDelete {
    pub from: String,
    pub to: String,
}

#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NpcDelete {
    pub name: String,
    pub item_location: Option<String>,
    pub item_description: Option<String>,
    #[serde(default)]
    pub delete_items: bool,
}

#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NameOnly {
    pub name: String,
}

pub enum Delete<'a> {
    Location(&'a LocationDelete),
    Connection(&'a ConnectionDelete),
    Npc(&'a NpcDelete),
    ItemType(&'a NameOnly),
    Item(&'a NameOnly),
}

impl Store {
    pub async fn change_object(&self, world_id: i64, change: Change<'_>) -> Result<Counts> {
        match change {
            Change::Create(object) => self.create_object(world_id, object).await,
            Change::Update(update) => self.update_object(world_id, update).await,
            Change::Delete(delete) => self.delete_object(world_id, delete).await,
        }
    }

    /// Replace some fields of an existing object. Fields left out keep their
    /// value, and the Questioner's review no longer holds for the object.
    pub async fn update_object(&self, world_id: i64, update: Update<'_>) -> Result<Counts> {
        let mut transaction = self
            .pool
            .begin()
            .await
            .with_context(|| format!("updating an object in world {world_id}"))?;
        Self::ensure_stage(&mut transaction, world_id, "build").await?;
        match update {
            Update::Location(update) => {
                let id = Self::named_id(
                    &mut transaction,
                    world_id,
                    ObjectKind::Location,
                    &update.name,
                )
                .await?;
                Self::ensure_renamed(
                    &mut transaction,
                    world_id,
                    ObjectKind::Location,
                    &update.name,
                    &update.new_name,
                )
                .await?;
                sqlx::query!(
                    "UPDATE location SET name = COALESCE(?, name), kind = COALESCE(?, kind), \
                     description = COALESCE(?, description) WHERE id = ?",
                    update.new_name,
                    update.kind,
                    update.description,
                    id,
                )
                .execute(&mut *transaction)
                .await
                .context("updating a location")?;
                Self::clear_review(&mut transaction, ObjectKind::Location, id).await?;
            }
            Update::Connection(update) => {
                let name = format!("{} -> {}", update.from, update.to);
                let id = Self::named_id(&mut transaction, world_id, ObjectKind::Connection, &name)
                    .await?;
                if update.travel_time.is_some_and(|time| time < 0) {
                    return reject("A connection's travel time cannot be negative.".into());
                }
                sqlx::query!(
                    "UPDATE connection SET travel_time = COALESCE(?, travel_time), \
                     description = COALESCE(?, description) WHERE id = ?",
                    update.travel_time,
                    update.description,
                    id,
                )
                .execute(&mut *transaction)
                .await
                .context("updating a connection")?;
                Self::clear_review(&mut transaction, ObjectKind::Connection, id).await?;
            }
            Update::Group(update) => {
                let id =
                    Self::named_id(&mut transaction, world_id, ObjectKind::Group, &update.name)
                        .await?;
                Self::ensure_renamed(
                    &mut transaction,
                    world_id,
                    ObjectKind::Group,
                    &update.name,
                    &update.new_name,
                )
                .await?;
                let location = match &update.location {
                    Some(name) => Some(
                        Self::named_id(&mut transaction, world_id, ObjectKind::Location, name)
                            .await?,
                    ),
                    None => None,
                };
                sqlx::query!(
                    "UPDATE \"group\" SET name = COALESCE(?, name), \
                     location_id = COALESCE(?, location_id), \
                     description = COALESCE(?, description) WHERE id = ?",
                    update.new_name,
                    location,
                    update.description,
                    id,
                )
                .execute(&mut *transaction)
                .await
                .context("updating a group")?;
                Self::clear_review(&mut transaction, ObjectKind::Group, id).await?;
            }
            Update::Npc(update) => {
                let id = Self::named_id(&mut transaction, world_id, ObjectKind::Npc, &update.name)
                    .await?;
                Self::ensure_renamed(
                    &mut transaction,
                    world_id,
                    ObjectKind::Npc,
                    &update.name,
                    &update.new_name,
                )
                .await?;
                let sheet = update
                    .sheet
                    .as_ref()
                    .map(serde_json::to_string)
                    .transpose()
                    .context("encoding an NPC sheet")?;
                sqlx::query!(
                    "UPDATE character SET name = COALESCE(?, name), \
                     description = COALESCE(?, description), background = COALESCE(?, background), \
                     motive = COALESCE(?, motive), ambition = COALESCE(?, ambition), \
                     sheet = COALESCE(?, sheet) WHERE id = ?",
                    update.new_name,
                    update.description,
                    update.background,
                    update.motive,
                    update.ambition,
                    sheet,
                    id,
                )
                .execute(&mut *transaction)
                .await
                .context("updating an NPC")?;
                if let Some(group) = &update.group {
                    let group =
                        Self::named_id(&mut transaction, world_id, ObjectKind::Group, group)
                            .await?;
                    sqlx::query!(
                        "UPDATE character_group SET group_id = ? WHERE character_id = ?",
                        group,
                        id,
                    )
                    .execute(&mut *transaction)
                    .await
                    .context("moving an NPC to another group")?;
                }
                Self::clear_review(&mut transaction, ObjectKind::Npc, id).await?;
            }
            Update::ItemType(update) => {
                let id = Self::named_id(
                    &mut transaction,
                    world_id,
                    ObjectKind::ItemType,
                    &update.name,
                )
                .await?;
                Self::ensure_renamed(
                    &mut transaction,
                    world_id,
                    ObjectKind::ItemType,
                    &update.name,
                    &update.new_name,
                )
                .await?;
                let stats = update
                    .stats
                    .as_ref()
                    .map(serde_json::to_string)
                    .transpose()
                    .context("encoding item type stats")?;
                sqlx::query!(
                    "UPDATE item_type SET name = COALESCE(?, name), \
                     description = COALESCE(?, description), stats = COALESCE(?, stats) WHERE id = ?",
                    update.new_name,
                    update.description,
                    stats,
                    id,
                )
                .execute(&mut *transaction)
                .await
                .context("updating an item type")?;
                Self::clear_review(&mut transaction, ObjectKind::ItemType, id).await?;
            }
            Update::Item(update) => {
                let id = Self::named_id(&mut transaction, world_id, ObjectKind::Item, &update.name)
                    .await?;
                Self::ensure_renamed(
                    &mut transaction,
                    world_id,
                    ObjectKind::Item,
                    &update.name,
                    &update.new_name,
                )
                .await?;
                let item_type = match &update.item_type {
                    Some(name) => Some(
                        Self::named_id(&mut transaction, world_id, ObjectKind::ItemType, name)
                            .await?,
                    ),
                    None => None,
                };
                sqlx::query!(
                    "UPDATE item SET name = COALESCE(?, name), item_type_id = COALESCE(?, item_type_id), \
                     description = COALESCE(?, description) WHERE id = ?",
                    update.new_name,
                    item_type,
                    update.description,
                    id,
                )
                .execute(&mut *transaction)
                .await
                .context("updating an item")?;
                // Whoever held the item, and whoever holds it now, changed with it.
                Self::clear_holder_review(&mut transaction, id).await?;
                Self::clear_review(&mut transaction, ObjectKind::Item, id).await?;
                match (&update.location, &update.held_by) {
                    (None, None) => {}
                    (Some(_), Some(_)) => {
                        return reject("Give either a location or an NPC to hold the item.".into());
                    }
                    (location, held_by) => {
                        let holder = match (location, held_by) {
                            (Some(location), _) => Holder::Location(
                                Self::named_id(
                                    &mut transaction,
                                    world_id,
                                    ObjectKind::Location,
                                    location,
                                )
                                .await?,
                            ),
                            (_, Some(npc)) => Holder::Character(
                                Self::named_id(&mut transaction, world_id, ObjectKind::Npc, npc)
                                    .await?,
                            ),
                            (None, None) => unreachable!("handled above"),
                        };
                        Self::move_item(&mut transaction, id, holder).await?;
                        Self::clear_holder_review(&mut transaction, id).await?;
                    }
                }
            }
        }
        let counts = Self::counts(&mut transaction, world_id).await?;
        transaction
            .commit()
            .await
            .with_context(|| format!("committing an update in world {world_id}"))?;
        Ok(counts)
    }

    /// Flag an object as deleted. What depends on it either blocks the delete or
    /// is dealt with as the call says; nothing is removed.
    pub async fn delete_object(&self, world_id: i64, delete: Delete<'_>) -> Result<Counts> {
        let mut transaction = self
            .pool
            .begin()
            .await
            .with_context(|| format!("deleting an object in world {world_id}"))?;
        Self::ensure_stage(&mut transaction, world_id, "build").await?;
        match delete {
            Delete::Location(delete) => {
                let id = Self::named_id(
                    &mut transaction,
                    world_id,
                    ObjectKind::Location,
                    &delete.name,
                )
                .await?;
                let lying_here = sqlx::query_scalar!(
                    "SELECT item.name FROM item_location JOIN item ON item.id = item_location.item_id \
                     WHERE item_location.location_id = ? AND item.deleted = 0",
                    id,
                )
                .fetch_all(&mut *transaction)
                .await
                .context("finding the items at a location")?;
                if !lying_here.is_empty() {
                    return reject(format!(
                        "These items lie at {}: {}. Move or delete them first.",
                        delete.name,
                        lying_here.join(", ")
                    ));
                }
                let groups = sqlx::query!(
                    "SELECT id, name FROM \"group\" WHERE location_id = ? AND deleted = 0",
                    id,
                )
                .fetch_all(&mut *transaction)
                .await
                .context("finding the groups at a location")?;
                if !groups.is_empty() && !delete.delete_groups {
                    return reject(format!(
                        "These groups are at {}: {}. Move them, or set delete_groups to delete them too.",
                        delete.name,
                        groups
                            .iter()
                            .map(|group| group.name.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    ));
                }
                for group in &groups {
                    if Self::active_members(&mut transaction, group.id).await? > 0 {
                        return reject(format!(
                            "Group {} still has characters. Move or delete them first.",
                            group.name
                        ));
                    }
                    Self::flag_deleted(&mut transaction, ObjectKind::Group, group.id).await?;
                }
                let connections = sqlx::query_scalar!(
                    "SELECT id FROM connection \
                     WHERE (from_location_id = ?1 OR to_location_id = ?1) AND deleted = 0",
                    id,
                )
                .fetch_all(&mut *transaction)
                .await
                .context("finding the connections of a location")?;
                for connection in connections {
                    Self::flag_deleted(&mut transaction, ObjectKind::Connection, connection)
                        .await?;
                }
                Self::flag_deleted(&mut transaction, ObjectKind::Location, id).await?;
            }
            Delete::Connection(delete) => {
                let name = format!("{} -> {}", delete.from, delete.to);
                let id = Self::named_id(&mut transaction, world_id, ObjectKind::Connection, &name)
                    .await?;
                Self::flag_deleted(&mut transaction, ObjectKind::Connection, id).await?;
            }
            Delete::Npc(delete) => {
                let id = Self::named_id(&mut transaction, world_id, ObjectKind::Npc, &delete.name)
                    .await?;
                let carried = sqlx::query!(
                    "SELECT item.id, item.name FROM item_character \
                     JOIN item ON item.id = item_character.item_id \
                     WHERE item_character.character_id = ? AND item.deleted = 0",
                    id,
                )
                .fetch_all(&mut *transaction)
                .await
                .context("finding what an NPC carries")?;
                if !carried.is_empty() {
                    match (&delete.item_location, delete.delete_items) {
                        (Some(_), true) => {
                            return reject("Give item_location or delete_items, not both.".into());
                        }
                        (None, false) => {
                            return reject(format!(
                                "{} carries {}. Give item_location to leave them somewhere, or set delete_items.",
                                delete.name,
                                carried
                                    .iter()
                                    .map(|item| item.name.as_str())
                                    .collect::<Vec<_>>()
                                    .join(", ")
                            ));
                        }
                        (Some(location), false) => {
                            let location = Self::named_id(
                                &mut transaction,
                                world_id,
                                ObjectKind::Location,
                                location,
                            )
                            .await?;
                            for item in &carried {
                                Self::move_item(
                                    &mut transaction,
                                    item.id,
                                    Holder::Location(location),
                                )
                                .await?;
                                if let Some(description) = &delete.item_description {
                                    sqlx::query!(
                                        "UPDATE item SET description = ? WHERE id = ?",
                                        description,
                                        item.id,
                                    )
                                    .execute(&mut *transaction)
                                    .await
                                    .context("describing a dropped item")?;
                                }
                            }
                        }
                        (None, true) => {
                            for item in &carried {
                                Self::flag_deleted(&mut transaction, ObjectKind::Item, item.id)
                                    .await?;
                            }
                        }
                    }
                }
                let group = sqlx::query_scalar!(
                    "SELECT group_id FROM character_group WHERE character_id = ?",
                    id,
                )
                .fetch_optional(&mut *transaction)
                .await
                .context("finding an NPC's group")?;
                Self::flag_deleted(&mut transaction, ObjectKind::Npc, id).await?;
                // A group goes with its last character.
                if let Some(group) = group
                    && Self::active_members(&mut transaction, group).await? == 0
                {
                    Self::flag_deleted(&mut transaction, ObjectKind::Group, group).await?;
                }
            }
            Delete::ItemType(delete) => {
                let id = Self::named_id(
                    &mut transaction,
                    world_id,
                    ObjectKind::ItemType,
                    &delete.name,
                )
                .await?;
                let using = sqlx::query_scalar!(
                    "SELECT name FROM item WHERE item_type_id = ? AND deleted = 0",
                    id,
                )
                .fetch_all(&mut *transaction)
                .await
                .context("finding the items of an item type")?;
                if !using.is_empty() {
                    return reject(format!(
                        "These items are of type {}: {}. Delete them first.",
                        delete.name,
                        using.join(", ")
                    ));
                }
                Self::flag_deleted(&mut transaction, ObjectKind::ItemType, id).await?;
            }
            Delete::Item(delete) => {
                let id = Self::named_id(&mut transaction, world_id, ObjectKind::Item, &delete.name)
                    .await?;
                Self::clear_holder_review(&mut transaction, id).await?;
                Self::flag_deleted(&mut transaction, ObjectKind::Item, id).await?;
            }
        }
        let counts = Self::counts(&mut transaction, world_id).await?;
        transaction
            .commit()
            .await
            .with_context(|| format!("committing a delete in world {world_id}"))?;
        Ok(counts)
    }

    /// Reject a new name another object of the kind already has.
    async fn ensure_renamed(
        transaction: &mut Transaction<'_, Sqlite>,
        world_id: i64,
        kind: ObjectKind,
        name: &str,
        new_name: &Option<String>,
    ) -> Result<()> {
        match new_name {
            Some(new_name) if new_name != name => {
                Self::ensure_unused(transaction, world_id, kind, new_name).await
            }
            _ => Ok(()),
        }
    }

    async fn flag_deleted(
        transaction: &mut Transaction<'_, Sqlite>,
        kind: ObjectKind,
        id: i64,
    ) -> Result<()> {
        match kind {
            ObjectKind::Location => {
                sqlx::query!("UPDATE location SET deleted = 1 WHERE id = ?", id)
                    .execute(&mut **transaction)
                    .await
            }
            ObjectKind::Connection => {
                sqlx::query!("UPDATE connection SET deleted = 1 WHERE id = ?", id)
                    .execute(&mut **transaction)
                    .await
            }
            ObjectKind::Group => {
                sqlx::query!("UPDATE \"group\" SET deleted = 1 WHERE id = ?", id)
                    .execute(&mut **transaction)
                    .await
            }
            ObjectKind::Npc => {
                sqlx::query!("UPDATE character SET deleted = 1 WHERE id = ?", id)
                    .execute(&mut **transaction)
                    .await
            }
            ObjectKind::ItemType => {
                sqlx::query!("UPDATE item_type SET deleted = 1 WHERE id = ?", id)
                    .execute(&mut **transaction)
                    .await
            }
            ObjectKind::Item => {
                sqlx::query!("UPDATE item SET deleted = 1 WHERE id = ?", id)
                    .execute(&mut **transaction)
                    .await
            }
        }
        .with_context(|| format!("deleting {} {id}", kind.as_str()))?;
        Self::clear_review(transaction, kind, id).await
    }

    /// How many characters that are not deleted are in the group.
    async fn active_members(
        transaction: &mut Transaction<'_, Sqlite>,
        group_id: i64,
    ) -> Result<i64> {
        sqlx::query_scalar!(
            "SELECT COUNT(*) FROM character_group \
             JOIN character ON character.id = character_group.character_id \
             WHERE character_group.group_id = ? AND character.deleted = 0",
            group_id,
        )
        .fetch_one(&mut **transaction)
        .await
        .with_context(|| format!("counting the members of group {group_id}"))
    }

    /// The NPC holding an item changed with it, so its review no longer holds.
    async fn clear_holder_review(
        transaction: &mut Transaction<'_, Sqlite>,
        item_id: i64,
    ) -> Result<()> {
        if let Some(npc) = sqlx::query_scalar!(
            "SELECT character_id FROM item_character WHERE item_id = ?",
            item_id,
        )
        .fetch_optional(&mut **transaction)
        .await
        .context("finding the holder of an item")?
        {
            Self::clear_review(transaction, ObjectKind::Npc, npc).await?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scenario::{Connection, ItemType, Notes};
    use crate::store::authoring::fixtures::*;
    use crate::store::{NewItem, NewObject};

    #[tokio::test]
    async fn an_update_replaces_only_the_named_fields_and_unreviews_the_object() {
        let (store, _db, world) = building_world().await;
        for object in [location("hut"), location("yard")] {
            store
                .create_object(world, NewObject::Location(&object))
                .await
                .unwrap();
        }
        store
            .create_object(world, NewObject::Group(&group_at("family", "hut")))
            .await
            .unwrap();
        store
            .create_object(world, NewObject::Npc(&npc_in("Toma", "family")))
            .await
            .unwrap();
        set_stage(&store, world, "review").await;
        store
            .set_reviewed(world, ObjectKind::Npc, "Toma", true)
            .await
            .unwrap();
        set_stage(&store, world, "build").await;

        let update = NpcUpdate {
            name: "Toma".into(),
            new_name: Some("Tomas".into()),
            group: None,
            description: None,
            background: None,
            motive: Some("Leave the valley.".into()),
            ambition: None,
            sheet: None,
        };
        store
            .update_object(world, Update::Npc(&update))
            .await
            .unwrap();

        let read = store
            .read_object(world, ObjectKind::Npc, "Tomas")
            .await
            .unwrap();
        assert!(
            read.contains("Leave the valley.") && read.contains("Has a past."),
            "{read}"
        );
        let listed = store.review_list(world).await.unwrap();
        assert!(
            !listed
                .iter()
                .find(|entry| entry.name == "Tomas")
                .unwrap()
                .reviewed
        );

        // A name another object of the kind has, or a reference to nothing, changes nothing.
        let taken = LocationUpdate {
            name: "yard".into(),
            new_name: Some("hut".into()),
            kind: None,
            description: None,
        };
        let error = store
            .update_object(world, Update::Location(&taken))
            .await
            .unwrap_err();
        assert!(format!("{error:#}").contains("already has a location called `hut`"));
        let nowhere = ItemUpdate {
            name: "none".into(),
            new_name: None,
            item_type: None,
            location: None,
            held_by: None,
            description: None,
        };
        let error = store
            .update_object(world, Update::Item(&nowhere))
            .await
            .unwrap_err();
        assert!(format!("{error:#}").contains("no item called `none`"));
    }

    #[tokio::test]
    async fn a_delete_only_flags_and_what_depends_on_the_object_decides() {
        let (store, _db, world) = building_world().await;
        for object in [location("hut"), location("yard")] {
            store
                .create_object(world, NewObject::Location(&object))
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
        store
            .create_object(world, NewObject::Group(&group_at("family", "hut")))
            .await
            .unwrap();
        store
            .create_object(world, NewObject::Npc(&npc_in("Toma", "family")))
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
        let axe = NewItem {
            name: "wood axe".into(),
            item_type: "axe".into(),
            description: String::new(),
            location: None,
            held_by: Some("Toma".into()),
        };
        store
            .create_object(world, NewObject::Item(&axe))
            .await
            .unwrap();
        let refused = |error: anyhow::Error| format!("{error:#}");

        // Each of these depends on something that is still there.
        let by_name = NameOnly { name: "axe".into() };
        assert!(
            refused(
                store
                    .delete_object(world, Delete::ItemType(&by_name))
                    .await
                    .unwrap_err()
            )
            .contains("wood axe")
        );
        let hut = LocationDelete {
            name: "hut".into(),
            delete_groups: true,
        };
        assert!(
            refused(
                store
                    .delete_object(world, Delete::Location(&hut))
                    .await
                    .unwrap_err()
            )
            .contains("still has characters")
        );
        let toma = NpcDelete {
            name: "Toma".into(),
            item_location: None,
            item_description: None,
            delete_items: false,
        };
        assert!(
            refused(
                store
                    .delete_object(world, Delete::Npc(&toma))
                    .await
                    .unwrap_err()
            )
            .contains("carries wood axe")
        );

        // The NPC leaves its axe in the yard; its group goes with its last character.
        let toma = NpcDelete {
            name: "Toma".into(),
            item_location: Some("yard".into()),
            item_description: Some("Dropped when Toma left.".into()),
            delete_items: false,
        };
        let counts = store
            .delete_object(world, Delete::Npc(&toma))
            .await
            .unwrap();
        assert_eq!((counts.npcs, counts.groups, counts.items), (0, 0, 1));
        let axe = store
            .read_object(world, ObjectKind::Item, "wood axe")
            .await
            .unwrap();
        assert!(
            axe.contains("yard") && axe.contains("Dropped when Toma left."),
            "{axe}"
        );

        // Nothing is removed, so the names stay taken.
        let rows = sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM character WHERE name = 'Toma' AND deleted = 1",
        )
        .fetch_one(&store.pool)
        .await
        .unwrap();
        assert_eq!(rows, 1);
        let again = store
            .create_object(world, NewObject::Npc(&npc_in("Toma", "family")))
            .await
            .unwrap_err();
        assert!(refused(again).contains("already has a npc called `Toma`"));

        // A location takes its routes with it once nothing is there.
        store
            .delete_object(
                world,
                Delete::Item(&NameOnly {
                    name: "wood axe".into(),
                }),
            )
            .await
            .unwrap();
        let counts = store
            .delete_object(world, Delete::Location(&hut))
            .await
            .unwrap();
        assert_eq!((counts.locations, counts.connections), (1, 0));
    }

    #[tokio::test]
    async fn a_deleted_npc_is_gone_from_what_the_others_see_and_from_scheduling() {
        let (store, _db, world) = building_world().await;
        store
            .create_object(world, NewObject::Location(&location("hut")))
            .await
            .unwrap();
        store
            .create_object(world, NewObject::Group(&group_at("family", "hut")))
            .await
            .unwrap();
        for name in ["Toma", "Mara"] {
            store
                .create_object(world, NewObject::Npc(&npc_in(name, "family")))
                .await
                .unwrap();
        }
        let group: i64 = sqlx::query_scalar("SELECT id FROM \"group\" WHERE name = 'family'")
            .fetch_one(&store.pool)
            .await
            .unwrap();
        let id = |name: &'static str| {
            let pool = store.pool.clone();
            async move {
                sqlx::query_scalar::<_, i64>("SELECT id FROM character WHERE name = ?")
                    .bind(name)
                    .fetch_one(&pool)
                    .await
                    .unwrap()
            }
        };
        assert_eq!(store.group_npc_ids(group).await.unwrap().len(), 2);

        let toma = NpcDelete {
            name: "Toma".into(),
            item_location: None,
            item_description: None,
            delete_items: false,
        };
        store
            .delete_object(world, Delete::Npc(&toma))
            .await
            .unwrap();

        assert_eq!(
            store.group_npc_ids(group).await.unwrap(),
            vec![id("Mara").await]
        );
        let scene = store.npc_scene(id("Mara").await).await.unwrap();
        assert!(scene.present.is_empty());
    }
}
