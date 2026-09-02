use anyhow::{Context, Result, bail};

use crate::{
    context,
    llm::{Backend, Content, Message, Response, Sampling},
    settings::Limits,
    store::Store,
    tools::{self, Tool},
};

/// Inference budget for one external trigger, shared by every agent it
/// reaches so recursive calls draw from the same total. Exhausting either
/// bound is a hard error: the bounds only ever fire on a loop that is already
/// wrong, so the failure must reach the user rather than an agent.
pub struct Budget {
    limits: Limits,
    total_spent: u32,
}

impl Budget {
    pub fn new(limits: Limits) -> Self {
        Self {
            limits,
            total_spent: 0,
        }
    }

    /// Charge one inference against the whole trigger and the current chat.
    fn spend(&mut self, chat_spent: u32) -> Result<()> {
        if chat_spent >= self.limits.max_inferences_per_chat {
            bail!(
                "chat reached its limit of {} inferences without settling on a reply \
                 (limits.max_inferences_per_chat)",
                self.limits.max_inferences_per_chat
            );
        }
        if self.total_spent >= self.limits.max_inferences_total {
            bail!(
                "this action reached its limit of {} inferences across all agents \
                 (limits.max_inferences_total)",
                self.limits.max_inferences_total
            );
        }
        self.total_spent += 1;
        Ok(())
    }

    #[cfg(test)]
    pub fn total_spent(&self) -> u32 {
        self.total_spent
    }
}

