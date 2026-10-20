//! Valid tool-call history after a failed or interrupted turn (design.md's
//! Server shutdown and crashes). Every call the model made keeps a result in
//! its agent's history, so the next inference never sees an unanswered call.
//! The result is the real error, the recorded outcome the database proves, or
//! an honest statement that completion was not confirmed; it never claims a
//! call had no effect unless it provably never began.

use anyhow::{Context, Result};
use sqlx::{Sqlite, Transaction};

use super::Store;
use crate::{
    inspection::MessageOrigin,
    llm::{Message, MessageContent},
};

/// Why a batch's unanswered calls are being closed.
pub(super) enum CallClosure<'a> {
    /// The server kept running and a call raised `error`. `failing_execution_id`
    /// names that call; every other unanswered call was not run. `None` means
    /// the batch's deferred work as a whole failed, so each unanswered call
    /// (those waiting on it) reports the error.
    Failed {
        failing_execution_id: Option<i64>,
        error: &'a str,
    },
    /// The process stopped. A call that had begun may have taken effect; one
    /// that had not begun did not.
    Interrupted,
}

impl Store {
    /// Give every call of `inference_id` that has no result message one, in
    /// call order, and finish its execution row to match.
    async fn close_tool_calls(
        transaction: &mut Transaction<'_, Sqlite>,
        inference_id: i64,
        closure: &CallClosure<'_>,
    ) -> Result<()> {
        let inference = sqlx::query!(
            "SELECT history_entry.branch_id, inference.output FROM inference \
             JOIN history_entry ON history_entry.id = inference.history_entry_id \
             WHERE inference.id = ?",
            inference_id,
        )
        .fetch_one(&mut **transaction)
        .await
        .with_context(|| format!("loading inference {inference_id} to close its tool calls"))?;
        let Some(output) = inference.output else {
            return Ok(());
        };
        let MessageContent::ToolCalls(calls) =
            Self::content_in_transaction(transaction, output).await?
        else {
            return Ok(());
        };
        for (ordinal, call) in calls.iter().enumerate() {
            let ordinal =
                i64::try_from(ordinal).context("tool call ordinal exceeds SQLite range")?;
            let existing = sqlx::query!(
                "SELECT id, status FROM tool_execution WHERE inference_id = ? AND ordinal = ?",
                inference_id,
                ordinal,
            )
            .fetch_optional(&mut **transaction)
            .await
            .context("loading tool execution to close")?;
            let (execution_id, began) = match existing {
                Some(row) => (row.id, row.status == "running"),
                None => (
                    sqlx::query!(
                        "INSERT INTO tool_execution (inference_id, ordinal, status) VALUES (?, ?, 'unexecuted')",
                        inference_id,
                        ordinal,
                    )
                    .execute(&mut **transaction)
                    .await
                    .context("creating tool execution to close")?
                    .last_insert_rowid(),
                    false,
                ),
            };
            let answered = sqlx::query_scalar!(
                "SELECT 1 FROM message WHERE source_tool_execution_id = ?",
                execution_id,
            )
            .fetch_optional(&mut **transaction)
            .await
            .context("checking for an existing tool result")?;
            if answered.is_some() {
                continue;
            }
            let (status, text) = match closure {
                CallClosure::Failed {
                    failing_execution_id,
                    error,
                } if failing_execution_id.is_none_or(|id| id == execution_id) => {
                    ("failed", format!("Tool failed: {error}"))
                }
                CallClosure::Failed { .. } => (
                    "unexecuted",
                    "Not executed: an earlier call in this batch failed.".to_string(),
                ),
                CallClosure::Interrupted if began => (
                    "failed",
                    Self::interrupted_text(transaction, execution_id).await?,
                ),
                CallClosure::Interrupted => (
                    "unexecuted",
                    "Not executed: the server stopped before this call ran.".to_string(),
                ),
            };
            let error = (status == "failed").then_some(text.as_str());
            let (_, result_content_id) = Self::insert_message(
                transaction,
                inference.branch_id,
                &Message::tool_result(call.id.clone(), text.clone()),
                Some(inference_id),
                Some(execution_id),
                None,
                MessageOrigin::Conversation,
            )
            .await?;
            sqlx::query!(
                "UPDATE tool_execution SET status = ?, result_content_id = ?, error = ?, \
                 finished_at = COALESCE(finished_at, CURRENT_TIMESTAMP) WHERE id = ?",
                status,
                result_content_id,
                error,
                execution_id,
            )
            .execute(&mut **transaction)
            .await
            .with_context(|| format!("closing tool execution {execution_id}"))?;
        }
        Ok(())
    }

