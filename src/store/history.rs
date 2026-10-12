//! The shared raw-history backbone: allocating positions, indexing entries,
//! and traversing bounded parent branches. design.md's Chat schema and
//! "Raw history queries traverse bounded parent branches, then their own
//! entries in position order" define the contract this module implements
//! once, so context assembly (live segments) and raw inspection paging both
//! walk the same selector instead of duplicating branch logic.

use anyhow::{Context, Result, ensure};
use sqlx::{FromRow, Sqlite, Transaction};

use super::Store;

// `HistoryHead` and `PositionRange` live in `crate::inspection` (shared with
// the browser); re-exported here since this module builds and consumes them.
pub use crate::inspection::{HistoryHead, PositionRange};

/// One entry kind stored in `history_entry.kind`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EntryKind {
    Message,
    Inference,
    Summary,
    Notice,
}

impl EntryKind {
    fn as_str(self) -> &'static str {
        match self {
            EntryKind::Message => "message",
            EntryKind::Inference => "inference",
            EntryKind::Summary => "summary",
            EntryKind::Notice => "notice",
        }
    }

    fn parse(value: &str) -> Result<Self> {
        Ok(match value {
            "message" => EntryKind::Message,
            "inference" => EntryKind::Inference,
            "summary" => EntryKind::Summary,
            "notice" => EntryKind::Notice,
            other => anyhow::bail!("unknown history_entry kind {other}"),
        })
    }
}

/// One row of the raw history index, resolved to its branch/position.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HistoryEntry {
    pub id: i64,
    pub branch_id: i64,
    pub position: i64,
    pub kind: EntryKind,
}

#[derive(Debug, FromRow)]
struct HistoryEntryRow {
    id: i64,
    branch_id: i64,
    position: i64,
    kind: String,
}

impl TryFrom<HistoryEntryRow> for HistoryEntry {
    type Error = anyhow::Error;

    fn try_from(row: HistoryEntryRow) -> Result<Self> {
        Ok(Self {
            id: row.id,
            branch_id: row.branch_id,
            position: row.position,
            kind: EntryKind::parse(&row.kind)?,
        })
    }
}

/// One immutable bounded parent reference, in ordinal order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ParentRef {
    parent_branch_id: i64,
    through_position: i64,
}

impl Store {
    /// Allocate the next position in `branch_id` and return it, using
    /// UPDATE...RETURNING so concurrent writers to the same branch serialize
    /// on the row rather than racing a separate SELECT MAX.
    pub(super) async fn allocate_position(
        transaction: &mut Transaction<'_, Sqlite>,
        branch_id: i64,
    ) -> Result<i64> {
        let position: i64 = sqlx::query_scalar(
            "UPDATE history_branch SET next_position = next_position + 1 \
             WHERE id = ? RETURNING next_position - 1",
        )
        .bind(branch_id)
        .fetch_optional(&mut **transaction)
        .await
        .with_context(|| format!("allocating history position on branch {branch_id}"))?
        .with_context(|| format!("history branch {branch_id} does not exist"))?;
        Ok(position)
    }

    /// Insert one `history_entry` row at an already-allocated position and
    /// return its id, which the caller's typed row (message/inference/notice)
    /// reuses as its own primary key.
    pub(super) async fn insert_history_entry(
        transaction: &mut Transaction<'_, Sqlite>,
        branch_id: i64,
        position: i64,
        kind: EntryKind,
    ) -> Result<i64> {
        let result = sqlx::query("INSERT INTO history_entry (branch_id, position, kind) VALUES (?, ?, ?)")
            .bind(branch_id)
            .bind(position)
            .bind(kind.as_str())
            .execute(&mut **transaction)
            .await
            .with_context(|| format!("inserting history entry on branch {branch_id}"))?;
        Ok(result.last_insert_rowid())
    }

    /// Allocate a position and insert its history_entry in one step, the
    /// common case for every producer (message append, inference start,
    /// summary activation, notice).
    pub(super) async fn append_history_entry(
        transaction: &mut Transaction<'_, Sqlite>,
        branch_id: i64,
        kind: EntryKind,
    ) -> Result<(i64, i64)> {
        let position = Self::allocate_position(transaction, branch_id).await?;
        let id = Self::insert_history_entry(transaction, branch_id, position, kind).await?;
        Ok((id, position))
    }

