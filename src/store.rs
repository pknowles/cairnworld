use std::path::Path;

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use sqlx::{
    FromRow, SqlitePool,
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions},
};

use crate::llm::{Request, Response, Sampling, Usage};

#[derive(Clone)]
pub struct Store {
    pool: SqlitePool,
}

#[derive(Debug, Deserialize, Serialize)]
struct Segment {
    text: String,
}

#[derive(Debug, FromRow)]
struct InferenceRow {
    id: i64,
    segments: String,
    sampling: String,
    output: String,
    input_hash: String,
    input_tokens: i64,
    output_tokens: i64,
    duration_ms: i64,
    model: String,
}

#[derive(Debug, PartialEq)]
pub struct RecordedInference {
    pub id: i64,
    pub request: Request,
    pub response: Response,
    pub model: String,
    pub duration_ms: u64,
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

    pub async fn record(
        &self,
        request: &Request,
        response: &Response,
        model: &str,
        duration_ms: u64,
    ) -> Result<i64> {
        let input = serde_json::to_string(request).context("serializing inference request")?;
        let input_hash = blake3::hash(input.as_bytes()).to_hex().to_string();
        let segments = serde_json::to_string(&[Segment {
            text: input_hash.clone(),
        }])
        .context("serializing inference segments")?;
        let sampling =
            serde_json::to_string(&request.sampling).context("serializing inference sampling")?;
        let output = serde_json::to_string(response).context("serializing inference output")?;
        let duration_ms =
            i64::try_from(duration_ms).context("inference duration exceeds SQLite range")?;
        let input_tokens = i64::try_from(response.usage.input_tokens)
            .context("input token count exceeds SQLite range")?;
        let output_tokens = i64::try_from(response.usage.output_tokens)
            .context("output token count exceeds SQLite range")?;

        let mut transaction = self
            .pool
            .begin()
            .await
            .context("starting inference record transaction")?;
        sqlx::query("INSERT OR IGNORE INTO text (hash, content) VALUES (?, ?)")
            .bind(&input_hash)
            .bind(&input)
            .execute(&mut *transaction)
            .await
            .context("storing content-addressed inference input")?;
        let result = sqlx::query(
            "INSERT INTO inference \
             (segments, sampling, output, input_hash, input_tokens, output_tokens, duration_ms, model) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(segments)
        .bind(sampling)
        .bind(output)
        .bind(input_hash)
        .bind(input_tokens)
        .bind(output_tokens)
        .bind(duration_ms)
        .bind(model)
        .execute(&mut *transaction)
        .await
        .context("storing inference record")?;
        transaction
            .commit()
            .await
            .context("committing inference record")?;
        Ok(result.last_insert_rowid())
    }

    pub async fn reconstruct_inference(&self, id: i64) -> Result<RecordedInference> {
        let row = sqlx::query_as::<_, InferenceRow>(
            "SELECT id, segments, sampling, output, input_hash, input_tokens, output_tokens, duration_ms, model \
             FROM inference WHERE id = ?",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .with_context(|| format!("loading inference {id}"))?
        .with_context(|| format!("inference {id} does not exist"))?;
        let segments: Vec<Segment> =
            serde_json::from_str(&row.segments).context("deserializing inference segments")?;
        ensure!(
            segments.len() == 1,
            "inference {} has unsupported segment count",
            row.id
        );
        let hash = &segments[0].text;
        let content: String = sqlx::query_scalar("SELECT content FROM text WHERE hash = ?")
            .bind(hash)
            .fetch_optional(&self.pool)
            .await
            .context("loading content-addressed inference input")?
            .with_context(|| format!("inference {} references missing text {hash}", row.id))?;
        ensure!(
            blake3::hash(content.as_bytes()).to_hex().as_str() == hash,
            "inference {} text content does not match its hash",
            row.id
        );
        ensure!(
            hash == &row.input_hash,
            "inference {} input hash differs from its segment",
            row.id
        );
        let request: Request = serde_json::from_str(&content)
            .context("deserializing reconstructed inference request")?;
        let sampling: Sampling =
            serde_json::from_str(&row.sampling).context("deserializing recorded sampling")?;
        ensure!(
            request.sampling == sampling,
            "inference {} sampling differs from request",
            row.id
        );
        let response: Response =
            serde_json::from_str(&row.output).context("deserializing recorded inference output")?;
        ensure!(
            response.usage
                == Usage {
                    input_tokens: usize::try_from(row.input_tokens)
                        .context("recorded input token count is negative")?,
                    output_tokens: usize::try_from(row.output_tokens)
                        .context("recorded output token count is negative")?,
                },
            "inference {} usage columns differ from output",
            row.id
        );
        Ok(RecordedInference {
            id: row.id,
            request,
            response,
            model: row.model,
            duration_ms: u64::try_from(row.duration_ms)
                .context("recorded inference duration is negative")?,
        })
    }

    #[cfg(test)]
    pub async fn close(self) {
        self.pool.close().await;
    }
}

#[cfg(test)]
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;
    use crate::llm::{Content, Message, Role};

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

    fn request() -> Request {
        Request {
            messages: vec![Message {
                role: Role::User,
                content: "What is beneath the floorboards?".to_string(),
            }],
            tools: vec![],
            sampling: Sampling { temperature: 0.7 },
        }
    }

    #[tokio::test]
    async fn reconstructs_recorded_request_and_rejects_corrupt_text() {
        let path = database_path();
        let store = Store::open(&path).await.expect("store should open");
        let request = request();
        let response = Response {
            content: Content::Text("A locked chest.".to_string()),
            usage: Usage {
                input_tokens: 12,
                output_tokens: 5,
            },
        };
        let id = store
            .record(&request, &response, "test-model", 14)
            .await
            .expect("record should persist");

        let recorded = store
            .reconstruct_inference(id)
            .await
            .expect("record should reconstruct");
        assert_eq!(recorded.request, request);
        assert_eq!(recorded.response, response);
        assert_eq!(recorded.model, "test-model");
        assert_eq!(recorded.duration_ms, 14);

        sqlx::query("UPDATE text SET content = 'corrupt' WHERE hash = ?")
            .bind(
                blake3::hash(serde_json::to_string(&request).unwrap().as_bytes())
                    .to_hex()
                    .to_string(),
            )
            .execute(&store.pool)
            .await
            .expect("test should corrupt stored text");
        let error = store
            .reconstruct_inference(id)
            .await
            .expect_err("corrupt text must fail reconstruction");
        assert!(error.to_string().contains("does not match its hash"));
        store.pool.close().await;
        std::fs::remove_file(path).expect("test database should be removable");
    }
}
