use anyhow::{Context, Result, ensure};

use crate::{
    context,
    llm::{Backend, Content, ContextCapacityExceeded, Role},
    settings::Limits,
    store::{PendingCompaction, PositionRange, Segment, Store},
};

/// This request only sees the material it replaces. Static role context and
/// tools are deliberately absent: they are supplied to every normal inference
/// and would waste both summary space and model attention.
const PROMPT: &str = "The conversation above is your own history. Replace it with a summary your next turn will read in its place: keep the facts, decisions, open questions, and commitments that still matter, and drop small talk and anything your standing instructions or tools already provide.";

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
        .start_invocation(
            job.sequence_id,
            job.agent_id,
            Some(job.branch_id),
            None,
            job.cause_inference_id,
        )
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
            summary_segments(store, job.branch_id, keep).await?;
        if split <= covered {
            if recovering_capacity {
                anyhow::bail!(
                    "compaction cannot reduce this context: retaining {keep} newest messages leaves no older history to summarize"
                );
            }
            return store.abandon_pending_compaction(job).await;
        }

        let covered_ranges = [PositionRange {
            branch_id,
            first_position: covered + 1,
            last_position: split,
        }];
        let reason = if recovering_capacity {
            "capacity_recovery"
        } else {
            "threshold"
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
            (
                reason,
                i64::try_from(keep).context("retained message count exceeds SQLite range")?,
            ),
            |_| {},
        )
        .await;
        return match completion {
            Ok(completion) => {
                let Content::Text(_) = completion.response.content else {
                    anyhow::bail!(
                        "compaction inference returned tool calls instead of summary text"
                    );
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
    branch_id: i64,
    keep_tail_messages: usize,
) -> Result<(Vec<Segment>, i64, i64, i64)> {
    let previous = store.latest_summary_for_branch(branch_id).await?;
    let covered = previous.as_ref().map_or(-1, |summary| {
        summary
            .covered_ranges
            .last()
            .map_or(-1, |range| range.last_position)
    });
    let split = store
        .tail_split_for_branch(branch_id, keep_tail_messages)
        .await?;
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
    use crate::{
        llm::{ContextCapacityExceeded, Message, MessageContent, Response, Sampling, Usage},
        store::CompactionPolicy,
    };

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
        response_with_usage(summary, 1, 1)
    }

    fn response_with_usage(text: &str, input_tokens: usize, output_tokens: usize) -> Response {
        Response {
            content: Content::Text(text.to_string()),
            reasoning: String::new(),
            usage: Usage {
                input_tokens,
                output_tokens,
            },
        }
    }

    async fn complete_turn<B: Backend>(
        store: &Store,
        backend: &B,
        agent_id: i64,
        limits: Limits,
        static_messages: &[Message],
    ) {
        let branch = store.agent_branch(agent_id).await.unwrap();
        let segments = context::segments(store, branch, static_messages, &[], &[])
            .await
            .unwrap();
        let static_segments = context::static_segments(&segments);
        let invocation_id = store
            .start_invocation(None, agent_id, Some(branch.branch_id()), None, None)
            .await
            .unwrap();
        context::complete_recipe(
            store,
            backend,
            context::RecipeCompletion {
                agent_id,
                sequence_id: None,
                invocation_id,
                segments: &segments,
                sampling: Sampling {
                    temperature: 0.0,
                    ..Default::default()
                },
                model: "scripted",
            },
            CompactionPolicy {
                sequence_id: None,
                compact_before_next_input_tokens: limits.compact_before_next_input_tokens,
                sampling: &Sampling {
                    temperature: 0.0,
                    ..Default::default()
                },
                model: "scripted",
                static_segments: &static_segments,
            },
            |_| {},
        )
        .await
        .unwrap();
    }

    async fn complete_and_compact<B: Backend>(
        store: &Store,
        backend: &B,
        agent_id: i64,
        limits: Limits,
    ) -> Result<()> {
        complete_turn(store, backend, agent_id, limits, &[]).await;
        let job = store
            .pending_compaction_for_branch(store.agent_branch(agent_id).await?.branch_id())
            .await?
            .context("completed turn did not queue compaction")?;
        run(store, backend, &job, limits).await
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

    fn limits(keep_tail_messages: usize) -> Limits {
        Limits {
            max_concurrent_inferences: 4,
            max_inferences_per_chat: 8,
            max_inferences_total: 64,
            max_context_tokens: 110,
            max_output_tokens: 10,
            compact_before_next_input_tokens: 100,
            keep_tail_messages,
            merge_branch_messages: 64,
            presence_departure_seconds: 60,
        }
    }

    #[tokio::test]
    async fn capacity_job_retains_its_static_recipe_across_reload() {
        let db = database();
        let store = Store::open(db.path()).await.unwrap();
        let agent = agent_with_history(&store).await;
        let backend = ScriptedBackend {
            responses: Mutex::new(VecDeque::from([response_with_usage("reply", 50, 50)])),
            requests: Mutex::new(Vec::new()),
        };
        complete_turn(
            &store,
            &backend,
            agent,
            limits(2),
            &[Message::text(Role::System, "standing instruction")],
        )
        .await;
        let reopened = Store::open(db.path()).await.unwrap();
        let job = reopened.pending_compaction(agent).await.unwrap().unwrap();
        assert!(matches!(
            job.static_segments.as_slice(),
            [Segment::Text {
                role: Role::System,
                ..
            }]
        ));
    }

    #[tokio::test]
    async fn capacity_recovery_uses_a_smaller_real_summary_request() {
        let db = database();
        let store = Store::open(db.path()).await.unwrap();
        let agent = agent_with_four_messages(&store).await;
        let backend = CapacityRecoveryBackend {
            responses: Mutex::new(VecDeque::from([
                Ok(response_with_usage("reply", 50, 50)),
                Err(ContextCapacityExceeded::FixedKv {
                    requested_tokens: 101,
                    max_context_tokens: 110,
                }
                .into()),
                Ok(response("first fact")),
            ])),
            requests: Mutex::new(Vec::new()),
        };
        let limits = limits(2);
        complete_and_compact(&store, &backend, agent, limits)
            .await
            .unwrap();

        assert_eq!(backend.requests.lock().unwrap().len(), 3);
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
            1
        );
        assert!(store.pending_compaction(agent).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn capacity_recovery_fails_when_no_history_is_eligible() {
        let db = database();
        let store = Store::open(db.path()).await.unwrap();
        let agent = agent_with_history(&store).await;
        let backend = CapacityRecoveryBackend {
            responses: Mutex::new(VecDeque::from([
                Ok(response_with_usage("reply", 50, 50)),
                Err(ContextCapacityExceeded::FixedKv {
                    requested_tokens: 101,
                    max_context_tokens: 100,
                }
                .into()),
            ])),
            requests: Mutex::new(Vec::new()),
        };
        let limits = limits(3);
        complete_turn(&store, &backend, agent, limits, &[]).await;
        let job = store.pending_compaction(agent).await.unwrap().unwrap();
        let error = run(&store, &backend, &job, limits)
            .await
            .expect_err("capacity recovery must not retry unchanged history");

        assert!(
            error
                .to_string()
                .contains("leaves no older history to summarize")
        );
        assert_eq!(backend.requests.lock().unwrap().len(), 2);
        assert!(store.latest_summary(agent).await.unwrap().is_none());
        assert!(store.pending_compaction(agent).await.unwrap().is_some());
    }

    #[tokio::test]
    async fn compaction_uses_only_the_range_it_covers_and_changes_live_context() {
        let db = database();
        let store = Store::open(db.path()).await.unwrap();
        let agent = agent_with_history(&store).await;
        let backend = ScriptedBackend {
            responses: Mutex::new(VecDeque::from([
                response_with_usage("reply", 50, 50),
                response("lasting fact and decision"),
            ])),
            requests: Mutex::new(Vec::new()),
        };
        let limits = limits(2);
        complete_and_compact(&store, &backend, agent, limits)
            .await
            .unwrap();

        {
            let requests = backend.requests.lock().unwrap();
            assert_eq!(requests.len(), 2);
            let compacted = &requests[1];
            assert!(compacted.messages.iter().all(|message| {
                !matches!(&message.content, MessageContent::Text(text) if text == "newest question")
            }));
        }

        let summary = store.latest_summary(agent).await.unwrap().unwrap();
        assert_eq!(summary.covered_ranges.last().unwrap().last_position, 1);
        let live = store
            .live_segments(store.agent_branch(agent).await.unwrap())
            .await
            .unwrap();
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
            matches!(&request.messages[1].content, MessageContent::Text(text) if text == "newest question")
        );
        assert!(
            matches!(&request.messages[2].content, MessageContent::Text(text) if text == "reply")
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
        let summary = store.latest_summary(agent).await.unwrap().unwrap();
        assert_eq!(summary.reason, "threshold");
        assert_eq!(summary.retained_messages, 2);
        assert!(store.pending_compaction(agent).await.unwrap().is_none());
        let notice_table: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'chat_notice'",
        )
        .fetch_one(&store.pool())
        .await
        .unwrap();
        assert_eq!(
            notice_table, 0,
            "compaction metadata must live on its one summary record, not in a second notice table"
        );
        let notices: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM history_entry WHERE kind = 'notice'")
                .fetch_one(&store.pool())
                .await
                .unwrap();
        assert_eq!(
            notices, 0,
            "one compaction must not create a second history entry"
        );
    }

    #[tokio::test]
    async fn compaction_does_not_run_before_the_token_threshold() {
        let db = database();
        let store = Store::open(db.path()).await.unwrap();
        let agent = agent_with_history(&store).await;
        let backend = ScriptedBackend {
            responses: Mutex::new(VecDeque::from([response_with_usage("reply", 49, 50)])),
            requests: Mutex::new(Vec::new()),
        };
        complete_turn(&store, &backend, agent, limits(2), &[]).await;
        assert_eq!(backend.requests.lock().unwrap().len(), 1);
        assert!(store.latest_summary(agent).await.unwrap().is_none());
        assert!(store.pending_compaction(agent).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn compaction_with_only_the_tail_retires_without_a_history_event() {
        let db = database();
        let store = Store::open(db.path()).await.unwrap();
        let agent = agent_with_history(&store).await;
        let backend = ScriptedBackend {
            responses: Mutex::new(VecDeque::from([response_with_usage("reply", 50, 50)])),
            requests: Mutex::new(Vec::new()),
        };
        complete_and_compact(&store, &backend, agent, limits(4))
            .await
            .unwrap();
        assert_eq!(backend.requests.lock().unwrap().len(), 1);
        assert!(store.latest_summary(agent).await.unwrap().is_none());
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
            merge_branch_messages: 64,
            presence_departure_seconds: 60,
        };

        // First compaction: summarize the two oldest of the three messages.
        let first_backend = ScriptedBackend {
            responses: Mutex::new(VecDeque::from([
                response_with_usage("reply", 50, 50),
                response("first summary"),
            ])),
            requests: Mutex::new(Vec::new()),
        };
        complete_and_compact(&store, &first_backend, agent, limits)
            .await
            .unwrap();
        let first_summary = store.latest_summary(agent).await.unwrap().unwrap();
        let first_captured_request = first_backend.requests.lock().unwrap()[1].clone();

        // More history accrues, then a second compaction folds the first
        // summary plus the new messages into a second one.
        for text in ["fourth question", "fifth question"] {
            store
                .append_message(agent, &Message::text(Role::User, text))
                .await
                .unwrap();
        }
        let second_backend = ScriptedBackend {
            responses: Mutex::new(VecDeque::from([
                response_with_usage("reply", 50, 50),
                response("second summary"),
            ])),
            requests: Mutex::new(Vec::new()),
        };
        complete_and_compact(&store, &second_backend, agent, limits)
            .await
            .unwrap();
        let second_summary = store.latest_summary(agent).await.unwrap().unwrap();
        assert_ne!(
            second_summary.id, first_summary.id,
            "the second compaction produces a new summary"
        );
        let second_captured_request = second_backend.requests.lock().unwrap()[1].clone();

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
