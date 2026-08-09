use std::path::Path;

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use sqlx::{
    FromRow, SqlitePool,
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions},
};

use crate::llm::{
    Message, MessageContent, Request, Response, Role, Sampling, ToolDefinition, Usage,
};

#[derive(Clone)]
pub struct Store {
    pool: SqlitePool,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(untagged)]
pub enum Segment {
    Text { text: i64, role: Role },
    Tools { text: i64 },
    Summary { summary: i64 },
    Messages { messages: MessageRange },
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct MessageRange {
    pub agent_id: i64,
    pub first_seq: i64,
    pub last_seq: i64,
}

#[derive(Debug)]
pub enum InferenceOutcome {
    Response(Response),
    Error(String),
}

#[derive(Debug, PartialEq)]
pub enum RecordedOutcome {
    Response(Response),
    Error(String),
}

#[derive(Debug, PartialEq)]
pub struct RecordedInference {
    pub id: i64,
    pub agent_id: i64,
    pub segments: Vec<Segment>,
    pub request: Request,
    pub outcome: RecordedOutcome,
    pub model: String,
    pub duration_ms: u64,
}

#[derive(Debug, FromRow)]
struct InferenceRow {
    id: i64,
    agent_id: i64,
    segments: String,
    sampling: String,
    output: Option<String>,
    error: Option<String>,
    input_hash: String,
    input_tokens: Option<i64>,
    output_tokens: Option<i64>,
    duration_ms: i64,
    model: String,
}

#[derive(Debug, FromRow)]
struct MessageRow {
    seq: i64,
    role: String,
    content: String,
}

#[derive(Debug, FromRow)]
struct SummaryRow {
    id: i64,
    agent_id: i64,
    covers_to_seq: i64,
    content: String,
    inference_id: i64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Summary {
    pub id: i64,
    pub agent_id: i64,
    pub covers_to_seq: i64,
    pub content: String,
    pub inference_id: i64,
}

impl Store {
    pub async fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating database directory {}", parent.display()))?;
        }
        let options = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            .foreign_keys(true)
            .journal_mode(SqliteJournalMode::Wal);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .with_context(|| format!("opening SQLite database {}", path.display()))?;
        sqlx::migrate!("./migrations")
            .run(&pool)
            .await
            .context("running database migrations")?;
        Ok(Self { pool })
    }

    pub async fn create_world(&self, name: &str) -> Result<i64> {
        let result = sqlx::query("INSERT INTO world (name) VALUES (?)")
            .bind(name)
            .execute(&self.pool)
            .await
            .context("creating world")?;
        Ok(result.last_insert_rowid())
    }

    pub async fn create_agent(&self, world_id: i64, kind: &str, name: &str) -> Result<i64> {
        let result = sqlx::query("INSERT INTO agent (world_id, kind, name) VALUES (?, ?, ?)")
            .bind(world_id)
            .bind(kind)
            .bind(name)
            .execute(&self.pool)
            .await
            .with_context(|| format!("creating agent {name} in world {world_id}"))?;
        Ok(result.last_insert_rowid())
    }

    pub async fn append_message(&self, agent_id: i64, message: &Message) -> Result<i64> {
        let mut transaction = self
            .pool
            .begin()
            .await
            .with_context(|| format!("starting message transaction for agent {agent_id}"))?;
        let seq: i64 =
            sqlx::query_scalar("SELECT COALESCE(MAX(seq) + 1, 0) FROM message WHERE agent_id = ?")
                .bind(agent_id)
                .fetch_one(&mut *transaction)
                .await
                .with_context(|| format!("finding next message sequence for agent {agent_id}"))?;
        let role = serde_json::to_string(&message.role).context("serializing message role")?;
        let content =
            serde_json::to_string(&message.content).context("serializing message content")?;
        let result = sqlx::query(
            "INSERT INTO message (agent_id, seq, role, content, reasoning) VALUES (?, ?, ?, ?, ?)",
        )
        .bind(agent_id)
        .bind(seq)
        .bind(role)
        .bind(content)
        .bind(&message.reasoning)
        .execute(&mut *transaction)
        .await
        .with_context(|| format!("appending message {seq} for agent {agent_id}"))?;
        transaction
            .commit()
            .await
            .with_context(|| format!("committing message {seq} for agent {agent_id}"))?;
        Ok(result.last_insert_rowid())
    }

