use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, Mutex},
};

use crate::{
    inspection::DevEvent,
    world::{World, Worlds},
};
use anyhow::{Context, Result, ensure};
use tokio::sync::{Notify, broadcast};

use crate::{
    compaction,
    llm::{Backend, Request, Response},
    settings::Limits,
    store::Store,
};

#[derive(Clone, Copy, PartialEq)]
enum Priority {
    Foreground,
    Deferred,
}

#[derive(Default)]
struct State {
    running: usize,
    foreground_waiting: usize,
    // Keyed by branch_id, not agent_id: a world_gm agent plays several
    // groups at once, each its own branch with its own independent
    // compaction obligation (design.md's `GroupHistory`); agent_id alone
    // cannot tell two groups' pending jobs apart.
    deferred_branches: HashSet<i64>,
    failures: HashMap<i64, String>,
}

/// Admission policy around one model. Mistral.rs still owns batching of its
/// admitted sequences; this only reserves capacity for player-visible work.
pub struct InferenceScheduler<B> {
    backend: Arc<B>,
    limits: Limits,
    state: Arc<Mutex<State>>,
    changed: Arc<Notify>,
}

pub struct ScheduledBackend<B> {
    scheduler: InferenceScheduler<B>,
    priority: Priority,
}

impl<B> Clone for InferenceScheduler<B> {
    fn clone(&self) -> Self {
        Self {
            backend: self.backend.clone(),
            limits: self.limits,
            state: self.state.clone(),
            changed: self.changed.clone(),
        }
    }
}

impl<B> Clone for ScheduledBackend<B> {
    fn clone(&self) -> Self {
        Self {
            scheduler: self.scheduler.clone(),
            priority: self.priority,
        }
    }
}

impl<B> InferenceScheduler<B>
where
    B: Backend + Send + Sync + 'static,
{
    pub fn new(backend: B, limits: Limits) -> Result<Self> {
        ensure!(
            limits.max_concurrent_inferences > 0,
            "limits.max_concurrent_inferences must be greater than zero"
        );
        Ok(Self {
            backend: Arc::new(backend),
            limits,
            state: Arc::new(Mutex::new(State::default())),
            changed: Arc::new(Notify::new()),
        })
    }

    pub fn foreground(&self) -> ScheduledBackend<B> {
        ScheduledBackend {
            scheduler: self.clone(),
            priority: Priority::Foreground,
        }
    }

    fn deferred(&self) -> ScheduledBackend<B> {
        ScheduledBackend {
            scheduler: self.clone(),
            priority: Priority::Deferred,
        }
    }

    /// Resume stored work after startup. Each job remains in SQLite until its
    /// summary and notice have committed. Incomplete inferences are marked
    /// interrupted before execution resumes.
    pub async fn resume(&self, store: &Store, worlds: &Worlds) -> Result<()> {
        store.interrupt_open_inferences().await?;
        for job in store.pending_compactions().await? {
            let world_id = store.world_id_for_agent(job.agent_id).await?;
            let events = World::get(worlds, world_id)?.events.clone();
            self.admit_deferred(store.clone(), job.branch_id, events)
                .await;
        }
        Ok(())
    }

    async fn admit(&self, priority: Priority) -> Admission<B> {
        let mut waiting_foreground =
            (priority == Priority::Foreground).then(|| ForegroundWaiter::new(self.clone()));
        loop {
            let changed = self.changed.notified();
            let admitted = {
                let mut state = self.state.lock().expect("scheduler state poisoned");
                let allowed = state.running < self.limits.max_concurrent_inferences
                    && (priority == Priority::Foreground || state.foreground_waiting == 0);
                if allowed {
                    state.running += 1;
                }
                allowed
            };
            if admitted {
                if let Some(waiter) = &mut waiting_foreground {
                    waiter.release();
                }
                return Admission {
                    scheduler: self.clone(),
                };
            }
            changed.await;
        }
    }

    async fn admit_deferred(
        &self,
        store: Store,
        branch_id: i64,
        events: broadcast::Sender<DevEvent>,
    ) {
        let mut state = self.state.lock().expect("scheduler state poisoned");
        if !state.deferred_branches.insert(branch_id) {
            return;
        }
        drop(state);
        let scheduler = self.clone();
        tokio::spawn(async move {
            let result = async {
                let job = store
                    .pending_compaction_for_branch(branch_id)
                    .await?
                    .with_context(|| {
                        format!("missing pending compaction for branch {branch_id}")
                    })?;
                let backend = scheduler.deferred();
                compaction::run(&store, &backend, &job, scheduler.limits, events).await
            }
            .await;
            let mut state = scheduler.state.lock().expect("scheduler state poisoned");
            state.deferred_branches.remove(&branch_id);
            if let Err(error) = result {
                state.failures.insert(branch_id, format!("{error:#}"));
            }
            drop(state);
            scheduler.changed.notify_waiters();
        });
    }

    /// Wait for `branch_id`'s own pending compaction, if any, to finish -
    /// the explicit branch a world_gm agent is using this turn, or an
    /// ordinary agent's single branch (design.md's `GroupHistory`: agent_id
    /// alone cannot select which of a world_gm's branches is due).
    async fn wait_for_branch(
        &self,
        store: &Store,
        branch_id: i64,
        events: &broadcast::Sender<DevEvent>,
    ) -> Result<()> {
        if store
            .pending_compaction_for_branch(branch_id)
            .await?
            .is_none()
        {
            return Ok(());
        }
        self.admit_deferred(store.clone(), branch_id, events.clone())
            .await;
        loop {
            let changed = self.changed.notified();
            let inactive = {
                let mut state = self.state.lock().expect("scheduler state poisoned");
                if let Some(error) = state.failures.remove(&branch_id) {
                    anyhow::bail!("deferred compaction for branch {branch_id} failed: {error}");
                }
                !state.deferred_branches.contains(&branch_id)
            };
            if inactive {
                if store
                    .pending_compaction_for_branch(branch_id)
                    .await?
                    .is_none()
                {
                    return Ok(());
                }
                self.admit_deferred(store.clone(), branch_id, events.clone())
                    .await;
            }
            changed.await;
        }
    }
}

