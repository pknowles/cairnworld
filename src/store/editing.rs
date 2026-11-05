//! Changing the objects of a world being built: replacing fields, and
//! deleting by flag. A delete only flags an object; nothing is removed.
//! Objects are addressed by the IDs the tools return.

use anyhow::{Context, Result};
use sqlx::{Sqlite, Transaction};

use super::{
    Store,
    authoring::{Changed, DeleteOptions, Fields, Holder, ObjectKind, reject},
};

/// One change to the world being built.
pub enum Change<'a> {
    Create {
        kind: ObjectKind,
        fields: &'a Fields,
    },
    Update {
        kind: ObjectKind,
        id: i64,
        fields: &'a Fields,
    },
    Delete {
        kind: ObjectKind,
        id: i64,
        options: &'a DeleteOptions,
    },
}

impl Store {
    pub async fn change_object(&self, world_id: i64, change: Change<'_>) -> Result<Changed> {
        match change {
            Change::Create { kind, fields } => self.create_object(world_id, kind, fields).await,
            Change::Update { kind, id, fields } => {
                self.update_object(world_id, kind, id, fields).await
            }
            Change::Delete { kind, id, options } => {
                self.delete_object(world_id, kind, id, options).await
            }
        }
    }

    /// Replace the given fields of an object; the others keep their value, and
    /// the Questioner's review no longer holds for the object.
    pub async fn update_object(
        &self,
        world_id: i64,
        kind: ObjectKind,
        id: i64,
        fields: &Fields,
    ) -> Result<Changed> {
        fields.check(kind, false)?;
        let mut transaction = self
            .pool
            .begin()
            .await
            .with_context(|| format!("updating an object in world {world_id}"))?;
        Self::ensure_stage(&mut transaction, world_id, "build").await?;
        Self::ensure_exists(&mut transaction, world_id, kind, id).await?;
        if let Some(name) = &fields.name {
            Self::ensure_name_free(&mut transaction, world_id, kind, name, id).await?;
        }
        match kind {
            ObjectKind::Location => {
                sqlx::query!(
                    "UPDATE location SET name = COALESCE(?, name), kind = COALESCE(?, kind), \
                     description = COALESCE(?, description) WHERE id = ?",
                    fields.name,
                    fields.kind,
                    fields.description,
                    id,
                )
                .execute(&mut *transaction)
                .await
                .context("updating a location")?;
            }
            ObjectKind::Connection => {
                if fields.from_id.is_some() || fields.to_id.is_some() {
                    return reject(
                        "A connection's ends cannot change; delete it and create another.".into(),
                    );
                }
                if fields.travel_time.is_some_and(|time| time < 0) {
                    return reject("A connection's travel time cannot be negative.".into());
                }
                sqlx::query!(
                    "UPDATE connection SET travel_time = COALESCE(?, travel_time), \
                     description = COALESCE(?, description) WHERE id = ?",
                    fields.travel_time,
                    fields.description,
                    id,
                )
                .execute(&mut *transaction)
                .await
                .context("updating a connection")?;
            }
            ObjectKind::Group => {
                if let Some(location) = fields.location_id {
                    Self::ensure_exists(&mut transaction, world_id, ObjectKind::Location, location)
                        .await?;
                }
                sqlx::query!(
                    "UPDATE \"group\" SET name = COALESCE(?, name), \
                     location_id = COALESCE(?, location_id), \
                     description = COALESCE(?, description) WHERE id = ?",
                    fields.name,
                    fields.location_id,
                    fields.description,
                    id,
                )
                .execute(&mut *transaction)
                .await
                .context("updating a group")?;
            }
            ObjectKind::Npc => {
                if let Some(group) = fields.group_id {
                    Self::ensure_exists(&mut transaction, world_id, ObjectKind::Group, group)
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
                let sheet = fields
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
                    fields.name,
                    fields.description,
                    fields.background,
                    fields.motive,
                    fields.ambition,
                    sheet,
                    id,
                )
                .execute(&mut *transaction)
                .await
                .context("updating an NPC")?;
            }
            ObjectKind::ItemType => {
                let stats = fields
                    .stats
                    .as_ref()
                    .map(serde_json::to_string)
                    .transpose()
                    .context("encoding item type stats")?;
                sqlx::query!(
                    "UPDATE item_type SET name = COALESCE(?, name), \
                     description = COALESCE(?, description), stats = COALESCE(?, stats) WHERE id = ?",
                    fields.name,
                    fields.description,
                    stats,
                    id,
                )
                .execute(&mut *transaction)
                .await
                .context("updating an item type")?;
            }
            ObjectKind::Item => {
                if let Some(item_type) = fields.item_type_id {
                    Self::ensure_exists(
                        &mut transaction,
                        world_id,
                        ObjectKind::ItemType,
                        item_type,
                    )
                    .await?;
                }
                sqlx::query!(
                    "UPDATE item SET name = COALESCE(?, name), \
                     item_type_id = COALESCE(?, item_type_id), \
                     description = COALESCE(?, description) WHERE id = ?",
                    fields.name,
                    fields.item_type_id,
                    fields.description,
                    id,
                )
                .execute(&mut *transaction)
                .await
                .context("updating an item")?;
                // Whoever held the item, and whoever holds it now, changed with it.
                Self::clear_holder_review(&mut transaction, id).await?;
                let holder = match (fields.location_id, fields.held_by_id) {
                    (None, None) => None,
                    (Some(location), None) => {
                        Self::ensure_exists(
                            &mut transaction,
                            world_id,
                            ObjectKind::Location,
                            location,
                        )
                        .await?;
                        Some(Holder::Location(location))
                    }
                    (None, Some(npc)) => {
                        Self::ensure_exists(&mut transaction, world_id, ObjectKind::Npc, npc)
                            .await?;
                        Some(Holder::Character(npc))
                    }
                    (Some(_), Some(_)) => {
                        return reject(
                            "Give either location_id or held_by_id to say where the item is."
                                .into(),
                        );
                    }
                };
                if let Some(holder) = holder {
                    Self::move_item(&mut transaction, id, holder).await?;
                    Self::clear_holder_review(&mut transaction, id).await?;
                }
            }
        }
        Self::clear_review(&mut transaction, kind, id).await?;
        let counts = Self::counts(&mut transaction, world_id).await?;
        transaction
            .commit()
            .await
            .with_context(|| format!("committing an update in world {world_id}"))?;
        Ok(Changed {
            kind: kind.as_str(),
            id,
            counts,
        })
    }