    /// Store an inline chat event without making it part of model context.
    pub async fn store_chat_notice(
        &self,
        agent_id: i64,
        after_message_id: i64,
        content: &str,
    ) -> Result<i64> {
        sqlx::query_scalar(
            "INSERT INTO chat_notice (agent_id, after_message_id, content) \
             SELECT ?, id, ? FROM message WHERE id = ? AND agent_id = ? RETURNING id",
        )
        .bind(agent_id)
        .bind(content)
        .bind(after_message_id)
        .bind(agent_id)
        .fetch_one(&self.pool)
        .await
        .with_context(|| format!("storing chat notice after message {after_message_id}"))
    }

    #[cfg(test)]
    pub async fn chat_notice_contents(&self, agent_id: i64) -> Result<Vec<String>> {
        sqlx::query_scalar("SELECT content FROM chat_notice WHERE agent_id = ? ORDER BY id")
            .bind(agent_id)
            .fetch_all(&self.pool)
            .await
            .with_context(|| format!("loading chat notices for agent {agent_id}"))
    }

    /// Store one static prompt piece and return the id a recipe refers to.
    /// Rows are never updated, so the reference stays true to what was sent.
    pub async fn store_prompt_text(&self, content: &str) -> Result<i64> {
        let result = sqlx::query("INSERT INTO text (content) VALUES (?)")
            .bind(content)
            .execute(&self.pool)
            .await
            .context("storing prompt text")?;
        Ok(result.last_insert_rowid())
    }

    async fn text(&self, id: i64) -> Result<String> {
        sqlx::query_scalar("SELECT content FROM text WHERE id = ?")
            .bind(id)
            .fetch_optional(&self.pool)
            .await
            .context("loading prompt text")?
            .with_context(|| format!("inference references missing text {id}"))
    }

    pub async fn store_summary(
        &self,
        agent_id: i64,
        covers_to_seq: i64,
        content: &str,
        inference_id: i64,
    ) -> Result<i64> {
        let result = sqlx::query(
            "INSERT INTO summary (agent_id, covers_to_seq, content, inference_id) VALUES (?, ?, ?, ?)",
        )
        .bind(agent_id)
        .bind(covers_to_seq)
        .bind(content)
        .bind(inference_id)
        .execute(&self.pool)
        .await
        .with_context(|| format!("storing summary through message {covers_to_seq} for agent {agent_id}"))?;
        Ok(result.last_insert_rowid())
    }

    pub async fn latest_summary(&self, agent_id: i64) -> Result<Option<Summary>> {
        let row = sqlx::query_as::<_, SummaryRow>(
            "SELECT id, agent_id, covers_to_seq, content, inference_id FROM summary \
             WHERE agent_id = ? ORDER BY covers_to_seq DESC, id DESC LIMIT 1",
        )
        .bind(agent_id)
        .fetch_optional(&self.pool)
        .await
        .with_context(|| format!("loading latest summary for agent {agent_id}"))?;
        Ok(row.map(|row| Summary {
            id: row.id,
            agent_id: row.agent_id,
            covers_to_seq: row.covers_to_seq,
            content: row.content,
            inference_id: row.inference_id,
        }))
    }

    /// The live history is the newest summary followed by every raw message it
    /// does not cover. Older messages remain stored for replay and debugging.
    pub async fn history_segments(&self, agent_id: i64) -> Result<Vec<Segment>> {
        let summary = self.latest_summary(agent_id).await?;
        let first_seq = summary
            .as_ref()
            .map_or(0, |summary| summary.covers_to_seq + 1);
        let last_seq: Option<i64> =
            sqlx::query_scalar("SELECT MAX(seq) FROM message WHERE agent_id = ? AND seq >= ?")
                .bind(agent_id)
                .bind(first_seq)
                .fetch_one(&self.pool)
                .await
                .with_context(|| format!("finding live message range for agent {agent_id}"))?;
        let mut segments = summary
            .as_ref()
            .map(|summary| {
                vec![Segment::Summary {
                    summary: summary.id,
                }]
            })
            .unwrap_or_default();
        if let Some(last_seq) = last_seq {
            segments.push(Segment::Messages {
                messages: MessageRange {
                    agent_id,
                    first_seq,
                    last_seq,
                },
            });
        }
        Ok(segments)
    }