    /// The result for a call that had begun when the process stopped. A direct
    /// action (a roll) or a GM decision records its own result in the same
    /// transaction as its effect, so that recorded outcome proves what
    /// happened; any other call, including one whose narration was still
    /// pending, is reported as unconfirmed.
    async fn interrupted_text(
        transaction: &mut Transaction<'_, Sqlite>,
        execution_id: i64,
    ) -> Result<String> {
        let recorded = sqlx::query_scalar!(
            "SELECT result_content_id FROM action \
             WHERE (requesting_tool_execution_id = ?1 AND pending_action_id IS NULL) \
                OR approving_tool_execution_id = ?1",
            execution_id,
        )
        .fetch_optional(&mut **transaction)
        .await
        .context("looking for the recorded outcome of an interrupted call")?;
        let prefix = "Interrupted: the server stopped before this call's result was recorded.";
        let Some(recorded) = recorded else {
            return Ok(format!(
                "{prefix} Its effects may already have been applied."
            ));
        };
        let MessageContent::Text(outcome) =
            Self::content_in_transaction(transaction, recorded).await?
        else {
            anyhow::bail!(
                "recorded outcome {recorded} of tool execution {execution_id} is not text"
            );
        };
        Ok(format!("{prefix} Recorded outcome: {outcome}"))
    }

    /// Resolve every action `sequence_id` left pending as rejected, so the GM
    /// cannot later approve an action from a turn that did not complete.
    pub(super) async fn close_pending_actions(
        transaction: &mut Transaction<'_, Sqlite>,
        sequence_id: i64,
        reason: &str,
    ) -> Result<()> {
        let pending = sqlx::query!(
            "SELECT id, requesting_tool_execution_id FROM pending_action WHERE sequence_id = ?",
            sequence_id,
        )
        .fetch_all(&mut **transaction)
        .await
        .context("loading pending actions to close")?;
        for action in pending {
            let result_content_id = Self::content_in(
                transaction,
                &MessageContent::Text(format!("rejected: {reason}")),
                "",
            )
            .await?;
            sqlx::query!(
                "INSERT INTO action (sequence_id, requesting_tool_execution_id, result_content_id, approved, pending_action_id) \
                 VALUES (?, ?, ?, FALSE, ?)",
                sequence_id,
                action.requesting_tool_execution_id,
                result_content_id,
                action.id,
            )
            .execute(&mut **transaction)
            .await
            .with_context(|| format!("closing pending action {}", action.id))?;
            sqlx::query!(
                "DELETE FROM pending_action WHERE sequence_id = ? AND id = ?",
                sequence_id,
                action.id,
            )
            .execute(&mut **transaction)
            .await
            .with_context(|| format!("retiring pending action {}", action.id))?;
        }
        Ok(())
    }

    /// A tool call failed while the server kept running: record the error as
    /// that call's result and mark the rest of the batch not executed.
    pub async fn close_failed_tool_calls(
        &self,
        inference_id: i64,
        failing_execution_id: Option<i64>,
        error: &str,
    ) -> Result<()> {
        let mut transaction = self
            .pool
            .begin()
            .await
            .context("starting failed tool call closure")?;
        Self::close_tool_calls(
            &mut transaction,
            inference_id,
            &CallClosure::Failed {
                failing_execution_id,
                error,
            },
        )
        .await?;
        transaction
            .commit()
            .await
            .context("committing failed tool call closure")
    }

