//! Questions put to characters whose answer the game waits for (design.md,
//! Player replies). A reply is open until the character's next input answers it
//! or its deadline passes and do-nothing is recorded for them.

use anyhow::{Context, Result};

use super::{MessageOrigin, PendingAction, Store};
use crate::llm::{MessageContent, Role};

/// Consecutive missed deadlines after which a player is treated as away.
pub const AFK_AFTER_MISSES: i64 = 3;

/// One character's settled answer to a question.
#[derive(Clone, Debug, PartialEq)]
pub struct Reply {
    pub character_id: i64,
    pub timed_out: bool,
    pub response: String,
}

/// An open reply whose deadline is next, and how long until it passes.
pub struct ReplyDeadline {
    pub action_id: i64,
    pub character_id: i64,
    pub after_seconds: i64,
}

impl Store {
    /// Ask a player's character about pending action `action_id`, due in
    /// `timeout_seconds`: the question reaches their chat now, whether or not
    /// they are connected. Asking again reopens the reply.
    pub async fn ask_player(
        &self,
        world_id: i64,
        action_id: i64,
        character_id: i64,
        question: &str,
        timeout_seconds: u64,
    ) -> Result<()> {
        let timeout_seconds = i64::try_from(timeout_seconds)?;
        let mut transaction = self.pool.begin().await.context("asking a player")?;
        sqlx::query!(
            "INSERT INTO pending_reply (world_id, action_id, character_id, deadline, status)              VALUES (?, ?, ?, datetime(CURRENT_TIMESTAMP, ? || ' seconds'), 'open')              ON CONFLICT (world_id, action_id, character_id) DO UPDATE              SET deadline = excluded.deadline, status = 'open', response = NULL",
            world_id,
            action_id,
            character_id,
            timeout_seconds,
        )
        .execute(&mut *transaction)
        .await
        .with_context(|| format!("asking character {character_id} about action {action_id}"))?;
        let agent_id = sqlx::query_scalar!(
            "SELECT agent_id FROM player_character WHERE character_id = ?",
            character_id,
        )
        .fetch_optional(&mut *transaction)
        .await
        .context("finding the asked player's agent")?
        .with_context(|| format!("character {character_id} is not a player character"))?;
        let branch_id = super::history::branch_for_agent(&mut transaction, agent_id).await?;
        let content_id = Self::content_in(
            &mut transaction,
            &MessageContent::Text(question.to_string()),
            "",
        )
        .await?;
        Self::insert_message_with_content(
            &mut transaction,
            branch_id,
            (Role::Narration, MessageOrigin::Conversation),
            content_id,
            None,
            None,
            None,
        )
        .await
        .context("storing the question in the player's chat")?;
        transaction.commit().await.context("asking a player")
    }

    /// The action a character has been asked about and not yet answered.
    pub async fn open_reply_action(&self, character_id: i64) -> Result<Option<i64>> {
        sqlx::query_scalar!(
            "SELECT action_id FROM pending_reply WHERE character_id = ? AND status = 'open' \
             ORDER BY deadline, action_id LIMIT 1",
            character_id,
        )
        .fetch_optional(&self.pool)
        .await
        .with_context(|| format!("loading the open reply of character {character_id}"))
    }

    /// The open reply that is due first in a world.
    pub async fn next_reply_deadline(&self, world_id: i64) -> Result<Option<ReplyDeadline>> {
        Ok(sqlx::query!(
            "SELECT action_id, character_id, \
             max(0, unixepoch(deadline) - unixepoch('now')) AS \"after_seconds!: i64\" \
             FROM pending_reply WHERE world_id = ? AND status = 'open' \
             ORDER BY deadline, action_id, character_id LIMIT 1",
            world_id,
        )
        .fetch_optional(&self.pool)
        .await
        .context("selecting the next reply deadline")?
        .map(|row| ReplyDeadline {
            action_id: row.action_id,
            character_id: row.character_id,
            after_seconds: row.after_seconds,
        }))
    }

    /// The character answered: record it, and reset their player's missed count.
    /// Returns false when the reply was not open (already settled).
    pub async fn answer_reply(
        &self,
        world_id: i64,
        action_id: i64,
        character_id: i64,
        response: &str,
    ) -> Result<bool> {
        self.settle_reply(world_id, action_id, character_id, response, false)
            .await
            .map(|missed| missed.is_some())
    }

    /// The reply's deadline passed: record do-nothing and count the miss against
    /// the character's player. Returns their consecutive misses so far, or `None`
    /// when the reply was not open or not yet due.
    pub async fn time_out_reply(
        &self,
        world_id: i64,
        action_id: i64,
        character_id: i64,
    ) -> Result<Option<i64>> {
        self.settle_reply(world_id, action_id, character_id, "does nothing", true)
            .await
    }