    /// Find the newest raw suffix containing exactly `keep_tail_messages` rows,
    /// or every available row when there are fewer.
    pub async fn tail_split(&self, agent_id: i64, keep_tail_messages: usize) -> Result<i64> {
        let covered = self
            .latest_summary(agent_id)
            .await?
            .map_or(-1, |summary| summary.covers_to_seq);
        let rows = sqlx::query_as::<_, (i64,)>(
            "SELECT seq FROM message WHERE agent_id = ? AND seq > ? ORDER BY seq DESC LIMIT ?",
        )
        .bind(agent_id)
        .bind(covered)
        .bind(keep_tail_messages as i64)
        .fetch_all(&self.pool)
        .await
        .with_context(|| format!("loading raw tail for agent {agent_id}"))?;
        Ok(rows.last().map_or(covered, |(seq,)| seq - 1))
    }

    pub async fn request_for_segments(
        &self,
        agent_id: i64,
        segments: &[Segment],
        sampling: Sampling,
    ) -> Result<Request> {
        let mut messages = Vec::new();
        let mut tools = Vec::new();
        for segment in segments {
            match segment {
                Segment::Text { text, role } => {
                    messages.push(Message::text(role.clone(), self.text(*text).await?));
                }
                Segment::Tools { text } => {
                    let definitions: Vec<ToolDefinition> =
                        serde_json::from_str(&self.text(*text).await?)
                            .context("deserializing tool definitions")?;
                    tools.extend(definitions);
                }
                Segment::Summary { summary } => {
                    let row = sqlx::query_as::<_, SummaryRow>(
                        "SELECT id, agent_id, covers_to_seq, content, inference_id FROM summary WHERE id = ?",
                    )
                    .bind(summary)
                    .fetch_optional(&self.pool)
                    .await
                    .context("loading summary")?
                    .with_context(|| format!("inference references missing summary {summary}"))?;
                    ensure!(
                        row.agent_id == agent_id,
                        "inference for agent {agent_id} references summary {} from agent {}",
                        row.id,
                        row.agent_id
                    );
                    messages.push(Message::text(Role::System, row.content));
                }
                Segment::Messages { messages: range } => {
                    ensure!(
                        range.agent_id == agent_id,
                        "inference for agent {agent_id} references messages from agent {}",
                        range.agent_id
                    );
                    ensure!(
                        range.first_seq <= range.last_seq,
                        "message range {}..={} is invalid",
                        range.first_seq,
                        range.last_seq
                    );
                    let rows = sqlx::query_as::<_, MessageRow>(
                        "SELECT seq, role, content FROM message \
                         WHERE agent_id = ? AND seq BETWEEN ? AND ? ORDER BY seq",
                    )
                    .bind(range.agent_id)
                    .bind(range.first_seq)
                    .bind(range.last_seq)
                    .fetch_all(&self.pool)
                    .await
                    .with_context(|| {
                        format!(
                            "loading messages {}..={} for agent {}",
                            range.first_seq, range.last_seq, range.agent_id
                        )
                    })?;
                    let expected = usize::try_from(range.last_seq - range.first_seq + 1)
                        .context("message range is too large")?;
                    ensure!(
                        rows.len() == expected,
                        "message range {}..={} for agent {} has missing rows",
                        range.first_seq,
                        range.last_seq,
                        range.agent_id
                    );
                    for (offset, row) in rows.into_iter().enumerate() {
                        let expected_seq = range.first_seq
                            + i64::try_from(offset)
                                .context("message range offset exceeds SQLite range")?;
                        ensure!(
                            row.seq == expected_seq,
                            "message range for agent {} is out of order at sequence {}",
                            range.agent_id,
                            expected_seq
                        );
                        let role = serde_json::from_str(&row.role).with_context(|| {
                            format!(
                                "deserializing role for agent {} message {}",
                                range.agent_id, row.seq
                            )
                        })?;
                        let content: MessageContent = serde_json::from_str(&row.content)
                            .with_context(|| {
                                format!(
                                    "deserializing content for agent {} message {}",
                                    range.agent_id, row.seq
                                )
                            })?;
                        messages.push(Message {
                            role,
                            content,
                            reasoning: String::new(),
                        });
                    }
                }
            }
        }
        Ok(Request {
            messages,
            tools,
            sampling,
        })
    }

