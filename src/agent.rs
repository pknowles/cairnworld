use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, bail, ensure};

use crate::{
    context,
    llm::{Backend, Content, Message, Response, Sampling},
    settings::Limits,
    store::{AgentBranch, CompactionPolicy, Store},
    tools::{self, Tool},
};

/// Inference budget for one external trigger, shared by every agent it
/// reaches so recursive calls draw from the same total. Exhausting either
/// bound is a hard error: the bounds only ever fire on a loop that is already
/// wrong, so the failure must reach the user rather than an agent.
struct BudgetState {
    limits: Limits,
    total_spent: u32,
}

/// Inference budget for one external trigger. Clones share the same counter so
/// a nested GM or NPC call cannot reset the trigger-wide limit.
#[derive(Clone)]
pub struct Budget {
    state: Arc<Mutex<BudgetState>>,
}

impl Budget {
    pub fn new(limits: Limits) -> Self {
        Self {
            state: Arc::new(Mutex::new(BudgetState {
                limits,
                total_spent: 0,
            })),
        }
    }

    /// A budget that has already spent `spent` inferences: the saved counter of
    /// a discussion whose earlier steps ran in other call stacks.
    pub fn restore(limits: Limits, spent: u32) -> Self {
        Self {
            state: Arc::new(Mutex::new(BudgetState {
                limits,
                total_spent: spent,
            })),
        }
    }

    /// Charge one inference against the whole trigger and the current chat.
    pub fn spend(&self, chat_spent: u32) -> Result<()> {
        let mut state = self.state.lock().expect("inference budget poisoned");
        if chat_spent >= state.limits.max_inferences_per_chat {
            bail!(
                "chat reached its limit of {} inferences without settling on a reply \
                 (limits.max_inferences_per_chat)",
                state.limits.max_inferences_per_chat
            );
        }
        if state.total_spent >= state.limits.max_inferences_total {
            bail!(
                "this action reached its limit of {} inferences across all agents \
                 (limits.max_inferences_total)",
                state.limits.max_inferences_total
            );
        }
        state.total_spent += 1;
        Ok(())
    }

    pub fn limits(&self) -> Limits {
        self.state.lock().expect("inference budget poisoned").limits
    }

    /// Inferences charged so far, to checkpoint between steps.
    pub fn spent(&self) -> u32 {
        self.state
            .lock()
            .expect("inference budget poisoned")
            .total_spent
    }
}

/// Per-trigger provenance shared by every nested agent call.
#[derive(Clone)]
pub struct CallContext {
    sequence_id: Option<i64>,
    parent_tool_execution_id: Option<i64>,
    agents: Arc<Mutex<Vec<i64>>>,
}

impl CallContext {
    pub fn root(sequence_id: Option<i64>) -> Self {
        Self {
            sequence_id,
            parent_tool_execution_id: None,
            agents: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// A nested agent call spawned by `parent_tool_execution_id` (design.md's
    /// `invocation.parent_tool_execution_id`: "Each nested invocation records
    /// this exact parent execution").
    pub fn child(&self, parent_tool_execution_id: i64) -> Self {
        Self {
            sequence_id: self.sequence_id,
            parent_tool_execution_id: Some(parent_tool_execution_id),
            agents: Arc::clone(&self.agents),
        }
    }

    fn enter(&self, agent_id: i64) -> Result<ActiveAgent> {
        let mut agents = self.agents.lock().expect("agent call stack poisoned");
        ensure!(
            !agents.contains(&agent_id),
            "agent {agent_id} would recursively call itself"
        );
        agents.push(agent_id);
        Ok(ActiveAgent {
            agents: Arc::clone(&self.agents),
            agent_id,
        })
    }
}

struct ActiveAgent {
    agents: Arc<Mutex<Vec<i64>>>,
    agent_id: i64,
}

impl Drop for ActiveAgent {
    fn drop(&mut self) {
        let removed = self.agents.lock().expect("agent call stack poisoned").pop();
        debug_assert_eq!(removed, Some(self.agent_id));
    }
}

/// All stored inputs for one agent completion. The callbacks that surface its
/// live output remain invocation-local rather than becoming persisted state.
pub struct Turn<'a> {
    pub events: tokio::sync::broadcast::Sender<crate::inspection::DevEvent>,
    /// The branch selected and validated before this invocation begins.
    pub branch: AgentBranch,
    pub static_messages: &'a [Message],
    pub dynamic_context: &'a [&'a str],
    pub tools: &'a [Tool],
    pub sampling: Sampling,
    pub model: &'a str,
}

/// A settled turn's final non-tool completion and the recorded_content reference its
/// own message already stores - so a caller (e.g. GM narration) can deliver
/// that exact reference to other recipients instead of re-recording the same
/// completion. Only returned once the loop has actually settled, not for an
/// intermediate tool-calling turn.
pub struct Settled {
    pub response: Response,
    /// The final non-tool inference that ended this invocation.
    pub inference_id: i64,
    pub output_content_id: i64,
    /// Whether this invocation executed a model-requested tool before its
    /// final response.
    pub used_tools: bool,
}

/// RAII guard over one `invocation` row (design.md: "An invocation wrapper
/// finishes its own outcome on every exit, including schema rejection,
/// execution error and safety-budget exhaustion"). `finish` records the real
/// outcome on the ordinary return path; anything still armed on drop (an
/// early `?` propagation this function did not explicitly finish) is an
/// unexpected interruption, not a normal outcome, and is recorded as such
/// rather than left permanently "running".
struct InvocationGuard<'a> {
    store: &'a Store,
    invocation_id: i64,
    finished: bool,
}