    /// Flag an object as deleted. What depends on it either blocks the delete or
    /// is dealt with as the options say; nothing is removed.
    pub async fn delete_object(
        &self,
        world_id: i64,
        kind: ObjectKind,
        id: i64,
        options: &DeleteOptions,
    ) -> Result<Changed> {
        let mut transaction = self
            .pool
            .begin()
            .await
            .with_context(|| format!("deleting an object in world {world_id}"))?;
        Self::ensure_stage(&mut transaction, world_id, "build").await?;
        Self::ensure_exists(&mut transaction, world_id, kind, id).await?;
        match kind {
            ObjectKind::Location => {
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
                        "These items lie at location {id}: {}. Move or delete them first.",
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
                if !groups.is_empty() && !options.delete_groups {
                    return reject(format!(
                        "These groups are at location {id}: {}. Move them, or set delete_groups to delete them too.",
                        groups
                            .iter()
                            .map(|group| format!("{} ({})", group.name, group.id))
                            .collect::<Vec<_>>()
                            .join(", ")
                    ));
                }
                for group in &groups {
                    if Self::active_members(&mut transaction, group.id).await? > 0 {
                        return reject(format!(
                            "Group {} ({}) still has characters. Move or delete them first.",
                            group.name, group.id
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
            ObjectKind::Connection | ObjectKind::Item => {
                if kind == ObjectKind::Item {
                    Self::clear_holder_review(&mut transaction, id).await?;
                }
                Self::flag_deleted(&mut transaction, kind, id).await?;
            }
            ObjectKind::Group => {
                return reject(
                    "A group is deleted along with its last character; delete or move them.".into(),
                );
            }
            ObjectKind::Npc => {
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
                    match (options.item_location_id, options.delete_items) {
                        (Some(_), true) => {
                            return reject(
                                "Give item_location_id or delete_items, not both.".into(),
                            );
                        }
                        (None, false) => {
                            return reject(format!(
                                "NPC {id} carries {}. Give item_location_id to leave them somewhere, or set delete_items.",
                                carried
                                    .iter()
                                    .map(|item| format!("{} ({})", item.name, item.id))
                                    .collect::<Vec<_>>()
                                    .join(", ")
                            ));
                        }
                        (Some(location), false) => {
                            Self::ensure_exists(
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
                                if let Some(description) = &options.item_description {
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
            ObjectKind::ItemType => {
                let using = sqlx::query!(
                    "SELECT id, name FROM item WHERE item_type_id = ? AND deleted = 0",
                    id,
                )
                .fetch_all(&mut *transaction)
                .await
                .context("finding the items of an item type")?;
                if !using.is_empty() {
                    return reject(format!(
                        "These items are of item type {id}: {}. Delete them first.",
                        using
                            .iter()
                            .map(|item| format!("{} ({})", item.name, item.id))
                            .collect::<Vec<_>>()
                            .join(", ")
                    ));
                }
                Self::flag_deleted(&mut transaction, ObjectKind::ItemType, id).await?;
            }
        }
        let counts = Self::counts(&mut transaction, world_id).await?;
        transaction
            .commit()
            .await
            .with_context(|| format!("committing a delete in world {world_id}"))?;
        Ok(Changed {
            kind: kind.as_str(),
            id,
            counts,
        })
    }

    /// Refuse a name another object of the kind already has.
    async fn ensure_name_free(
        transaction: &mut Transaction<'_, Sqlite>,
        world_id: i64,
        kind: ObjectKind,
        name: &str,
        own_id: i64,
    ) -> Result<()> {
        match Self::find_by_name(transaction, world_id, kind, name, true).await? {
            Some(other) if other != own_id => reject(format!(
                "The world already has a {} called `{name}`.",
                kind.as_str()
            )),
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
mod id_tests {
    use crate::store::authoring::fixtures::*;
    use crate::store::{DeleteOptions, Fields, ObjectKind};

    #[tokio::test]
    async fn an_update_replaces_only_given_fields_and_unreviews_the_object() {
        let (store, _db, world) = building_world().await;
        let hut = create(&store, world, ObjectKind::Location, location_fields("hut")).await;
        let yard = create(&store, world, ObjectKind::Location, location_fields("yard")).await;
        let family = create(
            &store,
            world,
            ObjectKind::Group,
            group_fields("family", hut),
        )
        .await;
        let toma = create(&store, world, ObjectKind::Npc, npc_fields("Toma", family)).await;
        set_stage(&store, world, "review").await;
        store
            .set_reviewed(world, ObjectKind::Npc, toma, true)
            .await
            .unwrap();
        set_stage(&store, world, "build").await;
        store
            .update_object(
                world,
                ObjectKind::Npc,
                toma,
                &Fields {
                    name: Some("Tomas".into()),
                    motive: Some("Leave the valley.".into()),
                    ..Fields::default()
                },
            )
            .await
            .unwrap();
        let read = store
            .read_object(world, ObjectKind::Npc, toma)
            .await
            .unwrap();
        assert!(
            read.contains("Leave the valley.") && read.contains("Has a past."),
            "{read}"
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
        let error = store
            .update_object(
                world,
                ObjectKind::Location,
                yard,
                &Fields {
                    name: Some("hut".into()),
                    ..Fields::default()
                },
            )
            .await
            .unwrap_err();
        assert!(format!("{error:#}").contains("already has a location called `hut`"));
    }

    #[tokio::test]
    async fn deletion_flags_an_npc_and_removes_it_from_scheduling() {
        let (store, _db, world) = building_world().await;
        let hut = create(&store, world, ObjectKind::Location, location_fields("hut")).await;
        let family = create(
            &store,
            world,
            ObjectKind::Group,
            group_fields("family", hut),
        )
        .await;
        let toma = create(&store, world, ObjectKind::Npc, npc_fields("Toma", family)).await;
        let mara = create(&store, world, ObjectKind::Npc, npc_fields("Mara", family)).await;
        assert_eq!(store.group_npc_ids(family).await.unwrap(), vec![toma, mara]);
        store
            .delete_object(world, ObjectKind::Npc, toma, &DeleteOptions::default())
            .await
            .unwrap();
        assert_eq!(store.group_npc_ids(family).await.unwrap(), vec![mara]);
        assert!(store.npc_scene(mara).await.unwrap().present.is_empty());
        let error = store
            .create_object(world, ObjectKind::Npc, &npc_fields("Toma", family))
            .await
            .unwrap_err();
        assert!(format!("{error:#}").contains("already has a npc called `Toma`"));
    }
}
