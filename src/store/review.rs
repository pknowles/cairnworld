//! The Questioner's review of the objects the Storyteller built: what it
//! reads, which objects it has marked reviewed, and whether the core objects
//! are whole enough to end the phase.

use anyhow::{Context, Result};
use sqlx::{Sqlite, Transaction};

use super::{Store, authoring::ObjectKind};

/// One top-level object and whether the Questioner has reviewed it.
#[derive(Debug, PartialEq)]
pub struct ReviewEntry {
    pub kind: String,
    pub name: String,
    pub reviewed: bool,
}

impl Store {
    /// Every top-level object of the world with whether the Questioner has
    /// reviewed it. What an object holds (an NPC's items) is part of it.
    pub async fn review_list(&self, world_id: i64) -> Result<Vec<ReviewEntry>> {
        let mut connection = self
            .pool
            .acquire()
            .await
            .context("acquiring a connection")?;
        Self::review_entries(&mut connection, world_id).await
    }

    async fn review_entries(
        connection: &mut sqlx::SqliteConnection,
        world_id: i64,
    ) -> Result<Vec<ReviewEntry>> {
        sqlx::query_as!(
            ReviewEntry,
            "SELECT 'location' AS \"kind!: String\", name AS \"name!: String\", \
                    EXISTS(SELECT 1 FROM object_reviewed WHERE kind = 'location' AND object_id = location.id) AS \"reviewed!: bool\" \
               FROM location WHERE world_id = ?1 AND deleted = 0 \
             UNION ALL \
             SELECT 'connection', origin.name || ' -> ' || target.name, \
                    EXISTS(SELECT 1 FROM object_reviewed WHERE kind = 'connection' AND object_id = connection.id) \
               FROM connection \
               JOIN location AS origin ON origin.id = connection.from_location_id \
               JOIN location AS target ON target.id = connection.to_location_id \
              WHERE connection.world_id = ?1 AND connection.deleted = 0 \
             UNION ALL \
             SELECT 'group', name, \
                    EXISTS(SELECT 1 FROM object_reviewed WHERE kind = 'group' AND object_id = \"group\".id) \
               FROM \"group\" WHERE world_id = ?1 AND deleted = 0 \
             UNION ALL \
             SELECT 'npc', name, \
                    EXISTS(SELECT 1 FROM object_reviewed WHERE kind = 'npc' AND object_id = character.id) \
               FROM character WHERE world_id = ?1 AND role = 'npc' AND deleted = 0 \
             UNION ALL \
             SELECT 'item_type', name, \
                    EXISTS(SELECT 1 FROM object_reviewed WHERE kind = 'item_type' AND object_id = item_type.id) \
               FROM item_type WHERE world_id = ?1 AND deleted = 0 \
             UNION ALL \
             SELECT 'item', name, \
                    EXISTS(SELECT 1 FROM object_reviewed WHERE kind = 'item' AND object_id = item.id) \
               FROM item JOIN item_location ON item_location.item_id = item.id WHERE world_id = ?1 AND item.deleted = 0",
            world_id,
        )
        .fetch_all(connection)
        .await
        .with_context(|| format!("listing the objects of world {world_id}"))
    }

    /// The names of the world's objects, for matching names mentioned in text.
    pub async fn object_names(&self, world_id: i64) -> Result<Vec<String>> {
        sqlx::query_scalar!(
            "SELECT name AS \"name!: String\" FROM location WHERE world_id = ?1 AND deleted = 0 \
             UNION SELECT name FROM \"group\" WHERE world_id = ?1 AND deleted = 0 \
             UNION SELECT name FROM character WHERE world_id = ?1 AND role = 'npc' AND deleted = 0 \
             UNION SELECT name FROM item_type WHERE world_id = ?1 AND deleted = 0 \
             UNION SELECT name FROM item WHERE world_id = ?1 AND deleted = 0",
            world_id,
        )
        .fetch_all(&self.pool)
        .await
        .with_context(|| format!("listing the object names of world {world_id}"))
    }