    /// This branch's own immutable parent references, ordered by ordinal.
    async fn parents_of(
        &self,
        branch_id: i64,
    ) -> Result<Vec<ParentRef>> {
        sqlx::query_as::<_, (i64, i64)>(
            "SELECT parent_branch_id, through_position FROM history_parent \
             WHERE branch_id = ? ORDER BY ordinal",
        )
        .bind(branch_id)
        .fetch_all(&self.pool)
        .await
        .with_context(|| format!("loading parent branches of {branch_id}"))?
        .into_iter()
        .map(|(parent_branch_id, through_position)| {
            Ok(ParentRef {
                parent_branch_id,
                through_position,
            })
        })
        .collect()
    }

    /// The agent and world a branch belongs to, for the world/agent boundary
    /// check every traversal entry point performs before walking anything.
    /// A branch belongs to its `agent_history` row, or - for a world_gm
    /// agent, which plays several groups and so has no single `agent_history`
    /// row - to the group that owns it (design.md's `GroupHistory { agent_id,
    /// branch_id }`).
    pub(super) async fn branch_owner(&self, branch_id: i64) -> Result<(i64, i64)> {
        if let Some(owner) = sqlx::query_as::<_, (i64, i64)>(
            "SELECT agent_history.agent_id, agent.world_id \
             FROM agent_history JOIN agent ON agent.id = agent_history.agent_id \
             WHERE agent_history.branch_id = ?",
        )
        .bind(branch_id)
        .fetch_optional(&self.pool)
        .await
        .with_context(|| format!("looking up owner of branch {branch_id}"))?
        {
            return Ok(owner);
        }
        sqlx::query_as::<_, (i64, i64)>(
            "SELECT world_gm.agent_id, \"group\".world_id \
             FROM \"group\" JOIN world_gm ON world_gm.world_id = \"group\".world_id \
             WHERE \"group\".branch_id = ?",
        )
        .bind(branch_id)
        .fetch_optional(&self.pool)
        .await
        .with_context(|| format!("looking up group owner of branch {branch_id}"))?
        .with_context(|| format!("history branch {branch_id} does not exist"))
    }

    /// Bounded parent-first traversal of one history head, for a world/agent
    /// already known to own it. Rejects missing parents and cycles: `visited`
    /// carries the branch ids already on the current recursion path, and
    /// finding one again is a cycle rather than legitimate reuse (a real
    /// convergent DAG still traverses fine, since revisiting a branch only on
    /// the *current path* is what indicates a cycle).
    fn range<'a>(
        &'a self,
        head: HistoryHead,
        visited: &'a mut Vec<i64>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Vec<HistoryEntry>>> + Send + 'a>>
    {
        Box::pin(async move {
            ensure!(
                !visited.contains(&head.branch_id),
                "history branch {} is its own ancestor (cycle)",
                head.branch_id
            );
            visited.push(head.branch_id);
            let mut entries = Vec::new();
            for parent in self.parents_of(head.branch_id).await? {
                let exists: Option<i64> = sqlx::query_scalar(
                    "SELECT 1 FROM history_branch WHERE id = ?",
                )
                .bind(parent.parent_branch_id)
                .fetch_optional(&self.pool)
                .await
                .with_context(|| format!("checking parent branch {}", parent.parent_branch_id))?;
                ensure!(
                    exists.is_some(),
                    "parent branch {} of {} does not exist",
                    parent.parent_branch_id,
                    head.branch_id
                );
                let parent_entries = self
                    .range(
                        HistoryHead {
                            branch_id: parent.parent_branch_id,
                            cut: parent.through_position,
                        },
                        visited,
                    )
                    .await
                    .with_context(|| {
                        format!(
                            "traversing parent branch {} of {}",
                            parent.parent_branch_id, head.branch_id
                        )
                    })?;
                // Subtract any positions this ancestor already covers via its
                // own parents: an entry emitted once by a deeper ancestor is
                // not re-emitted through this shorter path to the same range.
                for entry in parent_entries {
                    if !entries.iter().any(|existing: &HistoryEntry| {
                        existing.branch_id == entry.branch_id && existing.position == entry.position
                    }) {
                        entries.push(entry);
                    }
                }
            }
            visited.pop();
            let own = sqlx::query_as::<_, HistoryEntryRow>(
                "SELECT id, branch_id, position, kind FROM history_entry \
                 WHERE branch_id = ? AND position <= ? ORDER BY position",
            )
            .bind(head.branch_id)
            .bind(head.cut)
            .fetch_all(&self.pool)
            .await
            .with_context(|| format!("loading entries of branch {}", head.branch_id))?;
            for row in own {
                entries.push(HistoryEntry::try_from(row)?);
            }
            Ok(entries)
        })
    }

