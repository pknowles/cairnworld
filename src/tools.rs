use anyhow::{Context, Result};
use rand::Rng;
use schemars::{JsonSchema, schema_for};
use serde::Deserialize;

use crate::llm::{ToolCall, ToolDefinition};

pub struct Tool {
    definition: ToolDefinition,
    execute: fn(&str) -> Result<String>,
}

impl Tool {
    pub fn definition(&self) -> ToolDefinition {
        self.definition.clone()
    }

    pub fn execute(&self, arguments: &str) -> Result<String> {
        (self.execute)(arguments)
    }
}

/// The GM's Save tool (user_declarations.md, GM tools): a character rolls d20
/// against one attribute to resist or attempt something. Rust rolls, never the
/// model - the model asks for an outcome and receives a verdict.
pub fn save() -> Tool {
    Tool {
        definition: ToolDefinition {
            name: "save".to_string(),
            description:
                "Roll a character's save against one attribute. Give what the save is for. \
                 A higher difficulty makes success less likely."
                    .to_string(),
            schema: serde_json::to_value(schema_for!(Save)).expect("save schema should serialize"),
        },
        execute: save_arguments,
    }
}

pub fn definitions(tools: &[Tool]) -> Vec<ToolDefinition> {
    tools.iter().map(Tool::definition).collect()
}

pub fn execute(tools: &[Tool], call: &ToolCall) -> Result<String> {
    let tool = tools
        .iter()
        .find(|tool| tool.definition.name == call.name)
        .with_context(|| format!("unknown tool `{}`", call.name))?;
    tool.execute(&call.arguments)
        .with_context(|| format!("executing tool `{}`", call.name))
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct Save {
    // Becomes a character id once the character table lands (milestone 7).
    /// Who is saving.
    character: String,
    /// Cairn attribute being tested.
    attribute: Attribute,
    /// Short reason the save is being made, e.g. "dodge the falling beam".
    reason: String,
    /// Added to the roll; positive makes success less likely.
    #[serde(default, deserialize_with = "lenient_number")]
    difficulty: i8,
    // TODO: REPLACE ME WHEN CHARACTERS EXIST.
    //
    // `attribute_value` is a placeholder and MUST be deleted once the
    // `character` table lands (milestone 7). Rust will then look the value up
    // from `character` using the id above, because the model must never supply
    // a stat it does not own - a model that can choose the number can choose
    // to pass (prompt_standards.md: no hidden or model-supplied state behind
    // the tool's back).
    //
    // Removing this field is a breaking change to the tool schema. Every test
    // that constructs `save` arguments must be updated at the same time, and
    // recorded inferences made before the change will no longer deserialize.
    //
    // Doc comments on these fields become schema descriptions sent to the
    // model, so notes to ourselves belong in `//` comments like this one.
    /// The character's score in that attribute.
    #[serde(deserialize_with = "lenient_number")]
    attribute_value: u8,
}

/// Accept a number the model quoted as a string. Small models routinely emit
/// every JSON scalar as a string (observed from Llama 3.1 8B sending
/// `"attribute_value": "11"`), and `11` and `"11"` denote the same value, so
/// rejecting one is a parser limitation rather than a real disagreement. Any
/// text that is not a number still fails loudly.
fn lenient_number<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
    T: TryFrom<i64> + std::str::FromStr,
    <T as std::str::FromStr>::Err: std::fmt::Display,
{
    match serde_json::Value::deserialize(deserializer)? {
        serde_json::Value::Number(number) => {
            let value = number
                .as_i64()
                .ok_or_else(|| serde::de::Error::custom(format!("{number} is not an integer")))?;
            T::try_from(value)
                .map_err(|_| serde::de::Error::custom(format!("{value} is out of range")))
        }
        serde_json::Value::String(text) => text.trim().parse().map_err(serde::de::Error::custom),
        other => Err(serde::de::Error::custom(format!(
            "expected a number, found {other}"
        ))),
    }
}

#[derive(Debug, Deserialize, JsonSchema, PartialEq)]
#[serde(rename_all = "lowercase")]
enum Attribute {
    Str,
    Dex,
    Wil,
}

/// How a resolved save is worded back to the model.
///
/// The roll value never appears: it is theatre for the player, and a model
/// that sees the number treats the outcome as negotiable - observed for real,
/// where Llama 3.1 re-rolled the same save three times after each failure.
/// The variants differ in how they separate "the tool call worked" from "the
/// save did not pass", which is the confusion under test.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SaveWording {
    /// Bare outcome word.
    Terse,
    /// Names the resolution as settled and final.
    Settled,
    /// States the call succeeded, then the in-fiction consequence.
    Outcome,
}

impl SaveWording {
    fn render(self, save: &Save, passed: bool) -> String {
        let who = &save.character;
        let what = &save.reason;
        match self {
            Self::Terse => format!(
                "{who} {} the save to {what}.",
                if passed { "passes" } else { "fails" }
            ),
            Self::Settled => format!(
                "Save resolved: {who} {} to {what}. This result is final.",
                if passed {
                    "succeeds"
                } else {
                    "does not succeed"
                }
            ),
            Self::Outcome => format!(
                "Save complete. {who} {what}: {}. Narrate what happens next.",
                if passed {
                    "they manage it"
                } else {
                    "they do not manage it"
                }
            ),
        }
    }
}

/// Cairn save: roll d20 under the attribute to succeed; ties fail.
fn resolve_save(save: &Save, wording: SaveWording) -> String {
    let roll = rand::rng().random_range(1..=20);
    let target = i32::from(save.attribute_value) - i32::from(save.difficulty);
    wording.render(save, i32::from(roll) < target)
}

