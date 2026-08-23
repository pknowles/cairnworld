use anyhow::{Context, Result, ensure};

use crate::{
    context,
    llm::{Backend, Content, Role},
    settings::Limits,
    store::{MessageRange, PendingCompaction, Segment, Store},
};

#[cfg(test)]
use crate::llm::Sampling;

/// This request only sees the material it replaces. Static role context and
/// tools are deliberately absent: they are supplied to every normal inference
/// and would waste both summary space and model attention.
const PROMPT: &str = "Summarize this earlier chat for its next model context. Retain durable facts, decisions, unresolved questions, and commitments needed later. Omit transient discussion and anything supplied separately by the agent's standing instructions or tools.";

/// Compact once after a completed turn when the complete live request reaches
/// the configured context limit. No message is deleted; the new summary simply
/// becomes the first selected history segment on the next turn.
#[cfg(test)]
#[allow(clippy::too_many_arguments)]
pub async fn after_turn<B: Backend>(
    store: &Store,
    backend: &B,
    agent_id: i64,
    after_message_id: i64,
    input_tokens: usize,
    sampling: Sampling,
    model: &str,
    limits: Limits,
) -> Result<()> {
    if input_tokens < limits.compact_at_input_tokens {
        return Ok(());
    }
    let job = PendingCompaction {
        agent_id,
        after_message_id,
        input_tokens,
        sampling,
        model: model.to_string(),
    };
    store.enqueue_compaction_for_test(&job).await?;
    run(store, backend, &job, limits).await
}