    /// Every history entry reachable from `head`, in causal order: bounded
    /// parent branches first (each already deduplicated against branches it
    /// shares with an earlier-visited ancestor), then this branch's own
    /// entries up to its cut. Rejects a head from a foreign world/agent, a
    /// missing parent branch, or a cycle.
    pub async fn history_range(
        &self,
        head: HistoryHead,
        world_id: i64,
        agent_id: i64,
    ) -> Result<Vec<HistoryEntry>> {
        let (owner_agent_id, owner_world_id) = self.branch_owner(head.branch_id).await?;
        ensure!(
            owner_world_id == world_id,
            "history branch {} is not in world {world_id}",
            head.branch_id
        );
        ensure!(
            owner_agent_id == agent_id,
            "history branch {} does not belong to agent {agent_id}",
            head.branch_id
        );
        let mut visited = Vec::new();
        self.range(head, &mut visited).await
    }

    /// The current head of an agent's single writable branch: its
    /// `agent_history` branch, cut at that branch's latest position.
    pub(super) async fn current_head(&self, agent_id: i64) -> Result<HistoryHead> {
        let branch_id = self.branch_for_agent(agent_id).await?;
        self.current_head_for_branch(branch_id).await
    }

    /// As `current_head`, for a branch chosen explicitly rather than an
    /// agent's single `agent_history` row - a world_gm agent has one branch
    /// per group, never an `agent_history` row (design.md's `GroupHistory {
    /// agent_id, branch_id }`: "agent ID alone cannot select a GM history").
    pub(super) async fn current_head_for_branch(&self, branch_id: i64) -> Result<HistoryHead> {
        let next_position: i64 =
            sqlx::query_scalar("SELECT next_position FROM history_branch WHERE id = ?")
                .bind(branch_id)
                .fetch_optional(&self.pool)
                .await
                .with_context(|| format!("loading history branch {branch_id}"))?
                .with_context(|| format!("history branch {branch_id} does not exist"))?;
        Ok(HistoryHead {
            branch_id,
            cut: next_position - 1,
        })
    }

    /// The branch backing an agent's single writable history.
    pub(super) async fn branch_for_agent(&self, agent_id: i64) -> Result<i64> {
        sqlx::query_scalar("SELECT branch_id FROM agent_history WHERE agent_id = ?")
            .bind(agent_id)
            .fetch_optional(&self.pool)
            .await
            .with_context(|| format!("loading history branch for agent {agent_id}"))?
            .with_context(|| format!("agent {agent_id} has no history branch"))
    }