impl<'a> InvocationGuard<'a> {
    async fn start(
        store: &'a Store,
        sequence_id: Option<i64>,
        branch: AgentBranch,
        parent_tool_execution_id: Option<i64>,
    ) -> Result<Self> {
        let invocation_id = store
            .start_invocation(sequence_id, branch, parent_tool_execution_id, None)
            .await
            .context("starting agent invocation")?;
        Ok(Self {
            store,
            invocation_id,
            finished: false,
        })
    }

    async fn finish(mut self, outcome: &str) -> Result<()> {
        self.finished = true;
        self.store
            .finish_invocation(self.invocation_id, outcome)
            .await
            .context("finishing agent invocation")
    }
}

impl Drop for InvocationGuard<'_> {
    fn drop(&mut self) {
        // complete_with_call_context's match calls finish() on both the Ok
        // and Err arms before returning, so reaching drop still armed cannot
        // happen on any path that exists today. A detached background write
        // here would not actually be "failing loudly" - it would silently
        // paper over exactly the bug this exists to catch. If a future edit
        // adds a return path that skips finish(), this must fail the test/
        // debug build that exercises it, not quietly self-heal at runtime;
        // process-restart recovery (close_interrupted_turns) already
        // exists as the sanctioned recovery path for a genuine process
        // death (e.g. a panic), which Drop cannot reliably run for anyway.
        debug_assert!(
            self.finished,
            "invocation {} dropped without an explicit outcome - a return path skipped finish()",
            self.invocation_id
        );
    }
}

/// Resolve one chat turn, recording every model response and tool result in order.
pub async fn complete<B: Backend>(
    store: &Store,
    backend: &B,
    budget: &Budget,
    turn: Turn<'_>,
    on_token: impl FnMut(&str) + Send,
    on_activity: impl FnMut(String),
) -> Result<Response> {
    let call = CallContext::root(None);
    complete_with_call_context(store, backend, budget, turn, &call, on_token, on_activity)
        .await
        .map(|settled| settled.response)
}

