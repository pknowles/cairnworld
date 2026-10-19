//! The shared raw-history backbone: allocating positions, indexing entries,
//! and traversing bounded parent branches. design.md's Chat schema and
//! "Raw history queries traverse bounded parent branches, then their own
//! entries in position order" define the contract this module implements
//! once, so context assembly (live segments) and raw inspection paging both
//! walk the same selector instead of duplicating branch logic.

use anyhow::{Context, Result, ensure};
use sqlx::{FromRow, Sqlite, SqliteConnection, Transaction};

use super::Store;

// `PositionRange` and `Segment` live in `crate::inspection` (shared with the
// browser); re-export the range since this module builds and consumes it.
pub use crate::inspection::PositionRange;
use crate::inspection::Segment;

/// One branch proven to belong to one world and agent. The Store resolves this
/// once at the boundary where a caller selects a history; all later readers
/// receive the proof rather than raw IDs they would need to validate again.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AgentBranch {
    pub(super) world_id: i64,
    pub(super) agent_id: i64,
    pub(super) branch_id: i64,
}

impl AgentBranch {
    pub fn agent_id(self) -> i64 {
        self.agent_id
    }

    pub fn world_id(self) -> i64 {
        self.world_id
    }

    pub fn branch_id(self) -> i64 {
        self.branch_id
    }
}

/// An immutable snapshot of an [`AgentBranch`] for a raw-history page. It is
/// intentionally private: browser data becomes one only after `resume`
/// validates its owner, cut and next key against the selected history.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RawCursor {
    branch: AgentBranch,
    cut: i64,
    next_branch_id: i64,
    next_position: i64,
}

impl RawCursor {
    pub(super) fn branch(self) -> AgentBranch {
        self.branch
    }

    pub(super) fn cut(self) -> i64 {
        self.cut
    }
}

/// One entry kind stored in `history_entry.kind`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EntryKind {
    Message,
    Inference,
    Summary,
}

