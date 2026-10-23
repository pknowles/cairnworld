use std::{
    collections::{BTreeSet, HashMap, HashSet},
    sync::{Arc, Mutex as StdMutex, RwLock},
};

use anyhow::{Context, Result};
use tokio::sync::{Mutex, Notify, broadcast, mpsc, oneshot, watch};

use crate::{
    inspection::DevEvent,
    llm::Response,
    store::{PlayerAgent, Store},
};

pub type Worlds = Arc<RwLock<HashMap<i64, Arc<World>>>>;
pub type ChatChanges = broadcast::Sender<ChatChange>;

/// Completes once shutdown has begun. Returns no borrow of the flag, so it can
/// be raced in a `select!` inside a spawned task. A flag whose owner is gone
/// was never set, so it never completes.
pub async fn stopped(stopping: &mut watch::Receiver<bool>) {
    if stopping.wait_for(|stopping| *stopping).await.is_err() {
        std::future::pending::<()>().await;
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum ChatChange {
    Entries,
    Failed { sequence_id: i64 },
}

/// Live state exists independently of model loading. Database operations never
/// own these channels; their callers publish only after committed writes.
pub struct World {
    pub id: i64,
    pub events: broadcast::Sender<DevEvent>,
    pub sender: mpsc::Sender<WorldEvent>,
    pub queued: Mutex<mpsc::Receiver<WorldEvent>>,
    pub broadcasts: ChatChanges,
    pub presence: Mutex<Presence>,
    pub changed: Notify,
    pub openings: Mutex<HashMap<i64, watch::Receiver<OpeningState>>>,
    /// Raised hands (Interrupt), as (group, character). In memory only.
    hands: StdMutex<BTreeSet<(i64, i64)>>,
    /// Changes whenever a hand goes up or down, for viewers to re-read it.
    hands_changed: watch::Sender<u64>,
    /// Wakes the worker when a hand is lowered.
    pub released: Notify,
}

impl World {
    fn new(id: i64) -> Self {
        let (sender, queued) = mpsc::channel(64);
        Self {
            id,
            events: broadcast::channel(256).0,
            sender,
            queued: Mutex::new(queued),
            broadcasts: broadcast::channel(64).0,
            presence: Mutex::new(Presence::default()),
            changed: Notify::new(),
            openings: Mutex::new(HashMap::new()),
            hands: StdMutex::new(BTreeSet::new()),
            hands_changed: watch::channel(0).0,
            released: Notify::new(),
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

impl World {
    fn changed_hands(&self) {
        self.hands_changed.send_modify(|version| *version += 1);
    }

    /// Subscribe to raised-hand changes; the value only says that something changed.
    pub fn hands_changed(&self) -> watch::Receiver<u64> {
        self.hands_changed.subscribe()
    }

    /// Interrupt: this character raises a hand in their group. Accepted at any
    /// time; nothing running is cancelled.
    pub fn raise_hand(&self, group_id: i64, character_id: i64) {
        let raised = self
            .hands
            .lock()
            .expect("hands lock poisoned")
            .insert((group_id, character_id));
        if raised {
            self.changed_hands();
        }
    }

    /// Never mind, or the player sending or leaving: lower this character's hand
    /// and wake the worker. Without a raised hand this does nothing at all.
    pub fn lower_hand(&self, character_id: i64) {
        self.lower(|(_, hand)| *hand == character_id);
    }

    /// The conversation has moved on: lower every hand in the group.
    pub fn lower_hands_in(&self, group_id: i64) {
        self.lower(|(group, _)| *group == group_id);
    }

    fn lower(&self, matches: impl Fn(&(i64, i64)) -> bool) {
        let lowered = {
            let mut hands = self.hands.lock().expect("hands lock poisoned");
            let before = hands.len();
            hands.retain(|hand| !matches(hand));
            hands.len() != before
        };
        if lowered {
            self.changed_hands();
            self.released.notify_one();
        }
    }

    pub fn hand_raised(&self, group_id: i64, character_id: i64) -> bool {
        self.hands
            .lock()
            .expect("hands lock poisoned")
            .contains(&(group_id, character_id))
    }

    /// The groups in which NPC work waits for a raised hand.
    pub fn held_groups(&self) -> Vec<i64> {
        let hands = self.hands.lock().expect("hands lock poisoned");
        let mut groups: Vec<i64> = hands.iter().map(|(group, _)| *group).collect();
        groups.dedup();
        groups
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
    /// Kills this world's worker, to test that one world's failure is reported
    /// without disturbing the others.
    #[cfg(test)]
    Panic,
}