/// Resolve a turn while retaining its external sequence and, for a nested
/// agent call, the tool_execution that caused it.
pub async fn complete_with_call_context<B: Backend>(
    store: &Store,
    backend: &B,
    budget: &Budget,
    turn: Turn<'_>,
    call: &CallContext,
    mut on_token: impl FnMut(&str) + Send,
    mut on_activity: impl FnMut(String),
) -> Result<Settled> {
    let _active = call.enter(turn.branch.agent_id())?;
    let invocation = InvocationGuard::start(
        store,
        call.sequence_id,
        turn.branch,
        call.parent_tool_execution_id,
    )
    .await?;
    let invocation_id = invocation.invocation_id;
    let result: Result<Settled> = async {
        backend
            .before_agent(store, turn.branch, &turn.events)
            .await
            .context("waiting for earlier deferred work for this agent")?;
        let definitions =
            tools::definitions(turn.tools).context("validating declared tool schemas")?;
        let mut chat_spent = 0;
        let mut used_tools = false;
        loop {
            budget
                .spend(chat_spent)
                .context("resolving this chat turn")?;
            chat_spent += 1;
            let completion = context::complete_recorded(
                store,
                backend,
                context::RecordedCompletion {
                    events: turn.events.clone(),
                    branch: turn.branch,
                    sequence_id: call.sequence_id,
                    invocation_id,
                    static_messages: turn.static_messages,
                    dynamic_context: turn.dynamic_context,
                    tools: &definitions,
                    sampling: turn.sampling.clone(),
                    model: turn.model,
                    compact_before_next_input_tokens: budget
                        .limits()
                        .compact_before_next_input_tokens,
                },
                &mut on_token,
            )
            .await
            .context("running recorded agent inference")?;
            let response = completion.response;
            // finish_chat_inference already wrote this reply as the agent's
            // own message (and, if it settled, any due compaction obligation)
            // atomic with the inference completion. Tool calls still need
            // their results executed and appended below; a settled text
            // reply is done.
            let Content::ToolCalls(calls) = &response.content else {
                backend
                    .after_agent(store, turn.branch, &turn.events)
                    .await
                    .context("admitting any due deferred compaction")?;
                let output_content_id = completion
                    .output_content_id
                    .context("a settled reply always has an output content reference")?;
                return Ok(Settled {
                    response,
                    inference_id: completion.inference_id,
                    output_content_id,
                    used_tools,
                });
            };
            used_tools = true;
            // Create every call's tool_execution row up front, keyed by its
            // output ordinal, before any of them execute (design.md: "Create
            // rows for every call before executing the batch") - so sibling
            // and not-yet-reached calls are always visible even if an
            // earlier call in the batch fails outright.
            let executions = store
                .create_tool_executions(completion.inference_id, calls.len())
                .await
                .context("creating tool execution records")?;
            // Each call's execution, result recording and after-result
            // delivery run as one awaited block so a failure anywhere in it -
            // not only in execution itself - is caught by the same outer
            // match, rather than an after-result `?` bypassing sibling
            // finalization below (design.md: "One awaited batch boundary
            // covers tool execution, result persistence and result
            // delivery"). A failure leaves no call without a result: the
            // failing call's result is the real error, unless its own result
            // had already committed, in which case that result and any
            // completed mutation stand - never re-recorded as the call's
            // failure, never re-run. Every call not yet run is closed as not
            // executed.
            // Grouping proposals are never committed here (design.md's "Tool
            // boundary"): a topology change would retarget every sibling call
            // still to come in this same batch away from the group the model
            // actually saw when it wrote them. Collected in call order and
            // applied together, once, only after every ordinary call in the
            // batch has settled with no error.
            let mut pending_changes = Vec::new();
            // Deliveries put messages into histories, possibly the caller's
            // own, so they wait until every call of the batch has its result:
            // a message between a call and a sibling's result would break the
            // history.
            let mut deliveries = Vec::new();
            for (tool_call, tool_execution_id) in calls.iter().zip(&executions) {
                on_activity(format!(
                    "tool call {}: {}",
                    tool_call.name, tool_call.arguments
                ));
                let outcome: Result<()> = async {
                    store
                        .begin_tool_execution(*tool_execution_id)
                        .await
                        .with_context(|| format!("beginning tool call {}", tool_call.id))?;
                    let outcome = tools::execute(turn.tools, tool_call, *tool_execution_id)
                        .await
                        .with_context(|| format!("executing tool call {}", tool_call.id))?;
                    let (status, result, after_result) = match outcome {
                        tools::ToolOutcome::Completed(content) => ("completed", content, None),
                        tools::ToolOutcome::CompletedAfter {
                            content,
                            after_result,
                        } => ("completed", content, Some(after_result)),
                        tools::ToolOutcome::Rejected(content) => ("rejected", content, None),
                        tools::ToolOutcome::PendingTopologyChange(change) => {
                            pending_changes.push((
                                tool_call.id.clone(),
                                *tool_execution_id,
                                change,
                            ));
                            return Ok(());
                        }
                    };
                    store
                        .complete_tool_result(
                            completion.inference_id,
                            *tool_execution_id,
                            status,
                            &Message::tool_result(tool_call.id.clone(), result.clone()),
                        )
                        .await
                        .with_context(|| {
                            format!("storing result for tool call {}", tool_call.id)
                        })?;
                    on_activity(format!("tool result {}: {result}", tool_call.id));
                    if let Some(after_result) = after_result {
                        deliveries.push((tool_call.id.clone(), after_result));
                    }
                    Ok(())
                }
                .await;
                if let Err(error) = outcome {
                    return Err(close_failed_calls(
                        store,
                        completion.inference_id,
                        Some(*tool_execution_id),
                        error,
                    )
                    .await);
                }
            }
            for (call_id, delivery) in deliveries {
                if let Err(error) = delivery
                    .await
                    .with_context(|| format!("delivering result of tool call {call_id}"))
                {
                    // Every call has its result except a deferred group change,
                    // which reports the failure.
                    return Err(
                        close_failed_calls(store, completion.inference_id, None, error).await,
                    );
                }
            }
            if !pending_changes.is_empty() {
                // A failure here leaves each deferred call without a result.
                let applied: Result<()> = async {
                    let pending_changes = store
                        .resolve_topology_changes(completion.inference_id, pending_changes)
                        .await
                        .context("resolving group proposals in the invoking Game Master's world")?;
                    compact_merge_sources(store, backend, &pending_changes, budget.limits(), &turn)
                        .await
                        .context(
                            "compacting merge source branches before applying topology changes",
                        )?;
                    store
                        .apply_topology_changes(
                            pending_changes,
                            budget.limits().presence_departure_seconds,
                        )
                        .await
                        .context("applying group topology changes")
                }
                .await;
                if let Err(error) = applied {
                    return Err(
                        close_failed_calls(store, completion.inference_id, None, error).await,
                    );
                }
            }
        }
    }
    .await;
    match result {
        Ok(settled) => {
            invocation.finish("settled").await?;
            Ok(settled)
        }
        Err(error) => {
            // The turn's own error is the one the caller needs; if finishing
            // the invocation row also fails (e.g. an unexpected double-finish
            // race), that's a secondary bookkeeping problem and must not
            // replace or hide the real cause of this turn's failure.
            if let Err(finish_error) = invocation.finish("error").await {
                tracing::error!(
                    error = %format!("{finish_error:#}"),
                    "failed to finish invocation after a turn error"
                );
            }
            Err(error)
        }
    }
}

/// Close a failed batch's calls in history and return the error to propagate.
/// If closing itself fails, that failure is added to the original error's context.
async fn close_failed_calls(
    store: &Store,
    inference_id: i64,
    failing_execution_id: Option<i64>,
    error: anyhow::Error,
) -> anyhow::Error {
    match store
        .close_failed_tool_calls(inference_id, failing_execution_id, &format!("{error:#}"))
        .await
    {
        Ok(()) => error,
        Err(close_error) => error.context(format!(
            "also failed to record the failure in history: {close_error:#}"
        )),
    }
}

