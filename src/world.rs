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
    /// Live pause holds and what the worker is doing; see `Control`.
    control: StdMutex<Control>,
    /// Changes whenever the control state does, for every viewer to re-read it.
    control_changes: watch::Sender<u64>,
    /// Wakes the worker when a hold is released.
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
            control: StdMutex::new(Control::default()),
            control_changes: watch::channel(0).0,
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

/// What the world worker is doing, for acknowledging a pause: a pause is only
/// reached once nothing that can publish is running in the held group.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Activity {
    #[default]
    Idle,
    /// A human's chat-turn (or world entry) in the group.
    Foreground { group_id: i64 },
    /// An NPC step that only records intentions; it publishes nothing.
    NpcDecision { group_id: i64 },
    /// An NPC action: it may publish speech, so a pause waits for it.
    NpcAction { group_id: i64 },
}

/// Live, process-local pause state. Never held across an await.
#[derive(Default)]
struct Control {
    /// (group, character) pairs whose player pressed Interrupt.
    holds: BTreeSet<(i64, i64)>,
    activity: Activity,
}

impl Control {
    /// Nothing that can publish is running in this group.
    fn quiescent(&self, group_id: i64) -> bool {
        !matches!(
            self.activity,
            Activity::Foreground { group_id: g } | Activity::NpcAction { group_id: g } if g == group_id
        )
    }
}

/// What one player's controls show.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ControlView {
    /// This character pressed Interrupt and has not sent or said Never mind.
    pub own_hold: bool,
    /// Some player holds the character's group and nothing is still running
    /// that could publish: NPCs are paused.
    pub paused: bool,
}

/// Restores the worker to idle however the work it covers ends.
pub struct ActivityGuard<'a> {
    world: &'a World,
}

impl Drop for ActivityGuard<'_> {
    fn drop(&mut self) {
        self.world
            .control
            .lock()
            .expect("control lock poisoned")
            .activity = Activity::Idle;
        self.world.changed_control();
    }
}

impl World {
    fn changed_control(&self) {
        self.control_changes.send_modify(|version| *version += 1);
    }

    /// Subscribe to control changes; the value only says that something changed.
    pub fn control_changes(&self) -> watch::Receiver<u64> {
        self.control_changes.subscribe()
    }

    /// Accept an Interrupt at any time, even during the character's own turn.
    pub fn hold(&self, group_id: i64, character_id: i64) {
        let inserted = self
            .control
            .lock()
            .expect("control lock poisoned")
            .holds
            .insert((group_id, character_id));
        if inserted {
            self.changed_control();
        }
    }

    /// Never mind: release this character's hold and wake the worker. Without a
    /// hold this does nothing at all, so it cannot trigger execution.
    pub fn release(&self, character_id: i64) {
        let released = {
            let mut control = self.control.lock().expect("control lock poisoned");
            let before = control.holds.len();
            control
                .holds
                .retain(|(_, held_by)| *held_by != character_id);
            control.holds.len() != before
        };
        if released {
            self.changed_control();
            self.released.notify_one();
        }
    }

    /// Queue the character's input and release its hold together, so NPC work
    /// can never overtake input the player has just sent.
    pub fn send_releasing(
        &self,
        permit: mpsc::Permit<'_, WorldEvent>,
        event: WorldEvent,
        character_id: i64,
    ) {
        let released = {
            let mut control = self.control.lock().expect("control lock poisoned");
            permit.send(event);
            let before = control.holds.len();
            control
                .holds
                .retain(|(_, held_by)| *held_by != character_id);
            control.holds.len() != before
        };
        if released {
            self.changed_control();
        }
    }

    /// Every live hold, for checking that each still belongs to its group.
    pub fn holds(&self) -> Vec<(i64, i64)> {
        self.control
            .lock()
            .expect("control lock poisoned")
            .holds
            .iter()
            .copied()
            .collect()
    }

    /// Holds whose character has left the group, or whose group no longer
    /// exists, cannot outlive that.
    pub fn drop_holds(&self, stale: &[(i64, i64)]) {
        let dropped = {
            let mut control = self.control.lock().expect("control lock poisoned");
            let before = control.holds.len();
            control.holds.retain(|hold| !stale.contains(hold));
            control.holds.len() != before
        };
        if dropped {
            self.changed_control();
            self.released.notify_one();
        }
    }

    pub fn group_held(&self, group_id: i64) -> bool {
        self.control
            .lock()
            .expect("control lock poisoned")
            .holds
            .iter()
            .any(|(group, _)| *group == group_id)
    }

    /// What this player's controls show for their current group.
    pub fn control_view(&self, group_id: i64, character_id: i64) -> ControlView {
        let control = self.control.lock().expect("control lock poisoned");
        let held = control.holds.iter().any(|(group, _)| *group == group_id);
        ControlView {
            own_hold: control.holds.contains(&(group_id, character_id)),
            paused: held && control.quiescent(group_id),
        }
    }

    /// Start NPC work in a group unless it is held. The check and the claim
    /// share one lock, so an Interrupt cannot slip in between them.
    pub fn begin_npc(&self, group_id: i64, publishes: bool) -> Option<ActivityGuard<'_>> {
        let mut control = self.control.lock().expect("control lock poisoned");
        if control.holds.iter().any(|(group, _)| *group == group_id) {
            return None;
        }
        control.activity = if publishes {
            Activity::NpcAction { group_id }
        } else {
            Activity::NpcDecision { group_id }
        };
        drop(control);
        self.changed_control();
        Some(ActivityGuard { world: self })
    }

    /// Mark a human's turn in this group as running.
    pub fn begin_foreground(&self, group_id: i64) -> ActivityGuard<'_> {
        self.control.lock().expect("control lock poisoned").activity =
            Activity::Foreground { group_id };
        self.changed_control();
        ActivityGuard { world: self }
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
