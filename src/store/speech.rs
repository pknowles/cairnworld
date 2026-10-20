//! Speech as a completed game event (design.md's NPC speech and response
//! rounds). The approved words are the action's result content, stored once;
//! each actual hearer's history holds a message referencing that content, and
//! every reader derives attribution from the speech record, never from a
//! character's current name.

use anyhow::{Context, Result, bail};
use schemars::JsonSchema;
use serde::Deserialize;
use sqlx::{Sqlite, Transaction};

use super::{PendingAction, Store};
use crate::{
    inspection::MessageOrigin,
    llm::{MessageContent, Role},
};

/// What the model supplies to speak to the whole group.
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SayAction {
    /// The exact words your character says aloud.
    pub words: String,
    /// The one character you speak to, if any. Everyone present still hears you.
    #[serde(default)]
    pub to: Option<String>,
}

/// What the model supplies to speak privately.
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WhisperAction {
    /// The exact words your character whispers.
    pub words: String,
    /// The characters who hear the whisper. Nobody else does.
    pub to: Vec<String>,
}

/// The GM's adjustment of who hears an approved utterance, by character name
/// within the speaker's group.
#[derive(Default)]
pub struct Audience {
    pub include: Vec<String>,
    pub exclude: Vec<String>,
}

/// The GM named hearers that cannot be applied, so nothing was resolved and it
/// may try again.
#[derive(Debug)]
pub struct InvalidAudience(pub String);

impl std::fmt::Display for InvalidAudience {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for InvalidAudience {}

struct Member {
    id: i64,
    name: String,
    branch_id: i64,
}

/// An approved utterance and its snapshot of hearers, ready to commit.
pub(super) struct SpeechPlan {
    kind: &'static str,
    words: String,
    group_id: i64,
    speaker_id: i64,
    speaker_name: String,
    addressed_name: Option<String>,
    /// Everyone who hears it except the speaker, whose own history is written
    /// after the speaker's tool result (see `deliver_speech_to_speaker`).
    hearers: Vec<Member>,
}

impl SpeechPlan {
    pub(super) fn words(&self) -> &str {
        &self.words
    }

    pub(super) fn hearer_names(&self) -> Vec<String> {
        self.hearers
            .iter()
            .map(|hearer| hearer.name.clone())
            .collect()
    }
}

/// The text every reader shows for a speech message.
pub(super) fn attributed(
    kind: &str,
    speaker: &str,
    addressed: Option<&str>,
    words: &str,
) -> String {
    match (kind, addressed) {
        ("whisper", _) => format!("{speaker} whispers: {words}"),
        (_, Some(addressed)) => format!("{speaker} says to {addressed}: {words}"),
        (_, None) => format!("{speaker} says: {words}"),
    }
}

fn find<'a>(members: &'a [Member], name: &str) -> std::result::Result<&'a Member, String> {
    let name = name.trim();
    let mut matching = members
        .iter()
        .filter(|member| member.name.eq_ignore_ascii_case(name));
    match (matching.next(), matching.next()) {
        (Some(member), None) => Ok(member),
        (None, _) => Err(format!("`{name}` is not in this group")),
        (Some(_), Some(_)) => Err(format!("more than one character here is named `{name}`")),
    }
}

impl Store {
    async fn group_members(
        transaction: &mut Transaction<'_, Sqlite>,
        group_id: i64,
    ) -> Result<Vec<Member>> {
        let rows = sqlx::query!(
            "SELECT character.id AS \"id!: i64\", character.name AS \"name!\", \
                    agent_history.branch_id AS \"branch_id!: i64\" \
             FROM character_group \
             JOIN character ON character.id = character_group.character_id \
             LEFT JOIN player_character ON player_character.character_id = character.id \
             LEFT JOIN world_member ON world_member.id = player_character.member_id \
             LEFT JOIN npc_agent ON npc_agent.character_id = character.id \
             JOIN agent_history ON agent_history.agent_id = COALESCE(player_character.agent_id, npc_agent.agent_id) \
             WHERE character_group.group_id = ? \
               AND (npc_agent.agent_id IS NOT NULL OR world_member.access = 'active') \
             ORDER BY character.id",
            group_id,
        )
        .fetch_all(&mut **transaction)
        .await
        .context("loading the speaker's group")?;
        Ok(rows
            .into_iter()
            .map(|row| Member {
                id: row.id,
                name: row.name,
                branch_id: row.branch_id,
            })
            .collect())
    }

