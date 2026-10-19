use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, RwLock},
};

use anyhow::{Context, Result};
use tokio::sync::{Mutex, Notify, broadcast, mpsc, oneshot, watch};

use crate::{
    inspection::DevEvent,
    llm::Response,
    store::{PlayerAgent, Store},
};

pub type Worlds = Arc<RwLock<HashMap<i64, Arc<World>>>>;
pub type GroupBroadcasts = Arc<Mutex<HashMap<i64, broadcast::Sender<String>>>>;

/// Live state exists independently of model loading. Database operations never
/// own these channels; their callers publish only after committed writes.
pub struct World {
    pub id: i64,
    pub events: broadcast::Sender<DevEvent>,
    pub sender: mpsc::Sender<WorldEvent>,
    pub queued: Mutex<mpsc::Receiver<WorldEvent>>,
    pub broadcasts: GroupBroadcasts,
    pub presence: Mutex<Presence>,
    pub changed: Notify,
    pub openings: Mutex<HashMap<i64, watch::Receiver<OpeningState>>>,
}

impl World {
    fn new(id: i64) -> Self {
        let (sender, queued) = mpsc::channel(64);
        Self {
            id,
            events: broadcast::channel(256).0,
            sender,
            queued: Mutex::new(queued),
            broadcasts: Arc::new(Mutex::new(HashMap::new())),
            presence: Mutex::new(Presence::default()),
            changed: Notify::new(),
            openings: Mutex::new(HashMap::new()),
        }
    }

    pub async fn load(store: &Store) -> Result<Worlds> {
        Ok(Arc::new(RwLock::new(
            store
                .world_ids()
                .await?
                .into_iter()
                .map(|id| (id, Arc::new(Self::new(id))))
                .collect(),
        )))
    }

    pub fn install(worlds: &Worlds, id: i64) -> Arc<Self> {
        Arc::clone(
            worlds
                .write()
                .expect("worlds lock poisoned")
                .entry(id)
                .or_insert_with(|| Arc::new(Self::new(id))),
        )
    }

    pub fn get(worlds: &Worlds, id: i64) -> Result<Arc<Self>> {
        worlds
            .read()
            .expect("worlds lock poisoned")
            .get(&id)
            .cloned()
            .with_context(|| format!("world {id} is not loaded"))
    }

    pub fn publish(&self, event: DevEvent) {
        // Inference and committed changes remain valid with no current viewers.
        let _no_live_viewers = self.events.send(event);
    }

    pub async fn set_developer_mode(
        &self,
        store: &Store,
        owner_id: i64,
        enabled: bool,
    ) -> Result<()> {
        store.set_developer_mode(owner_id, self.id, enabled).await?;
        self.publish(DevEvent::DeveloperModeChanged { enabled });
        self.publish(DevEvent::AccessChanged);
        Ok(())
    }

    pub async fn remove_member(&self, store: &Store, owner_id: i64, user_id: i64) -> Result<()> {
        store.remove_member(owner_id, self.id, user_id).await?;
        self.publish(DevEvent::AccessChanged);
        Ok(())
    }
}

#[derive(Default)]
pub struct Presence {
    pub connections: HashMap<i64, HashSet<i64>>,
    pub changed: HashSet<i64>,
}

#[derive(Clone, Debug)]
pub enum OpeningState {
    Pending,
    Ready,
    Failed(String),
}

pub enum WorldEvent {
    PlayerMessage {
        member: PlayerAgent,
        text: String,
        reply: oneshot::Sender<Result<Response>>,
    },
    Enter {
        member: PlayerAgent,
        reply: oneshot::Sender<Result<Option<Response>>>,
    },
    PresenceDeadline {
        group_id: i64,
        generation: i64,
    },
    #[cfg(test)]
    Settled(oneshot::Sender<()>),
}
