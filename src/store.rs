use std::path::Path;

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use sqlx::{
    FromRow, SqlitePool,
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions},
};

use crate::llm::{Content, Message, Request, Response, Role, Sampling, Usage};

#[derive(Clone)]
pub struct Store {
    pool: SqlitePool,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(untagged)]
pub enum Segment {
    Text { text: String, role: Role },
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
        let content = serde_json::to_string(&Content::Text(message.content.clone()))
            .context("serializing message content")?;
        let result =
            sqlx::query("INSERT INTO message (agent_id, seq, role, content) VALUES (?, ?, ?, ?)")
                .bind(agent_id)
                .bind(seq)
                .bind(role)
                .bind(content)
                .execute(&mut *transaction)
                .await
                .with_context(|| format!("appending message {seq} for agent {agent_id}"))?;
        transaction
            .commit()
            .await
            .with_context(|| format!("committing message {seq} for agent {agent_id}"))?;
        Ok(result.last_insert_rowid())
    }

    pub async fn put_text(&self, content: &str) -> Result<String> {
        let hash = blake3::hash(content.as_bytes()).to_hex().to_string();
        sqlx::query("INSERT OR IGNORE INTO text (hash, content) VALUES (?, ?)")
            .bind(&hash)
            .bind(content)
            .execute(&self.pool)
            .await
            .context("storing content-addressed text")?;
        Ok(hash)
    }

    pub async fn message_segment(&self, agent_id: i64) -> Result<Option<Segment>> {
        let range = sqlx::query_as::<_, (Option<i64>, Option<i64>)>(
            "SELECT MIN(seq), MAX(seq) FROM message WHERE agent_id = ?",
        )
        .bind(agent_id)
        .fetch_one(&self.pool)
        .await
        .with_context(|| format!("finding message range for agent {agent_id}"))?;
        Ok(match range {
            (Some(first_seq), Some(last_seq)) if first_seq <= last_seq => Some(Segment::Messages {
                messages: MessageRange {
                    agent_id,
                    first_seq,
                    last_seq,
                },
            }),
            _ => None,
        })
    }

    pub async fn request_for_segments(
        &self,
        agent_id: i64,
        segments: &[Segment],
        sampling: Sampling,
    ) -> Result<Request> {
        let mut messages = Vec::new();
        for segment in segments {
            match segment {
                Segment::Text { text, role } => {
                    let content: String =
                        sqlx::query_scalar("SELECT content FROM text WHERE hash = ?")
                            .bind(text)
                            .fetch_optional(&self.pool)
                            .await
                            .context("loading content-addressed text")?
                            .with_context(|| format!("inference references missing text {text}"))?;
                    ensure!(
                        blake3::hash(content.as_bytes()).to_hex().as_str() == text,
                        "text content does not match its hash {text}"
                    );
                    messages.push(Message {
                        role: role.clone(),
                        content,
                    });
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
                        let Content::Text(content) = serde_json::from_str(&row.content)
                            .with_context(|| {
                                format!(
                                    "deserializing content for agent {} message {}",
                                    range.agent_id, row.seq
                                )
                            })?
                        else {
                            anyhow::bail!(
                                "agent {} message {} is not text content",
                                range.agent_id,
                                row.seq
                            );
                        };
                        messages.push(Message { role, content });
                    }
                }
            }
        }
        Ok(Request {
            messages,
            tools: vec![],
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

    #[cfg(test)]
    async fn inference_count(&self) -> Result<i64> {
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
                &Message {
                    role: Role::User,
                    content: "What is beneath the floorboards?".to_string(),
                },
            )
            .await
            .expect("user message should persist");
        store
            .append_message(
                agent,
                &Message {
                    role: Role::Assistant,
                    content: "A locked chest.".to_string(),
                },
            )
            .await
            .expect("assistant message should persist");
        let prompt = store
            .put_text("You are a careful guide.")
            .await
            .expect("prompt should persist");
        let segments = vec![
            Segment::Text {
                text: prompt,
                role: Role::System,
            },
            store
                .message_segment(agent)
                .await
                .expect("message range should load")
                .expect("history should have a range"),
        ];
        let request = store
            .request_for_segments(agent, &segments, Sampling { temperature: 0.7 })
            .await
            .expect("request should assemble");
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
        sqlx::query("UPDATE text SET content = 'corrupt' WHERE hash = ?")
            .bind(text)
            .execute(&store.pool)
            .await
            .unwrap();
        assert!(
            format!("{:#}", store.reconstruct_inference(id).await.unwrap_err())
                .contains("does not match")
        );

        sqlx::query("UPDATE text SET content = ? WHERE hash = ?")
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
}
