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
    /// Tools players and NPCs both call.
    pub shared: Shared,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Gm {
    pub role: String,
    pub action_request: String,
    pub tied_note: String,
    pub opening_request: String,
    pub presence_request: String,
    pub tools: Tools,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Player {
    pub creation: String,
    pub scene: String,
    pub turn_end: String,
    pub entered: String,
    pub tools: Tools,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Npc {
    pub role: String,
    pub situation: String,
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
    /// Texts for the fields of objects inside an argument, by that argument.
    #[serde(default)]
    nested: BTreeMap<String, BTreeMap<String, String>>,
}

/// The prompts as they are now: reread when the file has changed.
pub fn current() -> Result<Arc<Prompts>> {
    load(&PathBuf::from(
        std::env::var(PATH_VARIABLE).unwrap_or_else(|_| DEFAULT_PATH.into()),
    ))
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
/// or a value with no placeholder, means the file and the game disagree.
pub fn fill(template: &str, values: &[(&str, &str)]) -> Result<String> {
    let mut placeholders = Vec::new();
    let mut rest = template;
    while let Some(start) = rest.find('{') {
        let Some(length) = rest[start..].find('}') else {
            break;
        };
        let name = &rest[start + 1..start + length];
        if !name.is_empty() && name.chars().all(|c| c.is_ascii_lowercase() || c == '_') {
            placeholders.push(name);
        }
        rest = &rest[start + length + 1..];
    }
    for name in &placeholders {
        ensure!(
            values.iter().any(|(given, _)| given == name),
            "the prompt has a {{{name}}} the game does not fill: {template}"
        );
    }
    let mut text = template.to_string();
    for (name, value) in values {
        ensure!(
            placeholders.contains(name),
            "the game fills a {{{name}}} the prompt does not have: {template}"
        );
        text = text.replace(&format!("{{{name}}}"), value);
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
        describe(&mut schema, &prompt.arguments)
            .with_context(|| format!("describing the arguments of tool {name}"))?;
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
        Ok(ToolDefinition {
            name: name.to_string(),
            description: prompt.description.clone(),
            schema,
        })
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
    fn the_checked_in_prompt_file_loads() {
        current().unwrap();
    }

    #[test]
    fn an_edit_to_the_file_applies_to_the_next_request_and_a_mistake_names_itself() {
        let directory = std::env::temp_dir().join(format!("prompts-test-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("prompts.toml");
        let original = std::fs::read_to_string(DEFAULT_PATH).unwrap();
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
        std::fs::remove_dir_all(&directory).unwrap();
    }

    #[test]
    fn a_byte_order_mark_from_an_editor_is_ignored() {
        let directory = std::env::temp_dir().join(format!("prompts-bom-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("prompts.toml");
        let original = std::fs::read_to_string(DEFAULT_PATH).unwrap();
        std::fs::write(&path, format!("\u{feff}{original}")).unwrap();
        load(&path).unwrap();
        std::fs::remove_dir_all(&directory).unwrap();
    }

    #[test]
    fn placeholders_must_match_the_values_given() {
        assert_eq!(
            fill("{a} then {b}", &[("a", "1"), ("b", "{x}")]).unwrap(),
            "1 then {x}"
        );
        assert!(fill("{a}", &[]).is_err());
        assert!(fill("plain", &[("a", "1")]).is_err());
    }

    #[test]
    fn a_tool_needs_text_for_exactly_its_arguments() {
        let schema = serde_json::json!({
            "type": "object",
            "properties": {"words": {"type": "string"}, "to": {"type": "string"}},
        });
        let prompts = current().unwrap();
        let tools = &prompts.shared.tools;
        assert!(tools.tool("say", schema.clone()).is_ok());
        let extra = serde_json::json!({
            "type": "object",
            "properties": {"words": {"type": "string"}},
        });
        assert!(
            tools.tool("say", extra).is_err(),
            "`to` has text but no argument"
        );
        let missing = serde_json::json!({
            "type": "object",
            "properties": {"words": {"type": "string"}, "to": {"type": "string"}, "more": {"type": "string"}},
        });
        assert!(tools.tool("say", missing).is_err(), "`more` has no text");
        assert!(tools.tool("no_such_tool", schema).is_err());
    }
}