    async fn settle_reply(
        &self,
        world_id: i64,
        action_id: i64,
        character_id: i64,
        response: &str,
        timed_out: bool,
    ) -> Result<Option<i64>> {
        let status = if timed_out { "timed_out" } else { "answered" };
        let mut transaction = self.pool.begin().await.context("settling a reply")?;
        let settled = sqlx::query!(
            "UPDATE pending_reply SET status = ?, response = ? \
             WHERE world_id = ? AND action_id = ? AND character_id = ? AND status = 'open' \
             AND (? = 0 OR unixepoch(deadline) <= unixepoch('now'))",
            status,
            response,
            world_id,
            action_id,
            character_id,
            timed_out,
        )
        .execute(&mut *transaction)
        .await
        .context("recording a reply")?
        .rows_affected();
        if settled == 0 {
            return Ok(None);
        }
        let missed = sqlx::query_scalar!(
            "UPDATE world_member SET missed_replies = CASE WHEN ? THEN missed_replies + 1 ELSE 0 END \
             WHERE id = (SELECT member_id FROM player_character WHERE character_id = ?) \
             RETURNING missed_replies",
            timed_out,
            character_id,
        )
        .fetch_optional(&mut *transaction)
        .await
        .context("counting missed replies")?
        // An NPC has no player to count against.
        .unwrap_or(0);
        transaction.commit().await.context("settling a reply")?;
        Ok(Some(missed))
    }

    /// Whether any character is still to answer the question about this action.
    pub async fn has_open_replies(&self, world_id: i64, action_id: i64) -> Result<bool> {
        Ok(sqlx::query_scalar!(
            "SELECT count(*) FROM pending_reply WHERE world_id = ? AND action_id = ? AND status = 'open'",
            world_id,
            action_id,
        )
        .fetch_one(&self.pool)
        .await
        .context("counting open replies")?
            > 0)
    }

    /// Every settled answer to the question about this action, in the order asked.
    pub async fn replies(&self, world_id: i64, action_id: i64) -> Result<Vec<Reply>> {
        Ok(sqlx::query!(
            "SELECT character_id, status, response FROM pending_reply \
             WHERE world_id = ? AND action_id = ? AND status <> 'open' ORDER BY rowid",
            world_id,
            action_id,
        )
        .fetch_all(&self.pool)
        .await
        .context("loading replies")?
        .into_iter()
        .map(|row| Reply {
            character_id: row.character_id,
            timed_out: row.status == "timed_out",
            response: row.response.unwrap_or_default(),
        })
        .collect())
    }

    /// The reactions proposed to this action that still await the GM's decision.
    pub async fn reactions_to(&self, world_id: i64, action_id: i64) -> Result<Vec<PendingAction>> {
        sqlx::query_as!(
            PendingAction,
            "SELECT world_id, id, sequence_id, requesting_tool_execution_id, character_id,              gm_agent_id, gm_branch_id, tool, args FROM pending_action              WHERE world_id = ? AND reacts_to = ? ORDER BY id",
            world_id,
            action_id,
        )
        .fetch_all(&self.pool)
        .await
        .with_context(|| format!("loading reactions to action {action_id}"))
    }

    /// A pending action by id, while it still awaits the GM's decision.
    pub async fn pending_action(
        &self,
        world_id: i64,
        action_id: i64,
    ) -> Result<Option<PendingAction>> {
        sqlx::query_as!(
            PendingAction,
            "SELECT world_id, id, sequence_id, requesting_tool_execution_id, character_id,              gm_agent_id, gm_branch_id, tool, args FROM pending_action              WHERE world_id = ? AND id = ?",
            world_id,
            action_id,
        )
        .fetch_optional(&self.pool)
        .await
        .with_context(|| format!("loading pending action {action_id}"))
    }

    /// The turn that decides a question is the one that owns its actions: if it
    /// fails they are closed with it, and the NPC discussion that answers what
    /// they say belongs to it.
    pub async fn move_question_to_sequence(
        &self,
        world_id: i64,
        action_id: i64,
        sequence_id: i64,
    ) -> Result<()> {
        sqlx::query!(
            "UPDATE pending_action SET sequence_id = ? WHERE world_id = ? AND (id = ? OR reacts_to = ?)",
            sequence_id,
            world_id,
            action_id,
            action_id,
        )
        .execute(&self.pool)
        .await
        .with_context(|| format!("moving action {action_id} to the sequence deciding it"))?;
        Ok(())
    }
}
