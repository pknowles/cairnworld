use anyhow::{Context, Result, ensure};

use crate::{
    context,
    llm::{Backend, Content, ContextCapacityExceeded, Role},
    settings::Limits,
    store::{MessageRange, PendingCompaction, Segment, Store},
};

#[cfg(test)]
use crate::llm::Sampling;

/// This request only sees the material it replaces. Static role context and
/// tools are deliberately absent: they are supplied to every normal inference
/// and would waste both summary space and model attention.
const PROMPT: &str = "Summarize this earlier chat for its next model context. Retain facts, decisions, unresolved questions, and commitments that will matter later. Omit transient discussion and anything supplied separately by the agent's standing instructions or tools.";

/// Compact a completed turn only after the next inference would reach the
/// configured lazy-history threshold. No message is deleted; the new summary
/// becomes the first selected history segment on the next turn.
#[cfg(test)]
struct CompletedTurn<'a> {
    agent_id: i64,
    after_message_id: i64,
    next_input_tokens: usize,
    sampling: Sampling,
    model: &'a str,
    limits: Limits,
}

#[cfg(test)]
async fn after_turn<B: Backend>(store: &Store, backend: &B, turn: CompletedTurn<'_>) -> Result<()> {
    if turn.next_input_tokens < turn.limits.compact_before_next_input_tokens {
        return Ok(());
    }
    let job = PendingCompaction {
        agent_id: turn.agent_id,
        after_message_id: turn.after_message_id,
        next_input_tokens: turn.next_input_tokens,
        sampling: turn.sampling,
        model: turn.model.to_string(),
        static_segments: Vec::new(),
    };
    store.enqueue_compaction_for_test(&job).await?;
    run(store, backend, &job, turn.limits).await
}

/// Resolve one persisted compaction obligation. The normal path performs one
/// ordinary deferred summary without tokenizing: the just-finished inference
/// already reported its exact next-context size, and a second tokenization
/// would be pure overhead. A rejected summary keeps one more raw message and
/// retries the real request, never a predicted token count.
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

    let mut keep = limits.keep_tail_messages;
    let mut recovering_capacity = false;
    loop {
        let (segments, split, covered) = summary_segments(store, job.agent_id, keep).await?;
        if split <= covered {
            if recovering_capacity {
                anyhow::bail!(
                    "compaction cannot reduce this context: retaining {keep} newest messages leaves no older history to summarize"
                );
            }
            return store
                .finish_compaction(
                    job,
                    None,
                    &format!(
                        "The next context would start at {} tokens; no history older than the retained {keep} messages was eligible for compaction.",
                        job.next_input_tokens
                    ),
                )
                .await;
        }

        let completion = context::complete_recipe(
            store,
            backend,
            context::RecipeCompletion {
                agent_id: job.agent_id,
                sequence_id: None,
                parent_inference_id: None,
                segments: &segments,
                sampling: job.sampling.clone(),
                model: &job.model,
            },
            |_| {},
        )
        .await;
        let completion = match completion {
            Ok(completion) => completion,
            Err(error) if error.downcast_ref::<ContextCapacityExceeded>().is_some() => {
                recovering_capacity = true;
                keep += 1;
                continue;
            }
            Err(error) => return Err(error).context("running recorded compaction inference"),
        };
        let Content::Text(content) = completion.response.content else {
            anyhow::bail!("compaction inference returned tool calls instead of summary text");
        };
        let notice = if recovering_capacity {
            format!(
                "Compacted history through a capacity recovery; retained the newest {keep} messages."
            )
        } else {
            format!(
                "Compacted history before the next context reached {} tokens; retained the newest {keep} messages.",
                job.next_input_tokens
            )
        };
        return store
            .finish_compaction(
                job,
                Some((&content, split, completion.inference_id)),
                &notice,
            )
            .await;
    }
}

async fn summary_segments(
    store: &Store,
    agent_id: i64,
    keep_tail_messages: usize,
) -> Result<(Vec<Segment>, i64, i64)> {
    let previous = store.latest_summary(agent_id).await?;
    let covered = previous
        .as_ref()
        .map_or(-1, |summary| summary.covers_to_seq);
    let split = store.tail_split(agent_id, keep_tail_messages).await?;
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
    if split > covered {
        segments.push(Segment::Messages {
            messages: MessageRange {
                agent_id,
                first_seq: covered + 1,
                last_seq: split,
            },
        });
    }
    Ok((segments, split, covered))
}

