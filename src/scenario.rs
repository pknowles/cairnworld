use std::{fs, path::Path as FilePath};

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Notes {
    #[serde(default = "empty_object")]
    pub gm: Value,
    #[serde(default = "empty_object")]
    pub storyteller: Value,
}

fn empty_object() -> Value {
    Value::Object(Default::default())
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Scenario {
    pub name: String,
    #[serde(default)]
    pub initial_prompt: String,
    #[serde(default)]
    pub storyteller_summary: String,
    #[serde(default)]
    pub notes: Notes,
    pub starting_location: String,
    pub locations: Vec<Location>,
    #[serde(default)]
    pub paths: Vec<Path>,
    #[serde(default)]
    pub npcs: Vec<Npc>,
    #[serde(default)]
    pub item_types: Vec<ItemType>,
    #[serde(default)]
    pub spell_types: Vec<ItemType>,
    #[serde(default)]
    pub items: Vec<Item>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Location {
    pub name: String,
    pub kind: String,
    pub description: String,
    #[serde(default)]
    pub notes: Notes,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Path {
    pub from: String,
    pub to: String,
    pub travel_time: i64,
    pub description: String,
    #[serde(default)]
    pub notes: Notes,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Npc {
    pub name: String,
    pub location: String,
    pub description: String,
    #[serde(default)]
    pub background: String,
    #[serde(default)]
    pub motive: String,
    #[serde(default)]
    pub ambition: String,
    #[serde(default = "empty_object")]
    pub sheet: Value,
    #[serde(default)]
    pub notes: Notes,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ItemType {
    pub name: String,
    pub description: String,
    #[serde(default = "empty_object")]
    pub stats: Value,
    #[serde(default)]
    pub notes: Notes,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Item {
    pub name: String,
    pub item_type: String,
    pub location: String,
    pub description: String,
    #[serde(default)]
    pub notes: Notes,
}

impl Scenario {
    pub fn read(path: impl AsRef<FilePath>) -> Result<Self> {
        let path = path.as_ref();
        let source = fs::read_to_string(path)
            .with_context(|| format!("reading scenario {}", path.display()))?;
        let scenario: Self = serde_json::from_str(&source)
            .with_context(|| format!("parsing scenario {}", path.display()))?;
        scenario.validate()?;
        Ok(scenario)
    }

    pub fn validate(&self) -> Result<()> {
        let location_names = self
            .locations
            .iter()
            .map(|location| location.name.as_str())
            .collect::<std::collections::HashSet<_>>();
        ensure!(
            location_names.len() == self.locations.len(),
            "scenario has duplicate location names"
        );
        ensure!(
            location_names.contains(self.starting_location.as_str()),
            "scenario start location {} does not exist",
            self.starting_location
        );
        for path in &self.paths {
            ensure!(
                location_names.contains(path.from.as_str())
                    && location_names.contains(path.to.as_str()),
                "path references an unknown location"
            );
            ensure!(
                path.from != path.to,
                "path cannot connect a location to itself"
            );
            ensure!(path.travel_time >= 0, "path travel time cannot be negative");
        }
        for npc in &self.npcs {
            ensure!(
                location_names.contains(npc.location.as_str()),
                "NPC {} references an unknown location",
                npc.name
            );
        }
        let item_type_names = self
            .item_types
            .iter()
            .map(|item_type| item_type.name.as_str())
            .collect::<std::collections::HashSet<_>>();
        ensure!(
            item_type_names.len() == self.item_types.len(),
            "scenario has duplicate item type names"
        );
        for item in &self.items {
            ensure!(
                item_type_names.contains(item.item_type.as_str()),
                "item {} references an unknown item type",
                item.name
            );
            ensure!(
                location_names.contains(item.location.as_str()),
                "item {} references an unknown location",
                item.name
            );
        }
        let spell_type_names = self
            .spell_types
            .iter()
            .map(|spell_type| spell_type.name.as_str())
            .collect::<std::collections::HashSet<_>>();
        ensure!(
            spell_type_names.len() == self.spell_types.len(),
            "scenario has duplicate spell type names"
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_a_reference_to_a_missing_location() {
        let scenario = Scenario {
            name: "test".into(),
            initial_prompt: String::new(),
            storyteller_summary: String::new(),
            notes: Notes::default(),
            starting_location: "hut".into(),
            locations: vec![Location {
                name: "hut".into(),
                kind: "hut".into(),
                description: String::new(),
                notes: Notes::default(),
            }],
            paths: vec![],
            npcs: vec![Npc {
                name: "Toma".into(),
                location: "missing".into(),
                description: String::new(),
                background: String::new(),
                motive: String::new(),
                ambition: String::new(),
                sheet: empty_object(),
                notes: Notes::default(),
            }],
            item_types: vec![],
            spell_types: vec![],
            items: vec![],
        };

        assert!(scenario.validate().is_err());
    }
}
