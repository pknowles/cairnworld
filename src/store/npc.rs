//! NPC response rounds (design.md's NPC speech and response rounds). Speech an
//! NPC hears opens a round in which each hearing NPC may answer; progress is
//! derived from the invocation, tool-execution and sequence records the steps
//! produce, never copied into a parallel state machine.

use anyhow::{Context, Result};
use sqlx::{Sqlite, Transaction};

use super::Store;

impl Store {
    /// Called in the transaction that commits an utterance. Advances the
    /// group's scene revision, supersedes any round still collecting or
    /// selected (carrying the speech it had not yet answered), and opens a
    /// round at the new revision for every NPC that actually heard a source.
    /// The round stays ineligible until the sequence that produced the speech
    /// has finished successfully.
    pub(super) async fn open_response_round(
        transaction: &mut Transaction<'_, Sqlite>,
        group_id: i64,
        action_id: i64,
        sequence_id: i64,
    ) -> Result<()> {
        let revision = sqlx::query_scalar!(
            "UPDATE \"group\" SET scene_revision = scene_revision + 1 WHERE id = ? \
             RETURNING scene_revision",
            group_id,
        )
        .fetch_one(&mut **transaction)
        .await
        .context("advancing the group's scene revision")?;
        let Some(source) = sqlx::query_scalar!(
            "SELECT requesting_tool_execution_id FROM action WHERE id = ?",
            action_id,
        )
        .fetch_one(&mut **transaction)
        .await
        .context("finding the call that requested the speech")?
        else {
            return Ok(());
        };
        // A follow-up speech belongs to the discussion whose step produced it;
        // anything else is a human turn starting its own.
        let root = sqlx::query_scalar!(
            "SELECT npc_round.root_sequence_id FROM npc_candidate \
             JOIN npc_round ON npc_round.id = npc_candidate.round_id \
             WHERE npc_candidate.execution_sequence_id = ?",
            sequence_id,
        )
        .fetch_optional(&mut **transaction)
        .await
        .context("finding the discussion this speech continues")?
        .unwrap_or(sequence_id);
        let mut sources = Vec::new();
        if let Some(open) = sqlx::query_scalar!(
            "SELECT id FROM npc_round WHERE group_id = ? AND state IN ('collecting', 'selected')",
            group_id,
        )
        .fetch_optional(&mut **transaction)
        .await
        .context("finding the group's open round")?
        {
            sources = sqlx::query_scalar!(
                "SELECT source_tool_execution_id FROM npc_round_source WHERE round_id = ?",
                open,
            )
            .fetch_all(&mut **transaction)
            .await
            .context("carrying the open round's unanswered speech")?;
            sqlx::query!(
                "UPDATE npc_round SET state = 'superseded' WHERE id = ?",
                open
            )
            .execute(&mut **transaction)
            .await
            .context("superseding the open round")?;
        }
        sources.push(source);
        let mut roster = Vec::new();
        for source in &sources {
            roster.extend(
                sqlx::query_scalar!(
                    "SELECT DISTINCT npc_agent.character_id AS \"character_id!: i64\" \
                     FROM action \
                     JOIN speech ON speech.action_id = action.id \
                     JOIN speech_message ON speech_message.speech_action_id = speech.action_id \
                     JOIN history_entry ON history_entry.id = speech_message.message_id \
                     JOIN agent_history ON agent_history.branch_id = history_entry.branch_id \
                     JOIN npc_agent ON npc_agent.agent_id = agent_history.agent_id \
                     JOIN character_group ON character_group.character_id = npc_agent.character_id \
                     WHERE action.requesting_tool_execution_id = ? \
                       AND npc_agent.character_id <> speech.speaker_character_id \
                       AND character_group.group_id = ?",
                    source,
                    group_id,
                )
                .fetch_all(&mut **transaction)
                .await
                .context("finding the NPCs who heard the speech")?,
            );
        }
        roster.sort_unstable();
        roster.dedup();
        if roster.is_empty() {
            return Ok(());
        }
        let round_id = sqlx::query_scalar!(
            "INSERT INTO npc_round (root_sequence_id, group_id, scene_revision, state) \
             VALUES (?, ?, ?, 'collecting') RETURNING id",
            root,
            group_id,
            revision,
        )
        .fetch_one(&mut **transaction)
        .await
        .context("opening a response round")?;
        for source in sources {
            sqlx::query!(
                "INSERT INTO npc_round_source (round_id, source_tool_execution_id) VALUES (?, ?)",
                round_id,
                source,
            )
            .execute(&mut **transaction)
            .await
            .context("recording the speech a round answers")?;
        }
        for character_id in roster {
            sqlx::query!(
                "INSERT INTO npc_candidate (round_id, character_id) VALUES (?, ?)",
                round_id,
                character_id,
            )
            .execute(&mut **transaction)
            .await
            .context("inviting an NPC to answer")?;
        }
        Ok(())
    }
}
