use anyhow::{Context, Result, ensure};
use mistralrs::{ChatTemplateFormat, RequestLike, RequestMessage};

use super::Store;
use crate::{inspection::PromptEvidenceView, llm::Request};

/// Only formatting inputs and independent checksums are retained. The clock is
/// separate so every call can reuse the same immutable template artifact.
pub struct PromptEvidence {
    format: String,
    rendered_at: String,
    prompt_hash: String,
    tokens_hash: String,
    max_output_tokens: i64,
}

impl PromptEvidence {
    pub(crate) fn tokens_hash(tokens: &[u32]) -> String {
        let mut token_hash = blake3::Hasher::new();
        for token in tokens {
            token_hash.update(&token.to_le_bytes());
        }
        token_hash.finalize().to_hex().to_string()
    }

    pub fn new(
        format: &ChatTemplateFormat,
        prompt: &str,
        tokens: &[u32],
        max_output_tokens: usize,
    ) -> Result<Self> {
        let mut artifact =
            serde_json::to_value(format).context("serializing model formatting inputs")?;
        artifact
            .as_object_mut()
            .context("formatting artifact is not an object")?
            .remove("rendered_at");
        Ok(Self {
            format: serde_json::to_string(&artifact)
                .context("serializing immutable formatting artifact")?,
            rendered_at: format.rendered_at.clone(),
            prompt_hash: blake3::hash(prompt.as_bytes()).to_hex().to_string(),
            tokens_hash: Self::tokens_hash(tokens),
            max_output_tokens: i64::try_from(max_output_tokens)
                .context("output limit exceeds SQLite range")?,
        })
    }
}

impl Store {
    pub async fn record_prompt(&self, inference_id: i64, evidence: PromptEvidence) -> Result<()> {
        let mut transaction = self
            .pool
            .begin()
            .await
            .context("starting model prompt evidence record")?;
        let format_id = Self::text_in(&mut transaction, &evidence.format).await?;
        sqlx::query!(
            "INSERT INTO inference_prompt (inference_id, format_id, rendered_at, prompt_hash, tokens_hash, max_output_tokens) VALUES (?, ?, ?, ?, ?, ?)",
            inference_id,
            format_id,
            evidence.rendered_at,
            evidence.prompt_hash,
            evidence.tokens_hash,
            evidence.max_output_tokens,
        )
            .execute(&mut *transaction).await.with_context(|| format!("recording model prompt evidence for inference {inference_id}"))?;
        transaction
            .commit()
            .await
            .context("committing model prompt evidence")?;
        Ok(())
    }

    pub(crate) async fn prompt_evidence(
        &self,
        inference_id: i64,
    ) -> Result<Option<PromptEvidenceView>> {
        sqlx::query!(
            "SELECT format_id, rendered_at, prompt_hash, tokens_hash, max_output_tokens \
             FROM inference_prompt WHERE inference_id = ?",
            inference_id,
        )
        .fetch_optional(&self.pool)
        .await
        .with_context(|| format!("loading prompt evidence for inference {inference_id}"))?
        .map(|row| -> Result<PromptEvidenceView> {
            Ok(PromptEvidenceView {
                format_id: row.format_id,
                rendered_at: row.rendered_at,
                prompt_hash: row.prompt_hash,
                tokens_hash: row.tokens_hash,
                max_output_tokens: usize::try_from(row.max_output_tokens)
                    .context("negative recorded prompt output limit")?,
            })
        })
        .transpose()
    }