    /// Record that the Questioner has reviewed an object, or that it has not.
    pub async fn set_reviewed(
        &self,
        world_id: i64,
        kind: ObjectKind,
        name: &str,
        reviewed: bool,
    ) -> Result<()> {
        let mut transaction = self
            .pool
            .begin()
            .await
            .with_context(|| format!("reviewing an object of world {world_id}"))?;
        Self::ensure_stage(&mut transaction, world_id, "review").await?;
        let id = Self::named_id(&mut transaction, world_id, kind, name).await?;
        let kind = kind.as_str();
        if reviewed {
            sqlx::query!(
                "INSERT OR IGNORE INTO object_reviewed (kind, object_id) VALUES (?, ?)",
                kind,
                id,
            )
            .execute(&mut *transaction)
            .await
            .context("recording a review")?;
        } else {
            sqlx::query!(
                "DELETE FROM object_reviewed WHERE kind = ? AND object_id = ?",
                kind,
                id,
            )
            .execute(&mut *transaction)
            .await
            .context("clearing a review")?;
        }
        transaction
            .commit()
            .await
            .with_context(|| format!("committing a review in world {world_id}"))
    }

    /// Everything the Questioner may read about one object, as JSON text.
    pub async fn read_object(&self, world_id: i64, kind: ObjectKind, name: &str) -> Result<String> {
        let mut transaction = self
            .pool
            .begin()
            .await
            .with_context(|| format!("reading an object of world {world_id}"))?;
        let id = Self::named_id(&mut transaction, world_id, kind, name).await?;
        let mut object = match kind {
            ObjectKind::Location => {
                let row = sqlx::query!(
                    "SELECT name, kind, description FROM location WHERE id = ?",
                    id,
                )
                .fetch_one(&mut *transaction)
                .await
                .context("reading a location")?;
                let leads_to: Vec<String> = sqlx::query_scalar!(
                    "SELECT target.name || ' (' || connection.travel_time || ')' AS \"leads_to!: String\" \
                     FROM connection JOIN location AS target ON target.id = connection.to_location_id \
                     WHERE connection.from_location_id = ? AND connection.deleted = 0",
                    id,
                )
                .fetch_all(&mut *transaction)
                .await
                .context("reading a location's connections")?;
                serde_json::json!({
                    "name": row.name, "kind": row.kind, "description": row.description,
                    "leads_to": leads_to,
                })
            }
            ObjectKind::Connection => {
                let row = sqlx::query!(
                    "SELECT origin.name AS origin, target.name AS target, connection.travel_time, connection.description \
                     FROM connection \
                     JOIN location AS origin ON origin.id = connection.from_location_id \
                     JOIN location AS target ON target.id = connection.to_location_id \
                     WHERE connection.id = ?",
                    id,
                )
                .fetch_one(&mut *transaction)
                .await
                .context("reading a connection")?;
                serde_json::json!({
                    "from": row.origin, "to": row.target,
                    "travel_time": row.travel_time, "description": row.description,
                })
            }
            ObjectKind::Group => {
                let row = sqlx::query!(
                    "SELECT \"group\".name, \"group\".description, location.name AS location \
                     FROM \"group\" JOIN location ON location.id = \"group\".location_id \
                     WHERE \"group\".id = ?",
                    id,
                )
                .fetch_one(&mut *transaction)
                .await
                .context("reading a group")?;
                let members: Vec<String> = sqlx::query_scalar!(
                    "SELECT character.name FROM character_group \
                     JOIN character ON character.id = character_group.character_id \
                     WHERE character_group.group_id = ? AND character.deleted = 0",
                    id,
                )
                .fetch_all(&mut *transaction)
                .await
                .context("reading a group's members")?;
                serde_json::json!({
                    "name": row.name, "location": row.location,
                    "description": row.description, "members": members,
                })
            }
            ObjectKind::Npc => {
                let row = sqlx::query!(
                    "SELECT character.name, character.description, character.background, \
                            character.motive, character.ambition, character.sheet, \
                            \"group\".name AS \"group_name?\" \
                     FROM character \
                     LEFT JOIN character_group ON character_group.character_id = character.id \
                     LEFT JOIN \"group\" ON \"group\".id = character_group.group_id \
                     WHERE character.id = ?",
                    id,
                )
                .fetch_one(&mut *transaction)
                .await
                .context("reading an NPC")?;
                let items: Vec<String> = sqlx::query_scalar!(
                    "SELECT item.name FROM item_character \
                     JOIN item ON item.id = item_character.item_id \
                     WHERE item_character.character_id = ? AND item.deleted = 0",
                    id,
                )
                .fetch_all(&mut *transaction)
                .await
                .context("reading an NPC's items")?;
                serde_json::json!({
                    "name": row.name, "group": row.group_name, "description": row.description,
                    "background": row.background, "motive": row.motive,
                    "ambition": row.ambition,
                    "sheet": serde_json::from_str::<serde_json::Value>(&row.sheet)
                        .context("decoding an NPC sheet")?,
                    "items": items,
                })
            }
            ObjectKind::ItemType => {
                let row = sqlx::query!(
                    "SELECT name, description, stats FROM item_type WHERE id = ?",
                    id,
                )
                .fetch_one(&mut *transaction)
                .await
                .context("reading an item type")?;
                serde_json::json!({
                    "name": row.name, "description": row.description,
                    "stats": serde_json::from_str::<serde_json::Value>(&row.stats)
                        .context("decoding item type stats")?,
                })
            }
            ObjectKind::Item => {
                let row = sqlx::query!(
                    "SELECT item.name, item.description, item_type.name AS item_type, \
                            location.name AS \"location?\", holder.name AS \"held_by?\" \
                     FROM item JOIN item_type ON item_type.id = item.item_type_id \
                     LEFT JOIN item_location ON item_location.item_id = item.id \
                     LEFT JOIN location ON location.id = item_location.location_id \
                     LEFT JOIN item_character ON item_character.item_id = item.id \
                     LEFT JOIN character AS holder ON holder.id = item_character.character_id \
                     WHERE item.id = ?",
                    id,
                )
                .fetch_one(&mut *transaction)
                .await
                .context("reading an item")?;
                serde_json::json!({
                    "name": row.name, "item_type": row.item_type,
                    "location": row.location, "held_by": row.held_by,
                    "description": row.description,
                })
            }
        };
        crate::prompts::drop_blanks(&mut object);
        serde_json::to_string_pretty(&object).context("encoding an object")
    }

