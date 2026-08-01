use anyhow::{Context, Result, ensure};

use crate::{
    context,
    llm::{Backend, Content, Message, Request, Role, Sampling, ToolDefinition},
    settings::Limits,
    store::{MessageRange, Segment, Store},
};

/// This request only sees the material it replaces. Static role context and
/// tools are deliberately absent: they are supplied to every normal inference
/// and would waste both summary space and model attention.
const PROMPT: &str = "Summarize this earlier chat for its next model context. Retain durable facts, decisions, unresolved questions, and commitments needed later. Omit transient discussion and anything supplied separately by the agent's standing instructions or tools.";

/// Compact once after a completed turn when the complete live request reaches
/// the configured context limit. No message is deleted; the new summary simply
/// becomes the first selected history segment on the next turn.
#[allow(clippy::too_many_arguments)]
pub async fn after_turn<B: Backend>(
    store: &Store,
    backend: &B,
    agent_id: i64,
    static_messages: &[Message],
    tools: &[ToolDefinition],
    sampling: Sampling,
    model: &str,
    limits: Limits,
) -> Result<()> {
    let history = store
        .history_segments(agent_id)
        .await
        .context("selecting live history to check compaction")?;
    let history = store
        .request_for_segments(agent_id, &history, sampling.clone())
        .await
        .context("assembling live context to check compaction")?;
    let live_request = Request {
        messages: static_messages
            .iter()
            .cloned()
            .chain(history.messages)
            .collect(),
        tools: tools.to_vec(),
        sampling: sampling.clone(),
    };
    let token_count = backend
        .tokens(live_request)
        .await
        .context("counting live context tokens")?;
    if token_count < limits.compact_at_tokens {
        return Ok(());
    }

    let previous = store.latest_summary(agent_id).await?;
    let covered = previous
        .as_ref()
        .map_or(-1, |summary| summary.covers_to_seq);
    let split = store.tail_split(agent_id, limits.keep_tail_chars).await?;
    ensure!(
        split > covered,
        "agent {agent_id} reached {} context tokens but its raw tail cannot be compacted; \
         limits.keep_tail_chars leaves no older message to summarize",
        token_count
    );

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
            agent_id,
            first_seq: covered + 1,
            last_seq: split,
        },
    });
    let completion =
        context::complete_recipe(store, backend, agent_id, &segments, sampling, model, |_| {})
            .await
            .context("running recorded compaction inference")?;
    let Content::Text(content) = completion.response.content else {
        anyhow::bail!("compaction inference returned tool calls instead of summary text");
    };
    store
        .store_summary(agent_id, split, &content, completion.inference_id)
        .await
        .context("storing compaction result")?;
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
    use crate::llm::{MessageContent, Response, Usage};

    struct ScriptedBackend {
        responses: Mutex<VecDeque<Response>>,
        requests: Mutex<Vec<crate::llm::Request>>,
        tokens: usize,
    }

    impl Backend for ScriptedBackend {
        async fn complete(
            &self,
            request: crate::llm::Request,
            _on_token: impl FnMut(&str),
        ) -> Result<Response> {
            self.requests.lock().unwrap().push(request);
            self.responses
                .lock()
                .unwrap()
                .pop_front()
                .context("unexpected compaction inference")
        }

        async fn tokens(&self, _request: crate::llm::Request) -> Result<usize> {
            Ok(self.tokens)
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
        let agent = store.create_agent(world, "sandbox", "test").await.unwrap();
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
            tokens: 100,
        };
        let limits = Limits {
            max_inferences_per_chat: 8,
            max_inferences_total: 64,
            compact_at_tokens: 100,
            keep_tail_chars: 100,
        };
        after_turn(
            &store,
            &backend,
            agent,
            &[Message::text(Role::System, "standing instructions")],
            &[],
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
        assert!(compacted.messages.iter().all(|message| {
            !matches!(&message.content, MessageContent::Text(text) if text == "standing instructions")
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
            tokens: 99,
        };
        after_turn(
            &store,
            &backend,
            agent,
            &[],
            &[],
            Sampling {
                temperature: 0.0,
                enable_thinking: false,
            },
            "scripted",
            Limits {
                max_inferences_per_chat: 8,
                max_inferences_total: 64,
                compact_at_tokens: 100,
                keep_tail_chars: 40,
            },
        )
        .await
        .unwrap();
        assert!(backend.requests.lock().unwrap().is_empty());
        assert!(store.latest_summary(agent).await.unwrap().is_none());
        drop(store);
        std::fs::remove_file(path).unwrap();
    }
}
