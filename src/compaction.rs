use anyhow::{Context, Result, ensure};

use crate::{
    context,
    llm::{Backend, Content, ContextCapacityExceeded, Role},
    settings::Limits,
    store::{PendingCompaction, PositionRange, Segment, Store},
};

#[cfg(test)]
use crate::llm::Sampling;

/// This request only sees the material it replaces. Static role context and
/// tools are deliberately absent: they are supplied to every normal inference
/// and would waste both summary space and model attention.
const PROMPT: &str = "The conversation above is your own history. Replace it with a summary your next turn will read in its place: keep the facts, decisions, open questions, and commitments that still matter, and drop small talk and anything your standing instructions or tools already provide.";

/// Compact a completed turn only after the next inference would reach the
/// configured lazy-history threshold. No message is deleted; the new summary
/// becomes the first selected history segment on the next turn.
#[cfg(test)]
struct CompletedTurn<'a> {
    agent_id: i64,
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
    let branch_id = store.branch_id_for_agent(turn.agent_id).await?;
    let position = store.tail_split(turn.agent_id, 0).await?;
    let job = PendingCompaction {
        agent_id: turn.agent_id,
        branch_id,
        position,
        sequence_id: None,
        cause_inference_id: None,
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

    // Deferred compaction is its own invocation, separate from the sequence
    // whose reply queued it - design.md: "deferred compaction has its
    // triggering inference as cause" and "Pending compaction is shown as
    // caused deferred work rather than extending the foreground reply's
    // wall time". `job.sequence_id` is carried for display/linkage only;
    // this invocation's own start/finish never touches that sequence's row.
    let invocation_id = store
        .start_invocation(job.sequence_id, job.agent_id, None, job.cause_inference_id)
        .await
        .context("starting deferred compaction invocation")?;
    let result = run_invocation(store, backend, job, limits, invocation_id).await;
    // A failure to finish the invocation itself is a secondary bookkeeping
    // problem, logged rather than allowed to replace `result`'s own error as
    // the reason this compaction job failed.
    if let Err(finish_error) = store
        .finish_invocation(
            invocation_id,
            if result.is_ok() { "settled" } else { "error" },
        )
        .await
    {
        tracing::error!(
            invocation_id,
            error = %format!("{finish_error:#}"),
            "failed to finish deferred compaction invocation"
        );
    }
    result
}

async fn run_invocation<B: Backend>(
    store: &Store,
    backend: &B,
    job: &PendingCompaction,
    limits: Limits,
    invocation_id: i64,
) -> Result<()> {
    let mut keep = limits.keep_tail_messages;
    let mut recovering_capacity = false;
    loop {
        let (segments, branch_id, split, covered) =
            summary_segments(store, job.agent_id, keep).await?;
        if split <= covered {
            if recovering_capacity {
                anyhow::bail!(
                    "compaction cannot reduce this context: retaining {keep} newest messages leaves no older history to summarize"
                );
            }
            return store
                .abandon_pending_compaction(
                    job,
                    &format!(
                        "The next context would start at {} tokens; no history older than the retained {keep} messages was eligible for compaction.",
                        job.next_input_tokens
                    ),
                )
                .await;
        }

        let covered_ranges = [PositionRange {
            branch_id,
            first_position: covered + 1,
            last_position: split,
        }];
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
        let completion = context::complete_summary_recipe(
            store,
            backend,
            context::RecipeCompletion {
                agent_id: job.agent_id,
                // Ties the summary's inference to the event that queued this job.
                sequence_id: job.sequence_id,
                invocation_id,
                segments: &segments,
                sampling: job.sampling.clone(),
                model: &job.model,
            },
            job,
            Some(&covered_ranges),
            &notice,
            |_| {},
        )
        .await;
        return match completion {
            Ok(completion) => {
                let Content::Text(_) = completion.response.content else {
                    anyhow::bail!("compaction inference returned tool calls instead of summary text");
                };
                Ok(())
            }
            Err(error) if error.downcast_ref::<ContextCapacityExceeded>().is_some() => {
                recovering_capacity = true;
                keep += 1;
                continue;
            }
            Err(error) => Err(error).context("running recorded compaction inference"),
        };
    }
}