fn save_arguments(arguments: &str) -> Result<String> {
    let save: Save = serde_json::from_str(arguments).context("parsing save arguments")?;
    Ok(resolve_save(&save, SaveWording::Settled))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn arguments(attribute_value: u8, difficulty: i8) -> String {
        format!(
            r#"{{"character":"Rook","attribute":"dex","reason":"dodge","difficulty":{difficulty},"attribute_value":{attribute_value}}}"#
        )
    }

    #[test]
    fn schema_describes_every_argument_the_model_must_supply() {
        let schema = save().definition.schema;
        let properties = &schema["properties"];
        for field in ["character", "attribute", "reason", "attribute_value"] {
            assert!(
                properties.get(field).is_some(),
                "schema should describe `{field}`"
            );
        }
        assert_eq!(
            properties["attribute"]["$ref"], "#/$defs/Attribute",
            "attribute must be constrained to the Cairn attributes"
        );
    }

    #[test]
    fn no_description_leaks_notes_meant_for_us() {
        // Doc comments become schema descriptions the model reads. A note about
        // our own plans is context spent on something the model cannot act on,
        // and reads as an instruction it should account for. Observed shipping
        // "TEMPORARY: ... Replaced by a character lookup later." to the model.
        let schema = save().definition.schema;
        let mut described = vec![save().definition.description];
        for value in schema["properties"]
            .as_object()
            .expect("properties")
            .values()
        {
            if let Some(text) = value.get("description").and_then(|d| d.as_str()) {
                described.push(text.to_string());
            }
        }
        for text in described {
            let lowered = text.to_lowercase();
            for marker in ["temporary", "todo", "milestone", "for now", "until then"] {
                assert!(
                    !lowered.contains(marker),
                    "model-facing text contains `{marker}`: {text}"
                );
            }
        }
    }

    const WORDINGS: [SaveWording; 3] = [
        SaveWording::Terse,
        SaveWording::Settled,
        SaveWording::Outcome,
    ];

    /// Did this result report a pass? Every wording must make the two outcomes
    /// distinguishable without inspecting a roll value.
    fn reads_as_pass(result: &str, wording: SaveWording) -> bool {
        match wording {
            SaveWording::Terse => result.contains("passes"),
            SaveWording::Settled => !result.contains("does not"),
            SaveWording::Outcome => !result.contains("do not"),
        }
    }

    fn parse(json: &str) -> Save {
        serde_json::from_str(json).expect("arguments should parse")
    }

    #[test]
    fn outcomes_follow_the_cairn_rule_in_every_wording() {
        // d20 must roll *under* the attribute, so 1 can never pass and 21 can
        // never fail - outcomes fixed by the rules rather than by a seed.
        for wording in WORDINGS {
            for _ in 0..32 {
                let failed = resolve_save(&parse(&arguments(1, 0)), wording);
                assert!(!reads_as_pass(&failed, wording), "{wording:?}: {failed}");
                let passed = resolve_save(&parse(&arguments(21, 0)), wording);
                assert!(reads_as_pass(&passed, wording), "{wording:?}: {passed}");
                // Difficulty 20 drops an otherwise-certain pass below reach.
                let hard = resolve_save(&parse(&arguments(21, 20)), wording);
                assert!(!reads_as_pass(&hard, wording), "{wording:?}: {hard}");
            }
        }
    }

    #[test]
    fn no_wording_reveals_the_roll_value() {
        // The number is theatre for the player. A model that sees it treats
        // the outcome as negotiable and re-rolls until it likes the answer.
        for wording in WORDINGS {
            for _ in 0..64 {
                let result = resolve_save(&parse(&arguments(11, 0)), wording);
                assert!(
                    !result.chars().any(|c| c.is_ascii_digit()),
                    "{wording:?} leaked a number: {result}"
                );
            }
        }
    }

    #[test]
    fn every_wording_names_the_character_from_its_own_call() {
        // Attribution must come from the arguments, not a fixed or positional
        // assumption, so results cannot be paired with the wrong call.
        let save = parse(
            r#"{"character":"Mara","attribute":"wil","reason":"resist","attribute_value":1}"#,
        );
        for wording in WORDINGS {
            assert!(
                resolve_save(&save, wording).contains("Mara"),
                "{wording:?} lost the character"
            );
        }
    }

    #[test]
    fn numbers_quoted_as_strings_are_accepted() {
        // Observed verbatim from Llama 3.1 8B during a real chat: the call was
        // semantically correct but every scalar was quoted.
        let quoted = r#"{"attribute":"dex","attribute_value":"1","character":"Rook","difficulty":"0","reason":"edges along the rotten ledge"}"#;
        assert!(save_arguments(quoted).is_ok());
        // Text that is not a number must still fail loudly.
        assert!(
            save_arguments(
                r#"{"character":"Rook","attribute":"dex","reason":"d","attribute_value":"eleven"}"#
            )
            .is_err()
        );
    }

    #[test]
    fn malformed_and_unknown_arguments_are_rejected() {
        assert!(save_arguments(r#"{"character":"Rook"}"#).is_err());
        assert!(save_arguments(r#"{"attribute":"luck"}"#).is_err());
        assert!(
            save_arguments(
                r#"{"character":"Rook","attribute":"dex","reason":"d","attribute_value":10,"extra":1}"#
            )
            .is_err(),
            "unknown fields must be rejected so schema drift fails loudly"
        );
    }
}