/// Counts a foreground request from the instant it begins waiting until it is
/// admitted. Dropping its future (a disconnected websocket, for example) must
/// relinquish that priority, otherwise it would permanently starve deferred
/// work.
struct ForegroundWaiter<B> {
    scheduler: InferenceScheduler<B>,
    waiting: bool,
}

impl<B> ForegroundWaiter<B> {
    fn new(scheduler: InferenceScheduler<B>) -> Self {
        scheduler
            .state
            .lock()
            .expect("scheduler state poisoned")
            .foreground_waiting += 1;
        scheduler.changed.notify_waiters();
        Self {
            scheduler,
            waiting: true,
        }
    }

    fn release(&mut self) {
        if self.waiting {
            self.scheduler
                .state
                .lock()
                .expect("scheduler state poisoned")
                .foreground_waiting -= 1;
            self.scheduler.changed.notify_waiters();
            self.waiting = false;
        }
    }
}

impl<B> Drop for ForegroundWaiter<B> {
    fn drop(&mut self) {
        self.release();
    }
}

struct Admission<B> {
    scheduler: InferenceScheduler<B>,
}

impl<B> Drop for Admission<B> {
    fn drop(&mut self) {
        self.scheduler
            .state
            .lock()
            .expect("scheduler state poisoned")
            .running -= 1;
        self.scheduler.changed.notify_waiters();
    }
}

