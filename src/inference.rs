use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, Mutex},
};

use anyhow::{Context, Result, ensure};
use tokio::sync::Notify;

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
    deferred_agents: HashSet<i64>,
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

    /// Resume durable work after startup. Each job remains in SQLite until its
    /// summary and notice have committed, so this is safe to call repeatedly.
    pub async fn resume(&self, store: &Store) -> Result<()> {
        for job in store.pending_compactions().await? {
            self.admit_deferred(store.clone(), job.agent_id).await;
        }
        Ok(())
    }

    async fn admit(&self, priority: Priority) -> Admission<B> {
        let waiting_foreground = priority == Priority::Foreground;
        if waiting_foreground {
            self.state
                .lock()
                .expect("scheduler state poisoned")
                .foreground_waiting += 1;
            self.changed.notify_waiters();
        }
        loop {
            let changed = self.changed.notified();
            let admitted = {
                let mut state = self.state.lock().expect("scheduler state poisoned");
                let allowed = state.running < self.limits.max_concurrent_inferences
                    && (priority == Priority::Foreground || state.foreground_waiting == 0);
                if allowed {
                    state.running += 1;
                    if waiting_foreground {
                        state.foreground_waiting -= 1;
                    }
                }
                allowed
            };
            if admitted {
                return Admission {
                    scheduler: self.clone(),
                };
            }
            changed.await;
        }
    }

    async fn admit_deferred(&self, store: Store, agent_id: i64) {
        let mut state = self.state.lock().expect("scheduler state poisoned");
        if !state.deferred_agents.insert(agent_id) {
            return;
        }
        drop(state);
        let scheduler = self.clone();
        tokio::spawn(async move {
            let result = async {
                let job = store
                    .pending_compaction(agent_id)
                    .await?
                    .with_context(|| format!("missing pending compaction for agent {agent_id}"))?;
                let backend = scheduler.deferred();
                compaction::run(&store, &backend, &job, scheduler.limits).await
            }
            .await;
            let mut state = scheduler.state.lock().expect("scheduler state poisoned");
            state.deferred_agents.remove(&agent_id);
            if let Err(error) = result {
                state.failures.insert(agent_id, format!("{error:#}"));
            }
            drop(state);
            scheduler.changed.notify_waiters();
        });
    }

    async fn wait_for_agent(&self, store: &Store, agent_id: i64) -> Result<()> {
        if store.pending_compaction(agent_id).await?.is_none() {
            return Ok(());
        }
        self.admit_deferred(store.clone(), agent_id).await;
        loop {
            let changed = self.changed.notified();
            let mut state = self.state.lock().expect("scheduler state poisoned");
            if let Some(error) = state.failures.remove(&agent_id) {
                anyhow::bail!("deferred compaction for agent {agent_id} failed: {error}");
            }
            let inactive = !state.deferred_agents.contains(&agent_id);
            drop(state);
            if inactive {
                if store.pending_compaction(agent_id).await?.is_none() {
                    return Ok(());
                }
                self.admit_deferred(store.clone(), agent_id).await;
            }
            changed.await;
        }
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
    async fn before_agent(&self, store: &Store, agent_id: i64) -> Result<()> {
        self.scheduler.wait_for_agent(store, agent_id).await
    }

    async fn after_agent(&self, store: &Store, agent_id: i64) -> Result<()> {
        if store.pending_compaction(agent_id).await?.is_some() {
            self.scheduler.admit_deferred(store.clone(), agent_id).await;
        }
        Ok(())
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