    /// Called by the execution host before resuming jobs, never by read-only
    /// replay. Work lost with the process keeps what committed (design.md:
    /// "it never invents completed game effects") and invents no duration,
    /// token usage or output: open turns end in an error, each unanswered
    /// call gets an honest result, and pending actions are rejected, in one
    /// transaction so a repeated restart finds nothing more to close.
    pub async fn close_interrupted_turns(&self) -> Result<()> {
        let mut transaction = self
            .pool
            .begin()
            .await
            .context("starting interrupted turn closure")?;
        let inferences = sqlx::query_scalar!(
            "SELECT inference.id FROM inference \
             JOIN sequence ON sequence.id = inference.sequence_id \
             WHERE sequence.finished_at IS NULL AND inference.output IS NOT NULL \
             ORDER BY inference.id",
        )
        .fetch_all(&mut *transaction)
        .await
        .context("finding interrupted inferences")?;
        for inference_id in inferences {
            Self::close_tool_calls(&mut transaction, inference_id, &CallClosure::Interrupted)
                .await?;
        }
        let sequences =
            sqlx::query_scalar!("SELECT id FROM sequence WHERE finished_at IS NULL ORDER BY id")
                .fetch_all(&mut *transaction)
                .await
                .context("finding interrupted sequences")?;
        for sequence_id in sequences {
            Self::close_pending_actions(
                &mut transaction,
                sequence_id,
                "interrupted: the server stopped before the GM resolved this action",
            )
            .await?;
        }
        sqlx::query!(
            "UPDATE inference SET error = 'Inference interrupted by process restart', finished_at = CURRENT_TIMESTAMP WHERE finished_at IS NULL",
        )
        .execute(&mut *transaction)
        .await
        .context("marking interrupted inferences")?;
        sqlx::query!(
            "UPDATE invocation SET outcome = 'interrupted', finished_at = CURRENT_TIMESTAMP WHERE finished_at IS NULL",
        )
        .execute(&mut *transaction)
        .await
        .context("marking interrupted invocations")?;
        sqlx::query!(
            "UPDATE sequence SET error = 'Interrupted by process restart', finished_at = CURRENT_TIMESTAMP WHERE finished_at IS NULL",
        )
        .execute(&mut *transaction)
        .await
        .context("marking interrupted sequences")?;
        transaction
            .commit()
            .await
            .context("committing interrupted turn closure")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        llm::{Content, Response, Role, Sampling, ToolCall, Usage},
        scenario::Scenario,
        store::{AgentBranch, CompactionPolicy, InferenceInput, InferenceOutcome, TestDatabase},
    };

    /// A model turn that returned `names` as one tool batch: its assistant
    /// message is stored and the execution rows exist unfinished, exactly the
    /// state the agent loop leaves while the batch runs.
    async fn tool_batch(
        store: &Store,
        branch: AgentBranch,
        sequence_id: i64,
        names: &[&str],
    ) -> (i64, Vec<i64>) {
        let sampling = Sampling {
            temperature: 0.0,
            ..Default::default()
        };
        let invocation_id = store
            .start_invocation(Some(sequence_id), branch, None, None)
            .await
            .unwrap();
        let inference_id = store
            .start_inference(InferenceInput {
                agent_id: branch.agent_id(),
                sequence_id: Some(sequence_id),
                invocation_id,
                segments: &[],
                request: &crate::llm::Request {
                    messages: vec![],
                    tools: vec![],
                    sampling: sampling.clone(),
                },
                model: "scripted",
            })
            .await
            .unwrap();
        let calls = names
            .iter()
            .map(|name| ToolCall {
                id: format!("call-{name}"),
                name: name.to_string(),
                arguments: "{}".to_string(),
            })
            .collect();
        store
            .finish_chat_inference(
                inference_id,
                branch.agent_id(),
                InferenceOutcome::Response(Response {
                    content: Content::ToolCalls(calls),
                    reasoning: String::new(),
                    usage: Usage {
                        input_tokens: 1,
                        output_tokens: 1,
                    },
                }),
                0,
                CompactionPolicy {
                    sequence_id: Some(sequence_id),
                    compact_before_next_input_tokens: usize::MAX,
                    sampling: &sampling,
                    model: "scripted",
                    static_segments: &[],
                },
            )
            .await
            .unwrap();
        let executions = store
            .create_tool_executions(inference_id, names.len())
            .await
            .unwrap();
        (inference_id, executions)
    }