/// Before a merge commits, each source group's own branch may need
/// compacting first (design.md's "Groups and shared GM history": "Before
/// committing a merge, each incoming branch's unique suffix is eligible for
/// compaction if its raw-message count exceeds `merge_branch_messages`...
/// Run these jobs before capturing the final parent cuts, so their
/// activation entries belong to the inherited history" and "group changes
/// wait for affected pending compactions"). This runs before
/// `apply_topology_changes`'s own transaction - no database transaction may
/// span inference execution - so `apply_merge` naturally reads each source
/// branch's already-updated settled position as its parent cut, with no
/// change needed there. Only proposals resolved in the invoking Game Master's
/// world expose sources here; rejected proposals cannot trigger preparation.
///
/// A branch may already have a pending job unrelated to this merge (e.g. an
/// ordinary token-threshold trigger, independent of raw message count) even
/// when its own count is under `merge_branch_messages` - `backend
/// .before_agent` is used rather than calling `compaction::run` directly so
/// an already-pending job is waited on through the same admission path a
/// concurrently running deferred compaction uses, never resolved twice.
async fn compact_merge_sources<B: Backend>(
    store: &Store,
    backend: &B,
    changes: &crate::store::TopologyChanges,
    limits: Limits,
    turn: &Turn<'_>,
) -> Result<()> {
    for branch in changes.merge_branches() {
        let branch_id = branch.branch_id();
        if store.uncovered_message_count(branch_id).await? > limits.merge_branch_messages as i64 {
            store
                .enqueue_capacity_compaction(
                    branch,
                    0,
                    None,
                    CompactionPolicy {
                        sequence_id: None,
                        compact_before_next_input_tokens: 0,
                        sampling: &turn.sampling,
                        model: turn.model,
                        static_segments: &[],
                    },
                )
                .await
                .with_context(|| {
                    format!("enqueueing merge-time compaction for branch {branch_id}")
                })?;
        }
        backend
            .before_agent(store, branch, &turn.events)
            .await
            .with_context(|| {
                format!("waiting for compaction on merge source branch {branch_id}")
            })?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{collections::VecDeque, sync::Mutex};

    use sqlx::query;

    use super::*;
    use crate::llm::{MessageContent, Role, ToolCall, ToolDefinition, Usage};
    use crate::settings::Limits;

    struct ScriptedBackend {
        responses: Mutex<VecDeque<Response>>,
        requests: Mutex<Vec<crate::llm::Request>>,
    }

    impl ScriptedBackend {
        fn new(responses: impl IntoIterator<Item = Response>) -> Self {
            Self {
                responses: Mutex::new(responses.into_iter().collect()),
                requests: Mutex::new(Vec::new()),
            }
        }
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
                .context("scripted backend received an unexpected inference")
        }
    }

    fn response(content: Content, reasoning: &str) -> Response {
        Response {
            content,
            reasoning: reasoning.to_string(),
            usage: Usage {
                input_tokens: 1,
                output_tokens: 1,
            },
        }
    }

    async fn test_store() -> (Store, crate::store::TestDatabase, i64) {
        let db = crate::store::TestDatabase::new("agent-test");
        let store = Store::open(db.path()).await.unwrap();
        let world = store.create_world("test").await.unwrap();
        let agent = store.create_agent(world).await.unwrap();
        store
            .append_message(
                agent,
                &Message::text(Role::User, "Rook tries to dodge the falling beam."),
            )
            .await
            .unwrap();
        (store, db, agent)
    }

    fn scripted_turn<'a>(branch: AgentBranch, tools: &'a [Tool]) -> Turn<'a> {
        Turn {
            events: tokio::sync::broadcast::channel(256).0,
            branch,
            static_messages: &[],
            dynamic_context: &[],
            tools,
            sampling: Sampling {
                temperature: 0.0,
                ..Default::default()
            },
            model: "scripted",
        }
    }

    fn test_echo_arguments(text: &str) -> String {
        format!(r#"{{"text":"{text}"}}"#)
    }

    async fn history(store: &Store, agent: i64) -> Vec<Message> {
        let segments = store
            .live_segments(store.agent_branch(agent).await.unwrap())
            .await
            .unwrap();
        store
            .request_for_segments(
                agent,
                &segments,
                Sampling {
                    temperature: 0.0,
                    ..Default::default()
                },
            )
            .await
            .unwrap()
            .messages
    }

    #[tokio::test]
    async fn tool_call_is_persisted_then_its_actual_result_enters_the_next_inference() {
        let (store, _db, agent_id) = test_store().await;
        let backend = ScriptedBackend::new([
            response(
                Content::ToolCalls(vec![ToolCall {
                    id: "echo-1".to_string(),
                    name: "test_echo".to_string(),
                    arguments: test_echo_arguments("Rook"),
                }]),
                "private scratch work",
            ),
            response(Content::Text("The beam catches you.".to_string()), ""),
        ]);
        let mut activity = Vec::new();
        let final_response = complete(
            &store,
            &backend,
            &Budget::new(Limits::default()),
            scripted_turn(
                store.agent_branch(agent_id).await.unwrap(),
                &[tools::test_echo()],
            ),
            |_| {},
            |event| activity.push(event),
        )
        .await
        .unwrap();

        assert_eq!(
            final_response.content,
            Content::Text("The beam catches you.".to_string())
        );
        assert_eq!(store.inference_count().await.unwrap(), 2);
        assert!(
            matches!(activity.as_slice(), [call, result]
                if call.starts_with("tool call test_echo:")
                    && result.starts_with("tool result echo-1:")),
            "tool activity must be emitted as each call and result occurs: {activity:?}"
        );
        {
            let requests = backend.requests.lock().unwrap();
            assert_eq!(
                requests[0].tools,
                tools::definitions(&[tools::test_echo()]).unwrap()
            );
            // The second inference must see the callback result, not the call arguments.
            assert!(matches!(requests[1].messages.as_slice(), [
                Message { content: MessageContent::Text(_), .. },
                Message { content: MessageContent::ToolCalls(calls), reasoning, .. },
                Message { content: MessageContent::ToolResult { tool_call_id, content }, .. },
            ] if calls[0].id == "echo-1"
                && reasoning.is_empty()
                && tool_call_id == "echo-1"
                && content == "Rook"));
        }
        let entries = history(&store, agent_id).await;
        // Reasoning is stored on the message but never replayed into context.
        assert!(
            matches!(&entries[1], Message { content: MessageContent::ToolCalls(_), reasoning, .. } if reasoning.is_empty())
        );
        assert!(
            matches!(&entries[2].content, MessageContent::ToolResult { tool_call_id, content }
                if tool_call_id == "echo-1" && content == "Rook")
        );

        // Provenance: the assistant call message and its result both link to the
        // first inference; the result names the exact tool_execution it
        // answers. The user message that opened the turn links to no inference.
        let produced = store.inference_message_ids(1).await.unwrap();
        assert!(
            matches!(
                produced.as_slice(),
                [
                    crate::store::InferenceMessage {
                        role: Role::Assistant,
                        answers_tool_execution_id: None,
                        ..
                    },
                    crate::store::InferenceMessage {
                        role: Role::Tool,
                        answers_tool_execution_id: Some(_),
                        ..
                    },
                ]
            ),
            "inference 1 produced the tool-call message and its result, keyed to its tool_execution: {produced:?}"
        );
        let second = store.inference_message_ids(2).await.unwrap();
        assert!(
            matches!(
                second.as_slice(),
                [crate::store::InferenceMessage {
                    role: Role::Assistant,
                    answers_tool_execution_id: None,
                    ..
                }]
            ),
            "the final inference produced only its assistant reply: {second:?}"
        );
        assert_eq!(
            store.message_source_inference_for_test(agent_id, 0).await,
            None,
            "player input is attributed to no inference"
        );
    }

    /// The plan's headline reconstruction target: an inference carrying real
    /// tool definitions must reassemble hash-equal, and a mutated definition
    /// must be rejected rather than silently reconstructing something else.
    #[tokio::test]
    async fn inferences_carrying_tools_reconstruct_and_detect_tampering() {
        let (store, _db, agent_id) = test_store().await;
        let backend =
            ScriptedBackend::new([response(Content::Text("Nothing to roll.".into()), "")]);
        complete(
            &store,
            &backend,
            &Budget::new(Limits::default()),
            scripted_turn(
                store.agent_branch(agent_id).await.unwrap(),
                &[tools::test_echo()],
            ),
            |_| {},
            |_| {},
        )
        .await
        .unwrap();

        let recorded = store.reconstruct_inference(1).await.unwrap();
        assert_eq!(
            recorded.request.tools,
            tools::definitions(&[tools::test_echo()]).unwrap(),
            "the recorded recipe must rebuild the exact tool list that was sent"
        );

        let pool = store.pool();
        let rows = query("UPDATE text SET content = '[]' WHERE content LIKE ?")
            .bind("%\"name\":\"test_echo\"%")
            .execute(&pool)
            .await
            .unwrap()
            .rows_affected();
        assert_eq!(rows, 1, "the test fixture must corrupt one tool definition");
        let error = store
            .reconstruct_inference(1)
            .await
            .expect_err("a mutated tool definition must fail reconstruction");
        assert!(format!("{error:#}").contains("does not match"), "{error:#}");
    }

    #[tokio::test]
    async fn unavailable_tool_becomes_a_result_the_model_can_explain() {
        let (store, _db, agent_id) = test_store().await;
        let backend = ScriptedBackend::new([
            response(
                Content::ToolCalls(vec![ToolCall {
                    id: "bad".to_string(),
                    name: "not_a_tool".to_string(),
                    arguments: "{}".to_string(),
                }]),
                "",
            ),
            response(Content::Text("I cannot do that here.".to_string()), ""),
        ]);
        let response = complete(
            &store,
            &backend,
            &Budget::new(Limits::default()),
            scripted_turn(
                store.agent_branch(agent_id).await.unwrap(),
                &[tools::test_echo()],
            ),
            |_| {},
            |_| {},
        )
        .await
        .unwrap();

        assert!(
            matches!(response.content, Content::Text(ref text) if text == "I cannot do that here.")
        );
        assert_eq!(store.inference_count().await.unwrap(), 2);
        assert!(matches!(
            history(&store, agent_id).await.as_slice(),
            [
                Message {
                    content: MessageContent::Text(_),
                    ..
                },
                Message {
                    content: MessageContent::ToolCalls(_),
                    ..
                },
                Message {
                    content: MessageContent::ToolResult { .. },
                    ..
                },
                Message {
                    content: MessageContent::Text(_),
                    ..
                },
            ]
        ));
    }

    #[tokio::test]
    async fn rejected_known_tool_arguments_are_returned_to_the_model() {
        let (store, _db, agent_id) = test_store().await;
        let backend = ScriptedBackend::new([
            response(
                Content::ToolCalls(vec![ToolCall {
                    id: "bad-arguments".to_string(),
                    name: "test_echo".to_string(),
                    arguments: "{}".to_string(),
                }]),
                "",
            ),
            response(Content::Text("I need text to echo.".to_string()), ""),
        ]);

        complete(
            &store,
            &backend,
            &Budget::new(Limits::default()),
            scripted_turn(
                store.agent_branch(agent_id).await.unwrap(),
                &[tools::test_echo()],
            ),
            |_| {},
            |_| {},
        )
        .await
        .unwrap();

        let entries = history(&store, agent_id).await;
        let MessageContent::ToolResult { content, .. } = &entries[2].content else {
            panic!("the rejected call must be persisted as its tool result");
        };
        assert!(
            content.contains("Tool arguments were rejected: parsing test echo arguments"),
            "got {content}"
        );
        assert_eq!(store.inference_count().await.unwrap(), 2);
    }

    #[tokio::test]
    async fn multiple_tool_results_keep_their_call_ids() {
        let (store, _db, agent_id) = test_store().await;
        let backend = ScriptedBackend::new([
            response(
                Content::ToolCalls(vec![
                    ToolCall {
                        id: "rook-echo".to_string(),
                        name: "test_echo".to_string(),
                        arguments: test_echo_arguments("Rook"),
                    },
                    ToolCall {
                        id: "mara-echo".to_string(),
                        name: "test_echo".to_string(),
                        arguments: test_echo_arguments("Mara"),
                    },
                ]),
                "",
            ),
            response(Content::Text("Both saves are resolved.".to_string()), ""),
        ]);
        complete(
            &store,
            &backend,
            &Budget::new(Limits::default()),
            scripted_turn(
                store.agent_branch(agent_id).await.unwrap(),
                &[tools::test_echo()],
            ),
            |_| {},
            |_| {},
        )
        .await
        .unwrap();

        let entries = history(&store, agent_id).await;
        let MessageContent::ToolResult {
            tool_call_id,
            content,
        } = &entries[2].content
        else {
            panic!("first tool result should be stored");
        };
        // Each result must remain paired with its own call rather than a
        // positional assumption.
        assert_eq!(tool_call_id, "rook-echo");
        assert!(content.contains("Rook"), "got {content}");
        let MessageContent::ToolResult {
            tool_call_id,
            content,
        } = &entries[3].content
        else {
            panic!("second tool result should be stored");
        };
        assert_eq!(tool_call_id, "mara-echo");
        assert!(content.contains("Mara"), "got {content}");
    }

    /// A delivery failure after a call's result already committed (design.md:
    /// "One awaited batch boundary covers tool execution, result persistence
    /// and result delivery") must not re-record that call as failed or leave
    /// its result unset. Deliveries run once every call of the batch has its
    /// result, so the sibling still runs, and the failure surfaces as the
    /// turn's failure.
    #[tokio::test]
    async fn a_delivery_failure_after_commit_does_not_undo_any_result() {
        let (store, _db, agent_id) = test_store().await;
        let delivers_then_fails = Tool::new(
            ToolDefinition {
                name: "delivers_then_fails".to_string(),
                description: "Completes its result, then fails delivering it.".to_string(),
                schema: serde_json::json!({"type": "object", "properties": {}}),
            },
            |_context| {
                Box::pin(async move {
                    Ok(tools::ToolOutcome::CompletedAfter {
                        content: "delivered".to_string(),
                        after_result: Box::pin(
                            async move { anyhow::bail!("delivery channel closed") },
                        ),
                    })
                }) as tools::ToolFuture
            },
        );
        let backend = ScriptedBackend::new([response(
            Content::ToolCalls(vec![
                ToolCall {
                    id: "first-call".to_string(),
                    name: "delivers_then_fails".to_string(),
                    arguments: "{}".to_string(),
                },
                ToolCall {
                    id: "second-call".to_string(),
                    name: "test_echo".to_string(),
                    arguments: test_echo_arguments("never reached"),
                },
            ]),
            "",
        )]);
        let error = complete(
            &store,
            &backend,
            &Budget::new(Limits::default()),
            scripted_turn(
                store.agent_branch(agent_id).await.unwrap(),
                &[delivers_then_fails, tools::test_echo()],
            ),
            |_| {},
            |_| {},
        )
        .await
        .expect_err("a delivery failure must surface as the turn's own error");
        assert!(
            format!("{error:#}").contains("delivery channel closed"),
            "{error:#}"
        );
        let (first_status, second_status): (String, String) = sqlx::query_as(
            "SELECT \
                (SELECT status FROM tool_execution WHERE ordinal = 0), \
                (SELECT status FROM tool_execution WHERE ordinal = 1)",
        )
        .fetch_one(&store.pool())
        .await
        .unwrap();
        assert_eq!(
            first_status, "completed",
            "the first call's own result committed before delivery failed and must stand"
        );
        assert_eq!(
            second_status, "completed",
            "deliveries wait for the whole batch, so the sibling ran before the delivery failed"
        );
    }

    /// A system failure aborts the turn, but history must still show the
    /// agent exactly what happened: the real error as the failed call's
    /// result, and the calls that never ran as not executed.
    #[tokio::test]
    async fn a_failing_call_leaves_its_error_and_unrun_siblings_in_history() {
        let (store, _db, agent_id) = test_store().await;
        let explodes = Tool::new(
            ToolDefinition {
                name: "explodes".to_string(),
                description: "Always fails.".to_string(),
                schema: serde_json::json!({"type": "object", "properties": {}}),
            },
            |_context| Box::pin(async move { anyhow::bail!("disk full") }) as tools::ToolFuture,
        );
        let backend = ScriptedBackend::new([response(
            Content::ToolCalls(vec![
                ToolCall {
                    id: "boom".to_string(),
                    name: "explodes".to_string(),
                    arguments: "{}".to_string(),
                },
                ToolCall {
                    id: "after".to_string(),
                    name: "test_echo".to_string(),
                    arguments: test_echo_arguments("never reached"),
                },
            ]),
            "",
        )]);
        let error = complete(
            &store,
            &backend,
            &Budget::new(Limits::default()),
            scripted_turn(
                store.agent_branch(agent_id).await.unwrap(),
                &[explodes, tools::test_echo()],
            ),
            |_| {},
            |_| {},
        )
        .await
        .expect_err("a failing tool must fail the turn");
        assert!(format!("{error:#}").contains("disk full"), "{error:#}");

        let entries = history(&store, agent_id).await;
        let results: Vec<(&str, &str)> = entries
            .iter()
            .filter_map(|entry| match &entry.content {
                MessageContent::ToolResult {
                    tool_call_id,
                    content,
                } => Some((tool_call_id.as_str(), content.as_str())),
                _ => None,
            })
            .collect();
        assert!(
            matches!(results.as_slice(), [("boom", failed), ("after", skipped)]
                if failed.contains("disk full") && skipped.starts_with("Not executed")),
            "every call needs a result before the next inference: {results:?}"
        );
    }

    /// A model that keeps calling a tool must be stopped by the budget rather
    /// than looping forever.
    #[tokio::test]
    async fn a_model_that_never_settles_is_stopped_by_the_chat_limit() {
        let (store, _db, agent_id) = test_store().await;
        let limits = Limits {
            max_concurrent_inferences: 4,
            max_inferences_per_chat: 3,
            max_inferences_total: 64,
            max_context_tokens: 33_792,
            max_output_tokens: 1_024,
            compact_before_next_input_tokens: 32_768,
            keep_raw_messages: 32,
            merge_branch_messages: 64,
            presence_departure_seconds: 60,
        };
        let repeat = || {
            response(
                Content::ToolCalls(vec![ToolCall {
                    id: "again".to_string(),
                    name: "test_echo".to_string(),
                    arguments: test_echo_arguments("Rook"),
                }]),
                "",
            )
        };
        let backend = ScriptedBackend::new([repeat(), repeat(), repeat(), repeat(), repeat()]);
        let budget = Budget::new(limits);
        let error = complete(
            &store,
            &backend,
            &budget,
            scripted_turn(
                store.agent_branch(agent_id).await.unwrap(),
                &[tools::test_echo()],
            ),
            |_| {},
            |_| {},
        )
        .await
        .expect_err("an endless tool loop must be stopped");

        let message = format!("{error:#}");
        assert!(message.contains("max_inferences_per_chat"), "got {message}");
        assert_eq!(
            budget.spent(),
            limits.max_inferences_per_chat,
            "the loop must stop at the limit, not after it"
        );
    }

    /// The total bound is shared across agents, so a chat that stays under its
    /// own limit still stops once the whole action has spent its budget.
    #[tokio::test]
    async fn the_total_limit_stops_a_chat_that_is_within_its_own_limit() {
        let (store, _db, agent_id) = test_store().await;
        let budget = Budget::new(Limits {
            max_concurrent_inferences: 4,
            max_inferences_per_chat: 100,
            max_inferences_total: 2,
            max_context_tokens: 33_792,
            max_output_tokens: 1_024,
            compact_before_next_input_tokens: 32_768,
            keep_raw_messages: 32,
            merge_branch_messages: 64,
            presence_departure_seconds: 60,
        });
        let repeat = || {
            response(
                Content::ToolCalls(vec![ToolCall {
                    id: "again".to_string(),
                    name: "test_echo".to_string(),
                    arguments: test_echo_arguments("Rook"),
                }]),
                "",
            )
        };
        let backend = ScriptedBackend::new([repeat(), repeat(), repeat(), repeat()]);
        let error = complete(
            &store,
            &backend,
            &budget,
            scripted_turn(
                store.agent_branch(agent_id).await.unwrap(),
                &[tools::test_echo()],
            ),
            |_| {},
            |_| {},
        )
        .await
        .expect_err("the shared budget must stop the action");

        let message = format!("{error:#}");
        assert!(message.contains("max_inferences_total"), "got {message}");
    }

    #[test]
    fn nested_calls_share_one_budget() {
        let budget = Budget::new(Limits::default());
        let nested = budget.clone();
        budget.spend(0).unwrap();
        nested.spend(0).unwrap();
        assert_eq!(budget.spent(), 2);
    }

    #[test]
    fn call_context_rejects_an_agent_already_on_the_call_stack() {
        let call = CallContext::root(Some(12));
        let active = call.enter(4).unwrap();
        let error = match call.child(99).enter(4) {
            Ok(_) => panic!("an agent must not recursively invoke itself"),
            Err(error) => error,
        };
        assert!(format!("{error:#}").contains("recursively call itself"));
        drop(active);
        call.child(99)
            .enter(4)
            .expect("the agent is callable again after its prior turn ends");
    }

    /// Every real exit path (complete_with_call_context's match) explicitly
    /// finishes the guard before it drops. If a future edit adds a return
    /// path that skips that, this must fail the build/test that exercises
    /// it rather than silently self-heal at runtime - see InvocationGuard's
    /// Drop impl.
    #[tokio::test]
    #[should_panic(expected = "dropped without an explicit outcome")]
    async fn invocation_guard_panics_in_debug_if_dropped_unfinished() {
        let (store, _db, agent_id) = test_store().await;
        let guard = InvocationGuard::start(
            &store,
            None,
            store.agent_branch(agent_id).await.unwrap(),
            None,
        )
        .await
        .unwrap();
        drop(guard);
    }

    /// A merge source branch whose own uncovered messages exceed
    /// `merge_branch_messages` must be compacted before the merge commits;
    /// one that stays under the threshold must be left alone (design.md:
    /// "each incoming branch's unique suffix is eligible for compaction if
    /// its raw-message count exceeds merge_branch_messages").
    #[tokio::test]
    async fn compact_merge_sources_compacts_only_branches_over_the_threshold() {
        let db = crate::store::TestDatabase::new("agent-test");
        let store = Store::open(db.path()).await.unwrap();
        let owner = store
            .find_or_create_user("warden@example.test", "Warden")
            .await
            .unwrap();
        let scenario = crate::scenario::Scenario::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/scenarios/bread_thief.json"
        ))
        .unwrap();
        let installed = store.install_scenario(&owner, &scenario).await.unwrap();
        let original_group_id = store
            .character_group_id(installed.member.character_id)
            .await
            .unwrap();
        let toma_id: i64 = sqlx::query_scalar("SELECT id FROM character WHERE name = 'Toma'")
            .fetch_one(&store.pool())
            .await
            .unwrap();
        let mara_id: i64 = sqlx::query_scalar("SELECT id FROM character WHERE name = 'Mara'")
            .fetch_one(&store.pool())
            .await
            .unwrap();

        // Split the player off from Toma so each side gets its own branch:
        // the player's side is seeded with raw messages (over threshold),
        // Toma's stays empty (under threshold).
        let split_trigger = store
            .begin_sequence(installed.world_id, "split")
            .await
            .unwrap();
        let gm_history = store.group_gm_history(original_group_id).await.unwrap();
        let split_invocation = store
            .start_invocation(
                Some(split_trigger.id),
                store.history_branch(gm_history.branch_id).await.unwrap(),
                None,
                None,
            )
            .await
            .unwrap();
        let split_inference = store
            .start_inference(crate::store::InferenceInput {
                agent_id: gm_history.agent_id,
                sequence_id: Some(split_trigger.id),
                invocation_id: split_invocation,
                segments: &[],
                request: &crate::llm::Request {
                    messages: vec![Message::text(crate::llm::Role::User, "split")],
                    tools: Vec::new(),
                    sampling: Sampling::default(),
                },
                model: "test-model",
            })
            .await
            .unwrap();
        let executions = store
            .create_tool_executions(split_inference, 1)
            .await
            .unwrap();
        let changes = store
            .resolve_topology_changes(
                split_inference,
                vec![(
                    "split-1".to_string(),
                    executions[0],
                    tools::TopologyChange::Split {
                        group_id: original_group_id,
                        partitions: vec![
                            tools::GroupPartition {
                                character_ids: vec![installed.member.character_id, mara_id],
                                name: "player side".into(),
                                description: "outside".into(),
                            },
                            tools::GroupPartition {
                                character_ids: vec![toma_id],
                                name: "Toma side".into(),
                                description: "inside".into(),
                            },
                        ],
                        continue_with_character_id: Some(installed.member.character_id),
                    },
                )],
            )
            .await
            .unwrap();

        store
            .apply_topology_changes(changes, Limits::default().presence_departure_seconds)
            .await
            .unwrap();

        let over_group_id = store
            .character_group_id(installed.member.character_id)
            .await
            .unwrap();
        let under_group_id = store.character_group_id(toma_id).await.unwrap();
        let over_history = store.group_gm_history(over_group_id).await.unwrap();
        let under_history = store.group_gm_history(under_group_id).await.unwrap();

        // Seed the player's side with two raw messages - over a
        // merge_branch_messages threshold of 1; Toma's side stays empty.
        for text in ["The room creaks.", "A shadow moves."] {
            let trigger = store
                .begin_sequence(installed.world_id, "seed")
                .await
                .unwrap();
            let invocation = store
                .start_invocation(
                    Some(trigger.id),
                    store.history_branch(over_history.branch_id).await.unwrap(),
                    None,
                    None,
                )
                .await
                .unwrap();
            let inference = store
                .start_inference(crate::store::InferenceInput {
                    agent_id: over_history.agent_id,
                    sequence_id: Some(trigger.id),
                    invocation_id: invocation,
                    segments: &[],
                    request: &crate::llm::Request {
                        messages: vec![Message::text(crate::llm::Role::User, "narrate")],
                        tools: Vec::new(),
                        sampling: Sampling::default(),
                    },
                    model: "test-model",
                })
                .await
                .unwrap();
            store
                .finish_chat_inference(
                    inference,
                    over_history.agent_id,
                    crate::store::InferenceOutcome::Response(response(
                        Content::Text(text.into()),
                        "",
                    )),
                    1,
                    crate::store::CompactionPolicy {
                        sequence_id: Some(trigger.id),
                        compact_before_next_input_tokens: usize::MAX,
                        sampling: &Sampling::default(),
                        model: "test-model",
                        static_segments: &[],
                    },
                )
                .await
                .unwrap();
        }

        let limits = Limits {
            merge_branch_messages: 1,
            ..Limits::default()
        };
        // A plain ScriptedBackend's before_agent is a no-op (only
        // ScheduledBackend actually resolves a pending job) - the same
        // wrapping real production code always uses, so the enqueued merge
        // compaction is actually run rather than silently left pending.
        let inner = ScriptedBackend::new([response(
            Content::Text("Earlier: the room and the shadow.".into()),
            "",
        )]);
        let scheduler = crate::inference::InferenceScheduler::new(inner, limits).unwrap();
        let backend = scheduler.foreground();
        let turn = Turn {
            events: tokio::sync::broadcast::channel(256).0,
            branch: store.group_branch(over_group_id).await.unwrap(),
            static_messages: &[],
            dynamic_context: &[],
            tools: &[],
            sampling: Sampling::default(),
            model: "test-model",
        };
        let changes = vec![(
            "merge-1".to_string(),
            1,
            tools::TopologyChange::Merge {
                group_ids: vec![over_group_id, under_group_id],
                name: "merged".into(),
                description: "reunited".into(),
            },
        )];
        let changes = store
            .resolve_topology_changes(split_inference, changes)
            .await
            .unwrap();
        compact_merge_sources(&store, &backend, &changes, limits, &turn)
            .await
            .unwrap();

        assert!(
            store
                .latest_summary_for_branch(over_history.branch_id)
                .await
                .unwrap()
                .is_some(),
            "the over-threshold source branch must be compacted before the merge commits"
        );
        assert!(
            store
                .latest_summary_for_branch(under_history.branch_id)
                .await
                .unwrap()
                .is_none(),
            "an empty branch has nothing eligible and must be left untouched"
        );
    }
}