    /// What a speech message shows every reader (the model's context, player
    /// chat and inspection): the utterance attributed from its speech record.
    pub(super) async fn speech_text(&self, message_id: i64) -> Result<String> {
        let row = sqlx::query!(
            "SELECT speech.kind, speech.speaker_name, speech.addressed_name, message.content \
             FROM speech_message \
             JOIN speech ON speech.action_id = speech_message.speech_action_id \
             JOIN message ON message.id = speech_message.message_id \
             WHERE speech_message.message_id = ?",
            message_id,
        )
        .fetch_optional(&self.pool)
        .await
        .with_context(|| format!("loading speech for message {message_id}"))?
        .with_context(|| format!("speech message {message_id} has no speech record"))?;
        let (MessageContent::Text(words), _) = self.recorded_content(row.content).await? else {
            bail!("speech message {message_id} does not hold text");
        };
        Ok(attributed(
            &row.kind,
            &row.speaker_name,
            row.addressed_name.as_deref(),
            &words,
        ))
    }

    /// Check the names a speaker supplied against their current group, so a
    /// mistake is returned to the model before anything is submitted.
    pub async fn check_speech_targets(
        &self,
        character_id: i64,
        targets: &[&str],
    ) -> Result<std::result::Result<(), String>> {
        let mut transaction = self
            .pool
            .begin()
            .await
            .context("starting speech target check")?;
        let group_id = Self::character_group_in(&mut transaction, character_id).await?;
        let members = Self::group_members(&mut transaction, group_id).await?;
        for target in targets {
            if let Err(reason) = find(&members, target) {
                return Ok(Err(reason));
            }
        }
        Ok(Ok(()))
    }

    async fn character_group_in(
        transaction: &mut Transaction<'_, Sqlite>,
        character_id: i64,
    ) -> Result<i64> {
        sqlx::query_scalar!(
            "SELECT group_id FROM character_group WHERE character_id = ?",
            character_id,
        )
        .fetch_optional(&mut **transaction)
        .await
        .context("loading speaker's group")?
        .with_context(|| format!("character {character_id} has no group"))
    }