    /// Every tool call the agent's next inference would be shown, with the
    /// text of its result if it has one.
    async fn history_results(store: &Store, branch: AgentBranch) -> Vec<(String, Option<String>)> {
        let segments = store.live_segments(branch).await.unwrap();
        let request = store
            .request_for_segments(branch.agent_id(), &segments, Sampling::default())
            .await
            .unwrap();
        let mut results: Vec<(String, Option<String>)> = Vec::new();
        for message in request.messages {
            match message.content {
                MessageContent::ToolCalls(calls) => {
                    results.extend(calls.into_iter().map(|call| (call.id, None)));
                }
                MessageContent::ToolResult {
                    tool_call_id,
                    content,
                } => {
                    let call = results
                        .iter_mut()
                        .find(|(id, _)| *id == tool_call_id)
                        .expect("a result must answer an earlier call");
                    call.1 = Some(content);
                }
                _ => {}
            }
        }
        results
    }

    async fn agent_with_sequence(store: &Store) -> (AgentBranch, i64) {
        let world = store.create_world("test").await.unwrap();
        let agent = store.create_agent(world).await.unwrap();
        store
            .append_message(agent, &Message::text(Role::User, "Begin."))
            .await
            .unwrap();
        let sequence = store.begin_sequence(world, "test turn").await.unwrap();
        (store.agent_branch(agent).await.unwrap(), sequence.id)
    }

