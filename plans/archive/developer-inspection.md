# Developer Mode implementation

**Completed and verified 2026-09-29.** All A–F outcomes are implemented. The
production-path browser suite covers selection, paging, relationship navigation,
Close/reopen, and narrow layouts; the configured CUDA-model check covers exact
recorded evidence through normal play. This archived plan is not a reference for
new work.

## Deliverable and traceability

A player can inspect the exact turn/tool tree, model input/output, linear agent
history, GM group-history DAG and current objects while chat remains usable.
The implementation follows the data/protocol choices in
[design.md](../../design.md#chat-schema).

| Requirement | Declaration | Design | Steps |
| --- | --- | --- | --- |
| Owner toggle and continuing member access | [World detail](../../user_declarations.md#world-detail-page) | Membership and presence; Live inspection protocol | A, F |
| Every original message, summary and failed inference, exact reconstructed input | [Debugging and Telemetry](../../user_declarations.md#debugging-and-telemetry), [Chat compaction](../../user_declarations.md#chat-compaction) | Chat schema; Inference records | B, C, E |
| One GM context per interacting group, split/merge histories | [GM interaction](../../user_declarations.md#gm-interaction), [Game state](../../user_declarations.md#game-state) | Groups and shared GM history | B, D |
| Calls/results in their actual tree and order | [Agent syntax and tool calls](../../user_declarations.md#agent-syntax-and-tool-calls), [Developer Mode](../../user_declarations.md#developer-mode) | Durable execution relationships | C, F |
| Live inspectable raw data, usage, navigation | [Developer Mode](../../user_declarations.md#developer-mode) | Live inspection protocol; Browser selection and raw fields | A, E, F |

The bounded-history corrections, D, E (E1 and E2), and all F navigation and
completion slices are implemented. The user explicitly prioritized completing
Developer Mode on 2026-09-27; the broad Code quality refactors follow it.

A–F are ordered complete implementation slices. Each includes its own working
queries/UI integration and verification. Schema changes and their producers and
readers land together; there is no intermediate recorder or second transcript.
Use new isolated development databases for the schema transition under the
existing pre-user migration policy; do not delete the user's databases.

## A. Live access and selection consistency

1. In `store.rs`, share one indexed viewer/world access query between inspection
   HTTP handlers and the websocket. Have set_developer_mode and remove_member
   publish AccessChanged on the existing broadcast after commit. This event is
   only a wake-up; the query supplies current membership and mode.
2. Change `web.rs::dev_events_connection` to retain Store/viewer/world identity.
   Subscribe before querying initial access and recheck before each private
   outbound frame. Close removed members; for disabled mode send mode=false and
   suppress private data. Use ordinary socket.send with no access lock or open
   transaction. An already-authorized frame can finish during revocation; every
   subsequent frame requires another check. Query/send errors terminate visibly.
3. Add Changed, Token offset and ResyncRequired to the shared event enum. Emit
   Changed after the existing start/finish and action/result commits; token
   offset counts actual UTF-8 bytes emitted. On Lagged recheck access, send current mode and,
   when authorized, ResyncRequired instead of continuing silently. Errors retain context and terminate the bad stream.
4. In `ui.rs`, own the websocket and callbacks with island cleanup. Add visible
   error/Reconnect controls. Subscribe before fetching; track selection request
   generation and matching invalidations during the fetch. Discard stale request
   responses; upsert durable IDs. Completion reloads a selected standalone
   inference as well as a selected sequence. Disable clears all private pane data.
5. Extend the existing real Axum browser harness to connect websocket upgrades
   to the actual server. Remove the JavaScript replacement inspection socket
   from check_developer_mode.cjs. A controlled Backend may pause generation; the
   production Store, Game, routes and broadcast must remain in the exercised path.

Verify a live ban, disable during a full token queue, re-enable, failed socket,
reconnect, completion during snapshot load and rapid selection changes. A slow
viewer must not hold up generation event publication or a ban. Exercise the
authorization boundary with a paused send: allow the previously authorized frame,
then require no private frame whose check occurs after revocation. Measure access
query contention during real token streaming with multiple viewers. Final
displayed records equal a fresh
HTTP query; a missing partial token prefix is visibly marked. Chat/draft survive.

## B. Original content and one ordered history index

1. Replace recorded_content's fragment references with the full content/reasoning
   body from design.md, keeping ordinary integer IDs and dropping content-value
   uniqueness. Keep text only for immutable prompt/schema/formatter artifacts.
   Use plain recorded_content IDs: reasoning is read alongside its own content,
   and GM narration references its settled text reply. No producer requires a
   typed selector into a content row.
2. Add history_branch, history_parent, agent_history and history_entry with the
   columns/constraints in design.md. Make message/notice IDs refer to their
   history_entry; give inference starts and summary activations their own entries.
   Allocate position with the branch update in the same insertion transaction.
   Initialize one branch for each existing linear agent at construction.
3. Replace agent/seq history lookup with explicit HistoryHead(branch_id,cut).
   Implement bounded parent-first range traversal in store/history.rs; reject
   foreign agents/worlds, missing parents or cycles. Subtract visited branch
   intervals and select summaries before fetching content. Load only selected
   summaries/raw ranges for inference; raw inspection pages use the same ranges.
4. Update context.rs, agent.rs and compaction.rs to assemble from that selector.
   Inference recipes store ordered immutable branch ranges/summary/content refs.
   Summary.covered_ranges records message coverage; activation remains a separate
   history entry. Pending compaction is keyed by branch and stores its exact cut.
   Replace separate finish/append writes with finish_chat_inference and
   finish_summary_inference transactions from the design; detached replay retains
   its outcome-only operation. Each reuses the same private content writer.
5. Change agent_raw_history to page the history index, returning all event kinds
   plus the typed cursor and selected head. Update player-chat reload and the
   existing linear history UI to consume it. Inference completion updates its
   existing row; failures before the first message or after the last are visible.
6. Change Game::Narration to carry the GM's own settled-reply content id, not
   copied text. Delivery inserts message references for every present
   recipient rather than re-recording the narration text per recipient.
   Assistant replies and summaries must reuse their original output
   references via finish_chat_inference/finish_summary_inference (step 4).
   Action results have no duplicate-writer to remove at this slice: every
   current record_action/resolve_pending_action caller synthesizes its result
   text once, directly, from Rust game logic - reusing tool_execution/action
   references is slice C's job, once tool_execution exists.

Verify repeated equal text stays distinct as events, while one narration/output
shared across recipients has one original payload. Drive unrelated agents so
message/inference counts diverge; paginate failures, summaries and notices across
all boundaries. Append new messages, compact twice, reopen the database and
compare earlier inputs/outputs against captured originals. Corruption must name
the failing reference. This slice retains working linear play and inspection.

## C. Complete execution ancestry

1. Add invocation and tool_execution and sequence lifecycle columns from
   design.md. Replace parent_inference_id as the source of nesting with the
   actual parent_tool_execution_id. Preserve cause_inference_id for compaction.
   Give each inference its invocation ordinal and actual branch ID.
2. Wrap agent::complete_with_call_context entry/exit to create/finish invocation.
   Extend CallContext with invocation/parent-tool identity; resolve and record
   each invocation's starting branch_id explicitly (design.md's `invocation`
   schema has no acting-character field - only branch_id, which is still a
   plain agent_history lookup while GM heads remain per-location; groups in
   slice D are what make resolving it require more than that). Keep recursion
   and both inference budgets.
3. Create executions for the entire returned tool batch, using output ordinal as
   the key and resolving model call ID/name/arguments from the original output.
   tools::execute completes/rejects/fails the matching row. Nested game calls
   receive that execution ID. Unexecuted siblings retain an explicit status.
4. Convert action records to reference requesting/approving executions and
   result content; pending
   actions retain their world-local action ID and source call. Approval atomically
   writes state/action/result and consumes the pending action. Repeated approval
   reports the recorded outcome without executing the mutation again.
5. Finish sequence on foreground invocation return; show deferred compaction as
   a caused invocation. Recovery marks open execution records interrupted and
   resumes only durable obligations. Update sequence_tree to return ordered
   invocation/inference/tool nodes and player entry links to exact source nodes.

Verify sibling nested calls, two identical tool calls with different ordinals,
malformed/unknown tools, failure before a model output and failure after a
committed action. Check no retry duplicates state or result payload. Count usage
once per inference; distinguish foreground wall time from summed inference time.
The brief future-agent failure-context proposal is separate from raw recording
and requires the user's Product decision before changing model messages.

## Bounded history ranges

**Requirement:** Original, inspectable histories and isolated GM branches
([Developer Mode](../../user_declarations.md#developer-mode),
[GM interaction](../../user_declarations.md#gm-interaction)); player latency
([Scheduling](../../user_declarations.md#scheduling)); design's Chat schema ("Raw
history queries traverse bounded parent branches, then their own entries in
position order") and Context assembly.

The other corrections to A–C landed (browser harness `778f865`, output identity
`8decc4d`, transactional action resolution `8ddff77`, batch finalization
`a6be8ce`), and parent-link validity is a schema property (`8f32f75`). This is
the last one, and it is on every inference's path, not only Developer Mode's.

**2026-09-25 status.** Steps 1 (`5809f24`), 3 (`7ba72c0`), 4 (`1a87f14`) and 5
(`15afd86`) are done and measured (`fbe2108`: a 64-path merge fan-in resolves
live context in about 9.5 ms; live context stays at about 0.4 ms from 100 to
10,000 compacted entries; query plans use indexes). Step 2 (`8d00d01`) is only
partly done, and review found further defects; see
[Remaining corrections](#remaining-corrections). This section is not complete
until they land.

**2026-09-25 implementation update.** The remaining ownership corrections are
implemented: `AgentBranch` is constructed only from an ordinary agent, a
group, or an authorized browser selection; turns and context assembly carry it
instead of optional branch IDs. `RawCursor` is validated before every raw page,
and `reach()` is the sole recursive query. `history_range` is removed and its
tests assert reach bounds directly. The full Rust suite and hydration/browser
suite pass; the prior 64-path and 10,000-entry measurements remain applicable
because the reach algorithm and live-context query shape are unchanged.

**Problems** (as of `8f32f75`, before this section's own work):

- `store/history.rs::range` recurses once per parent *path*. A branch reachable
  by k paths is read k times; each split-then-merge round doubles the paths to
  everything above it, so the root is read 2^rounds times. Deduplication is a
  linear scan per entry (O(n²)). **Fixed** (step 1).
- `Store::live_segments` loads every entry row of every reachable branch, of
  every kind and including everything already covered by summaries, on every
  inference. It only needs, per branch, the newest summary and the first/last
  uncovered message position. **Fixed** (step 3).
- `agent_raw_history` re-walks the whole history for each page, issues one
  inference-outcome query per inference entry, slices in memory, and treats a
  cursor it cannot find as end-of-history (`unwrap_or(len)`). **Fixed** (step
  4): `raw_page` selects and pages entirely in SQL via a keyset scan, and an
  unknown cursor position is now a rejected error, not silent
  end-of-history - `store.rs::agent_raw_history_rejects_a_cursor_position_outside_the_reach`
  is the regression test for that.
- `request_for_segments` runs `branch_owner` once per message segment and one
  query per summary; `branch_owner` resolves through `agent_history`/`group`
  with a fallback rather than `history_branch.agent_id`. **Fixed** (steps 2
  and 5): the fallback is gone, and `check_message_range_ownership` validates
  every `Segment::Messages` branch a request references in one batched query
  (`history_branch WHERE id IN (...)`) before assembly starts, rather than
  once per range as the loop reaches it -
  `request_for_segments_rejects_a_foreign_branch_among_several_message_ranges`
  proves a foreign branch is still caught even when mixed with the agent's
  own. `Segment::Summary`'s ownership check (`summary.agent_id == agent_id`,
  already a single cheap row read against the already-loaded summary) was
  left as-is.
- `live_segments(agent_id, Option<branch_id>)` and `agent_raw_history(...,
  Option<branch_id>, Option<cursor>)` use `Option` to select between two head
  resolutions, and every entry point re-validates ownership. `HistoryHead` is
  a plain `{branch_id, cut}` any caller can construct. **Partly fixed**:
  `BranchHead` (step 2) means validation happens once, at construction, not
  on every read - `history_range`/`history_live_segments` no longer
  re-validate. The `Option<branch_id>` parameters themselves stay on
  `live_segments`/`agent_raw_history` (real callers thread a genuine
  agent-vs-explicit-group-branch choice through `agent.rs`/`context.rs`'s
  `Invocation`, not just this module) - see
  [code-quality.md](../code-quality.md#design-refactors)'s broader `Option`
  triage for that.

A 2026-09-17 probe measured a two-row raw page at about 5.5 ms for 100
inference records and 52 ms for 1,000 (observations, not thresholds, from
before steps 1-5 below).

**2026-09-25 verification results** (temporary in-process benchmarks, not
committed - measurements, not portable thresholds): `live_segments` with a
summary covering everything except a small raw tail stayed at ~0.4-0.5ms as
the compacted-away history grew from 100 to 10,000 entries - the actual point
of step 3, confirmed to hold, not merely argued. A 64-way diamond fan-in (6
rounds of every branch splitting into 2, converging back on one 3-message
root - the exact k=2^rounds re-read pathology step 1's "Problems" described)
resolved `live_segments` in ~9.5ms; the old per-path recursion would have
re-read that 3-message root 64 separate times. Reconstructed requests already
match before/after for every existing recorded-inference test
(`reconstructs_history_recipe_without_copying_messages` and others), and a
real configured-model chat with compaction
(`real_model_compacts_a_long_chat_and_keeps_an_early_fact`) passed against the
final code twice.

`EXPLAIN QUERY PLAN` on the `reach`, `raw_page` and `history_live_segments`
queries against a populated test database (temporary, not committed) showed
every table access as an indexed `SEARCH` (`history_parent`'s
`(branch_id, ordinal)` key for `reach`'s recursive step, `history_entry`'s
`(branch_id, position)` unique index for `raw_page`'s keyset scan and the
inference-outcome exclusion join, and a `COVERING INDEX` for
`history_live_segments`' summary lookup) - no full table scan anywhere; the
`TEMP B-TREE` steps for `GROUP BY`/`ORDER BY` operate on the small
reach/bounded result set (branch count), not on `history_entry` itself.

A real split-then-merge exercised together with a real configured-model chat
turn: `real_model_assembles_context_after_a_split_and_merge`
(`src/game.rs`) reuses the existing merge test's real split_group/merge_groups
tool-call setup, then attaches an actual `MistralRsBackend` for one more
turn against the resulting merged branch DAG. It passed: one real inference,
no tool call, a genuine text reply, proving `live_segments`/
`request_for_segments` assemble a chat-template-valid context from a real
multi-branch merge for a real model to consume - not only for the scripted
test backend the structural merge test itself uses. (An earlier attempt with
an open "what do we do?" prompt hit `limits.max_inferences_per_chat` from the
small model repeatedly calling `look` without settling on a reply - every one
of those inferences completed successfully, so this was a prompt-design
issue, not a context-assembly failure; a closed, descriptive prompt matching
the other real-model tests' style settled in one turn.)

All items from this section's own verification bar are now satisfied.

**Design.** Make the reachable history a value computed once by one query, and
make its validity a property of its type.

1. *Reach as one recursive query.* `history_parent`'s keys guarantee a parent
   shares the agent and has a smaller id. The reachable prefixes are:

   ```sql
   WITH RECURSIVE reach(branch_id, cut) AS (
       SELECT ?1, ?2
       UNION
       SELECT p.parent_branch_id, p.through_position
       FROM history_parent p JOIN reach r ON p.branch_id = r.branch_id
   )
   SELECT branch_id, MAX(cut) FROM reach GROUP BY branch_id ORDER BY branch_id
   ```

   Several paths to one branch collapse to its longest prefix (the union of
   prefixes), so there is nothing to deduplicate. Cost scales with branch
   count, not entry count. Branch-id order is a valid ancestor-first order
   because of the `CHECK`. Every reader joins this reach on
   `position <= cut` and reads only the rows it returns.
2. *Validated head type.* Replace public use of `HistoryHead` with a
   `BranchHead { world_id, agent_id, branch_id, cut }` whose fields are private
   and whose only constructors are Store resolutions that validate ownership
   once from `history_branch.agent_id JOIN agent`: `for_agent` (an agent's
   single writable branch), `for_group` (a group's GM branch) and `from_cursor`
   (a browser-supplied head, validated against the viewer's world). This
   removes the `Option<branch_id>` parameters, `branch_owner`'s fallback lookup
   and per-reader ownership checks.
3. *Live context without entry rows.* From the reach, one query selects each
   branch's newest summary activation at or before its cut, then one query
   selects each branch's first/last message position after that summary's
   coverage. Segment shape is unchanged, so recorded recipes and
   reconstruction are unchanged.
4. *Raw pages by keyset.* Page over `(branch_id, position)` within the reach
   from the cursor, excluding successful inference entries in SQL (join
   `inference.error`), `LIMIT limit + 1` to produce the next cursor, and load
   payloads for the returned rows only. A cursor outside the reach is an error.
5. `request_for_segments` checks ownership once per request (one query over all
   message ranges), not once per segment. It stops checking entirely when
   [code quality's relational segments](../code-quality.md#design-refactors) make
   segment references foreign keys.

**Alternatives.**

| Option | Pros | Cons / risks |
| --- | --- | --- |
| Recursive CTE reach (recommended) | One query; no duplicate reads by construction; shared by live and raw paths; uses existing immutable edges; gives design.md's git-like creation order directly | None found |
| Rust walk over edge metadata only, then one entry query | Ordering logic stays in Rust | Still one query per branch; hand-written graph walk to maintain |
| Persisted closure table `branch_ancestor(branch, ancestor, cut)` written by `spawn_group_branch` | Trivial reads | A second, derived copy of the topology; more rows written per split/merge |
| Interval subtraction over the current recursion (previous proposal) | Minimal change | Keeps the per-path recursion; adds interval code instead of removing the cause |

**Verification.** Keep the existing traversal tests (bounded cuts, shared
ancestors) and add: several split/merge rounds (each branch read once; checked
with `EXPLAIN QUERY PLAN` and row counts), pagination across mixed messages,
failures, summaries and notices with appends between pages and a fixed head,
an unknown cursor rejected, and foreign agent/world heads rejected at
construction. Measure live-context assembly and a fixed-size page at 100,
1,000 and 10,000 entries before and after: the page cost must not grow with
history, and live context must not grow with compacted-away history.
Reconstructed requests must match before/after for recorded inferences. Run a
real configured-model chat with compaction and a split/merge, and the browser
suite.

### Remaining corrections

Review of `1c0cf1d`, 2026-09-25. Intent is unchanged from step 2: validity and
ownership are established once, by construction, and every reader takes the
proof instead of re-checking or choosing behavior from an `Option`.

**Problems.**

- The `Option` parameters step 2 was to remove remain:
  `live_segments(agent_id, Option<branch_id>)`,
  `agent_raw_history(..., Option<branch_id>, Option<cursor>)`,
  `agent::Turn { branch_id: Option<i64> }`, and
  `raw_page(after: Option<(i64, i64)>)` with an `i64::MIN` sentinel for "no
  cursor".
- The planned `for_group` constructor became `branch_head_for_branch(branch_id,
  world_id, agent_id)`, which accepts any caller-supplied branch.
- On the live path, validation is circular: `live_segments` derives `world_id`
  from `agent_id`, then checks the agent belongs to that same world.
- `BranchHead.world_id`/`agent_id` are never read. A different
  `inspection::BranchHead { group_id, group_name }` shares the name.
- The recursive reach query is written out three times in `store/history.rs`
  (`reach`, and twice in `raw_page`). This also blocks
  [checked SQL](../code-quality.md#compile-time-checked-sql), whose macros need
  one literal per query.
- `history_range` survives as `#[cfg(test)]` production-shaped code that no
  production path runs, contrary to the testing standards.
- `split_then_merge_groups` returns a seven-element tuple.
- New code is not rustfmt-formatted (8 diffs in `store/history.rs`).

**A branch's identity versus its head.** A turn appends entries while it runs,
so a type holding a cut cannot be what a turn carries. The fix separates the
identity (`world_id, agent_id, branch_id`) from a head (identity plus cut) read
from it when needed.

| Option | Pros | Cons / risks |
| --- | --- | --- |
| Keep `BranchHead` and add `for_group`; leave `Option` callers | Smallest change | `Turn` and the wrappers keep `Option`; the circular check stays |
| `AgentBranch { world_id, agent_id, branch_id }`, private fields, constructed only by `for_agent(agent_id)` (world and branch derived, nothing to check), `for_group(group_id)` (agent and branch from the group row) and `for_viewer(viewer, world, agent, branch)` for browser input; `head()` reads the current cut (recommended) | One type carried by `Turn`, context assembly, compaction and inspection; fields are used; checks exist only where input is external | Touches every `Turn`/`live_segments` call site |
| Enum selector `History::Agent(id) / Group(id)` resolved inside each reader | No new validated type | Every reader re-resolves and re-validates; the selector is data, not proof |

**One reach definition.**

| Option | Pros | Cons / risks |
| --- | --- | --- |
| `reach()` runs the recursive query once; callers pass its result to later queries as a JSON parameter read with `json_each`, and check the cursor in Rust against it (recommended) | One definition; every query stays a literal the checked-SQL macros accept | Two round trips per page instead of one |
| Shared SQL string constant concatenated into each query | One round trip | Runtime-built SQL cannot use checked macros |
| Recursive view over all branches | One definition in schema | SQLite cannot push `head = ?` into a recursive CTE, so every query computes every branch's reach |
| Persisted closure table written by `spawn_group_branch` | Plain joins | A second, derived copy of the topology to keep consistent |

**Raw page cursor.**

| Option | Pros | Cons / risks |
| --- | --- | --- |
| Validated `RawCursor { branch: AgentBranch, cut, next: (branch_id, position) }` built by `first(branch)` or `resume(viewer input)`, which checks `next` lies within the reach; `raw_page(cursor, limit)` has one path (recommended) | No `Option`, no sentinel; invalid cursors cannot reach the query | Adds a small type |
| Keep `Option` with the sentinel | No change | Two meanings in one parameter; a sentinel in keyset SQL |

**Steps.**

1. Add `AgentBranch` with its three constructors in `store/history.rs`. Replace
   `BranchHead` with `AgentBranch::head()`, and rename
   `inspection::BranchHead` to `BranchGroup`.
2. Replace `Turn.branch_id`, the `live_segments` wrapper and
   `agent_raw_history`'s `Option` parameters with `AgentBranch`, and delete
   `branch_head_for_branch`. Callers that have a group use `for_group`.
3. Make `reach()` the only recursive query. `raw_page` takes a `RawCursor` and
   joins the reach through `json_each`. Delete the other two copies.
4. Delete `#[cfg(test)] history_range`. Its tests assert `reach()`'s ranges
   directly (the bounding/dedup property they were written for).
5. Replace the seven-element tuple with a named fixture struct. Format the
   touched files.

**Verification.** Existing traversal, paging and merge tests pass unchanged in
intent. Add: `for_viewer` rejects another world's branch and another agent's
branch; `resume` rejects a cursor outside the reach; a second GM turn on a
split branch reads the group's branch (not the agent's). Re-run the 64-path and
10,000-entry measurements to confirm no regression, then the browser suite.
Complete the [AGENTS.md checklist](../../AGENTS.md#post-feature-and-pre-commit-checklist).

## D. Groups and real GM branches

D1–D6 are one coupled slice. Run the embedded presence plan's split/merge cases
after D4; archive presence only with the passing D completion review.

1. Add world_gm, group and character_group; remove location_gm and
   character_location. Create the world GM, groups and character memberships in
   scenario/character-construction transactions. Update scenario import/export
   to use groups and Connections (rename path and its types/JSON fields).
   Bread Thief declares its actual starting group rather than deriving groups
   from identical location IDs. Retain all archived branch/group identities.
   Reordering note (2026-09-18): a world_gm agent has one branch per group,
   never an `agent_history` row (the migration's own comment: "GM heads move
   to groups once groups exist"). `branch_for_agent`/`start_invocation`/
   `agent::Turn` only resolve an agent's single `agent_history` branch, so D1
   cannot compile a working world_gm without also accepting an explicit
   branch_id through invocation start and context assembly - design.md's
   `GroupHistory { agent_id, branch_id }`. That plumbing is pulled forward
   into D1 rather than left to D1.3; D1.3 becomes the query layer (resolving
   the acting character's actual group and its HistoryHead) built on top of
   branch selection D1 already made explicit, not the first place agent_id
   stops being sufficient.
2. Implement the [presence plan](../multiplayer-presence.md) now, using the group
   rows from D1 and the live connection/access work from A. This delivers the
   online recipient facts needed for group narration; it is a dependency inside
   this feature, not deferred to after inspection is called complete.
3. Change gm_scene and player action routing to resolve the acting character's
   group and explicit GM HistoryHead. Include this group's characters, location
   objects/notes and other local group descriptions. Replace location narration
   subscription keys with group IDs; recipient selection follows the approved
   narration policy. Group changes update an existing socket's subscription.
   Status (2026-09-21): gm_scene/action routing and the group-keyed
   subscription/broadcast rename (`Game::subscribe`, `GroupBroadcasts`,
   `Narration.group_id`, `deliver_narration`/`narrate_presence_transition`)
   are done and verified, including a regression test proving two groups
   sharing one location no longer cross-leak live narration. "Group changes
   update an existing socket's subscription" is not yet implemented -
   nothing can actually change a character's group before D4's split_group/
   merge_groups exist, so `play_connection` still subscribes once at
   connection time with no resubscription path. This needs revisiting once
   D4 lands, since only then does a real trigger exist to build and verify
   against.
4. Implement split_group and merge_groups arguments/validation from design.md.
   Add the typed grouping ToolOutcome; settle ordinary tool results, append the
   whole batch in returned order, and apply the topology transaction at the
   complete-exchange boundary. Next inference resolves the acting character's
   new group; source-less GM events supply continue_with_character_id on split.
5. `enqueue_capacity_compaction`, `latest_summary` and `tail_split` still
   resolve a plain `branch_for_agent(agent_id)` and would panic given a
   world_gm agent_id, since it has no single `agent_history` row (D1's branch
   plumbing note above). Give these the same explicit-branch treatment before
   or alongside adding group-aware compaction here, so a GM branch nearing
   capacity does not panic instead of compacting.
   Status (2026-09-23): this gap was more severe than the note above implied
   - it was reachable in real play, not just a future risk. A world_gm's
   deferred compaction jobs were invisible to `pending_compaction`/
   `pending_compactions` (both JOINed through `agent_history`, which has zero
   rows for world_gm), so the scheduler's admission gate never drained them
   and `resume()` never recovered them at startup. A second over-threshold
   reply on the same branch then hit `finish_chat_inference`'s INSERT into
   `pending_compaction`, which (unlike its sibling
   `enqueue_capacity_compaction`) was missing `ON CONFLICT(branch_id) DO
   NOTHING`, producing a UNIQUE constraint violation. Separately,
   `compaction::run` passed `None` for branch_id into `start_invocation`,
   which fell back to `branch_for_agent` and failed outright with "agent {id}
   has no history branch" for any world_gm job that did get enqueued. Fixed
   by threading `branch_id: Option<i64>` through the `Backend` trait's
   `before_agent`/`after_agent` (previously silently dropped in
   `agent::complete_with_call_context`), rekeying `InferenceScheduler`'s
   admission state from agent_id to branch_id, adding
   `pending_compaction_for_branch`/`latest_summary_for_branch`/
   `tail_split_for_branch`, adding the missing `ON CONFLICT`, and passing
   `job.branch_id` explicitly into `start_invocation`. `latest_summary` stays
   `#[cfg(test)]`-only; `tail_split` (agent_id form) was removed, having no
   remaining callers. Verified with a new store.rs test
   (`a_world_gm_groups_branch_compacts_without_an_agent_history_row`) and
   individual revert-and-reproduce against the exact production error
   strings for all three defects.
   Reuse the corrected bounded parent traversal and add the specified
   longest-disjoint-summary selector.
   Status (2026-09-23): the bounded parent traversal (`Store::range`/
   `history_range` in `src/store/history.rs`) already exists, already dedups
   entries a shared ancestor would otherwise emit twice, and needed no further
   correction - it was built and verified for D4's split/merge DAGs.
   The actual gap, found while scoping this item: `live_segments`
   (`src/store.rs`) still assumed a single branch with no parents - its own
   comment said so ("this slice has no branch with parents yet"). It walked
   the full traversal but kept only the single newest `Summary` entry across
   ALL branches (`.rev().find(...)`) and computed `covered_through` using
   only `head.branch_id`'s own covered ranges. After a merge, each parent
   branch can have its own independent summary covering a disjoint range;
   the old code silently ignored every summary but the newest one and fed
   the model raw, uncompacted messages from the other parent branch(es)
   instead - not a correctness bug (no content was lost, `history_range`
   still returns every entry), but a token-budget one: exactly the kind of
   "compact before capacity" contract this feature exists to keep.
   Fixed: `live_segments` now groups the traversal's entries by branch
   (already emitted as contiguous per-branch runs - ancestors before the
   branch that owns them) and selects, per branch, that branch's own newest
   summary (branch-local summaries are already monotonic, so cross-branch
   overlap cannot occur - a `PositionRange` is scoped to one branch_id).  It
   pushes one `Segment::Summary` + at most one `Segment::Messages` per
   branch touched by the traversal, not just `head.branch_id`, in traversal
   order. Verified with a new store.rs test
   (`live_segments_after_a_merge_carries_each_parent_branchs_own_summary`)
   building two independently-summarized group branches and merging them,
   confirming both summaries survive and no raw message leaks in
   uncovered; verified by reverting the fix and confirming the test fails
   with exactly that symptom (1 summary instead of 2, one branch's raw
   message present).
   `tail_split_for_branch`'s single-branch assumption
   (`latest_summary_for_branch(branch_id)` only ever looks at its own
   branch's covered ranges) needed no change for the merge-compaction step
   below: each source branch's own newest summary and own tail cut is
   exactly what that step operates on, called once per eligible source
   branch - "unique suffix" falls out for free, since a shared ancestor's
   messages live on the ancestor's own branch, never copied onto a split
   child.
   Add limits.merge_branch_messages=64 with positive validation. Before committing
   a merge, queue summaries for eligible unique source suffixes through their
   last settled exchanges, excluding the grouping batch. Await these jobs before
   capturing parent cuts so the merged history includes their activations. Use existing recorded compaction execution and
   admission; group changes wait for affected pending compactions.
   Status (2026-09-23): implemented. `agent::compact_merge_sources` runs
   before `Store::apply_topology_changes`'s own transaction (design.md: "no
   database transaction spans inference execution"), so `apply_merge`'s
   existing fresh `next_position - 1` read naturally picks up the new
   summary/notice as part of the branch's settled position - no change
   needed in `apply_merge` itself. For each `TopologyChange::Merge`
   proposal's `group_ids`: `Store::active_group_branch` gates existence/
   retirement (skips soft, leaving the real rejection to `apply_merge`'s own
   validation instead of erroring on a doomed proposal); `Store::
   uncovered_message_count` (new) checks that branch's own unique suffix
   against `merge_branch_messages`, and when it exceeds the threshold
   enqueues a durable job via the existing `enqueue_capacity_compaction`
   (branch-explicit, `ON CONFLICT(branch_id) DO NOTHING` - reuses an
   already-pending job rather than double-enqueueing). Either way it then
   calls `backend.before_agent(store, agent_id, Some(branch_id))` - not
   `compaction::run` directly - so a branch that already has an unrelated
   pending job (e.g. an ordinary token-threshold trigger, independent of raw
   message count, so reachable even under the merge threshold) is waited on
   through the same admission path a concurrently running deferred
   compaction uses, rather than resolved a second time and racing it; this
   is also literally "group changes wait for affected pending compactions".
   Compaction itself reuses `limits.keep_tail_messages` for the retained
   tail exactly like ordinary compaction. "Excluding the grouping batch" is
   automatic: this step runs before the batch's own tool-result message is
   written to any branch. Verified with a new agent.rs test
   (`compact_merge_sources_compacts_only_branches_over_the_threshold`,
   backed by a real `InferenceScheduler`/`ScheduledBackend` so
   `before_agent` actually resolves the job, not the default no-op) that
   splits a real group, seeds one side over threshold and leaves the other
   empty, then confirms only the over-threshold side gets a summary;
   verified by reverting the eligibility gate and separately the
   `before_agent` wait to confirm the test catches both a never-compact and
   an always-compact regression.
6. Return branches, ordered parent cuts and current group heads from the GM
   history endpoint. Render them as linked branch sections in the same history
   component, with shared ancestors shown once and expandable summary markers.
   Player/NPC histories remain the one-branch form. Connect branch/head/object
   links immediately; do not leave a fake linear GM view after changing storage.
   Plan (2026-09-23): `AgentHistoryView`'s own comment already names this gap
   ("Currently linear per agent, including the location GM's -
   user_declarations.md calls for the GM's history to show a DAG with one
   head per Group"). `history_range`'s traversal and the cursor's
   `HistoryHead{branch_id, cut}` are already branch-aware (D1); what's
   missing is (a) a way to ask for a *specific* GM branch's page instead of
   always falling back to `current_head(agent_id)`, which errors for a
   world_gm (no single `agent_history` row), and (b) a small, unpaged query
   describing the whole branch DAG so the browser can render every branch as
   its own section and link them, instead of only ever showing whichever one
   branch happened to resolve.
   - `Store::agent_raw_history` gains an explicit `branch_id: Option<i64>`,
     used only to resolve the *first* page's head (`None` keeps today's
     `current_head(agent_id)` behavior for player/NPC/Storyteller); every
     later page already resumes from the cursor's own branch-explicit head,
     unchanged.
   - New `Store::gm_branch_graph(viewer_id, world_id, agent_id) ->
     Vec<BranchNode>`: every `history_branch` this agent owns, each with its
     ordered `history_parent` edges (`parent_branch_id, through_position`)
     and, when a non-retired `"group"` row's `branch_id` points at it, that
     group's id/name as its "current head" label. Branch counts are small
     (one per group ever split/merged into existence) - this is one plain
     query, not paged like the entry list.
   - `web.rs`: `inspect_agent_history` accepts an optional `branch` query
     param, used only when `after` is absent. New route returning
     `gm_branch_graph`, gated the same `inspecting()` way as every other
     inspection query.
   - `migrations/0001_recording.sql`'s `history_parent` comment ("Empty in
     this slice: no branch has parents until split/merge (groups) exist") is
     stale since D4; update it while touching this area.
   Status (2026-09-23): implemented, differing from the plan above in one
   respect - `InspectSelection::AgentHistory(agent_id)` did not need a
   `branch_id` field. Selecting an agent fetches its whole `/branches` graph
   first, then fetches every returned branch's own first `/history?branch=`
   page in parallel and combines them once all have arrived into one
   `InspectData::AgentHistory { agent_id, branches: Vec<BranchSection> }` -
   the plain single-branch case (`branches.len() == 1`, the overwhelming
   majority) costs one extra small unpaged `/branches` request over the old
   direct fetch, not an extra round trip, and renders with no heading,
   identical to the prior single-branch view. `AgentHistoryView` still
   exists but is now the trivial-vs-DAG dispatcher; a new `BranchSectionView`
   (one per branch, its own independent load-more state) does the actual
   per-branch rendering `AgentHistoryView` used to do directly - a shared
   ancestor branch is one `BranchSection`, never duplicated across its
   children, satisfying "shared ancestors shown once" structurally rather
   than needing deduplication logic. Verified with a new browser check
   (`check_developer_mode.cjs`, using a real split in the shared browser
   fixture) asserting the retired source branch, both new current-head
   sections and a "continues from branch ..." parent link all render, and
   with two new store.rs tests for `gm_branch_graph` itself (a plain agent's
   trivial one-branch/no-parents/no-head graph, and a split's exact
   retired-source-plus-two-heads shape with the correct parent
   through_position). Verified by reverting `AgentHistoryView`'s branch-count
   check to always render single-section and confirming the browser test
   fails exactly as expected, then restoring it and confirming green again.

Examples to verify through Store/Game and the browser: root [a,b], split left
[c,d]/right [e], merge [f] yields [a,b,c,d,e,f]; a second split/remerge still shows
shared entries once. Equal text with different entry IDs appears twice. Compact
root, one suffix, both suffixes and then the merged prefix; earlier recipes
stay byte-identical. Different groups at one location do not share private GM
context. Invalid partitions/cross-world merges leave all membership/head rows
unchanged. A grouping call with ordinary siblings preserves the full exchange.

## E. Exact inference evidence and complete raw fields

1. Export complete inference/turn/history/object types through src/inspection.rs
   in the existing library. Gate database/runtime dependencies to server code.
   Remove hand-maintained UI mirrors and content_text projections that lose IDs
   or arguments. Render raw JSON and separate exact prompt/message text fields,
   with full output/reasoning/errors, settings, sources and clipboard controls.
2. Extend the existing runtime capture test through normal Game calls: capture
   request and formatted prompt/tokens independently, then open those same
   inference IDs in the browser. Include a real tool round trip and compaction;
   change notes/settings after capture and reopen before comparing again.

Pass exact request/prompt/token comparisons and browser clipboard comparisons
for Unicode, whitespace, HTML, tool IDs/arguments/results and reasoning. Empty
or unavailable fields are distinguished from zero-valued measurements. Evidence
from the earlier direct runtime test alone does not satisfy this product-path gate.

## F. Navigation and completion

1. Implement the design's query-string InspectSelection adapter using the native
   URL API and history.pushState/popstate. Initial hydration/reload selects the
   same authorized JSON resource. Keep PlayerChat mounted throughout. Back and
   forward do not add history entries. Entry/roll links select and highlight
   the exact history/tool node, and inference links include its history position.
2. Add cursor-driven Load more plus IntersectionObserver to agent/branch histories.
   Preserve the head/cut and selected entry across loading; a live refresh starts
   a new head snapshot explicitly. Add name/type/ID search and stable object-ID
   paging to world_objects; render every returned relationship as a typed link.
3. Fix the Inspect pane's "Close" button: it currently only clears the current
   selection (`selection.set(None)`), leaving an empty panel permanently
   visible, since the `<aside>` itself is never conditionally hidden. The
   user's chosen combination is options 1+3 from 2026-09-25's brainstorm in
   user_exerpts.md, not option 1 alone: Close collapses the full panel down to
   a thin, always-visible vertical strip on the right edge (an icon/label, no
   selection content), never removing it from the DOM entirely and never
   requiring a separate chat-header button. Clicking the strip expands back to
   the full panel and the last selection (already preserved by step 1's
   URL-driven selection state). Chosen over a floating/draggable panel: fixes
   the actual bug with the least new surface area, while keeping a persistent
   affordance rather than an easily-missed reopen control.
4. Add narrow-screen Chat/Inspect tabs using the existing sm breakpoint and CSS
   visibility. Both panes remain mounted. Verify draft text and one chat socket
   survive navigation, pagination, toggling and responsive switches.
5. Run the complete production-path verification above, update README and
   implementation_reference.md, and remove obsolete viewer/query helpers.

## Acceptance and commit gate

For each slice, re-read [coding standards](../../coding_standards.md) and
[prompt standards](../../prompt_standards.md), and complete the
[AGENTS.md post-feature/pre-commit checklist](../../AGENTS.md#post-feature-and-pre-commit-checklist).
The cases above form the test design for user review before implementing any
LLM-facing changes. Exercise real configured-model chats for grouping/context,
tool and compaction changes; scripts may control only the external backend,
not replace game assembly, persistence or sockets. Run focused Rust tests,
server/WASM builds and npm run test:hydration. Archive/remove from todo only
when all A–F outcomes pass; a missing product decision blocks only its dependent
slice and is reported explicitly.
