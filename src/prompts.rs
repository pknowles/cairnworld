//! Everything the models are told lives in `prompts.toml`, not in code, so it
//! can be edited without rebuilding. The file is read when a prompt is next
//! needed and again after it changes; a problem in it fails the request that
//! needs it with the file's own words.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::SystemTime,
};

use anyhow::{Context, Result, ensure};
use serde::Deserialize;

use crate::llm::ToolDefinition;

const DEFAULT_PATH: &str = "prompts.toml";
const PATH_VARIABLE: &str = "CAIRNWORLD_PROMPTS";

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Prompts {
    pub gm: Gm,
    pub player: Player,
    pub npc: Npc,
    pub compaction: Compaction,
    pub history: History,
    /// Tools players and NPCs both call.
    pub shared: Shared,
    pub world: WorldTexts,
    pub questioner: Questioner,
    pub storyteller: Storyteller,
}

/// How history is shown to the model.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct History {
    /// Ends a refused reply that is shown shortened.
    pub refused_reply_cut: String,
}

/// Stand-ins for what world generation has not been given yet.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorldTexts {
    pub no_seed: String,
    pub no_summary: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Questioner {
    pub role: String,
    pub meta_request: String,
    pub summary_state: String,
    pub objects_result: String,
    pub ask_request: String,
    pub ask_reminder: String,
    pub review_request: String,
    pub review_state: String,
    pub review_missing: String,
    pub review_reminder: String,
    pub sample_size: usize,
    pub example_questions: Vec<String>,
    pub meta_questions: Vec<String>,
    pub tools: Tools,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Storyteller {
    pub first_round_temperature: f32,
    pub role: String,
    pub world_state: String,
    pub round_start: String,
    pub next_question: String,
    pub summary_request: String,
    pub summary_note_request: String,
    pub name_request: String,
    pub uncreated_mentions: String,
    pub create_locations_request: String,
    pub connect_locations_request: String,
    pub next_npc_request: String,
    pub create_items_request: String,
    pub group_npcs_request: String,
    pub fix_request: String,
    pub objects_state: String,
    pub tools: Tools,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Gm {
    pub role: String,
    pub ask_role: String,
    pub ask_request: String,
    pub narrate_reminder: String,
    pub narrate_next: String,
    pub decide_next: String,
    pub outcome_untold: String,
    pub speech_conveyed: String,
    pub action_request: String,
    pub tied_note: String,
    pub scene_request: String,
    pub entrance_request: String,
    pub presence_request: String,
    pub intervention_request: String,
    pub waiting_for: String,
    pub answers_request: String,
    pub reply_answered: String,
    pub reply_timed_out: String,
    pub tools: Tools,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Player {
    pub creation: String,
    pub scene: String,
    pub turn_end: String,
    pub entered: String,
    pub reply_sent: String,
    pub action_waiting: String,
    pub tools: Tools,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Npc {
    pub role: String,
    pub situation: String,
    pub before_deciding: String,
    pub eagerness: String,
    pub tools: Tools,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Shared {
    pub tools: Tools,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Compaction {
    pub summary_request: String,
}

/// The tools one agent calls, by name.
#[derive(Debug, Deserialize)]
#[serde(transparent)]
pub struct Tools(BTreeMap<String, ToolPrompt>);

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ToolPrompt {
    description: String,
    /// One text per argument the tool takes.
    #[serde(default)]
    arguments: BTreeMap<String, String>,
    /// The arguments carry no text: their meaning is given where they are
    /// created, and repeating it would cost context in every request.
    #[serde(default)]
    bare: bool,
    /// Texts for the fields of objects inside an argument, by that argument.
    #[serde(default)]
    nested: BTreeMap<String, BTreeMap<String, String>>,
}

/// The prompts as they are now: reread when the file has changed.
pub fn current() -> Result<Arc<Prompts>> {
    #[cfg(test)]
    if let Some(path) = TEST_FILE.with(|file| file.borrow().clone()) {
        return load(&path);
    }
    load(&PathBuf::from(
        std::env::var(PATH_VARIABLE).unwrap_or_else(|_| DEFAULT_PATH.into()),
    ))
}

#[cfg(test)]
thread_local! {
    static TEST_FILE: std::cell::RefCell<Option<PathBuf>> = const { std::cell::RefCell::new(None) };
}

/// Make this test's thread read `path` instead of the checked-in file, until the
/// returned guard is dropped. Tests run on one thread each, so others are
/// unaffected.
#[cfg(test)]
pub fn use_file_in_this_test(path: PathBuf) -> impl Drop {
    struct Guard;
    impl Drop for Guard {
        fn drop(&mut self) {
            TEST_FILE.with(|file| *file.borrow_mut() = None);
        }
    }
    TEST_FILE.with(|file| *file.borrow_mut() = Some(path));
    Guard
}

fn load(path: &Path) -> Result<Arc<Prompts>> {
    static CACHE: Mutex<BTreeMap<PathBuf, (SystemTime, Arc<Prompts>)>> =
        Mutex::new(BTreeMap::new());
    let modified = std::fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .with_context(|| format!("reading the prompt file {}", path.display()))?;
    let mut cache = CACHE
        .lock()
        .map_err(|_| anyhow::anyhow!("the prompt cache was poisoned"))?;
    if let Some((cached, prompts)) = cache.get(path)
        && *cached == modified
    {
        return Ok(Arc::clone(prompts));
    }
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("reading the prompt file {}", path.display()))?;
    // Windows editors may save a byte-order mark.
    let text = text.trim_start_matches('\u{feff}');
    let prompts: Prompts = config::Config::builder()
        .add_source(config::File::from_str(text, config::FileFormat::Toml))
        .build()
        .and_then(config::Config::try_deserialize)
        .with_context(|| format!("parsing the prompt file {}", path.display()))?;
    let prompts = Arc::new(prompts);
    cache.insert(path.to_path_buf(), (modified, Arc::clone(&prompts)));
    Ok(prompts)
}

/// Fill the `{name}` placeholders of a template. A placeholder with no value,
/// or a value with no placeholder, means the file and the game disagree. Only
/// the template's own placeholders are filled: a value that happens to contain
/// `{name}` (a character's speech, say) stays exactly as it is.
pub fn fill(template: &str, values: &[(&str, &str)]) -> Result<String> {
    let mut text = String::with_capacity(template.len());
    let mut used = Vec::new();
    let mut rest = template;
    while let Some(start) = rest.find('{') {
        let Some(length) = rest[start..].find('}') else {
            break;
        };
        let name = &rest[start + 1..start + length];
        text.push_str(&rest[..start]);
        if !name.is_empty() && name.chars().all(|c| c.is_ascii_lowercase() || c == '_') {
            let (_, value) = values
                .iter()
                .find(|(given, _)| *given == name)
                .with_context(|| {
                    format!("the prompt has a {{{name}}} the game does not fill: {template}")
                })?;
            text.push_str(value);
            used.push(name);
        } else {
            text.push_str(&rest[start..=start + length]);
        }
        rest = &rest[start + length + 1..];
    }
    text.push_str(rest);
    for (name, _) in values {
        ensure!(
            used.contains(name),
            "the game fills a {{{name}}} the prompt does not have: {template}"
        );
    }
    Ok(text)
}

impl Tools {
    /// The definition of the tool `name` whose arguments are described by
    /// `schema`, with the text from the file in place of anything derived from
    /// the code. Every argument must have text and none may be left over.
    pub fn tool(&self, name: &str, mut schema: serde_json::Value) -> Result<ToolDefinition> {
        let prompt = self
            .0
            .get(name)
            .with_context(|| format!("the prompt file has no tool {name} here"))?;
        if prompt.bare {
            // Names and types only: the arguments are described by another
            // tool that takes the same ones.
            ensure!(
                prompt.arguments.is_empty(),
                "tool {name} is bare, so it has no argument text"
            );
        } else {
            describe(&mut schema, &prompt.arguments)
                .with_context(|| format!("describing the arguments of tool {name}"))?;
        }
        for (argument, fields) in &prompt.nested {
            let definition = schema["properties"][argument]
                .get("$ref")
                .or_else(|| schema["properties"][argument]["items"].get("$ref"))
                .and_then(|reference| reference.as_str())
                .and_then(|reference| reference.strip_prefix("#/$defs/"))
                .map(str::to_string)
                .with_context(|| {
                    format!("tool {name}: `{argument}` does not hold objects to describe")
                })?;
            describe(&mut schema["$defs"][&definition], fields).with_context(|| {
                format!("describing the fields inside `{argument}` of tool {name}")
            })?;
        }
        strip_generated(&mut schema);
        Ok(ToolDefinition {
            name: name.to_string(),
            description: prompt.description.clone(),
            schema,
        })
    }
}

/// Remove the fields of a JSON packet that hold nothing: empty text, lists and
/// objects, and nulls. A model reads an absent field as nothing there, at a
/// fraction of the tokens (experiments/0006-context-size.md).
pub fn drop_blanks(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(map) => {
            map.values_mut().for_each(drop_blanks);
            map.retain(|_, field| match field {
                serde_json::Value::Null => false,
                serde_json::Value::String(text) => !text.is_empty(),
                serde_json::Value::Array(items) => !items.is_empty(),
                serde_json::Value::Object(fields) => !fields.is_empty(),
                _ => true,
            });
        }
        serde_json::Value::Array(items) => items.iter_mut().for_each(drop_blanks),
        _ => {}
    }
}

/// Remove what the schema generator adds that says nothing to the model: the
/// schema URL, the type's title, integer formats and null defaults. They cost
/// about a tenth of a request (experiments/0006-context-size.md) and replaying
/// requests without them changed none of the model's choices. The names of an
/// object's properties are not schema keys, so a property called `format` stays.
///
/// An optional field's `["integer", "null"]` becomes `"integer"`: leaving the field
/// out already means nothing, and mistral.rs's Qwen call parser reads only a single
/// `type`, so it returned an optional number as a string that Rust then rejected.
fn strip_generated(schema: &mut serde_json::Value) {
    match schema {
        serde_json::Value::Object(map) => {
            for key in ["$schema", "title", "format", "default"] {
                map.remove(key);
            }
            let null = serde_json::Value::from("null");
            let plain = match map.get("type") {
                Some(serde_json::Value::Array(kinds))
                    if kinds.len() == 2 && kinds.contains(&null) =>
                {
                    kinds.iter().find(|kind| **kind != null).cloned()
                }
                _ => None,
            };
            if let Some(plain) = plain {
                map.insert("type".to_string(), plain);
            }
            for (key, child) in map.iter_mut() {
                match (key.as_str(), child) {
                    ("properties", serde_json::Value::Object(properties)) => {
                        properties.values_mut().for_each(strip_generated)
                    }
                    (_, child) => strip_generated(child),
                }
            }
        }
        serde_json::Value::Array(items) => items.iter_mut().for_each(strip_generated),
        _ => {}
    }
}

/// A tool that takes no arguments.
pub fn no_arguments() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {},
    })
}

/// Set each property's description from `texts`: every property needs text and
/// every text needs a property.
fn describe(schema: &mut serde_json::Value, texts: &BTreeMap<String, String>) -> Result<()> {
    let empty = serde_json::Map::new();
    let names: Vec<String> = schema
        .get("properties")
        .and_then(|properties| properties.as_object())
        .unwrap_or(&empty)
        .keys()
        .cloned()
        .collect();
    for name in &names {
        let text = texts
            .get(name)
            .with_context(|| format!("no text for the argument `{name}`"))?;
        schema["properties"][name]["description"] = serde_json::Value::String(text.clone());
    }
    for name in texts.keys() {
        ensure!(
            names.contains(name),
            "text for `{name}`, which the tool does not take"
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_optional_fields_null_alternative_is_left_out_of_its_type() {
        let mut schema = serde_json::json!({
            "type": "object",
            "properties": {
                "id": {"type": ["integer", "null"]},
                "text": {"type": ["null", "string"], "maxLength": 9},
                "either": {"type": ["integer", "string"]},
                "plain": {"type": "integer"},
            },
        });
        strip_generated(&mut schema);
        let kinds: Vec<_> = ["id", "text", "either", "plain"]
            .iter()
            .map(|name| schema["properties"][name]["type"].clone())
            .collect();
        assert_eq!(
            kinds,
            [
                serde_json::json!("integer"),
                serde_json::json!("string"),
                serde_json::json!(["integer", "string"]),
                serde_json::json!("integer"),
            ]
        );
        assert_eq!(schema["properties"]["text"]["maxLength"], 9);
    }

    #[test]
    fn the_checked_in_prompt_file_loads() {
        current().unwrap();
    }

    #[test]
    fn an_edit_to_the_file_applies_to_the_next_request_and_a_mistake_names_itself() {
        let scratch = crate::store::ScratchDir::new("prompts-test");
        let path = scratch.path().join("prompts.toml");
        // The search below looks for a blank line, so it needs the file with
        // `\n` endings whatever the checkout converted them to.
        let original = std::fs::read_to_string(DEFAULT_PATH)
            .unwrap()
            .replace("\r\n", "\n");
        std::fs::write(&path, &original).unwrap();
        let before = load(&path).unwrap().npc.role.clone();

        // Replace the whole `role = ...` value of the [npc] section.
        let section = original.find("[npc]").unwrap();
        let start = section + original[section..].find("role = ").unwrap();
        let end = start
            + original[start..]
                .find(
                    "

",
                )
                .unwrap();
        let edited = format!(
            "{}role = \"Changed role.\"{}",
            &original[..start],
            &original[end..]
        );
        std::thread::sleep(std::time::Duration::from_millis(50));
        std::fs::write(&path, &edited).unwrap();
        assert_ne!(before, "Changed role.");
        assert_eq!(load(&path).unwrap().npc.role, "Changed role.");

        std::thread::sleep(std::time::Duration::from_millis(50));
        std::fs::write(
            &path,
            original.replace(
                "[npc]",
                "[npc]
rolle = \"typo\"",
            ),
        )
        .unwrap();
        let error = format!("{:#}", load(&path).unwrap_err());
        assert!(error.contains("rolle"), "{error}");
    }

    #[test]
    fn a_byte_order_mark_from_an_editor_is_ignored() {
        let scratch = crate::store::ScratchDir::new("prompts-bom");
        let path = scratch.path().join("prompts.toml");
        let original = std::fs::read_to_string(DEFAULT_PATH).unwrap();
        std::fs::write(&path, format!("\u{feff}{original}")).unwrap();
        load(&path).unwrap();
    }

    #[test]
    fn placeholders_must_match_the_values_given() {
        assert_eq!(
            fill("{a} then {b}", &[("a", "1"), ("b", "{x}")]).unwrap(),
            "1 then {x}"
        );
        // Values are inserted literally, whatever they contain.
        assert_eq!(
            fill(
                "{arguments} then {note}",
                &[("arguments", "Repeat {note} exactly"), ("note", "!")]
            )
            .unwrap(),
            "Repeat {note} exactly then !"
        );
        assert!(fill("{a}", &[]).is_err());
        assert!(fill("plain", &[("a", "1")]).is_err());
    }

    #[test]
    fn a_tool_needs_text_for_exactly_its_arguments() {
        let schema = serde_json::json!({
            "type": "object",
            "properties": {"words": {"type": "string"}, "to": {"type": "string"}, "private": {"type": "boolean"}},
        });
        let prompts = current().unwrap();
        let tools = &prompts.shared.tools;
        assert!(tools.tool("speak", schema.clone()).is_ok());
        let extra = serde_json::json!({
            "type": "object",
            "properties": {"words": {"type": "string"}},
        });
        assert!(
            tools.tool("speak", extra).is_err(),
            "`to` and `private` have text but no argument"
        );
        let missing = serde_json::json!({
            "type": "object",
            "properties": {"words": {"type": "string"}, "to": {"type": "string"}, "private": {"type": "boolean"}, "more": {"type": "string"}},
        });
        assert!(tools.tool("speak", missing).is_err(), "`more` has no text");
        assert!(tools.tool("no_such_tool", schema).is_err());
    }

    #[test]
    fn generated_schema_noise_is_removed_but_not_properties_with_the_same_names() {
        let mut schema = serde_json::json!({
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "title": "Speak",
            "properties": {
                "to": {"type": ["integer", "null"], "format": "int64", "default": null},
                "format": {"type": "string", "title": "Format"},
            },
        });
        strip_generated(&mut schema);
        assert_eq!(
            schema,
            serde_json::json!({"properties": {
                "to": {"type": "integer"},
                "format": {"type": "string"},
            }})
        );
    }

    #[test]
    fn blank_fields_are_dropped_but_zero_and_false_are_kept() {
        let mut packet = serde_json::json!({
            "name": "Mara",
            "background": "",
            "notes": null,
            "inventory": [],
            "sheet": {"hp": 0, "armor": null, "flags": {}},
            "items": [{"name": "sack", "description": ""}],
            "ready": false,
        });
        drop_blanks(&mut packet);
        assert_eq!(
            packet,
            serde_json::json!({
                "name": "Mara",
                "sheet": {"hp": 0},
                "items": [{"name": "sack"}],
                "ready": false,
            })
        );
    }
}