    pub async fn record_inference(
        &self,
        agent_id: i64,
        segments: &[Segment],
        request: &Request,
        outcome: InferenceOutcome,
        model: &str,
        duration_ms: u64,
    ) -> Result<i64> {
        let segments = serde_json::to_string(segments).context("serializing inference segments")?;
        let sampling =
            serde_json::to_string(&request.sampling).context("serializing inference sampling")?;
        let input =
            serde_json::to_vec(request).context("serializing assembled inference request")?;
        let input_hash = blake3::hash(&input).to_hex().to_string();
        let duration_ms =
            i64::try_from(duration_ms).context("inference duration exceeds SQLite range")?;
        let (output, error, input_tokens, output_tokens) = match outcome {
            InferenceOutcome::Response(response) => (
                Some(serde_json::to_string(&response).context("serializing inference output")?),
                None,
                Some(
                    i64::try_from(response.usage.input_tokens)
                        .context("input token count exceeds SQLite range")?,
                ),
                Some(
                    i64::try_from(response.usage.output_tokens)
                        .context("output token count exceeds SQLite range")?,
                ),
            ),
            InferenceOutcome::Error(error) => (None, Some(error), None, None),
        };
        let result = sqlx::query(
            "INSERT INTO inference \
             (agent_id, segments, sampling, output, error, input_hash, input_tokens, output_tokens, duration_ms, model) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(agent_id)
        .bind(segments)
        .bind(sampling)
        .bind(output)
        .bind(error)
        .bind(input_hash)
        .bind(input_tokens)
        .bind(output_tokens)
        .bind(duration_ms)
        .bind(model)
        .execute(&self.pool)
        .await
        .context("storing inference record")?;
        Ok(result.last_insert_rowid())
    }

    pub async fn reconstruct_inference(&self, id: i64) -> Result<RecordedInference> {
        let row = sqlx::query_as::<_, InferenceRow>(
            "SELECT id, agent_id, segments, sampling, output, error, input_hash, input_tokens, output_tokens, duration_ms, model \
             FROM inference WHERE id = ?",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .with_context(|| format!("loading inference {id}"))?
        .with_context(|| format!("inference {id} does not exist"))?;
        let segments: Vec<Segment> =
            serde_json::from_str(&row.segments).context("deserializing inference segments")?;
        let sampling: Sampling =
            serde_json::from_str(&row.sampling).context("deserializing recorded sampling")?;
        let request = self
            .request_for_segments(row.agent_id, &segments, sampling)
            .await
            .with_context(|| format!("reassembling inference {} from its recipe", row.id))?;
        let input =
            serde_json::to_vec(&request).context("serializing reconstructed inference request")?;
        ensure!(
            blake3::hash(&input).to_hex().as_str() == row.input_hash,
            "inference {} reconstructed input does not match its hash",
            row.id
        );
        let outcome = match (row.output, row.error, row.input_tokens, row.output_tokens) {
            (Some(output), None, Some(input_tokens), Some(output_tokens)) => {
                let response: Response = serde_json::from_str(&output)
                    .context("deserializing recorded inference output")?;
                ensure!(
                    response.usage
                        == Usage {
                            input_tokens: usize::try_from(input_tokens)
                                .context("recorded input token count is negative")?,
                            output_tokens: usize::try_from(output_tokens)
                                .context("recorded output token count is negative")?,
                        },
                    "inference {} usage columns differ from output",
                    row.id
                );
                RecordedOutcome::Response(response)
            }
            (None, Some(error), None, None) => RecordedOutcome::Error(error),
            _ => anyhow::bail!(
                "inference {} has an invalid success or failure outcome",
                row.id
            ),
        };
        Ok(RecordedInference {
            id: row.id,
            agent_id: row.agent_id,
            segments,
            request,
            outcome,
            model: row.model,
            duration_ms: u64::try_from(row.duration_ms)
                .context("recorded inference duration is negative")?,
        })
    }

    /// Rewrite a prompt row that should never change, so tests can prove
    /// tampering is detected rather than silently reconstructed.
    #[cfg(test)]
    pub(crate) async fn corrupt_text_for_test(&self, containing: &str) {
        let rows = sqlx::query("UPDATE text SET content = '[]' WHERE content LIKE ?")
            .bind(format!("%{containing}%"))
            .execute(&self.pool)
            .await
            .expect("corrupting text should succeed")
            .rows_affected();
        assert_eq!(
            rows, 1,
            "expected exactly one text row matching {containing}"
        );
    }

    #[cfg(test)]
    pub(crate) async fn inference_count(&self) -> Result<i64> {
        sqlx::query_scalar("SELECT COUNT(*) FROM inference")
            .fetch_one(&self.pool)
            .await
            .context("counting inference rows")
    }
}

#[cfg(test)]
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;
    use crate::llm::Content;

    fn database_path() -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "cairnworld-store-test-{}-{}.sqlite",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system clock before Unix epoch")
                .as_nanos()
        ))
    }

    fn response() -> Response {
        Response {
            content: Content::Text("A locked chest.".to_string()),
            reasoning: String::new(),
            usage: Usage {
                input_tokens: 12,
                output_tokens: 5,
            },
        }
    }

    async fn store_with_history() -> (Store, std::path::PathBuf, i64, Vec<Segment>, Request) {
        let path = database_path();
        let store = Store::open(&path).await.expect("store should open");
        let world = store
            .create_world("test world")
            .await
            .expect("world should persist");
        let agent = store
            .create_agent(world, "sandbox", "test agent")
            .await
            .expect("agent should persist");
        store
            .append_message(
                agent,
                &Message::text(Role::User, "What is beneath the floorboards?"),
            )
            .await
            .expect("user message should persist");
        store
            .append_message(
                agent,
                &Message::assistant(
                    Content::Text("A locked chest.".to_string()),
                    "scratch work".to_string(),
                ),
            )
            .await
            .expect("assistant message should persist");
        let prompt = store
            .store_prompt_text("You are a careful guide.")
            .await
            .expect("prompt should persist");
        let segments = vec![
            Segment::Text {
                text: prompt,
                role: Role::System,
            },
            store.history_segments(agent).await.unwrap().pop().unwrap(),
        ];
        let request = store
            .request_for_segments(
                agent,
                &segments,
                Sampling {
                    temperature: 0.7,
                    enable_thinking: false,
                },
            )
            .await
            .expect("request should assemble");
        let stored_reasoning: String =
            sqlx::query_scalar("SELECT reasoning FROM message WHERE agent_id = ? AND seq = 1")
                .bind(agent)
                .fetch_one(&store.pool)
                .await
                .expect("assistant reasoning should persist");
        assert_eq!(stored_reasoning, "scratch work");
        assert!(request.messages[1].reasoning.is_empty());
        (store, path, agent, segments, request)
    }

    #[tokio::test]
    async fn reconstructs_history_recipe_without_copying_messages() {
        let (store, path, agent, segments, request) = store_with_history().await;
        let first = store
            .record_inference(
                agent,
                &segments,
                &request,
                InferenceOutcome::Response(response()),
                "test-model",
                14,
            )
            .await
            .expect("record should persist");
        let second = store
            .record_inference(
                agent,
                &segments,
                &request,
                InferenceOutcome::Response(response()),
                "test-model",
                15,
            )
            .await
            .expect("repeat record should persist");

        let recorded = store
            .reconstruct_inference(first)
            .await
            .expect("recorded request should reconstruct");
        assert_eq!(recorded.request, request);
        assert_eq!(recorded.outcome, RecordedOutcome::Response(response()));
        assert_eq!(store.inference_count().await.unwrap(), 2);
        assert_ne!(first, second);
        let messages: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM message")
            .fetch_one(&store.pool)
            .await
            .unwrap();
        assert_eq!(messages, 2);
        drop(store);
        std::fs::remove_file(path).expect("test database should be removable");
    }

    #[tokio::test]
    async fn reconstruction_rejects_corrupt_or_invalid_references() {
        let (store, path, agent, segments, request) = store_with_history().await;
        let id = store
            .record_inference(
                agent,
                &segments,
                &request,
                InferenceOutcome::Response(response()),
                "test-model",
                14,
            )
            .await
            .expect("record should persist");
        let text = match &segments[0] {
            Segment::Text { text, .. } => text,
            _ => unreachable!(),
        };
        // Prompt rows are written once and never updated. Rewriting one is
        // therefore corruption, and `input_hash` over the whole reassembled
        // request is what catches it.
        sqlx::query("UPDATE text SET content = 'corrupt' WHERE id = ?")
            .bind(text)
            .execute(&store.pool)
            .await
            .unwrap();
        assert!(
            format!("{:#}", store.reconstruct_inference(id).await.unwrap_err())
                .contains("does not match")
        );

        sqlx::query("UPDATE text SET content = ? WHERE id = ?")
            .bind("You are a careful guide.")
            .bind(text)
            .execute(&store.pool)
            .await
            .unwrap();
        sqlx::query("DELETE FROM message WHERE agent_id = ? AND seq = 1")
            .bind(agent)
            .execute(&store.pool)
            .await
            .unwrap();
        assert!(
            format!("{:#}", store.reconstruct_inference(id).await.unwrap_err())
                .contains("missing rows")
        );
        drop(store);
        std::fs::remove_file(path).expect("test database should be removable");
    }

    #[tokio::test]
    async fn reconstruction_rejects_cross_agent_references_and_preserves_failures() {
        let (store, path, agent, mut segments, request) = store_with_history().await;
        let world: i64 = sqlx::query_scalar("SELECT world_id FROM agent WHERE id = ?")
            .bind(agent)
            .fetch_one(&store.pool)
            .await
            .unwrap();
        let other = store
            .create_agent(world, "sandbox", "other agent")
            .await
            .unwrap();
        if let Segment::Messages { messages } = &mut segments[1] {
            messages.agent_id = other;
        }
        let id = store
            .record_inference(
                agent,
                &segments,
                &request,
                InferenceOutcome::Error("backend disconnected".to_string()),
                "test-model",
                14,
            )
            .await
            .expect("failed inference should persist");
        assert!(
            format!("{:#}", store.reconstruct_inference(id).await.unwrap_err())
                .contains("references messages from agent")
        );

        if let Segment::Messages { messages } = &mut segments[1] {
            messages.agent_id = agent;
        }
        sqlx::query("UPDATE inference SET segments = ? WHERE id = ?")
            .bind(serde_json::to_string(&segments).unwrap())
            .bind(id)
            .execute(&store.pool)
            .await
            .unwrap();
        let recorded = store.reconstruct_inference(id).await.unwrap();
        assert_eq!(
            recorded.outcome,
            RecordedOutcome::Error("backend disconnected".to_string())
        );
        drop(store);
        std::fs::remove_file(path).expect("test database should be removable");
    }

    #[tokio::test]
    async fn latest_summary_replaces_only_the_history_it_covers() {
        let (store, path, agent, segments, request) = store_with_history().await;
        let inference = store
            .record_inference(
                agent,
                &segments,
                &request,
                InferenceOutcome::Response(response()),
                "test-model",
                14,
            )
            .await
            .unwrap();
        let summary = store
            .store_summary(agent, 0, "The floorboards hide a locked chest.", inference)
            .await
            .unwrap();
        let history = store.history_segments(agent).await.unwrap();
        assert!(matches!(
            history.as_slice(),
            [
                Segment::Summary { summary: stored },
                Segment::Messages { messages: MessageRange { first_seq: 1, last_seq: 1, .. } },
            ] if *stored == summary
        ));
        let assembled = store
            .request_for_segments(
                agent,
                &history,
                Sampling {
                    temperature: 0.7,
                    enable_thinking: false,
                },
            )
            .await
            .unwrap();
        assert!(matches!(assembled.messages.as_slice(), [
            Message { role: Role::System, content: MessageContent::Text(summary), .. },
            Message { role: Role::Assistant, .. },
        ] if summary == "The floorboards hide a locked chest."));
        drop(store);
        std::fs::remove_file(path).unwrap();
    }

    #[tokio::test]
    async fn summary_recipe_reconstructs_and_rejects_cross_agent_summary() {
        let (store, path, agent, segments, request) = store_with_history().await;
        let source = store
            .record_inference(
                agent,
                &segments,
                &request,
                InferenceOutcome::Response(response()),
                "test-model",
                14,
            )
            .await
            .unwrap();
        let summary = store
            .store_summary(agent, 0, "Earlier events.", source)
            .await
            .unwrap();
        let recipe = vec![Segment::Summary { summary }];
        let summary_request = store
            .request_for_segments(
                agent,
                &recipe,
                Sampling {
                    temperature: 0.7,
                    enable_thinking: false,
                },
            )
            .await
            .unwrap();
        let recorded = store
            .record_inference(
                agent,
                &recipe,
                &summary_request,
                InferenceOutcome::Response(response()),
                "test-model",
                14,
            )
            .await
            .unwrap();
        assert_eq!(
            store.reconstruct_inference(recorded).await.unwrap().request,
            summary_request
        );

        let world: i64 = sqlx::query_scalar("SELECT world_id FROM agent WHERE id = ?")
            .bind(agent)
            .fetch_one(&store.pool)
            .await
            .unwrap();
        let other = store.create_agent(world, "sandbox", "other").await.unwrap();
        let error = store
            .request_for_segments(
                other,
                &recipe,
                Sampling {
                    temperature: 0.7,
                    enable_thinking: false,
                },
            )
            .await
            .unwrap_err();
        assert!(format!("{error:#}").contains("references summary"));
        drop(store);
        std::fs::remove_file(path).unwrap();
    }
}