    pub(crate) async fn reconstruct_model_prompt(
        &self,
        inference_id: i64,
        request: &Request,
        evidence: Option<&PromptEvidenceView>,
    ) -> Result<Option<String>> {
        let Some(evidence) = evidence else {
            return Ok(None);
        };
        let mut artifact: serde_json::Value =
            serde_json::from_str(&self.text(evidence.format_id).await?)
                .context("decoding retained model formatting artifact")?;
        artifact
            .as_object_mut()
            .context("formatting artifact is not an object")?
            .insert("rendered_at".into(), evidence.rendered_at.clone().into());
        let format: ChatTemplateFormat =
            serde_json::from_value(artifact).context("restoring original prompt formatting")?;
        let mut builder =
            crate::mistralrs_backend::request_builder(request, evidence.max_output_tokens)?;
        let tools = builder
            .take_tools()
            .map(|(tools, _)| tools)
            .unwrap_or_default();
        let RequestMessage::Chat { messages, .. } = builder.take_messages() else {
            anyhow::bail!("inference {inference_id} is not a chat request")
        };
        let prompt = format.render(messages, tools).with_context(|| {
            format!("rendering original model prompt for inference {inference_id}")
        })?;
        ensure!(
            blake3::hash(prompt.as_bytes()).to_hex().as_str() == evidence.prompt_hash,
            "inference {inference_id} reconstructed model prompt does not match the observed generation input"
        );
        Ok(Some(prompt))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        context,
        llm::{Message, Role, Sampling},
        store::{InferenceInput, TestDatabase},
    };

    #[tokio::test]
    async fn original_clock_and_shared_template_survive_reopen_and_detect_corruption() {
        let db = TestDatabase::new("prompt-clock");
        let store = Store::open(db.path()).await.unwrap();
        let world = store.create_world("clock test").await.unwrap();
        let agent = store.create_agent(world).await.unwrap();
        let request = Request {
            messages: vec![Message::text(Role::User, "<gate> & café\nwater")],
            tools: vec![],
            sampling: Sampling::default(),
        };
        let segments = context::segments(
            &store,
            store.agent_branch(agent).await.unwrap(),
            &request.messages,
            &[],
            &[],
        )
        .await
        .unwrap();
        let invocation_id = store
            .start_invocation(None, store.agent_branch(agent).await.unwrap(), None, None)
            .await
            .unwrap();
        let mut ids = Vec::new();
        for day in [8, 9] {
            let format: ChatTemplateFormat = serde_json::from_value(serde_json::json!({
                "template": "{{ strftime_now('%Y-%m-%d') }}|{{ messages[0]['content'] }}",
                "bos_tok": null, "eos_tok": null, "unk_tok": null,
                "add_generation_prompt": true, "enable_thinking": false, "reasoning_effort": null,
                "rendered_at": format!("2026-09-{day:02}T12:34:56-07:00")
            }))
            .unwrap();
            let id = store
                .start_inference(InferenceInput {
                    agent_id: agent,
                    sequence_id: None,
                    invocation_id,
                    segments: &segments,
                    request: &request,
                    model: "test",
                })
                .await
                .unwrap();
            let expected = format!("2026-09-{day:02}|<gate> & café\nwater");
            store
                .record_prompt(
                    id,
                    PromptEvidence::new(&format, &expected, &[10, 20], 16).unwrap(),
                )
                .await
                .unwrap();
            ids.push((id, expected));
        }
        let artifacts: i64 =
            sqlx::query_scalar("SELECT COUNT(DISTINCT format_id) FROM inference_prompt")
                .fetch_one(&store.pool)
                .await
                .unwrap();
        assert_eq!(artifacts, 1, "per-call clocks must not duplicate templates");
        let reopened = Store::open(db.path()).await.unwrap();
        for (id, expected) in &ids {
            let inference = reopened.reconstruct_inference(*id).await.unwrap();
            assert_eq!(inference.model_prompt.as_ref(), Some(expected));
            let evidence = inference
                .prompt_evidence
                .expect("recorded prompt must expose its evidence");
            assert!(!evidence.prompt_hash.is_empty());
            assert!(!evidence.tokens_hash.is_empty());
            assert_eq!(evidence.max_output_tokens, 16);
        }
        sqlx::query("UPDATE inference_prompt SET rendered_at = '2030-01-01T00:00:00Z' WHERE inference_id = ?")
            .bind(ids[0].0).execute(&reopened.pool).await.unwrap();
        assert!(
            format!(
                "{:#}",
                reopened.reconstruct_inference(ids[0].0).await.unwrap_err()
            )
            .contains("does not match the observed generation input")
        );
        assert_eq!(
            reopened
                .reconstruct_inference(ids[1].0)
                .await
                .unwrap()
                .model_prompt
                .as_ref(),
            Some(&ids[1].1)
        );
    }
}