async fn summary_segments(
    store: &Store,
    agent_id: i64,
    keep_tail_messages: usize,
) -> Result<(Vec<Segment>, i64, i64, i64)> {
    let branch_id = store.branch_id_for_agent(agent_id).await?;
    let previous = store.latest_summary(agent_id).await?;
    let covered = previous
        .as_ref()
        .map_or(-1, |summary| {
            summary
                .covered_ranges
                .last()
                .map_or(-1, |range| range.last_position)
        });
    let split = store.tail_split(agent_id, keep_tail_messages).await?;
    let prompt = store
        .store_prompt_text(PROMPT)
        .await
        .context("storing compaction prompt")?;
    let mut segments = Vec::new();
    if let Some(summary) = previous {
        segments.push(Segment::Summary {
            summary: summary.id,
        });
    }
    if split > covered {
        segments.push(Segment::Messages {
            messages: PositionRange {
                branch_id,
                first_position: covered + 1,
                last_position: split,
            },
        });
    }
    // The task comes last, as a user turn: the history above is the material
    // to summarize, and instruct models expect the request at the end.
    segments.push(Segment::Text {
        text: prompt,
        role: Role::User,
    });
    Ok((segments, branch_id, split, covered))
}

#[cfg(test)]
mod tests {
    use std::{collections::VecDeque, sync::Mutex};

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

    fn database() -> crate::store::TestDatabase {
        crate::store::TestDatabase::new("compaction-test")
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
        let db = database();
        let store = Store::open(db.path()).await.unwrap();
        let agent = agent_with_history(&store).await;
        let prompt = store
            .store_prompt_text("standing instruction")
            .await
            .unwrap();
        let branch_id = store.branch_id_for_agent(agent).await.unwrap();
        let job = PendingCompaction {
            agent_id: agent,
            branch_id,
            position: 2,
            sequence_id: None,
            cause_inference_id: None,
            next_input_tokens: 100,
            sampling: Sampling {
                temperature: 0.0,
                ..Default::default()
            },
            model: "scripted".to_string(),
            static_segments: vec![Segment::Text {
                text: prompt,
                role: Role::System,
            }],
        };
        store.enqueue_compaction_for_test(&job).await.unwrap();
        assert_eq!(store.pending_compaction(agent).await.unwrap(), Some(job));
    }

