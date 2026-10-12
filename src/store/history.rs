//! The shared raw-history backbone: allocating positions, indexing entries,
//! and traversing bounded parent branches. design.md's Chat schema and
//! "Raw history queries traverse bounded parent branches, then their own
//! entries in position order" define the contract this module implements
//! once, so context assembly (live segments) and raw inspection paging both
//! walk the same selector instead of duplicating branch logic.

use anyhow::{Context, Result, ensure};
use sqlx::{FromRow, Sqlite, Transaction};

use super::Store;

// `HistoryHead`, `PositionRange` and `Segment` live in `crate::inspection`
// (shared with the browser); re-exported here since this module builds and
// consumes them.
pub use crate::inspection::{HistoryHead, PositionRange};
use crate::inspection::Segment;

/// A `HistoryHead` already validated against the world/agent that owns it -
/// holding one proves the check passed, so `history_range` and its callers
/// never re-validate a head they were only just given. Private fields: the
/// only way to get one is `Store::branch_head_for_agent`,
/// `Store::branch_head_for_branch` or `Store::branch_head_from_cursor`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BranchHead {
    world_id: i64,
    agent_id: i64,
    head: HistoryHead,
}

impl BranchHead {
    pub fn head(&self) -> HistoryHead {
        self.head
    }
}

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
        let result =
            sqlx::query("INSERT INTO history_entry (branch_id, position, kind) VALUES (?, ?, ?)")
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

    /// The agent and world a branch belongs to. `history_branch.agent_id`
    /// already names the owning agent directly for every branch, including a
    /// world_gm agent's group branches - `spawn_group_branch` always binds
    /// the world's single `world_gm.agent_id` at creation (design.md's
    /// `GroupHistory { agent_id, branch_id }`) - so this needs no fallback
    /// through `agent_history`/`group`.
    pub(super) async fn branch_owner(&self, branch_id: i64) -> Result<(i64, i64)> {
        sqlx::query_as::<_, (i64, i64)>(
            "SELECT history_branch.agent_id, agent.world_id \
             FROM history_branch JOIN agent ON agent.id = history_branch.agent_id \
             WHERE history_branch.id = ?",
        )
        .bind(branch_id)
        .fetch_optional(&self.pool)
        .await
        .with_context(|| format!("looking up owner of branch {branch_id}"))?
        .with_context(|| format!("history branch {branch_id} does not exist"))
    }

    /// Every branch reachable from `head`, each bounded to the furthest
    /// position any path from `head` reaches it through (several paths to
    /// one branch collapse to its longest prefix - the union of prefixes -
    /// so nothing downstream needs to deduplicate), in ancestor-first order.
    /// One query: `history_parent`'s keys guarantee every parent exists,
    /// shares this branch's agent and has a strictly smaller id, so a plain
    /// `ORDER BY branch_id` is already a valid topological (ancestors-first)
    /// order, and the recursion always terminates without a visited-set of
    /// its own.
    async fn reach(&self, head: HistoryHead) -> Result<Vec<HistoryHead>> {
        let rows = sqlx::query_as::<_, (i64, i64)>(
            "WITH RECURSIVE reach(branch_id, cut) AS ( \
                 SELECT ?, ? \
                 UNION \
                 SELECT p.parent_branch_id, p.through_position \
                 FROM history_parent p JOIN reach r ON p.branch_id = r.branch_id \
             ) \
             SELECT branch_id, MAX(cut) FROM reach GROUP BY branch_id ORDER BY branch_id",
        )
        .bind(head.branch_id)
        .bind(head.cut)
        .fetch_all(&self.pool)
        .await
        .with_context(|| format!("computing reach of branch {}", head.branch_id))?;
        Ok(rows
            .into_iter()
            .map(|(branch_id, cut)| HistoryHead { branch_id, cut })
            .collect())
    }

    /// Every history entry reachable from `head`, in causal order: each
    /// reached branch's own entries up to its bounded cut, ancestor-first
    /// (`reach`'s order). No production caller needs this unfiltered a load
    /// - `history_live_segments` and `raw_page` each select only what they
    /// need directly - so this exists to test `reach`'s bounding/dedup
    /// behavior precisely and directly, which neither of those two (each
    /// with its own additional filtering) can exercise as exactly.
    #[cfg(test)]
    async fn history_range(&self, head: BranchHead) -> Result<Vec<HistoryEntry>> {
        let mut entries = Vec::new();
        for bounded in self.reach(head.head).await? {
            let rows = sqlx::query_as::<_, HistoryEntryRow>(
                "SELECT id, branch_id, position, kind FROM history_entry \
                 WHERE branch_id = ? AND position <= ? ORDER BY position",
            )
            .bind(bounded.branch_id)
            .bind(bounded.cut)
            .fetch_all(&self.pool)
            .await
            .with_context(|| format!("loading entries of branch {}", bounded.branch_id))?;
            for row in rows {
                entries.push(HistoryEntry::try_from(row)?);
            }
        }
        Ok(entries)
    }

    /// One page of raw history entries reachable from `head`, ancestor-first
    /// then by position (`reach`'s order), selected and paged entirely in
    /// SQL: `after` resumes a keyset scan at `(branch_id, position)` rather
    /// than loading and re-slicing every entry on every page. A successful
    /// inference's own entry is excluded
    /// in the same query (its assistant message entry is what should
    /// render); only a failed inference (no message was ever produced)
    /// passes through. Returns up to `limit` entries plus, when there are
    /// more, the `(branch_id, position)` to resume after.
    ///
    /// `after`, when given, must name a real position within `head`'s reach;
    /// a stale or foreign cursor is rejected rather than silently treated as
    /// end-of-history.
    pub async fn raw_page(
        &self,
        head: BranchHead,
        after: Option<(i64, i64)>,
        limit: i64,
    ) -> Result<(Vec<HistoryEntry>, Option<(i64, i64)>)> {
        if let Some((branch_id, position)) = after {
            let bounded_cut: Option<i64> = sqlx::query_scalar(
                "WITH RECURSIVE reach(branch_id, cut) AS ( \
                     SELECT ?, ? \
                     UNION \
                     SELECT p.parent_branch_id, p.through_position \
                     FROM history_parent p JOIN reach r ON p.branch_id = r.branch_id \
                 ) \
                 SELECT MAX(cut) FROM reach WHERE branch_id = ?",
            )
            .bind(head.head.branch_id)
            .bind(head.head.cut)
            .bind(branch_id)
            .fetch_one(&self.pool)
            .await
            .with_context(|| format!("checking raw history cursor branch {branch_id}"))?;
            ensure!(
                bounded_cut.is_some_and(|cut| position <= cut),
                "raw history cursor position {position} on branch {branch_id} is outside this history"
            );
        }
        let (after_branch_id, after_position) = after.unwrap_or((i64::MIN, i64::MIN));
        let limit_plus_one = limit + 1;
        let rows = sqlx::query_as::<_, HistoryEntryRow>(
            "WITH RECURSIVE reach(branch_id, cut) AS ( \
                 SELECT ?, ? \
                 UNION \
                 SELECT p.parent_branch_id, p.through_position \
                 FROM history_parent p JOIN reach r ON p.branch_id = r.branch_id \
             ), bounded AS ( \
                 SELECT branch_id, MAX(cut) AS cut FROM reach GROUP BY branch_id \
             ) \
             SELECT history_entry.id, history_entry.branch_id, history_entry.position, history_entry.kind \
             FROM history_entry \
             JOIN bounded ON bounded.branch_id = history_entry.branch_id \
                 AND history_entry.position <= bounded.cut \
             LEFT JOIN inference ON inference.history_entry_id = history_entry.id \
             WHERE (history_entry.kind != 'inference' OR inference.error IS NOT NULL) \
                 AND (history_entry.branch_id > ? \
                      OR (history_entry.branch_id = ? AND history_entry.position >= ?)) \
             ORDER BY history_entry.branch_id, history_entry.position \
             LIMIT ?",
        )
        .bind(head.head.branch_id)
        .bind(head.head.cut)
        .bind(after_branch_id)
        .bind(after_branch_id)
        .bind(after_position)
        .bind(limit_plus_one)
        .fetch_all(&self.pool)
        .await
        .with_context(|| format!("loading raw history page from branch {}", head.head.branch_id))?;
        let mut entries: Vec<HistoryEntry> = rows
            .into_iter()
            .map(HistoryEntry::try_from)
            .collect::<Result<_>>()?;
        let next = if entries.len() > usize::try_from(limit).context("raw history page limit must not be negative")? {
            let next_entry = entries.pop().expect("checked len above");
            Some((next_entry.branch_id, next_entry.position))
        } else {
            None
        };
        Ok((entries, next))
    }

    /// The live history is, per branch touched by the bounded-parent
    /// traversal, that branch's own newest disjoint summary (if any)
    /// followed by every one of its raw messages not covered by any selected
    /// summary. A merge can bring in several branches, each with its own
    /// independent summary covering its own disjoint range - unlike a plain
    /// linear branch, "the newest summary in the whole traversal" is not
    /// enough, since an older parent branch's summary is never superseded by
    /// a sibling branch's newer one. Older messages remain stored for replay
    /// and debugging.
    ///
    /// Unlike `history_range`, this never loads a covered or non-message
    /// entry: one query finds each reached branch's newest summary (if any)
    /// within its bounded cut, `covered_ranges` decoding (bounded by branch
    /// count, not entry count) finds how far it covers *this* branch, and a
    /// second query per branch selects only the uncovered message positions.
    pub async fn history_live_segments(&self, head: BranchHead) -> Result<Vec<Segment>> {
        let mut segments = Vec::new();
        for bounded in self.reach(head.head).await? {
            let summary: Option<(i64, String)> = sqlx::query_as(
                "SELECT summary.id, summary.covered_ranges \
                 FROM history_entry \
                 JOIN summary ON summary.history_entry_id = history_entry.id \
                 WHERE history_entry.branch_id = ? AND history_entry.position = ( \
                     SELECT MAX(he.position) FROM history_entry he \
                     JOIN summary s ON s.history_entry_id = he.id \
                     WHERE he.branch_id = ? AND he.position <= ? \
                 )",
            )
            .bind(bounded.branch_id)
            .bind(bounded.branch_id)
            .bind(bounded.cut)
            .fetch_optional(&self.pool)
            .await
            .with_context(|| format!("loading latest summary for branch {}", bounded.branch_id))?;
            // Not covered by the selected summary, not merely positioned
            // after it: a summary activation's own position in the unified
            // index can land after messages it does not cover, since it is
            // produced by a separate compaction inference recorded later.
            let covered_through = match summary {
                Some((summary_id, covered_ranges)) => {
                    let covered_ranges: Vec<PositionRange> = serde_json::from_str(&covered_ranges)
                        .context("decoding summary covered ranges")?;
                    segments.push(Segment::Summary { summary: summary_id });
                    covered_ranges
                        .iter()
                        .filter(|range| range.branch_id == bounded.branch_id)
                        .map(|range| range.last_position)
                        .max()
                }
                None => None,
            };
            let range: (Option<i64>, Option<i64>) = sqlx::query_as(
                "SELECT MIN(position), MAX(position) FROM history_entry \
                 WHERE branch_id = ? AND kind = 'message' AND position <= ? \
                 AND position > ?",
            )
            .bind(bounded.branch_id)
            .bind(bounded.cut)
            .bind(covered_through.unwrap_or(-1))
            .fetch_one(&self.pool)
            .await
            .with_context(|| format!("finding uncovered message range for branch {}", bounded.branch_id))?;
            if let (Some(first_position), Some(last_position)) = range {
                segments.push(Segment::Messages {
                    messages: PositionRange {
                        branch_id: bounded.branch_id,
                        first_position,
                        last_position,
                    },
                });
            }
        }
        Ok(segments)
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

    /// An agent's own current writable head, validated against the world
    /// claimed to own that agent - a caller-supplied `agent_id` alone is not
    /// proof it belongs to the caller's authorized `world_id`.
    pub async fn branch_head_for_agent(&self, world_id: i64, agent_id: i64) -> Result<BranchHead> {
        let owner_world_id = self.world_id_for_agent(agent_id).await?;
        ensure!(
            owner_world_id == world_id,
            "agent {agent_id} is not in world {world_id}"
        );
        let head = self.current_head(agent_id).await?;
        Ok(BranchHead { world_id, agent_id, head })
    }

    /// An explicit branch's current head, validated against the agent and
    /// world claimed to own it - a world_gm agent selecting one of its own
    /// group branches, or any other caller that already has a branch id but
    /// still needs its ownership confirmed before trusting it.
    pub async fn branch_head_for_branch(
        &self,
        branch_id: i64,
        world_id: i64,
        agent_id: i64,
    ) -> Result<BranchHead> {
        let head = self.current_head_for_branch(branch_id).await?;
        self.validate_branch_head(head, world_id, agent_id).await
    }

    /// A browser-supplied cursor's head, validated against the viewer's
    /// authorized world/agent before any traversal trusts it - the same
    /// validation `history_range` used to perform internally on every call,
    /// now performed once when the head is constructed.
    pub async fn branch_head_from_cursor(
        &self,
        head: HistoryHead,
        world_id: i64,
        agent_id: i64,
    ) -> Result<BranchHead> {
        self.validate_branch_head(head, world_id, agent_id).await
    }

    async fn validate_branch_head(
        &self,
        head: HistoryHead,
        world_id: i64,
        agent_id: i64,
    ) -> Result<BranchHead> {
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
        Ok(BranchHead { world_id, agent_id, head })
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

    async fn link_parent(
        store: &Store,
        branch_id: i64,
        agent_id: i64,
        ordinal: i64,
        parent_branch_id: i64,
        through_position: i64,
    ) -> sqlx::Result<()> {
        sqlx::query(
            "INSERT INTO history_parent (branch_id, agent_id, ordinal, parent_branch_id, through_position) \
             VALUES (?, ?, ?, ?, ?)",
        )
        .bind(branch_id)
        .bind(agent_id)
        .bind(ordinal)
        .bind(parent_branch_id)
        .bind(through_position)
        .execute(&store.pool)
        .await
        .map(|_| ())
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
        link_parent(&store, child, agent, 0, root, 1).await.unwrap(); // bounded through position 1 (2 entries)
        add_entries(&store, child, 2).await; // positions 0,1

        // create_agent already inserted this agent's default branch/row;
        // point its single writable head at the synthetic branch this test built.
        sqlx::query("UPDATE agent_history SET branch_id = ? WHERE agent_id = ?")
            .bind(child)
            .bind(agent)
            .execute(&store.pool)
            .await
            .unwrap();

        let head = store
            .branch_head_from_cursor(HistoryHead { branch_id: child, cut: 1 }, world, agent)
            .await
            .unwrap();
        let entries = store.history_range(head).await.unwrap();
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
        link_parent(&store, mid, agent, 0, root, 1).await.unwrap();
        add_entries(&store, mid, 1).await; // mid position 0

        let leaf = new_branch(&store, agent).await;
        link_parent(&store, leaf, agent, 0, mid, 0).await.unwrap();
        link_parent(&store, leaf, agent, 1, root, 1).await.unwrap(); // same range mid already covered
        add_entries(&store, leaf, 1).await; // leaf position 0

        // create_agent already inserted this agent's default branch/row;
        // point its single writable head at the synthetic branch this test built.
        sqlx::query("UPDATE agent_history SET branch_id = ? WHERE agent_id = ?")
            .bind(leaf)
            .bind(agent)
            .execute(&store.pool)
            .await
            .unwrap();

        let head = store
            .branch_head_from_cursor(HistoryHead { branch_id: leaf, cut: 0 }, world, agent)
            .await
            .unwrap();
        let entries = store.history_range(head).await.unwrap();
        // root's two entries appear exactly once even though both mid and
        // leaf's own parent list reach them.
        let root_entries = entries
            .iter()
            .filter(|entry| entry.branch_id == root)
            .count();
        assert_eq!(
            root_entries, 2,
            "root entries must not be duplicated: {entries:?}"
        );
        assert_eq!(
            entries
                .iter()
                .map(|entry| (entry.branch_id, entry.position))
                .collect::<Vec<_>>(),
            vec![(root, 0), (root, 1), (mid, 0), (leaf, 0)]
        );
    }

    /// design.md: parents "belong to the same agent and predate their child".
    /// The schema rejects any other link, so no traversal can reach a foreign
    /// world's history or loop.
    #[tokio::test]
    async fn parent_links_must_share_the_agent_and_predate_the_child() {
        let db = TestDatabase::new("history-test");
        let store = Store::open(db.path()).await.unwrap();
        let world = store.create_world("test").await.unwrap();
        let agent = store.create_agent(world).await.unwrap();
        let other_agent = store.create_agent(world).await.unwrap();
        let parent = new_branch(&store, agent).await;
        let foreign = new_branch(&store, other_agent).await;
        let child = new_branch(&store, agent).await;

        link_parent(&store, child, agent, 0, foreign, 0)
            .await
            .expect_err("a parent owned by another agent must be rejected");
        link_parent(&store, child, other_agent, 0, foreign, 0)
            .await
            .expect_err("a child owned by another agent must be rejected");
        link_parent(&store, parent, agent, 0, child, 0)
            .await
            .expect_err("a parent newer than its child (a possible cycle) must be rejected");
        link_parent(&store, child, agent, 0, parent, 0)
            .await
            .expect("an older same-agent parent is the valid link");
    }

    /// `history_range` itself no longer validates - a `BranchHead` already
    /// proves it belongs to its world/agent by construction, so this tests
    /// the constructor that check now lives in.
    #[tokio::test]
    async fn branch_head_from_cursor_rejects_a_foreign_agent_or_world() {
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

        let head = HistoryHead {
            branch_id: branch,
            cut: -1,
        };
        assert!(
            store
                .branch_head_from_cursor(head, world, other_agent)
                .await
                .unwrap_err()
                .to_string()
                .contains("does not belong to agent")
        );
        assert!(
            store
                .branch_head_from_cursor(head, other_world, agent)
                .await
                .unwrap_err()
                .to_string()
                .contains("is not in world")
        );
    }
}