    /// Decide the hearers of an approved say or whisper. `Err` is a typed
    /// `InvalidAudience` for a GM override that cannot be applied; the inner
    /// `Err` is an utterance that cannot happen in the current scene.
    pub(super) async fn plan_speech(
        transaction: &mut Transaction<'_, Sqlite>,
        action: &PendingAction,
        audience: &Audience,
    ) -> Result<std::result::Result<SpeechPlan, String>> {
        let (kind, words, addressed, targets): (&'static str, String, Option<String>, Vec<String>) =
            match action.tool.as_str() {
                "say" => {
                    let say: SayAction = serde_json::from_str(&action.args)
                        .context("parsing stored say action arguments")?;
                    ("say", say.words, say.to, Vec::new())
                }
                "whisper" => {
                    let whisper: WhisperAction = serde_json::from_str(&action.args)
                        .context("parsing stored whisper action arguments")?;
                    ("whisper", whisper.words, None, whisper.to)
                }
                tool => bail!("`{tool}` is not a speech action"),
            };
        let group_id = Self::character_group_in(transaction, action.character_id).await?;
        let members = Self::group_members(transaction, group_id).await?;
        let Some(speaker) = members
            .iter()
            .find(|member| member.id == action.character_id)
        else {
            return Ok(Err("the speaker is no longer here".to_string()));
        };
        let mut hearer_ids: Vec<i64> = if kind == "say" {
            members.iter().map(|member| member.id).collect()
        } else {
            let mut ids = Vec::new();
            for target in &targets {
                match find(&members, target) {
                    Ok(member) => ids.push(member.id),
                    Err(reason) => return Ok(Err(reason)),
                }
            }
            ids
        };
        let named = |names: &[String]| -> std::result::Result<Vec<i64>, InvalidAudience> {
            names
                .iter()
                .map(|name| {
                    find(&members, name)
                        .map(|member| member.id)
                        .map_err(InvalidAudience)
                })
                .collect()
        };
        let included = named(&audience.include)?;
        let excluded = named(&audience.exclude)?;
        if let Some(id) = included.iter().find(|id| excluded.contains(id)) {
            let name = &members
                .iter()
                .find(|member| member.id == *id)
                .expect("named")
                .name;
            return Err(InvalidAudience(format!("`{name}` is both included and excluded")).into());
        }
        if excluded.contains(&speaker.id) {
            return Err(InvalidAudience("the speaker always hears their own words".into()).into());
        }
        hearer_ids.extend(included);
        hearer_ids.retain(|id| *id != speaker.id && !excluded.contains(id));
        hearer_ids.sort_unstable();
        hearer_ids.dedup();
        let speaker_id = speaker.id;
        let speaker_name = speaker.name.clone();
        let hearers = members
            .into_iter()
            .filter(|member| hearer_ids.contains(&member.id))
            .collect();
        Ok(Ok(SpeechPlan {
            kind,
            words,
            group_id,
            speaker_id,
            speaker_name,
            addressed_name: addressed,
            hearers,
        }))
    }

    /// Record the utterance and give each hearer's history a message that
    /// shares the action's result content.
    pub(super) async fn commit_speech(
        transaction: &mut Transaction<'_, Sqlite>,
        plan: &SpeechPlan,
        world_id: i64,
        sequence_id: i64,
        action_id: i64,
        words_content_id: i64,
    ) -> Result<()> {
        let batch_id = sqlx::query_scalar!(
            "INSERT INTO speech_batch (world_id, group_id, sequence_id) VALUES (?, ?, ?) RETURNING id",
            world_id,
            plan.group_id,
            sequence_id,
        )
        .fetch_one(&mut **transaction)
        .await
        .context("recording speech batch")?;
        sqlx::query!(
            "INSERT INTO speech (action_id, batch_id, speaker_character_id, speaker_name, kind, addressed_name) \
             VALUES (?, ?, ?, ?, ?, ?)",
            action_id,
            batch_id,
            plan.speaker_id,
            plan.speaker_name,
            plan.kind,
            plan.addressed_name,
        )
        .execute(&mut **transaction)
        .await
        .context("recording utterance")?;
        for hearer in &plan.hearers {
            Self::append_speech_message(
                transaction,
                hearer.branch_id,
                action_id,
                words_content_id,
                sequence_id,
            )
            .await?;
        }
        Ok(())
    }

    async fn append_speech_message(
        transaction: &mut Transaction<'_, Sqlite>,
        branch_id: i64,
        action_id: i64,
        words_content_id: i64,
        sequence_id: i64,
    ) -> Result<()> {
        let message_id = Self::insert_message_with_content(
            transaction,
            branch_id,
            (Role::Speech, MessageOrigin::Conversation),
            words_content_id,
            None,
            None,
            Some(sequence_id),
        )
        .await?;
        sqlx::query!(
            "INSERT INTO speech_message (message_id, speech_action_id) VALUES (?, ?)",
            message_id,
            action_id,
        )
        .execute(&mut **transaction)
        .await
        .context("linking speech to its hearer's history")?;
        Ok(())
    }

    /// The speaker's own history gets the utterance after their tool result,
    /// since a message between a call and its result would break the history
    /// (design.md's Server shutdown and crashes). Does nothing for a call that
    /// requested no speech.
    pub async fn deliver_speech_to_speaker(&self, requesting_tool_execution_id: i64) -> Result<()> {
        let mut transaction = self
            .pool
            .begin()
            .await
            .context("starting speech delivery to the speaker")?;
        let Some(speech) = sqlx::query!(
            "SELECT action.id AS \"action_id!: i64\", action.sequence_id, action.result_content_id, \
                    agent_history.branch_id AS \"branch_id!: i64\" \
             FROM action \
             JOIN speech ON speech.action_id = action.id \
             JOIN player_character ON player_character.character_id = speech.speaker_character_id \
             JOIN agent_history ON agent_history.agent_id = player_character.agent_id \
             WHERE action.requesting_tool_execution_id = ?",
            requesting_tool_execution_id,
        )
        .fetch_optional(&mut *transaction)
        .await
        .context("finding the speaker's utterance")?
        else {
            return Ok(());
        };
        Self::append_speech_message(
            &mut transaction,
            speech.branch_id,
            speech.action_id,
            speech.result_content_id,
            speech.sequence_id,
        )
        .await?;
        transaction
            .commit()
            .await
            .context("committing speech delivery to the speaker")
    }
}