    #[tokio::test]
    async fn capacity_recovery_uses_a_smaller_real_summary_request() {
        let db = database();
        let store = Store::open(db.path()).await.unwrap();
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
                next_input_tokens: 100,
                sampling: Sampling {
                    temperature: 0.0,
                    ..Default::default()
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
                .covered_ranges
                .last()
                .unwrap()
                .last_position,
            0
        );
        assert!(store.pending_compaction(agent).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn capacity_recovery_fails_when_no_history_is_eligible() {
        let db = database();
        let store = Store::open(db.path()).await.unwrap();
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
                next_input_tokens: 100,
                sampling: Sampling {
                    temperature: 0.0,
                    ..Default::default()
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
    }

    #[tokio::test]
    async fn compaction_uses_only_the_range_it_covers_and_changes_live_context() {
        let db = database();
        let store = Store::open(db.path()).await.unwrap();
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
                next_input_tokens: 100,
                sampling: Sampling {
                    temperature: 0.0,
                    ..Default::default()
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
        assert_eq!(summary.covered_ranges.last().unwrap().last_position, 0);
        let live = store.live_segments(agent).await.unwrap();
        let request = store
            .request_for_segments(
                agent,
                &live,
                Sampling {
                    temperature: 0.0,
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        // The summary replaces the history it covers, carried in the leading
        // system message; the retained tail follows unchanged.
        assert!(
            matches!(&request.messages[0].content, MessageContent::Text(text)
                if text.contains("lasting fact and decision"))
        );
        assert_eq!(request.messages[0].role, Role::System);
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
    }

    #[tokio::test]
    async fn compaction_does_not_run_before_the_token_threshold() {
        let db = database();
        let store = Store::open(db.path()).await.unwrap();
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
                next_input_tokens: 99,
                sampling: Sampling {
                    temperature: 0.0,
                    ..Default::default()
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
    }

    #[tokio::test]
    async fn compaction_records_a_notice_when_only_the_tail_remains() {
        let db = database();
        let store = Store::open(db.path()).await.unwrap();
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
                next_input_tokens: 100,
                sampling: Sampling {
                    temperature: 0.0,
                    ..Default::default()
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
    }

    /// design.md's Inference records: reconstruction must reproduce the
    /// *verbatim* input and output for every inference, including
    /// compaction, from the shared record - across a restart, and across
    /// repeated compaction (a summary of a summary). This appends more
    /// history, compacts twice, reopens the database file (not a fresh
    /// TestDatabase), and compares reconstruction against the exact request/
    /// response captured while the original process ran.
    #[tokio::test]
    async fn reopened_database_reconstructs_twice_compacted_inferences_byte_identical() {
        let db = database();
        let store = Store::open(db.path()).await.unwrap();
        let agent = agent_with_history(&store).await; // "first/middle/newest question", 3 messages
        let limits = Limits {
            max_concurrent_inferences: 4,
            max_inferences_per_chat: 8,
            max_inferences_total: 64,
            max_context_tokens: 110,
            max_output_tokens: 10,
            compact_before_next_input_tokens: 100,
            keep_tail_messages: 1,
        };

        // First compaction: summarize the two oldest of the three messages.
        let first_backend = ScriptedBackend {
            responses: Mutex::new(VecDeque::from([response("first summary")])),
            requests: Mutex::new(Vec::new()),
        };
        after_turn(
            &store,
            &first_backend,
            CompletedTurn {
                agent_id: agent,
                next_input_tokens: 100,
                sampling: Sampling { temperature: 0.0, ..Default::default() },
                model: "scripted",
                limits,
            },
        )
        .await
        .unwrap();
        let first_summary = store.latest_summary(agent).await.unwrap().unwrap();
        let first_captured_request = first_backend.requests.lock().unwrap()[0].clone();

        // More history accrues, then a second compaction folds the first
        // summary plus the new messages into a second one.
        for text in ["fourth question", "fifth question"] {
            store
                .append_message(agent, &Message::text(Role::User, text))
                .await
                .unwrap();
        }
        let second_backend = ScriptedBackend {
            responses: Mutex::new(VecDeque::from([response("second summary")])),
            requests: Mutex::new(Vec::new()),
        };
        after_turn(
            &store,
            &second_backend,
            CompletedTurn {
                agent_id: agent,
                next_input_tokens: 100,
                sampling: Sampling { temperature: 0.0, ..Default::default() },
                model: "scripted",
                limits,
            },
        )
        .await
        .unwrap();
        let second_summary = store.latest_summary(agent).await.unwrap().unwrap();
        assert_ne!(second_summary.id, first_summary.id, "the second compaction produces a new summary");
        let second_captured_request = second_backend.requests.lock().unwrap()[0].clone();

        // Reopen the same database file, as a restarted process would.
        let reopened = Store::open(db.path()).await.unwrap();
        let first_reconstructed = reopened
            .reconstruct_inference(first_summary.inference_id)
            .await
            .unwrap();
        assert_eq!(first_reconstructed.request, first_captured_request);
        assert_eq!(
            first_reconstructed.outcome,
            crate::store::RecordedOutcome::Response(response("first summary"))
        );
        let second_reconstructed = reopened
            .reconstruct_inference(second_summary.inference_id)
            .await
            .unwrap();
        assert_eq!(second_reconstructed.request, second_captured_request);
        assert_eq!(
            second_reconstructed.outcome,
            crate::store::RecordedOutcome::Response(response("second summary"))
        );
        // The second recipe's summary segment refers to the first summary,
        // proving a summary-of-a-summary chain reconstructs too.
        assert!(
            second_reconstructed
                .segments
                .iter()
                .any(|segment| matches!(segment, Segment::Summary { summary } if *summary == first_summary.id))
        );
    }
}