#[cfg(test)]
mod tests {
    use std::{
        collections::VecDeque,
        sync::Mutex,
        time::{SystemTime, UNIX_EPOCH},
    };

    use super::*;
    use crate::llm::{ContextCapacityExceeded, Message, MessageContent, Response, Usage};

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

    struct CapacityRecoveryBackend {
        responses: Mutex<VecDeque<Result<Response>>>,
        requests: Mutex<Vec<crate::llm::Request>>,
    }

    impl Backend for CapacityRecoveryBackend {
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
                .context("unexpected capacity fallback inference")?
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
        for text in ["first lasting fact", "middle decision", "newest question"] {
            store
                .append_message(agent, &Message::text(Role::User, text))
                .await
                .unwrap();
        }
        agent
    }

    async fn agent_with_four_messages(store: &Store) -> i64 {
        let world = store.create_world("test").await.unwrap();
        let agent = store.create_agent(world).await.unwrap();
        for text in ["first", "second", "third", "fourth"] {
            store
                .append_message(agent, &Message::text(Role::User, text))
                .await
                .unwrap();
        }
        agent
    }

    #[tokio::test]
    async fn capacity_job_retains_its_static_recipe_across_reload() {
        let path = path();
        let store = Store::open(&path).await.unwrap();
        let agent = agent_with_history(&store).await;
        let prompt = store
            .store_prompt_text("standing instruction")
            .await
            .unwrap();
        let job = PendingCompaction {
            agent_id: agent,
            after_message_id: 3,
            next_input_tokens: 100,
            sampling: Sampling {
                temperature: 0.0,
                enable_thinking: false,
            },
            model: "scripted".to_string(),
            static_segments: vec![Segment::Text {
                text: prompt,
                role: Role::System,
            }],
        };
        store.enqueue_compaction_for_test(&job).await.unwrap();
        assert_eq!(store.pending_compaction(agent).await.unwrap(), Some(job));
        drop(store);
        std::fs::remove_file(path).unwrap();
    }

    #[tokio::test]
    async fn capacity_recovery_uses_a_smaller_real_summary_request() {
        let path = path();
        let store = Store::open(&path).await.unwrap();
        let agent = agent_with_four_messages(&store).await;
        let backend = CapacityRecoveryBackend {
            responses: Mutex::new(VecDeque::from([
                Err(ContextCapacityExceeded::FixedKv {
                    requested_tokens: 101,
                    max_context_tokens: 110,
                }
                .into()),
                Ok(response("first fact")),
            ])),
            requests: Mutex::new(Vec::new()),
        };
        let limits = Limits {
            max_concurrent_inferences: 1,
            max_inferences_per_chat: 8,
            max_inferences_total: 64,
            max_context_tokens: 110,
            max_output_tokens: 10,
            compact_before_next_input_tokens: 100,
            keep_tail_messages: 2,
        };
        after_turn(
            &store,
            &backend,
            CompletedTurn {
                agent_id: agent,
                after_message_id: 4,
                next_input_tokens: 100,
                sampling: Sampling {
                    temperature: 0.0,
                    enable_thinking: false,
                },
                model: "scripted",
                limits,
            },
        )
        .await
        .unwrap();

        assert_eq!(backend.requests.lock().unwrap().len(), 2);
        assert_eq!(
            store
                .latest_summary(agent)
                .await
                .unwrap()
                .unwrap()
                .covers_to_seq,
            0
        );
        assert!(store.pending_compaction(agent).await.unwrap().is_none());
        drop(store);
        std::fs::remove_file(path).unwrap();
    }