    /// Why the core objects cannot yet end the world's creation: empty when
    /// they can. Used to refuse the Questioner's completion and again where it
    /// takes effect.
    pub async fn core_object_problems(&self, world_id: i64) -> Result<Vec<String>> {
        let mut connection = self
            .pool
            .acquire()
            .await
            .context("acquiring a connection")?;
        Self::core_problems(&mut connection, world_id).await
    }

    pub(super) async fn core_problems(
        connection: &mut sqlx::SqliteConnection,
        world_id: i64,
    ) -> Result<Vec<String>> {
        let mut problems = Vec::new();
        let locations = sqlx::query!(
            "SELECT id, name FROM location WHERE world_id = ? AND deleted = 0",
            world_id
        )
        .fetch_all(&mut *connection)
        .await
        .context("loading locations")?;
        if locations.is_empty() {
            problems.push("The world has no locations.".to_string());
        }
        let links = sqlx::query!(
            "SELECT from_location_id, to_location_id FROM connection WHERE world_id = ? AND deleted = 0",
            world_id,
        )
        .fetch_all(&mut *connection)
        .await
        .context("loading connections")?;
        if let Some(first) = locations.first() {
            let mut reached = std::collections::HashSet::from([first.id]);
            let mut grew = true;
            while grew {
                grew = false;
                for link in &links {
                    let (a, b) = (link.from_location_id, link.to_location_id);
                    if reached.contains(&a) != reached.contains(&b) {
                        reached.insert(a);
                        reached.insert(b);
                        grew = true;
                    }
                }
            }
            let apart: Vec<&str> = locations
                .iter()
                .filter(|location| !reached.contains(&location.id))
                .map(|location| location.name.as_str())
                .collect();
            if !apart.is_empty() {
                problems.push(format!(
                    "These locations cannot be reached from {}: {}.",
                    first.name,
                    apart.join(", ")
                ));
            }
        }
        let groups = sqlx::query_scalar!(
            "SELECT COUNT(*) FROM \"group\" WHERE world_id = ? AND deleted = 0",
            world_id
        )
        .fetch_one(&mut *connection)
        .await
        .context("counting groups")?;
        if groups == 0 {
            problems.push("The world has no groups.".to_string());
        }
        let incomplete = sqlx::query_scalar!(
            "SELECT name FROM character WHERE world_id = ? AND role = 'npc' AND deleted = 0 \
             AND (trim(background) = '' OR trim(motive) = '')",
            world_id,
        )
        .fetch_all(&mut *connection)
        .await
        .context("checking NPC fields")?;
        for name in incomplete {
            problems.push(format!("NPC {name} needs a background and a motive."));
        }
        let unreviewed: Vec<String> = Self::review_entries(connection, world_id)
            .await?
            .into_iter()
            .filter(|entry| !entry.reviewed)
            .map(|entry| format!("{} {}", entry.kind, entry.name))
            .collect();
        if !unreviewed.is_empty() {
            problems.push(format!("Not yet reviewed: {}.", unreviewed.join("; ")));
        }
        Ok(problems)
    }