impl EntryKind {
    fn as_str(self) -> &'static str {
        match self {
            EntryKind::Message => "message",
            EntryKind::Inference => "inference",
            EntryKind::Summary => "summary",
        }
    }

    fn parse(value: &str) -> Result<Self> {
        Ok(match value {
            "message" => EntryKind::Message,
            "inference" => EntryKind::Inference,
            "summary" => EntryKind::Summary,
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
        let position: i64 = sqlx::query_scalar!(
            "UPDATE history_branch SET next_position = next_position + 1 \
             WHERE id = ? RETURNING next_position - 1",
            branch_id,
        )
        .fetch_optional(&mut **transaction)
        .await
        .with_context(|| format!("allocating history position on branch {branch_id}"))?
        .with_context(|| format!("history branch {branch_id} does not exist"))?;
        Ok(position)
    }

    /// Insert one `history_entry` row at an already-allocated position and
    /// return its id, which the caller's typed row (message, inference, or
    /// summary activation)
    /// reuses as its own primary key.
    pub(super) async fn insert_history_entry(
        transaction: &mut Transaction<'_, Sqlite>,
        branch_id: i64,
        position: i64,
        kind: EntryKind,
    ) -> Result<i64> {
        let kind = kind.as_str();
        let result = sqlx::query!(
            "INSERT INTO history_entry (branch_id, position, kind) VALUES (?, ?, ?)",
            branch_id,
            position,
            kind,
        )
        .execute(&mut **transaction)
        .await
        .with_context(|| format!("inserting history entry on branch {branch_id}"))?;
        Ok(result.last_insert_rowid())
    }

    /// Allocate a position and insert its history_entry in one step, the
    /// common case for every producer (message append, inference start,
    /// summary activation).
    pub(super) async fn append_history_entry(
        transaction: &mut Transaction<'_, Sqlite>,
        branch_id: i64,
        kind: EntryKind,
    ) -> Result<(i64, i64)> {
        let position = Self::allocate_position(transaction, branch_id).await?;
        let id = Self::insert_history_entry(transaction, branch_id, position, kind).await?;
        Ok((id, position))
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
    async fn reach(&self, branch: AgentBranch, cut: i64) -> Result<Vec<(i64, i64)>> {
        // The seed is two Rust integers; parent keys/cuts are NOT NULL. Every
        // aggregate group therefore contains at least one non-null cut.
        let rows = sqlx::query!(
            "WITH RECURSIVE reach(branch_id, cut) AS ( \
                 SELECT ?, ? \
                 UNION \
                 SELECT p.parent_branch_id, p.through_position \
                 FROM history_parent p JOIN reach r ON p.branch_id = r.branch_id \
             ) \
             SELECT branch_id AS \"branch_id!: i64\", MAX(cut) AS \"cut!: i64\" FROM reach GROUP BY branch_id ORDER BY branch_id",
            branch.branch_id,
            cut,
        )
        .map(|row| (row.branch_id, row.cut))
        .fetch_all(&self.pool)
        .await
        .with_context(|| format!("computing reach of branch {}", branch.branch_id))?;
        Ok(rows)
    }

    /// One page of raw history entries reachable from the cursor's fixed
    /// head, ancestor-first
    /// then by position (`reach`'s order), selected and paged entirely in
    /// SQL: `after` resumes a keyset scan at `(branch_id, position)` rather
    /// than loading and re-slicing every entry on every page. Inference
    /// entries remain present so the raw history can link to every recorded
    /// model call, including tool-only calls. Returns up to `limit` entries plus, when there are
    /// more, the `(branch_id, position)` to resume after.
    ///
    /// The cursor's next position must be within its head's reach; a stale or
    /// foreign cursor is rejected rather than silently treated as end-of-history.
    pub async fn raw_page(
        &self,
        cursor: RawCursor,
        limit: i64,
    ) -> Result<(Vec<HistoryEntry>, Option<(i64, i64)>)> {
        let limit_plus_one = limit + 1;
        let reach = serde_json::to_string(&self.reach(cursor.branch, cursor.cut).await?)
            .context("serializing raw history reach")?;
        let rows = sqlx::query_as!(
            HistoryEntryRow,
            "WITH bounded AS ( \
                 SELECT json_extract(value, '$[0]') AS branch_id, \
                        json_extract(value, '$[1]') AS cut \
                 FROM json_each(?) \
             ) \
             SELECT history_entry.id, history_entry.branch_id, history_entry.position, history_entry.kind \
             FROM history_entry \
             JOIN bounded ON bounded.branch_id = history_entry.branch_id \
                 AND history_entry.position <= bounded.cut \
             WHERE (history_entry.branch_id > ? \
                      OR (history_entry.branch_id = ? AND history_entry.position >= ?)) \
             ORDER BY history_entry.branch_id, history_entry.position \
             LIMIT ?",
            reach,
            cursor.next_branch_id,
            cursor.next_branch_id,
            cursor.next_position,
            limit_plus_one,
        )
        .fetch_all(&self.pool)
        .await
        .with_context(|| format!("loading raw history page from branch {}", cursor.branch.branch_id))?;
        let mut entries: Vec<HistoryEntry> = rows
            .into_iter()
            .map(HistoryEntry::try_from)
            .collect::<Result<_>>()?;
        let next = if entries.len()
            > usize::try_from(limit).context("raw history page limit must not be negative")?
        {
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
    /// Unlike raw inspection, this never loads a covered or non-message
    /// entry: one query finds each reached branch's newest summary (if any)
    /// within its bounded cut, `covered_ranges` decoding (bounded by branch
    /// count, not entry count) finds how far it covers *this* branch, and a
    /// second query per branch selects only the uncovered message positions.
    pub async fn history_live_segments(&self, branch: AgentBranch) -> Result<Vec<Segment>> {
        let mut segments = Vec::new();
        let cut = self.current_cut(branch.branch_id).await?;
        for (branch_id, cut) in self.reach(branch, cut).await? {
            let summary: Option<(i64, String)> = sqlx::query!(
                "SELECT summary.id, summary.covered_ranges \
                 FROM history_entry \
                 JOIN summary ON summary.history_entry_id = history_entry.id \
                 WHERE history_entry.branch_id = ? AND history_entry.position = ( \
                     SELECT MAX(he.position) FROM history_entry he \
                     JOIN summary s ON s.history_entry_id = he.id \
                     WHERE he.branch_id = ? AND he.position <= ? \
                 )",
                branch_id,
                branch_id,
                cut,
            )
            .map(|row| (row.id, row.covered_ranges))
            .fetch_optional(&self.pool)
            .await
            .with_context(|| format!("loading latest summary for branch {branch_id}"))?;
            // Not covered by the selected summary, not merely positioned
            // after it: a summary activation's own position in the unified
            // index can land after messages it does not cover, since it is
            // produced by a separate compaction inference recorded later.
            let covered_through = match summary {
                Some((summary_id, covered_ranges)) => {
                    let covered_ranges: Vec<PositionRange> = serde_json::from_str(&covered_ranges)
                        .context("decoding summary covered ranges")?;
                    segments.push(Segment::Summary {
                        summary: summary_id,
                    });
                    covered_ranges
                        .iter()
                        .filter(|range| range.branch_id == branch_id)
                        .map(|range| range.last_position)
                        .max()
                }
                None => None,
            };
            let covered_through = covered_through.unwrap_or(-1);
            let range: (Option<i64>, Option<i64>) = sqlx::query!(
                "SELECT MIN(position) AS \"first_position?: i64\", MAX(position) AS \"last_position?: i64\" FROM history_entry \
                 WHERE branch_id = ? AND kind = 'message' AND position <= ? \
                 AND position > ?",
                branch_id,
                cut,
                covered_through,
            )
            .map(|row| (row.first_position, row.last_position))
            .fetch_one(&self.pool)
            .await
            .with_context(|| format!("finding uncovered message range for branch {branch_id}"))?;
            if let (Some(first_position), Some(last_position)) = range {
                segments.push(Segment::Messages {
                    messages: PositionRange {
                        branch_id,
                        first_position,
                        last_position,
                    },
                });
            }
        }
        Ok(segments)
    }

    async fn current_cut(&self, branch_id: i64) -> Result<i64> {
        let next_position: i64 = sqlx::query_scalar!(
            "SELECT next_position FROM history_branch WHERE id = ?",
            branch_id,
        )
        .fetch_optional(&self.pool)
        .await
        .with_context(|| format!("loading history branch {branch_id}"))?
        .with_context(|| format!("history branch {branch_id} does not exist"))?;
        Ok(next_position - 1)
    }

    /// Resolve an ordinary agent's one writable branch. This is not a browser
    /// boundary: both its world and branch come from durable relationships.
    pub async fn agent_branch(&self, agent_id: i64) -> Result<AgentBranch> {
        let row: Option<(i64, i64)> = sqlx::query!(
            "SELECT agent.world_id, agent_history.branch_id FROM agent \
             JOIN agent_history ON agent_history.agent_id = agent.id WHERE agent.id = ?",
            agent_id,
        )
        .map(|row| (row.world_id, row.branch_id))
        .fetch_optional(&self.pool)
        .await
        .with_context(|| format!("loading history branch for agent {agent_id}"))?;
        let (world_id, branch_id) =
            row.with_context(|| format!("agent {agent_id} has no history branch"))?;
        Ok(AgentBranch {
            world_id,
            agent_id,
            branch_id,
        })
    }

    /// Resolve a group's current GM branch. A world GM has several branches,
    /// so the group relationship is the only valid selector.
    pub async fn group_branch(&self, group_id: i64) -> Result<AgentBranch> {
        let row: Option<(i64, i64, i64)> = sqlx::query!(
            "SELECT \"group\".world_id, world_gm.agent_id, \"group\".branch_id \
             FROM \"group\" JOIN world_gm ON world_gm.world_id = \"group\".world_id \
             WHERE \"group\".id = ?",
            group_id,
        )
        .map(|row| (row.world_id, row.agent_id, row.branch_id))
        .fetch_optional(&self.pool)
        .await
        .with_context(|| format!("loading GM history for group {group_id}"))?;
        let (world_id, agent_id, branch_id) =
            row.with_context(|| format!("group {group_id} does not exist"))?;
        Ok(AgentBranch {
            world_id,
            agent_id,
            branch_id,
        })
    }

    /// Resolve an exact stored branch, including a retired group's history.
    /// Browser-selected identifiers must use `branch_for_viewer` instead.
    pub async fn history_branch(&self, branch_id: i64) -> Result<AgentBranch> {
        let (world_id, agent_id, branch_id) = sqlx::query!(
            "SELECT agent.world_id, history_branch.agent_id, history_branch.id AS branch_id \
             FROM history_branch JOIN agent ON agent.id = history_branch.agent_id \
             WHERE history_branch.id = ?",
            branch_id,
        )
        .map(|row| (row.world_id, row.agent_id, row.branch_id))
        .fetch_optional(&self.pool)
        .await
        .with_context(|| format!("loading history branch {branch_id}"))?
        .with_context(|| format!("history branch {branch_id} does not exist"))?;
        Ok(AgentBranch {
            world_id,
            agent_id,
            branch_id,
        })
    }

    /// Validate a browser-selected branch against its authorized world and
    /// requested agent before any reader receives it.
    pub async fn branch_for_viewer(
        &self,
        viewer_id: i64,
        world_id: i64,
        agent_id: i64,
        branch_id: i64,
    ) -> Result<AgentBranch> {
        self.inspecting(viewer_id, world_id).await?;
        let exists: Option<i64> = sqlx::query_scalar!(
            "SELECT history_branch.id FROM history_branch \
             JOIN agent ON agent.id = history_branch.agent_id \
             WHERE history_branch.id = ? AND history_branch.agent_id = ? AND agent.world_id = ?",
            branch_id,
            agent_id,
            world_id,
        )
        .fetch_optional(&self.pool)
        .await
        .with_context(|| format!("validating history branch {branch_id}"))?;
        ensure!(
            exists.is_some(),
            "history branch {branch_id} does not belong to agent {agent_id} in world {world_id}"
        );
        Ok(AgentBranch {
            world_id,
            agent_id,
            branch_id,
        })
    }

    pub(super) async fn first_raw_cursor(&self, branch: AgentBranch) -> Result<RawCursor> {
        let cut = self.current_cut(branch.branch_id).await?;
        let (next_branch_id, _) = self
            .reach(branch, cut)
            .await?
            .into_iter()
            .next()
            .expect("a branch always reaches itself");
        Ok(RawCursor {
            branch,
            cut,
            next_branch_id,
            next_position: 0,
        })
    }

    pub(super) async fn raw_cursor_at(
        &self,
        branch: AgentBranch,
        entry_id: i64,
    ) -> Result<RawCursor> {
        let cut = self.current_cut(branch.branch_id).await?;
        let position: i64 = sqlx::query_scalar!(
            "SELECT position FROM history_entry WHERE id = ? AND branch_id = ? AND position <= ?",
            entry_id,
            branch.branch_id,
            cut,
        )
        .fetch_optional(&self.pool)
        .await
        .with_context(|| format!("locating history entry {entry_id}"))?
        .with_context(|| {
            format!(
                "history entry {entry_id} is not visible on branch {}",
                branch.branch_id
            )
        })?;
        Ok(RawCursor {
            branch,
            cut,
            next_branch_id: branch.branch_id,
            next_position: position,
        })
    }

    pub(super) async fn resume_raw_cursor(
        &self,
        branch: AgentBranch,
        cut: i64,
        next_branch_id: i64,
        next_position: i64,
    ) -> Result<RawCursor> {
        ensure!(
            cut <= self.current_cut(branch.branch_id).await?,
            "raw history cursor cut {cut} is beyond branch {}'s current head",
            branch.branch_id
        );
        let reach = self.reach(branch, cut).await?;
        ensure!(
            reach.into_iter().any(|(reached_branch, reached_cut)| {
                reached_branch == next_branch_id && next_position <= reached_cut
            }),
            "raw history cursor position {next_position} on branch {next_branch_id} is outside this history"
        );
        Ok(RawCursor {
            branch,
            cut,
            next_branch_id,
            next_position,
        })
    }
}

/// Reuse the caller's connection so reads observe its uncommitted writes too.
pub(super) async fn branch_for_agent(
    connection: &mut SqliteConnection,
    agent_id: i64,
) -> Result<i64> {
    sqlx::query_scalar!(
        "SELECT branch_id FROM agent_history WHERE agent_id = ?",
        agent_id,
    )
    .fetch_optional(connection)
    .await
    .with_context(|| format!("loading history branch for agent {agent_id}"))?
    .with_context(|| format!("agent {agent_id} has no history branch"))
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
            Store::append_history_entry(&mut transaction, branch_id, EntryKind::Inference)
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

        let reach = store
            .reach(
                AgentBranch {
                    world_id: world,
                    agent_id: agent,
                    branch_id: child,
                },
                1,
            )
            .await
            .unwrap();
        assert_eq!(
            reach,
            vec![(root, 1), (child, 1)],
            "the parent contributes only its bounded prefix before the child"
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

        assert_eq!(
            store
                .reach(
                    AgentBranch {
                        world_id: world,
                        agent_id: agent,
                        branch_id: leaf
                    },
                    0
                )
                .await
                .unwrap(),
            vec![(root, 1), (mid, 0), (leaf, 0)],
            "shared parents appear once at their longest reached cut"
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

    /// Ownership is carried by AgentBranch; an ordinary agent's constructor
    /// obtains both its world and writable branch from durable relationships.
    #[tokio::test]
    async fn agent_branch_resolves_an_agents_owned_history() {
        let db = TestDatabase::new("history-test");
        let store = Store::open(db.path()).await.unwrap();
        let world = store.create_world("test").await.unwrap();
        let agent = store.create_agent(world).await.unwrap();
        let branch = store.agent_branch(agent).await.unwrap();
        assert_eq!(branch.world_id(), world);
        assert_eq!(
            store.history_branch(branch.branch_id()).await.unwrap(),
            branch
        );
        assert!(store.history_branch(i64::MAX).await.is_err());
    }

    #[tokio::test]
    async fn branch_lookup_observes_the_callers_uncommitted_history() {
        let db = TestDatabase::new("history-test");
        let store = Store::open(db.path()).await.unwrap();
        let world = store.create_world("test").await.unwrap();
        let mut transaction = store.pool.begin().await.unwrap();
        let agent = sqlx::query("INSERT INTO agent (world_id) VALUES (?)")
            .bind(world)
            .execute(&mut *transaction)
            .await
            .unwrap()
            .last_insert_rowid();
        let branch = Store::create_agent_history(&mut transaction, agent)
            .await
            .unwrap();
        assert_eq!(
            branch_for_agent(&mut transaction, agent).await.unwrap(),
            branch
        );
        transaction.rollback().await.unwrap();
        {
            let mut connection = store.pool.acquire().await.unwrap();
            let error = branch_for_agent(&mut connection, agent).await.unwrap_err();
            assert!(format!("{error:#}").contains(&format!("agent {agent} has no history branch")));
        }
        let committed_agent = store.create_agent(world).await.unwrap();
        let committed_branch = store.agent_branch(committed_agent).await.unwrap();
        let mut connection = store.pool.acquire().await.unwrap();
        assert_eq!(
            branch_for_agent(&mut connection, committed_agent)
                .await
                .unwrap(),
            committed_branch.branch_id(),
        );
    }
}