    #[tokio::test]
    async fn capacity_recovery_fails_when_no_history_is_eligible() {
        let path = path();
        let store = Store::open(&path).await.unwrap();
        let agent = agent_with_history(&store).await;
        let backend = CapacityRecoveryBackend {
            responses: Mutex::new(VecDeque::from([Err(ContextCapacityExceeded::FixedKv {
                requested_tokens: 101,
                max_context_tokens: 100,
            }
            .into())])),
            requests: Mutex::new(Vec::new()),
        };
        let error = after_turn(
            &store,
            &backend,
            CompletedTurn {
                agent_id: agent,
                after_message_id: 3,
                next_input_tokens: 100,
                sampling: Sampling {
                    temperature: 0.0,
                    enable_thinking: false,
                },
                model: "scripted",
                limits: Limits {
                    max_concurrent_inferences: 1,
                    max_inferences_per_chat: 8,
                    max_inferences_total: 64,
                    max_context_tokens: 110,
                    max_output_tokens: 10,
                    compact_before_next_input_tokens: 100,
                    keep_tail_messages: 2,
                },
            },
        )
        .await
        .expect_err("capacity recovery must not retry unchanged history");

        assert!(
            error
                .to_string()
                .contains("leaves no older history to summarize")
        );
        assert_eq!(backend.requests.lock().unwrap().len(), 1);
        assert!(store.latest_summary(agent).await.unwrap().is_none());
        assert!(store.pending_compaction(agent).await.unwrap().is_some());
        drop(store);
        std::fs::remove_file(path).unwrap();
    }

    #[tokio::test]
    async fn compaction_uses_only_the_range_it_covers_and_changes_live_context() {
        let path = path();
        let store = Store::open(&path).await.unwrap();
        let agent = agent_with_history(&store).await;
        let backend = ScriptedBackend {
            responses: Mutex::new(VecDeque::from([response("lasting fact and decision")])),
            requests: Mutex::new(Vec::new()),
        };
        let limits = Limits {
            max_concurrent_inferences: 4,
            max_inferences_per_chat: 8,
            max_inferences_total: 64,
            max_context_tokens: 110,
            max_output_tokens: 10,
            compact_before_next_input_tokens: 100,
            keep_tail_messages: 2,
        };
        after_turn(
            &store,
            &backend,
            CompletedTurn {
                agent_id: agent,
                after_message_id: 3,
                next_input_tokens: 100,
                sampling: Sampling {
                    temperature: 0.0,
                    enable_thinking: false,
                },
                model: "scripted",
                limits,
            },
        )
        .await
        .unwrap();

        {
            let requests = backend.requests.lock().unwrap();
            assert_eq!(requests.len(), 1);
            let compacted = &requests[0];
            assert!(compacted.messages.iter().all(|message| {
                !matches!(&message.content, MessageContent::Text(text) if text == "newest question")
            }));
        }

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
            matches!(&request.messages[0].content, MessageContent::Text(text) if text == "lasting fact and decision")
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
            "Compacted history before the next context reached 100 tokens; retained the newest 2 messages."
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
            CompletedTurn {
                agent_id: agent,
                after_message_id: 3,
                next_input_tokens: 99,
                sampling: Sampling {
                    temperature: 0.0,
                    enable_thinking: false,
                },
                model: "scripted",
                limits: Limits {
                    max_concurrent_inferences: 4,
                    max_inferences_per_chat: 8,
                    max_inferences_total: 64,
                    max_context_tokens: 110,
                    max_output_tokens: 10,
                    compact_before_next_input_tokens: 100,
                    keep_tail_messages: 2,
                },
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
            CompletedTurn {
                agent_id: agent,
                after_message_id: 3,
                next_input_tokens: 100,
                sampling: Sampling {
                    temperature: 0.0,
                    enable_thinking: false,
                },
                model: "scripted",
                limits: Limits {
                    max_concurrent_inferences: 4,
                    max_inferences_per_chat: 8,
                    max_inferences_total: 64,
                    max_context_tokens: 110,
                    max_output_tokens: 10,
                    compact_before_next_input_tokens: 100,
                    keep_tail_messages: 3,
                },
            },
        )
        .await
        .unwrap();
        assert!(backend.requests.lock().unwrap().is_empty());
        assert!(store.latest_summary(agent).await.unwrap().is_none());
        assert_eq!(
            store.chat_notice_contents(agent).await.unwrap()[0],
            "The next context would start at 100 tokens; no history older than the retained 3 messages was eligible for compaction."
        );
        assert!(store.pending_compaction(agent).await.unwrap().is_none());
        drop(store);
        std::fs::remove_file(path).unwrap();
    }
}