    /// As `branch_for_agent`, but reading through an already-open transaction
    /// rather than a fresh pool connection. The pool is configured with a
    /// single connection, so any query against `&self.pool` while a
    /// transaction is open would otherwise deadlock waiting for a connection
    /// the open transaction is holding.
    pub(super) async fn branch_for_agent_tx(
        transaction: &mut Transaction<'_, Sqlite>,
        agent_id: i64,
    ) -> Result<i64> {
        sqlx::query_scalar("SELECT branch_id FROM agent_history WHERE agent_id = ?")
            .bind(agent_id)
            .fetch_optional(&mut **transaction)
            .await
            .with_context(|| format!("loading history branch for agent {agent_id}"))?
            .with_context(|| format!("agent {agent_id} has no history branch"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::TestDatabase;

    async fn new_branch(store: &Store, agent_id: i64) -> i64 {
        sqlx::query_scalar("INSERT INTO history_branch (agent_id) VALUES (?) RETURNING id")
            .bind(agent_id)
            .fetch_one(&store.pool)
            .await
            .unwrap()
    }

    async fn add_entries(store: &Store, branch_id: i64, count: i64) {
        let mut transaction = store.pool.begin().await.unwrap();
        for _ in 0..count {
            Store::append_history_entry(&mut transaction, branch_id, EntryKind::Notice)
                .await
                .unwrap();
        }
        transaction.commit().await.unwrap();
    }

    async fn link_parent(store: &Store, branch_id: i64, ordinal: i64, parent_branch_id: i64, through_position: i64) {
        sqlx::query(
            "INSERT INTO history_parent (branch_id, ordinal, parent_branch_id, through_position) VALUES (?, ?, ?, ?)",
        )
        .bind(branch_id)
        .bind(ordinal)
        .bind(parent_branch_id)
        .bind(through_position)
        .execute(&store.pool)
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn traversal_walks_bounded_parents_before_its_own_entries() {
        let db = TestDatabase::new("history-test");
        let store = Store::open(db.path()).await.unwrap();
        let world = store.create_world("test").await.unwrap();
        let agent = store.create_agent(world).await.unwrap();

        let root = new_branch(&store, agent).await;
        add_entries(&store, root, 3).await; // positions 0,1,2

        let child = new_branch(&store, agent).await;
        link_parent(&store, child, 0, root, 1).await; // bounded through position 1 (2 entries)
        add_entries(&store, child, 2).await; // positions 0,1

        // create_agent already inserted this agent's default branch/row;
        // point its single writable head at the synthetic branch this test built.
        sqlx::query("UPDATE agent_history SET branch_id = ? WHERE agent_id = ?")
            .bind(child)
            .bind(agent)
            .execute(&store.pool)
            .await
            .unwrap();

        let entries = store
            .history_range(HistoryHead { branch_id: child, cut: 1 }, world, agent)
            .await
            .unwrap();
        // Parent contributes only positions 0..=1 (bounded), then child's own
        // positions 0..=1, in that order.
        assert_eq!(
            entries
                .iter()
                .map(|entry| (entry.branch_id, entry.position))
                .collect::<Vec<_>>(),
            vec![(root, 0), (root, 1), (child, 0), (child, 1)]
        );
    }

    #[tokio::test]
    async fn traversal_subtracts_ranges_already_covered_by_a_shared_ancestor() {
        let db = TestDatabase::new("history-test");
        let store = Store::open(db.path()).await.unwrap();
        let world = store.create_world("test").await.unwrap();
        let agent = store.create_agent(world).await.unwrap();

        // root -> mid -> leaf, and leaf also references root directly. The
        // direct root reference must not re-emit what mid already carried.
        let root = new_branch(&store, agent).await;
        add_entries(&store, root, 2).await; // 0,1

        let mid = new_branch(&store, agent).await;
        link_parent(&store, mid, 0, root, 1).await;
        add_entries(&store, mid, 1).await; // mid position 0

        let leaf = new_branch(&store, agent).await;
        link_parent(&store, leaf, 0, mid, 0).await;
        link_parent(&store, leaf, 1, root, 1).await; // same range mid already covered
        add_entries(&store, leaf, 1).await; // leaf position 0

        // create_agent already inserted this agent's default branch/row;
        // point its single writable head at the synthetic branch this test built.
        sqlx::query("UPDATE agent_history SET branch_id = ? WHERE agent_id = ?")
            .bind(leaf)
            .bind(agent)
            .execute(&store.pool)
            .await
            .unwrap();

        let entries = store
            .history_range(HistoryHead { branch_id: leaf, cut: 0 }, world, agent)
            .await
            .unwrap();
        // root's two entries appear exactly once even though both mid and
        // leaf's own parent list reach them.
        let root_entries = entries.iter().filter(|entry| entry.branch_id == root).count();
        assert_eq!(root_entries, 2, "root entries must not be duplicated: {entries:?}");
        assert_eq!(
            entries
                .iter()
                .map(|entry| (entry.branch_id, entry.position))
                .collect::<Vec<_>>(),
            vec![(root, 0), (root, 1), (mid, 0), (leaf, 0)]
        );
    }

    #[tokio::test]
    async fn traversal_rejects_a_cycle_instead_of_looping() {
        let db = TestDatabase::new("history-test");
        let store = Store::open(db.path()).await.unwrap();
        let world = store.create_world("test").await.unwrap();
        let agent = store.create_agent(world).await.unwrap();

        let a = new_branch(&store, agent).await;
        let b = new_branch(&store, agent).await;
        link_parent(&store, a, 0, b, 0).await;
        link_parent(&store, b, 0, a, 0).await;
        // create_agent already inserted this agent's default branch/row;
        // point its single writable head at the synthetic branch this test built.
        sqlx::query("UPDATE agent_history SET branch_id = ? WHERE agent_id = ?")
            .bind(a)
            .bind(agent)
            .execute(&store.pool)
            .await
            .unwrap();

        let error = store
            .history_range(HistoryHead { branch_id: a, cut: 0 }, world, agent)
            .await
            .expect_err("a branch cycle must be rejected, not looped or stack-overflowed");
        assert!(format!("{error:#}").contains("cycle"), "{error:#}");
    }

    #[tokio::test]
    async fn traversal_rejects_a_foreign_agent_or_world() {
        let db = TestDatabase::new("history-test");
        let store = Store::open(db.path()).await.unwrap();
        let world = store.create_world("test").await.unwrap();
        let other_world = store.create_world("other").await.unwrap();
        let agent = store.create_agent(world).await.unwrap();
        let other_agent = store.create_agent(world).await.unwrap();
        let branch = new_branch(&store, agent).await;
        // create_agent already inserted this agent's default branch/row;
        // point its single writable head at the synthetic branch this test built.
        sqlx::query("UPDATE agent_history SET branch_id = ? WHERE agent_id = ?")
            .bind(branch)
            .bind(agent)
            .execute(&store.pool)
            .await
            .unwrap();

        let head = HistoryHead { branch_id: branch, cut: -1 };
        assert!(
            store
                .history_range(head, world, other_agent)
                .await
                .unwrap_err()
                .to_string()
                .contains("does not belong to agent")
        );
        assert!(
            store
                .history_range(head, other_world, agent)
                .await
                .unwrap_err()
                .to_string()
                .contains("is not in world")
        );
    }

    #[tokio::test]
    async fn traversal_rejects_a_missing_parent_branch() {
        let db = TestDatabase::new("history-test");
        let store = Store::open(db.path()).await.unwrap();
        let world = store.create_world("test").await.unwrap();
        let agent = store.create_agent(world).await.unwrap();
        let branch = new_branch(&store, agent).await;
        // A parent reference to a nonexistent branch cannot occur through
        // normal writes - history_parent.parent_branch_id is a foreign key.
        // This constructs the corrupted state directly (FK checks off for
        // just this insert) to prove the traversal also independently
        // rejects it, rather than relying on the constraint alone.
        sqlx::query("PRAGMA foreign_keys = OFF")
            .execute(&store.pool)
            .await
            .unwrap();
        link_parent(&store, branch, 0, 999_999, 0).await;
        sqlx::query("PRAGMA foreign_keys = ON")
            .execute(&store.pool)
            .await
            .unwrap();
        // create_agent already inserted this agent's default branch/row;
        // point its single writable head at the synthetic branch this test built.
        sqlx::query("UPDATE agent_history SET branch_id = ? WHERE agent_id = ?")
            .bind(branch)
            .bind(agent)
            .execute(&store.pool)
            .await
            .unwrap();

        let error = store
            .history_range(HistoryHead { branch_id: branch, cut: -1 }, world, agent)
            .await
            .expect_err("a missing parent branch must fail, not be silently skipped");
        assert!(format!("{error:#}").contains("does not exist"), "{error:#}");
    }
}
