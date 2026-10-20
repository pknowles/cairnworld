use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sqlx::{Sqlite, Transaction};

use super::Store;
use crate::llm::MessageContent;

/// The body of one `recorded_content` row: the original message content and
/// reasoning, serialized together (design.md's Chat schema: "body serializes
/// the existing content enum and reasoning once").
#[derive(Serialize, Deserialize)]
struct ContentBody {
    content: MessageContent,
    reasoning: String,
}

impl Store {
    pub(super) async fn text_in(
        transaction: &mut Transaction<'_, Sqlite>,
        text: &str,
    ) -> Result<i64> {
        sqlx::query!(
            "INSERT INTO text (content) VALUES (?) ON CONFLICT(content) DO NOTHING",
            text,
        )
        .execute(&mut **transaction)
        .await
        .context("retaining original text")?;
        sqlx::query_scalar!("SELECT id FROM text WHERE content = ?", text,)
            .fetch_one(&mut **transaction)
            .await
            .context("resolving retained text")
    }

    /// Store the original content and reasoning of one event as a new
    /// `recorded_content` row. Deliberately not deduplicated: two
    /// independently produced events with equal text are distinct events
    /// (design.md's Chat schema: "No uniqueness constraint on body").
    pub(super) async fn content_in(
        transaction: &mut Transaction<'_, Sqlite>,
        content: &MessageContent,
        reasoning: &str,
    ) -> Result<i64> {
        let body = serde_json::to_string(&ContentBody {
            content: content.clone(),
            reasoning: reasoning.to_string(),
        })
        .context("encoding recorded content")?;
        let result = sqlx::query!("INSERT INTO recorded_content (body) VALUES (?)", body,)
            .execute(&mut **transaction)
            .await
            .context("storing recorded content")?;
        Ok(result.last_insert_rowid())
    }

    async fn content_body(&self, id: i64) -> Result<ContentBody> {
        let body: String =
            sqlx::query_scalar!("SELECT body FROM recorded_content WHERE id = ?", id,)
                .fetch_optional(&self.pool)
                .await
                .with_context(|| format!("loading recorded content {id}"))?
                .with_context(|| format!("recorded content {id} does not exist"))?;
        serde_json::from_str(&body).with_context(|| format!("decoding recorded content {id}"))
    }

    /// As `recorded_content`, inside an open transaction: the pool has one
    /// connection, which the transaction holds.
    pub(super) async fn content_in_transaction(
        transaction: &mut Transaction<'_, Sqlite>,
        id: i64,
    ) -> Result<MessageContent> {
        let body: String =
            sqlx::query_scalar!("SELECT body FROM recorded_content WHERE id = ?", id,)
                .fetch_optional(&mut **transaction)
                .await
                .with_context(|| format!("loading recorded content {id}"))?
                .with_context(|| format!("recorded content {id} does not exist"))?;
        let body: ContentBody = serde_json::from_str(&body)
            .with_context(|| format!("decoding recorded content {id}"))?;
        Ok(body.content)
    }

    pub(super) async fn recorded_content(&self, id: i64) -> Result<(MessageContent, String)> {
        let body = self.content_body(id).await?;
        Ok((body.content, body.reasoning))
    }
}