/// Resolve one chat turn, recording every model response and tool result in order.
#[allow(clippy::too_many_arguments)]
pub async fn complete<B: Backend>(
    store: &Store,
    backend: &B,
    budget: &mut Budget,
    agent_id: i64,
    static_messages: &[Message],
    tools: &[Tool],
    sampling: Sampling,
    model: &str,
    mut on_token: impl FnMut(&str),
) -> Result<Response> {
    let definitions = tools::definitions(tools);
    let mut chat_spent = 0;
    loop {
        budget
            .spend(chat_spent)
            .context("resolving this chat turn")?;
        chat_spent += 1;
        let response = context::complete(
            store,
            backend,
            agent_id,
            static_messages,
            &definitions,
            sampling.clone(),
            model,
            &mut on_token,
        )
        .await
        .context("running recorded agent inference")?;
        store
            .append_message(
                agent_id,
                &Message::assistant(response.content.clone(), response.reasoning.clone()),
            )
            .await
            .context("storing agent response")?;
        let Content::ToolCalls(calls) = &response.content else {
            return Ok(response);
        };
        for call in calls {
            let result = tools::execute(tools, call)
                .with_context(|| format!("running tool call {}", call.id))?;
            store
                .append_message(agent_id, &Message::tool_result(call.id.clone(), result))
                .await
                .with_context(|| format!("storing result for tool call {}", call.id))?;
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
            _on_token: impl FnMut(&str),
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
        let agent = store.create_agent(world, "sandbox", "test").await.unwrap();
        store
            .append_message(
                agent,
                &Message::text(Role::User, "Rook tries to dodge the falling beam."),
            )
            .await
            .unwrap();
        (store, path, agent)
    }

    /// A save the character cannot pass: d20 must roll *under* the attribute,
    /// so an attribute of 1 always fails. Fixed by the Cairn rules, not a seed.
    fn certain_failure(character: &str) -> String {
        format!(
            r#"{{"character":"{character}","attribute":"dex","reason":"dodge","attribute_value":1}}"#
        )
    }

    async fn history(store: &Store, agent: i64) -> Vec<Message> {
        let segment = store.message_segment(agent).await.unwrap().unwrap();
        store
            .request_for_segments(
                agent,
                &[segment],
                Sampling {
                    temperature: 0.0,
                    enable_thinking: false,
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
                    id: "save-1".to_string(),
                    name: "save".to_string(),
                    arguments: certain_failure("Rook"),
                }]),
                "private scratch work",
            ),
            response(Content::Text("The beam catches you.".to_string()), ""),
        ]);
        let final_response = complete(
            &store,
            &backend,
            &mut Budget::new(Limits::default()),
            agent_id,
            &[],
            &[tools::save()],
            Sampling {
                temperature: 0.0,
                enable_thinking: false,
            },
            "scripted",
            |_| {},
        )
        .await
        .unwrap();

        assert_eq!(
            final_response.content,
            Content::Text("The beam catches you.".to_string())
        );
        assert_eq!(store.inference_count().await.unwrap(), 2);
        let requests = backend.requests.lock().unwrap();
        assert_eq!(requests[0].tools, tools::definitions(&[tools::save()]));
        // The second inference must see Rust's verdict, not the model's guess.
        assert!(matches!(requests[1].messages.as_slice(), [
            Message { content: MessageContent::Text(_), .. },
            Message { content: MessageContent::ToolCalls(calls), reasoning, .. },
            Message { content: MessageContent::ToolResult { tool_call_id, content }, .. },
        ] if calls[0].id == "save-1"
            && reasoning.is_empty()
            && tool_call_id == "save-1"
            && content.contains("does not succeed")));
        drop(requests);
        let entries = history(&store, agent_id).await;
        // Reasoning is stored on the message but never replayed into context.
        assert!(
            matches!(&entries[1], Message { content: MessageContent::ToolCalls(_), reasoning, .. } if reasoning.is_empty())
        );
        assert!(
            matches!(&entries[2].content, MessageContent::ToolResult { tool_call_id, content } if tool_call_id == "save-1" && content.contains("does not succeed"))
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
            &mut Budget::new(Limits::default()),
            agent_id,
            &[],
            &[tools::save()],
            Sampling {
                temperature: 0.0,
                enable_thinking: false,
            },
            "scripted",
            |_| {},
        )
        .await
        .unwrap();

        let recorded = store.reconstruct_inference(1).await.unwrap();
        assert_eq!(
            recorded.request.tools,
            tools::definitions(&[tools::save()]),
            "the recorded recipe must rebuild the exact tool list that was sent"
        );

        store.corrupt_text_for_test("\"name\":\"save\"").await;
        let error = store
            .reconstruct_inference(1)
            .await
            .expect_err("a mutated tool definition must fail reconstruction");
        assert!(format!("{error:#}").contains("does not match"), "{error:#}");
        drop(store);
        std::fs::remove_file(path).unwrap();
    }

    #[tokio::test]
    async fn invalid_tool_call_stops_after_recording_the_call_without_a_result() {
        let (store, path, agent_id) = test_store().await;
        let backend = ScriptedBackend::new([response(
            Content::ToolCalls(vec![ToolCall {
                id: "bad".to_string(),
                name: "not_a_tool".to_string(),
                arguments: "{}".to_string(),
            }]),
            "",
        )]);
        let error = complete(
            &store,
            &backend,
            &mut Budget::new(Limits::default()),
            agent_id,
            &[],
            &[tools::save()],
            Sampling {
                temperature: 0.0,
                enable_thinking: false,
            },
            "scripted",
            |_| {},
        )
        .await
        .unwrap_err();

        assert!(format!("{error:#}").contains("unknown tool `not_a_tool`"));
        assert_eq!(store.inference_count().await.unwrap(), 1);
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
            ]
        ));
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
                        id: "rook-save".to_string(),
                        name: "save".to_string(),
                        arguments: certain_failure("Rook"),
                    },
                    ToolCall {
                        id: "mara-save".to_string(),
                        name: "save".to_string(),
                        arguments: certain_failure("Mara"),
                    },
                ]),
                "",
            ),
            response(Content::Text("Both saves are resolved.".to_string()), ""),
        ]);
        complete(
            &store,
            &backend,
            &mut Budget::new(Limits::default()),
            agent_id,
            &[],
            &[tools::save()],
            Sampling {
                temperature: 0.0,
                enable_thinking: false,
            },
            "scripted",
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
        // Each result must name the character from *its own* call, so results
        // attributed to the wrong call are caught rather than looking plausible.
        assert_eq!(tool_call_id, "rook-save");
        assert!(content.contains("Rook"), "got {content}");
        let MessageContent::ToolResult {
            tool_call_id,
            content,
        } = &entries[3].content
        else {
            panic!("second tool result should be stored");
        };
        assert_eq!(tool_call_id, "mara-save");
        assert!(content.contains("Mara"), "got {content}");
        drop(store);
        std::fs::remove_file(path).unwrap();
    }

    /// A model that keeps calling a tool must be stopped by the budget rather
    /// than looping forever. Observed for real: Llama 3.1 re-rolled the same
    /// save repeatedly, hoping for a better result.
    #[tokio::test]
    async fn a_model_that_never_settles_is_stopped_by_the_chat_limit() {
        let (store, path, agent_id) = test_store().await;
        let limits = Limits {
            max_inferences_per_chat: 3,
            max_inferences_total: 64,
        };
        let repeat = || {
            response(
                Content::ToolCalls(vec![ToolCall {
                    id: "again".to_string(),
                    name: "save".to_string(),
                    arguments: certain_failure("Rook"),
                }]),
                "",
            )
        };
        let backend = ScriptedBackend::new([repeat(), repeat(), repeat(), repeat(), repeat()]);
        let mut budget = Budget::new(limits);
        let error = complete(
            &store,
            &backend,
            &mut budget,
            agent_id,
            &[],
            &[tools::save()],
            Sampling {
                temperature: 0.0,
                enable_thinking: false,
            },
            "scripted",
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
        let mut budget = Budget::new(Limits {
            max_inferences_per_chat: 100,
            max_inferences_total: 2,
        });
        let repeat = || {
            response(
                Content::ToolCalls(vec![ToolCall {
                    id: "again".to_string(),
                    name: "save".to_string(),
                    arguments: certain_failure("Rook"),
                }]),
                "",
            )
        };
        let backend = ScriptedBackend::new([repeat(), repeat(), repeat(), repeat()]);
        let error = complete(
            &store,
            &backend,
            &mut budget,
            agent_id,
            &[],
            &[tools::save()],
            Sampling {
                temperature: 0.0,
                enable_thinking: false,
            },
            "scripted",
            |_| {},
        )
        .await
        .expect_err("the shared budget must stop the action");

        let message = format!("{error:#}");
        assert!(message.contains("max_inferences_total"), "got {message}");
        drop(store);
        std::fs::remove_file(path).unwrap();
    }
}