/// Resolve one persisted compaction obligation. Its summary and developer
/// notice become visible in the same transaction that retires the job.
pub async fn run<B: Backend>(
    store: &Store,
    backend: &B,
    job: &PendingCompaction,
    limits: Limits,
) -> Result<()> {
    ensure!(
        limits.keep_tail_messages > 0,
        "limits.keep_tail_messages must be greater than zero"
    );

    let previous = store.latest_summary(job.agent_id).await?;
    let covered = previous
        .as_ref()
        .map_or(-1, |summary| summary.covers_to_seq);
    let split = store
        .tail_split(job.agent_id, limits.keep_tail_messages)
        .await?;
    if split <= covered {
        return store
            .finish_compaction(
                job,
                None,
                &format!(
                    "Context used {} input tokens; no history older than the retained {} messages was eligible for compaction.",
                    job.input_tokens, limits.keep_tail_messages
                ),
            )
            .await;
    }

    let prompt = store
        .store_prompt_text(PROMPT)
        .await
        .context("storing compaction prompt")?;
    let mut segments = vec![Segment::Text {
        text: prompt,
        role: Role::System,
    }];
    if let Some(summary) = previous {
        segments.push(Segment::Summary {
            summary: summary.id,
        });
    }
    segments.push(Segment::Messages {
        messages: MessageRange {
            agent_id: job.agent_id,
            first_seq: covered + 1,
            last_seq: split,
        },
    });
    let completion = context::complete_recipe(
        store,
        backend,
        job.agent_id,
        &segments,
        job.sampling.clone(),
        &job.model,
        |_| {},
    )
    .await
    .context("running recorded compaction inference")?;
    let Content::Text(content) = completion.response.content else {
        anyhow::bail!("compaction inference returned tool calls instead of summary text");
    };
    store
        .finish_compaction(
            job,
            Some((&content, split, completion.inference_id)),
            &format!(
                "Compacted history after a request used {} input tokens; retained the newest {} messages.",
                job.input_tokens,
                limits.keep_tail_messages
            ),
        )
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{
        collections::VecDeque,
        sync::Mutex,
        time::{SystemTime, UNIX_EPOCH},
    };

    use super::*;
    use crate::llm::{Message, MessageContent, Response, Usage};

    struct ScriptedBackend {
        responses: Mutex<VecDeque<Response>>,
        requests: Mutex<Vec<crate::llm::Request>>,
    }

    impl Backend for ScriptedBackend {
        async fn complete(
            &self,
            request: crate::llm::Request,
            _on_token: impl FnMut(&str) + Send,
        ) -> Result<Response> {
            self.requests.lock().unwrap().push(request);
            self.responses
                .lock()
                .unwrap()
                .pop_front()
                .context("unexpected compaction inference")
        }
    }

    fn path() -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "cairnworld-compaction-test-{}-{}.sqlite",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    fn response(summary: &str) -> Response {
        Response {
            content: Content::Text(summary.to_string()),
            reasoning: String::new(),
            usage: Usage {
                input_tokens: 1,
                output_tokens: 1,
            },
        }
    }

    async fn agent_with_history(store: &Store) -> i64 {
        let world = store.create_world("test").await.unwrap();
        let agent = store.create_agent(world).await.unwrap();
        for text in ["first durable fact", "middle decision", "newest question"] {
            store
                .append_message(agent, &Message::text(Role::User, text))
                .await
                .unwrap();
        }
        agent
    }

    #[tokio::test]
    async fn compaction_uses_only_the_range_it_covers_and_changes_live_context() {
        let path = path();
        let store = Store::open(&path).await.unwrap();
        let agent = agent_with_history(&store).await;
        let backend = ScriptedBackend {
            responses: Mutex::new(VecDeque::from([response("durable fact and decision")])),
            requests: Mutex::new(Vec::new()),
        };
        let limits = Limits {
            max_concurrent_inferences: 4,
            max_inferences_per_chat: 8,
            max_inferences_total: 64,
            compact_at_input_tokens: 100,
            keep_tail_messages: 2,
        };
        after_turn(
            &store,
            &backend,
            agent,
            3,
            100,
            Sampling {
                temperature: 0.0,
                enable_thinking: false,
            },
            "scripted",
            limits,
        )
        .await
        .unwrap();

        let requests = backend.requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        let compacted = &requests[0];
        assert!(compacted.messages.iter().all(|message| {
            !matches!(&message.content, MessageContent::Text(text) if text == "newest question")
        }));
        drop(requests);

        let summary = store.latest_summary(agent).await.unwrap().unwrap();
        assert_eq!(summary.covers_to_seq, 0);
        let live = store.history_segments(agent).await.unwrap();
        let request = store
            .request_for_segments(
                agent,
                &live,
                Sampling {
                    temperature: 0.0,
                    enable_thinking: false,
                },
            )
            .await
            .unwrap();
        assert!(
            matches!(&request.messages[0].content, MessageContent::Text(text) if text == "durable fact and decision")
        );
        assert!(
            matches!(&request.messages[1].content, MessageContent::Text(text) if text == "middle decision")
        );
        assert!(
            matches!(&request.messages[2].content, MessageContent::Text(text) if text == "newest question")
        );
        assert_eq!(
            store
                .reconstruct_inference(summary.inference_id)
                .await
                .unwrap()
                .segments
                .len(),
            2
        );
        assert_eq!(
            store.chat_notice_contents(agent).await.unwrap()[0],
            "Compacted history after a request used 100 input tokens; retained the newest 2 messages."
        );
        assert!(store.pending_compaction(agent).await.unwrap().is_none());
        drop(store);
        std::fs::remove_file(path).unwrap();
    }

    #[tokio::test]
    async fn compaction_does_not_run_before_the_token_threshold() {
        let path = path();
        let store = Store::open(&path).await.unwrap();
        let agent = agent_with_history(&store).await;
        let backend = ScriptedBackend {
            responses: Mutex::new(VecDeque::new()),
            requests: Mutex::new(Vec::new()),
        };
        after_turn(
            &store,
            &backend,
            agent,
            3,
            99,
            Sampling {
                temperature: 0.0,
                enable_thinking: false,
            },
            "scripted",
            Limits {
                max_concurrent_inferences: 4,
                max_inferences_per_chat: 8,
                max_inferences_total: 64,
                compact_at_input_tokens: 100,
                keep_tail_messages: 2,
            },
        )
        .await
        .unwrap();
        assert!(backend.requests.lock().unwrap().is_empty());
        assert!(store.latest_summary(agent).await.unwrap().is_none());
        assert!(store.pending_compaction(agent).await.unwrap().is_none());
        drop(store);
        std::fs::remove_file(path).unwrap();
    }

    #[tokio::test]
    async fn compaction_records_a_notice_when_only_the_tail_remains() {
        let path = path();
        let store = Store::open(&path).await.unwrap();
        let agent = agent_with_history(&store).await;
        let backend = ScriptedBackend {
            responses: Mutex::new(VecDeque::new()),
            requests: Mutex::new(Vec::new()),
        };
        after_turn(
            &store,
            &backend,
            agent,
            3,
            100,
            Sampling {
                temperature: 0.0,
                enable_thinking: false,
            },
            "scripted",
            Limits {
                max_concurrent_inferences: 4,
                max_inferences_per_chat: 8,
                max_inferences_total: 64,
                compact_at_input_tokens: 100,
                keep_tail_messages: 3,
            },
        )
        .await
        .unwrap();
        assert!(backend.requests.lock().unwrap().is_empty());
        assert!(store.latest_summary(agent).await.unwrap().is_none());
        assert_eq!(
            store.chat_notice_contents(agent).await.unwrap()[0],
            "Context used 100 input tokens; no history older than the retained 3 messages was eligible for compaction."
        );
        assert!(store.pending_compaction(agent).await.unwrap().is_none());
        drop(store);
        std::fs::remove_file(path).unwrap();
    }
}
