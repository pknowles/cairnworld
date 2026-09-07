use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, bail, ensure};

use crate::{
    context,
    llm::{Backend, Content, Message, Response, Sampling},
    settings::Limits,
    store::{CompletedReply, Store},
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

    /// Charge one inference against the whole trigger and the current chat.
    fn spend(&self, chat_spent: u32) -> Result<()> {
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

    fn limits(&self) -> Limits {
        self.state.lock().expect("inference budget poisoned").limits
    }

    #[cfg(test)]
    pub fn total_spent(&self) -> u32 {
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
    parent_inference_id: Option<i64>,
    agents: Arc<Mutex<Vec<i64>>>,
}

impl CallContext {
    pub fn root(sequence_id: Option<i64>) -> Self {
        Self {
            sequence_id,
            parent_inference_id: None,
            agents: Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub fn child(&self, parent_inference_id: i64) -> Self {
        Self {
            sequence_id: self.sequence_id,
            parent_inference_id: Some(parent_inference_id),
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
    pub agent_id: i64,
    pub static_messages: &'a [Message],
    pub tools: &'a [Tool],
    pub sampling: Sampling,
    pub model: &'a str,
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
    complete_with_call_context(store, backend, budget, turn, &call, on_token, on_activity).await
}

/// Resolve a turn while retaining its external sequence and, for a nested
/// agent call, the inference that caused it.
pub async fn complete_with_call_context<B: Backend>(
    store: &Store,
    backend: &B,
    budget: &Budget,
    turn: Turn<'_>,
    call: &CallContext,
    mut on_token: impl FnMut(&str) + Send,
    mut on_activity: impl FnMut(String),
) -> Result<Response> {
    let _active = call.enter(turn.agent_id)?;
    backend
        .before_agent(store, turn.agent_id)
        .await
        .context("waiting for earlier deferred work for this agent")?;
    let definitions = tools::definitions(turn.tools).context("validating declared tool schemas")?;
    let mut chat_spent = 0;
    loop {
        budget
            .spend(chat_spent)
            .context("resolving this chat turn")?;
        chat_spent += 1;
        let completion = context::complete_recorded(
            store,
            backend,
            context::RecordedCompletion {
                agent_id: turn.agent_id,
                sequence_id: call.sequence_id,
                parent_inference_id: call.parent_inference_id,
                static_messages: turn.static_messages,
                tools: &definitions,
                sampling: turn.sampling.clone(),
                model: turn.model,
            },
            &mut on_token,
        )
        .await
        .context("running recorded agent inference")?;
        let response = completion.response;
        let Content::ToolCalls(calls) = &response.content else {
            // The completed model request reports its exact template-expanded
            // input and generated output. That is the next normal context, so
            // this happy path deliberately does not tokenize it again: doing
            // so would be pure overhead. Capacity recovery instead retries
            // real summary inferences with progressively smaller prefixes.
            let next_input_tokens = response
                .usage
                .input_tokens
                .checked_add(response.usage.output_tokens)
                .context("completed inference input plus output token count overflowed")?;
            let static_segments = context::static_segments(&completion.segments);
            store
                .append_reply_and_enqueue_compaction(
                    turn.agent_id,
                    CompletedReply {
                        message: &Message::assistant(
                            response.content.clone(),
                            response.reasoning.clone(),
                        ),
                        next_input_tokens,
                        compact_before_next_input_tokens: budget
                            .limits()
                            .compact_before_next_input_tokens,
                        sampling: &turn.sampling,
                        model: turn.model,
                        static_segments: &static_segments,
                    },
                )
                .await
                .context("storing final agent response and any due compaction")?;
            backend
                .after_agent(store, turn.agent_id)
                .await
                .context("admitting any due deferred compaction")?;
            return Ok(response);
        };
        store
            .append_message(
                turn.agent_id,
                &Message::assistant(response.content.clone(), response.reasoning.clone()),
            )
            .await
            .context("storing agent response")?;
        for call in calls {
            on_activity(format!("tool call {}: {}", call.name, call.arguments));
            let (result, after_result) = tools::execute(turn.tools, call, completion.inference_id)
                .await
                .with_context(|| format!("executing tool call {}", call.id))?
                .into_parts();
            let activity = format!("tool result {}: {result}", call.id);
            store
                .append_message(
                    turn.agent_id,
                    &Message::tool_result(call.id.clone(), result),
                )
                .await
                .with_context(|| format!("storing result for tool call {}", call.id))?;
            if let Some(after_result) = after_result {
                after_result
                    .await
                    .with_context(|| format!("delivering result of tool call {}", call.id))?;
            }
            on_activity(activity);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::VecDeque,
        sync::Mutex,
        time::{SystemTime, UNIX_EPOCH},
    };

    use super::*;
    use crate::llm::{MessageContent, Role, ToolCall, Usage};
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

    async fn test_store() -> (Store, std::path::PathBuf, i64) {
        let path = std::env::temp_dir().join(format!(
            "cairnworld-agent-test-{}-{}.sqlite",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let store = Store::open(&path).await.unwrap();
        let world = store.create_world("test").await.unwrap();
        let agent = store.create_agent(world).await.unwrap();
        store
            .append_message(
                agent,
                &Message::text(Role::User, "Rook tries to dodge the falling beam."),
            )
            .await
            .unwrap();
        (store, path, agent)
    }

    fn scripted_turn<'a>(agent_id: i64, tools: &'a [Tool]) -> Turn<'a> {
        Turn {
            agent_id,
            static_messages: &[],
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
        let segments = store.history_segments(agent).await.unwrap();
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
        let (store, path, agent_id) = test_store().await;
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
            scripted_turn(agent_id, &[tools::test_echo()]),
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
        drop(store);
        std::fs::remove_file(path).unwrap();
    }

    /// The plan's headline reconstruction target: an inference carrying real
    /// tool definitions must reassemble hash-equal, and a mutated definition
    /// must be rejected rather than silently reconstructing something else.
    #[tokio::test]
    async fn inferences_carrying_tools_reconstruct_and_detect_tampering() {
        let (store, path, agent_id) = test_store().await;
        let backend =
            ScriptedBackend::new([response(Content::Text("Nothing to roll.".into()), "")]);
        complete(
            &store,
            &backend,
            &Budget::new(Limits::default()),
            scripted_turn(agent_id, &[tools::test_echo()]),
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

        store.corrupt_text_for_test("\"name\":\"test_echo\"").await;
        let error = store
            .reconstruct_inference(1)
            .await
            .expect_err("a mutated tool definition must fail reconstruction");
        assert!(format!("{error:#}").contains("does not match"), "{error:#}");
        drop(store);
        std::fs::remove_file(path).unwrap();
    }

    #[tokio::test]
    async fn unavailable_tool_becomes_a_result_the_model_can_explain() {
        let (store, path, agent_id) = test_store().await;
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
            scripted_turn(agent_id, &[tools::test_echo()]),
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
        drop(store);
        std::fs::remove_file(path).unwrap();
    }

    #[tokio::test]
    async fn rejected_known_tool_arguments_are_returned_to_the_model() {
        let (store, path, agent_id) = test_store().await;
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
            scripted_turn(agent_id, &[tools::test_echo()]),
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
        drop(store);
        std::fs::remove_file(path).unwrap();
    }

    #[tokio::test]
    async fn multiple_tool_results_keep_their_call_ids() {
        let (store, path, agent_id) = test_store().await;
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
            scripted_turn(agent_id, &[tools::test_echo()]),
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
        drop(store);
        std::fs::remove_file(path).unwrap();
    }

    /// A model that keeps calling a tool must be stopped by the budget rather
    /// than looping forever.
    #[tokio::test]
    async fn a_model_that_never_settles_is_stopped_by_the_chat_limit() {
        let (store, path, agent_id) = test_store().await;
        let limits = Limits {
            max_concurrent_inferences: 4,
            max_inferences_per_chat: 3,
            max_inferences_total: 64,
            max_context_tokens: 33_792,
            max_output_tokens: 1_024,
            compact_before_next_input_tokens: 32_768,
            keep_tail_messages: 32,
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
            scripted_turn(agent_id, &[tools::test_echo()]),
            |_| {},
            |_| {},
        )
        .await
        .expect_err("an endless tool loop must be stopped");

        let message = format!("{error:#}");
        assert!(message.contains("max_inferences_per_chat"), "got {message}");
        assert_eq!(
            budget.total_spent(),
            limits.max_inferences_per_chat,
            "the loop must stop at the limit, not after it"
        );
        drop(store);
        std::fs::remove_file(path).unwrap();
    }

    /// The total bound is shared across agents, so a chat that stays under its
    /// own limit still stops once the whole action has spent its budget.
    #[tokio::test]
    async fn the_total_limit_stops_a_chat_that_is_within_its_own_limit() {
        let (store, path, agent_id) = test_store().await;
        let budget = Budget::new(Limits {
            max_concurrent_inferences: 4,
            max_inferences_per_chat: 100,
            max_inferences_total: 2,
            max_context_tokens: 33_792,
            max_output_tokens: 1_024,
            compact_before_next_input_tokens: 32_768,
            keep_tail_messages: 32,
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
            scripted_turn(agent_id, &[tools::test_echo()]),
            |_| {},
            |_| {},
        )
        .await
        .expect_err("the shared budget must stop the action");

        let message = format!("{error:#}");
        assert!(message.contains("max_inferences_total"), "got {message}");
        drop(store);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn nested_calls_share_one_budget() {
        let budget = Budget::new(Limits::default());
        let nested = budget.clone();
        budget.spend(0).unwrap();
        nested.spend(0).unwrap();
        assert_eq!(budget.total_spent(), 2);
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
}