    /// Put the Adventurers that joined while the world was being created into
    /// its first group.
    pub(super) async fn place_waiting_adventurers(
        transaction: &mut Transaction<'_, Sqlite>,
        world_id: i64,
    ) -> Result<()> {
        let group_id = sqlx::query_scalar!(
            "SELECT id FROM \"group\" WHERE world_id = ? ORDER BY id LIMIT 1",
            world_id,
        )
        .fetch_one(&mut **transaction)
        .await
        .with_context(|| format!("finding the first group of world {world_id}"))?;
        let waiting = sqlx::query_scalar!(
            "SELECT character.id FROM character              LEFT JOIN character_group ON character_group.character_id = character.id              WHERE character.world_id = ? AND character.role = 'pc'                AND character_group.character_id IS NULL",
            world_id,
        )
        .fetch_all(&mut **transaction)
        .await
        .with_context(|| format!("finding the waiting Adventurers of world {world_id}"))?;
        for character_id in waiting {
            Self::place_adventurer(transaction, character_id, group_id).await?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scenario::{Group, Notes, Npc};
    use crate::store::NewObject;
    use crate::store::authoring::fixtures::*;

    #[tokio::test]
    async fn core_objects_are_not_whole_until_connected_complete_and_reviewed() {
        let (store, _db, world) = building_world().await;
        for name in ["hut", "yard"] {
            store
                .create_object(world, NewObject::Location(&location(name)))
                .await
                .unwrap();
        }
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
            background: String::new(),
            motive: String::new(),
            ambition: String::new(),
            sheet: serde_json::json!({}),
            notes: Notes::default(),
        };
        store
            .create_object(world, NewObject::Npc(&toma))
            .await
            .unwrap();

        let problems = store.core_object_problems(world).await.unwrap().join(" ");
        assert!(problems.contains("cannot be reached"), "{problems}");
        assert!(
            problems.contains("NPC Toma needs a background and a motive"),
            "{problems}"
        );
        assert!(problems.contains("Not yet reviewed"), "{problems}");

        // Reviewing needs the review stage and clears the review problem only.
        sqlx::query!(
            "UPDATE world SET generation_stage = 'review' WHERE id = ?",
            world
        )
        .execute(&store.pool)
        .await
        .unwrap();
        for entry in store.review_list(world).await.unwrap() {
            let kind = match entry.kind.as_str() {
                "location" => ObjectKind::Location,
                "group" => ObjectKind::Group,
                "npc" => ObjectKind::Npc,
                other => panic!("unexpected {other}"),
            };
            store
                .set_reviewed(world, kind, &entry.name, true)
                .await
                .unwrap();
        }
        let problems = store.core_object_problems(world).await.unwrap().join(" ");
        assert!(!problems.contains("Not yet reviewed"), "{problems}");
        assert!(problems.contains("cannot be reached"), "{problems}");
    }
}