    async fn installed_player(store: &Store) -> crate::store::InstalledWorld {
        let owner = store
            .find_or_create_user("player@example.test", "Player")
            .await
            .unwrap();
        let scenario = Scenario::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/scenarios/bread_thief.json"
        ))
        .unwrap();
        let installed = store.install_scenario(&owner, &scenario).await.unwrap();
        store
            .append_message(
                installed.member.agent_id,
                &Message::text(Role::User, "Begin."),
            )
            .await
            .unwrap();
        installed
    }

    async fn message_count(store: &Store) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM message")
            .fetch_one(&store.pool)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn a_crash_between_calls_leaves_honest_results_and_valid_history() {
        let db = TestDatabase::new("interruption-test");
        let store = Store::open(db.path()).await.unwrap();
        let (branch, sequence) = agent_with_sequence(&store).await;
        let (inference, executions) =
            tool_batch(&store, branch, sequence, &["done", "active", "waiting"]).await;
        // The first call finished; the second had begun when the process died.
        store.begin_tool_execution(executions[0]).await.unwrap();
        store
            .complete_tool_result(
                inference,
                executions[0],
                "completed",
                &Message::tool_result("call-done".into(), "original result".into()),
            )
            .await
            .unwrap();
        store.begin_tool_execution(executions[1]).await.unwrap();

        store.close_interrupted_turns().await.unwrap();

        let results = history_results(&store, branch).await;
        assert_eq!(results.len(), 3);
        assert_eq!(results[0].1.as_deref(), Some("original result"));
        let active = results[1].1.as_deref().unwrap();
        assert!(
            active.contains("Interrupted") && active.contains("may already have been applied"),
            "a begun call must not be reported as having had no effect: {active}"
        );
        assert!(
            results[2].1.as_deref().unwrap().starts_with("Not executed"),
            "a call that never began did not run: {:?}",
            results[2]
        );
        let statuses: Vec<String> =
            sqlx::query_scalar("SELECT status FROM tool_execution ORDER BY ordinal")
                .fetch_all(&store.pool)
                .await
                .unwrap();
        assert_eq!(statuses, ["completed", "failed", "unexecuted"]);
        let sequence_error: Option<String> =
            sqlx::query_scalar("SELECT error FROM sequence WHERE id = ?")
                .bind(sequence)
                .fetch_one(&store.pool)
                .await
                .unwrap();
        assert_eq!(
            sequence_error.as_deref(),
            Some("Interrupted by process restart")
        );

        let messages = message_count(&store).await;
        store.close_interrupted_turns().await.unwrap();
        assert_eq!(
            message_count(&store).await,
            messages,
            "a second restart must add nothing"
        );
    }

    #[tokio::test]
    async fn a_crash_before_execution_rows_exist_marks_every_call_not_executed() {
        let db = TestDatabase::new("interruption-test");
        let store = Store::open(db.path()).await.unwrap();
        let (branch, sequence) = agent_with_sequence(&store).await;
        let (inference, _) = tool_batch(&store, branch, sequence, &["one", "two"]).await;
        sqlx::query("DELETE FROM tool_execution WHERE inference_id = ?")
            .bind(inference)
            .execute(&store.pool)
            .await
            .unwrap();

        store.close_interrupted_turns().await.unwrap();

        let results = history_results(&store, branch).await;
        assert_eq!(results.len(), 2);
        assert!(
            results.iter().all(|(_, result)| result
                .as_deref()
                .is_some_and(|text| text.starts_with("Not executed"))),
            "{results:?}"
        );
    }

    #[tokio::test]
    async fn an_interrupted_roll_reports_its_recorded_outcome() {
        let db = TestDatabase::new("interruption-test");
        let store = Store::open(db.path()).await.unwrap();
        let installed = installed_player(&store).await;
        let branch = store.agent_branch(installed.member.agent_id).await.unwrap();
        let sequence = store
            .begin_sequence(installed.world_id, "test turn")
            .await
            .unwrap();
        let (_, executions) =
            tool_batch(&store, branch, sequence.id, &["roll_hit_protection"]).await;
        store.begin_tool_execution(executions[0]).await.unwrap();
        // The roll committed with its recorded result; the process died before
        // the tool result message.
        let hp = store
            .roll_hit_protection(&installed.member, sequence.id, Some(executions[0]))
            .await
            .unwrap();

        store.close_interrupted_turns().await.unwrap();

        let results = history_results(&store, branch).await;
        let result = results[0].1.as_deref().unwrap();
        assert!(
            result.contains(&format!("\"hp\":{hp}")),
            "the committed roll must be reported, not denied: {result}"
        );
    }

    #[tokio::test]
    async fn a_pending_action_of_a_failed_or_interrupted_turn_cannot_be_approved_later() {
        for interrupted in [true, false] {
            let db = TestDatabase::new("interruption-test");
            let store = Store::open(db.path()).await.unwrap();
            let installed = installed_player(&store).await;
            let branch = store.agent_branch(installed.member.agent_id).await.unwrap();
            let sequence = store
                .begin_sequence(installed.world_id, "test turn")
                .await
                .unwrap();
            let (_, executions) = tool_batch(&store, branch, sequence.id, &["take"]).await;
            store.begin_tool_execution(executions[0]).await.unwrap();
            let pending = store
                .create_pending_action(
                    &installed.member,
                    sequence.id,
                    executions[0],
                    "take",
                    r#"{"item":"flour sack"}"#,
                )
                .await
                .unwrap();

            if interrupted {
                store.close_interrupted_turns().await.unwrap();
            } else {
                store
                    .finish_sequence(sequence.id, Some("backend exploded"))
                    .await
                    .unwrap();
            }

            assert!(
                !store
                    .has_pending_action(installed.world_id, pending.id)
                    .await
                    .unwrap(),
                "interrupted={interrupted}: the action must be closed"
            );
            let (approved, result): (bool, String) = sqlx::query_as(
                "SELECT action.approved, json_extract(recorded_content.body, '$.content.value') \
                 FROM action JOIN recorded_content ON recorded_content.id = action.result_content_id \
                 WHERE action.pending_action_id = ?",
            )
            .bind(pending.id)
            .fetch_one(&store.pool)
            .await
            .unwrap();
            assert!(!approved && result.starts_with("rejected:"), "{result}");
            let held: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM item_character")
                .fetch_one(&store.pool)
                .await
                .unwrap();
            assert_eq!(held, 0, "no item may move for a closed action");
        }
    }

    #[tokio::test]
    async fn a_failed_call_records_its_error_and_the_rest_of_the_batch_is_not_executed() {
        let db = TestDatabase::new("interruption-test");
        let store = Store::open(db.path()).await.unwrap();
        let (branch, sequence) = agent_with_sequence(&store).await;
        let (inference, executions) = tool_batch(&store, branch, sequence, &["a", "b", "c"]).await;
        store.begin_tool_execution(executions[0]).await.unwrap();
        store
            .complete_tool_result(
                inference,
                executions[0],
                "completed",
                &Message::tool_result("call-a".into(), "fine".into()),
            )
            .await
            .unwrap();
        store.begin_tool_execution(executions[1]).await.unwrap();

        store
            .close_failed_tool_calls(inference, Some(executions[1]), "disk full")
            .await
            .unwrap();

        let results = history_results(&store, branch).await;
        let texts: Vec<_> = results
            .iter()
            .map(|(_, result)| result.clone().unwrap())
            .collect();
        assert_eq!(texts[0], "fine");
        assert_eq!(texts[1], "Tool failed: disk full");
        assert!(texts[2].starts_with("Not executed"), "{texts:?}");
    }
}