impl<B> Backend for ScheduledBackend<B>
where
    B: Backend + Send + Sync + 'static,
{
    async fn before_agent(
        &self,
        store: &Store,
        agent_id: i64,
        branch_id: Option<i64>,
        events: &broadcast::Sender<DevEvent>,
    ) -> Result<()> {
        // A pending job was created with the previous final reply. It must
        // finish before this branch can assemble another context, otherwise
        // the same over-threshold history would be inferred again. Other
        // agents/branches remain schedulable while this one waits.
        let branch_id = match branch_id {
            Some(branch_id) => branch_id,
            None => store.branch_id_for_agent(agent_id).await?,
        };
        self.scheduler
            .wait_for_branch(store, branch_id, events)
            .await
    }

    async fn after_agent(
        &self,
        store: &Store,
        agent_id: i64,
        branch_id: Option<i64>,
        events: &broadcast::Sender<DevEvent>,
    ) -> Result<()> {
        let branch_id = match branch_id {
            Some(branch_id) => branch_id,
            None => store.branch_id_for_agent(agent_id).await?,
        };
        if store
            .pending_compaction_for_branch(branch_id)
            .await?
            .is_some()
        {
            self.scheduler
                .admit_deferred(store.clone(), branch_id, events.clone())
                .await;
        }
        Ok(())
    }

    async fn complete_with_recording(
        &self,
        request: Request,
        on_token: impl FnMut(&str) + Send,
        store: &Store,
        inference_id: i64,
    ) -> Result<Response> {
        let _admission = self.scheduler.admit(self.priority).await;
        self.scheduler
            .backend
            .complete_with_recording(request, on_token, store, inference_id)
            .await
    }

    async fn complete(
        &self,
        request: Request,
        on_token: impl FnMut(&str) + Send,
    ) -> Result<Response> {
        let _admission = self.scheduler.admit(self.priority).await;
        self.scheduler.backend.complete(request, on_token).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        llm::{Content, Message, Role, Sampling, Usage},
        store::{CompactionPolicy, InferenceInput, InferenceOutcome},
    };

    struct TestBackend;

    impl Backend for TestBackend {
        async fn complete(
            &self,
            _request: Request,
            _on_token: impl FnMut(&str) + Send,
        ) -> Result<Response> {
            Ok(Response {
                content: crate::llm::Content::Text(String::new()),
                reasoning: String::new(),
                usage: Usage {
                    input_tokens: 0,
                    output_tokens: 0,
                },
            })
        }
    }

    fn scheduler(cap: usize) -> InferenceScheduler<TestBackend> {
        InferenceScheduler::new(
            TestBackend,
            Limits {
                max_concurrent_inferences: cap,
                ..Limits::default()
            },
        )
        .unwrap()
    }

    #[tokio::test]
    async fn deferred_work_runs_when_capacity_is_idle() {
        let scheduler = scheduler(1);
        let admission = scheduler.admit(Priority::Deferred).await;
        assert_eq!(scheduler.state.lock().unwrap().running, 1);
        drop(admission);
    }

    #[tokio::test]
    async fn admission_never_exceeds_the_configured_cap() {
        let scheduler = scheduler(1);
        let first = scheduler.admit(Priority::Foreground).await;
        let waiting = scheduler.clone();
        let second = tokio::spawn(async move { waiting.admit(Priority::Foreground).await });
        tokio::task::yield_now().await;
        assert_eq!(scheduler.state.lock().unwrap().running, 1);
        assert!(!second.is_finished());
        drop(first);
        drop(second.await.unwrap());
        assert_eq!(scheduler.state.lock().unwrap().running, 0);
    }

    #[tokio::test]
    async fn foreground_overtakes_waiting_deferred_work() {
        let scheduler = scheduler(1);
        let first = scheduler.admit(Priority::Foreground).await;
        let deferred_scheduler = scheduler.clone();
        let deferred =
            tokio::spawn(async move { deferred_scheduler.admit(Priority::Deferred).await });
        tokio::task::yield_now().await;
        let foreground_scheduler = scheduler.clone();
        let foreground =
            tokio::spawn(async move { foreground_scheduler.admit(Priority::Foreground).await });
        tokio::task::yield_now().await;
        drop(first);
        let foreground = foreground.await.unwrap();
        assert!(!deferred.is_finished());
        drop(foreground);
        drop(deferred.await.unwrap());
    }

    #[tokio::test]
    async fn cancelling_a_foreground_waiter_releases_deferred_work() {
        let scheduler = scheduler(1);
        let first = scheduler.admit(Priority::Foreground).await;
        let foreground_scheduler = scheduler.clone();
        let cancelled =
            tokio::spawn(async move { foreground_scheduler.admit(Priority::Foreground).await });
        tokio::task::yield_now().await;
        cancelled.abort();
        let _ = cancelled.await;
        assert_eq!(scheduler.state.lock().unwrap().foreground_waiting, 0);
        let deferred_scheduler = scheduler.clone();
        let deferred =
            tokio::spawn(async move { deferred_scheduler.admit(Priority::Deferred).await });
        drop(first);
        drop(deferred.await.unwrap());
    }

    #[tokio::test]
    async fn same_agent_waits_until_its_persisted_job_finishes() {
        let db = crate::store::TestDatabase::new("inference-test");
        let store = Store::open(db.path()).await.unwrap();
        let world = store.create_world("test").await.unwrap();
        let agent = store.create_agent(world).await.unwrap();
        store
            .append_message(agent, &Message::text(Role::User, "older history"))
            .await
            .unwrap();
        let request = Request {
            messages: vec![Message::text(Role::User, "older history")],
            tools: Vec::new(),
            sampling: Sampling {
                temperature: 0.0,
                ..Default::default()
            },
        };
        let invocation_id = store
            .start_invocation(None, agent, None, None, None)
            .await
            .unwrap();
        let inference_id = store
            .start_inference(InferenceInput {
                agent_id: agent,
                sequence_id: None,
                invocation_id,
                segments: &[],
                request: &request,
                model: "test",
            })
            .await
            .unwrap();
        store
            .finish_chat_inference(
                inference_id,
                agent,
                InferenceOutcome::Response(Response {
                    content: Content::Text("reply".into()),
                    reasoning: String::new(),
                    usage: Usage {
                        input_tokens: 1,
                        output_tokens: 1,
                    },
                }),
                1,
                CompactionPolicy {
                    sequence_id: None,
                    compact_before_next_input_tokens: 2,
                    sampling: &Sampling {
                        temperature: 0.0,
                        ..Default::default()
                    },
                    model: "test",
                    static_segments: &[],
                },
            )
            .await
            .unwrap();
        assert!(
            store.pending_compaction(agent).await.unwrap().is_some(),
            "a reply whose output makes the next input reach the threshold must queue compaction"
        );
        // Keep one raw reply so this stored job performs an actual summary
        // inference, rather than merely retiring a job whose entire history fits.
        let scheduler = InferenceScheduler::new(
            TestBackend,
            Limits {
                max_concurrent_inferences: 1,
                keep_tail_messages: 1,
                ..Limits::default()
            },
        )
        .unwrap();
        let worlds = World::load(&store).await.unwrap();
        let events = World::get(&worlds, world).unwrap().events.clone();
        let mut notifications = events.subscribe();
        scheduler.resume(&store, &worlds).await.unwrap();
        let branch_id = store.branch_id_for_agent(agent).await.unwrap();
        scheduler
            .wait_for_branch(&store, branch_id, &events)
            .await
            .unwrap();
        assert!(store.pending_compaction(agent).await.unwrap().is_none());
        let summary_id = match notifications.try_recv().unwrap() {
            DevEvent::InferenceStarted {
                inference_id,
                agent_id,
                parent_inference_id: None,
                ..
            } => {
                assert_eq!(agent_id, agent);
                inference_id
            }
            event => {
                panic!("expected a deferred root inference on this world's channel: {event:?}")
            }
        };
        let cause: i64 = sqlx::query_scalar("SELECT invocation.cause_inference_id FROM inference JOIN invocation ON invocation.id = inference.invocation_id WHERE inference.id = ?")
            .bind(summary_id).fetch_one(&store.pool()).await.unwrap();
        assert_eq!(
            cause, inference_id,
            "a deferred root retains its cause separately from tool-call nesting"
        );
        assert!(matches!(
            notifications.try_recv(),
            Ok(DevEvent::InferenceCompleted { error: None, .. })
        ));
    }
}
