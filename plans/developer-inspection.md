# Developer Mode implementation

## Deliverable and traceability

A player can inspect the exact turn/tool tree, model input/output, linear agent
history, GM group-history DAG and current objects while chat remains usable.
Implement the data/protocol choices in [design.md](../design.md#chat-schema);
this plan specifies changes and their order. It does not mark the feature done.

| Requirement | Declaration | Design | Steps |
| --- | --- | --- | --- |
| Owner toggle and continuing member access | [World detail](../user_declarations.md#world-detail-page) | Membership and presence; Live inspection protocol | A, F |
| Every original message, summary and failed inference, exact reconstructed input | [Debugging and Telemetry](../user_declarations.md#debugging-and-telemetry), [Chat compaction](../user_declarations.md#chat-compaction) | Chat schema; Inference records | B, C, E |
| One GM context per interacting group, split/merge histories | [GM interaction](../user_declarations.md#gm-interaction), [Game state](../user_declarations.md#game-state) | Groups and shared GM history | B, D |
| Calls/results in their actual tree and order | [Agent syntax and tool calls](../user_declarations.md#agent-syntax-and-tool-calls), [Developer Mode](../user_declarations.md#developer-mode) | Durable execution relationships | C, F |
| Live inspectable raw data, usage, navigation | [Developer Mode](../user_declarations.md#developer-mode) | Live inspection protocol; Browser selection and raw fields | A, E, F |

The next increment is [Required corrections before D](#required-corrections-before-d);
A–C are not yet accepted as complete.

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

## Required corrections before D

This is the next work in this plan. Finish these corrections to A–C before
starting group/presence work. They restore existing requirements; they do not
change product policy or await the outstanding Product decisions. Each subsection
below is an ordered, independently verifiable increment. Apply the acceptance and
commit gate at the end of this plan to each increment.

Review baseline: `a7191bd`, 2026-09-17. The existing Rust suite passed 84 tests
with 9 opt-in tests skipped. The real-browser suite failed. Temporary probes
through production Store/tool-loop methods reproduced the defects below; those
probes were removed after review. Add permanent behavioral regressions with the
fixes. These observations are evidence of unresolved work, not completed tests
for the future implementation.

### Restore the browser verification path

**Requirement:** Developer Mode's live inspection and exact execution ancestry
([declaration](../user_declarations.md#developer-mode)); design's Live inspection
protocol and Durable execution relationships; A5/C3.

**Problem:** `web.rs`'s test-only `start_live_inference` handler calls
`create_tool_executions(parent_inference_id, 1)` for every synthetic child.
The parent already owns ordinal 0. `npm run test:hydration` fails with
`UNIQUE constraint failed: tool_execution.inference_id, tool_execution.ordinal`
at the first live-inference step in `check_developer_mode.cjs`.

**Implementation:** Have the fixture record its tool-bearing parent inference
and complete batch once. Include its actual parent tool-execution ID in the
fixture data. Change the test-only start request and JavaScript helper to pass
that ID into `start_invocation`; a root passes no parent. Each independent
synthetic invocation needing a different causal call gets a fresh recorded
parent call, not another execution row fabricated on an existing output.
Keep the real Axum inspection websocket, Store writes and broadcast path.

**Alternatives:** Reusing the real recorded call follows the production schema
and removes fixture-only creation logic; it requires updating the fixture wire
fields. Ignoring uniqueness errors would hide invalid ancestry. Dropping the
constraint or restoring a fake inspection socket would lose the guarantee being
tested; neither is acceptable.

**Verification:** Run the complete browser suite without skipping assertions.
Check each child invocation references an existing ordinal in its parent's
recorded output. Repeated fixture requests cannot duplicate executions. This
restores the harness only; it does not by itself prove every A acceptance case.

### Use bounded history ranges and validate their owners

**Requirement:** Original, inspectable histories and isolated GM branches
([Developer Mode](../user_declarations.md#developer-mode),
[GM interaction](../user_declarations.md#gm-interaction)); design's Chat schema
and Groups and shared GM history / Context order; B3–B5.

**Problems:** `history.rs::range` recursively loads every history-entry row and
uses linear searches through accumulated entries to remove duplicates.
`live_segments` scans that vector to select a summary/tail. `agent_raw_history`
also queries each inference's outcome before slicing a page. A two-row page in
a local probe took about 5.5 ms for 100 inference records and 52 ms for 1,000;
these are observations, not portable performance thresholds. `branch_owner`
looks through the current `agent_history` head, and only the requested head is
validated. A probe linking an authorized head to another world's branch returned
that world's message. Normal gameplay has no parent writer yet; correct this
before D introduces one.

**Implementation:** Replace the entry-vector traversal with ordered
`PositionRange` metadata in `store/history.rs`. Resolve ownership from
`history_branch.agent_id JOIN agent`, including retained historical branches,
and validate every traversed parent against the requested agent/world. Keep
cycle/missing-reference errors with branch IDs. Subtract visited intervals per
branch before traversing shared ancestry again; do not deduplicate loaded message
rows or equal text. Preserve parent ordinal order and bounded cuts.

For raw pages, walk those ranges from the cursor and select visible entries in
SQL, joining inference outcome/ID there. Apply position bounds, ordered keyset
continuation and `LIMIT remaining + 1`; load payloads for the returned page only.
Keep the selected head fixed across pages. Reject invalid cursor positions
rather than silently treating them as end-of-history. For live context, query
summary activation/coverage metadata first, then fetch only the selected summary
and uncovered raw ranges. Preserve the existing linear summary semantics;
D5 adds the declared multiple-parent summary selection using this same range
representation, without another traversal implementation.

**Alternatives:** The current load-all approach is short locally but makes each
page and inference depend on total history and duplicates ancestor work. A
cached flattened transcript adds invalidation and another persisted/copied
history. Bounded range metadata uses the existing index and immutable edges;
it needs interval traversal code, but bounds entry/payload reads and serves both
inspection and model context. Select it as already required by design.md.

**Verification:** Paginate mixed messages, failures, summaries and notices across
boundaries; append data between pages and retain the original head. Exercise a
shared-ancestor diamond, overlapping parent cuts, equal-text distinct entries,
retired heads, cycles, missing parents, and foreign-agent/world parents. No
foreign payload may be returned. Measure fixed-size pages at increasing history
sizes and inspect query plans/row counts: no per-inference query over the entire
past, and no covered historical entries loaded for ordinary compacted context.
Topology metadata may scale with branch count. Compare reconstructed requests
before/after and exercise normal chat/compaction through the configured model.

### Reuse output content and preserve inference identity

**Requirement:** Record once, reference original data, inspect failures accurately
([Debugging and Telemetry](../user_declarations.md#debugging-and-telemetry),
[Developer Mode](../user_declarations.md#developer-mode)); design's Chat schema
and Inference records; B4–B6.

**Problems:** `finish_chat_inference` writes an inference output, reads it back,
then calls `insert_message`, which creates a second recorded_content row. A probe
found output ID 1 and assistant-message content ID 2 for the same response.
Narration recipients reuse the output ID, but the GM's own message does not.
Separately, `resolve_raw_history_entry` returns `history_entry.id` as the failed
inference's ID. A probe found the displayed ID 2 for actual inference 1. The
current UI labels it incorrectly; it does not yet render a clickable failure link.

**Implementation:** Pass the existing output ID directly to
`insert_message_with_content` with Role::Assistant and source inference ID,
inside the existing completion transaction. Remove the read/deserialization and
second insertion. Preserve compaction enqueueing against the returned message
entry ID. Return that same output reference to narration delivery. In raw
history queries, select `inference.id` alongside its error by history_entry_id;
keep history position/entry identity separate from inference identity throughout
paging and presentation.

**Alternatives:** Direct references remove work and preserve provenance; they
require callers to distinguish producing content from linking existing content.
Global equality deduplication would incorrectly merge independent equal outputs.
Copying then repairing links retains extra writes and failure boundaries. For
IDs, joining the existing relationship is sufficient; introducing a global
shared ID allocator would conflate unrelated entities.

**Verification:** Assert inference output, its own assistant message and all
narration recipients share one content ID, for both text and tool-bearing output
where applicable. Two independent equal outputs must still have different IDs.
Exercise error outcomes with no message. Interleave unrelated messages/inferences
so their IDs diverge; every failed row must identify the actual failed inference
and its reconstruction must return the recorded error. Retain summary/tool-result
sharing tests; do not loosen them to compare text alone.

### Keep action resolution authoritative in the transaction

**Requirement:** Rust applies approved actions once and records their real result
([Agent syntax and tool calls](../user_declarations.md#agent-syntax-and-tool-calls),
[Character actions](../user_declarations.md#character-actions)); design's Durable
execution relationships; C4.

**Problem:** `game.rs::arbitrate` duplicates the Store's durable resolution state
in an `Arc<Mutex<bool>>`, with claim/release helpers and separate checks in both
tools. Its retry behavior differs from Store's same-outcome idempotency. This is
an extra authority rather than structural prevention of a duplicate mutation.

**Implementation:** Make `resolve_pending_action` return a typed
`ResolvedAction { id, approved, result_content_id }` representing the persisted
result. For a pending action, atomically apply/record the result and consume the
pending row. For an already resolved action, validate world/GM ownership and
return its original resolution without executing anything. Approval/rejection
tool closures compare the requested decision with that returned decision:
matching repeats report the original stored result; a conflicting decision
becomes `ToolOutcome::Rejected` describing the existing resolution. A new
rejection reason cannot replace the original reason. Database/infrastructure
failures remain errors with full context. Remove resolution_claimed,
claim_resolution and release_resolution; retain the transactional uniqueness
boundary. Update all callers of the old PendingAction return value together.

**Alternatives:** A Store-owned result works across calls and process restarts
and removes synchronization state, at the cost of adapting the tool return path.
Keeping both guards requires synchronizing two policies and lifetimes. Keeping
only the in-memory flag fails across invocations/restarts and cannot protect the
mutation. Choose the Store-owned result, with ordinary typed game rejection at
the tool boundary rather than parsing database error strings.

**Verification:** Both approve-then-reject and reject-then-approve preserve the
first outcome and return a recorded tool rejection. Same-decision repeats return
the original result after reopening the database, with one mutation/action row.
Changed rejection text does not replace persisted text. Foreign-world/GM calls
fail. Exercise the real tool loop as well as Store; preserve behavioral coverage
of existing conflict tests while updating their expected typed API outcome.

### Finalize the entire tool batch through one exit boundary

**Requirement:** Accurate execution tree and failure propagation
([Debugging and Telemetry](../user_declarations.md#debugging-and-telemetry),
[Chat directives](../user_declarations.md#chat)); design's Durable execution
relationships; C2–C5.

**Problem:** `agent.rs::run_invocation` handles an execution error explicitly,
but `?` exits from result persistence or CompletedAfter delivery bypass sibling
finalization. A probe with two calls and a failing first delivery left statuses
`[completed, running]` even though the invocation had returned an error.

**Implementation:** Run each batch's execution, result recording and delivery in
one awaited Result-producing block. Track the current ordinal and whether its
result committed; finalize from one outer match. If execution failed before a
result committed, record that call failed with its full error. If its result
already committed, preserve it and the completed mutation; record the delivery
failure on the invocation/sequence. Mark only subsequent, never-entered calls
unexecuted. Do not label an entered call unexecuted, replay a committed mutation,
or rewrite a completed result to make finalization convenient. If persistence
itself is unavailable, propagate the primary error plus finalization failure;
startup recovery handles records whose outcome could not be committed.

Use the existing invocation wrapper for one explicit awaited completion on
normal success/error returns. The current InvocationGuard's Drop only asserts;
it provides no async cleanup. Remove that wrapper/finished flag if the single
result boundary makes it redundant, retaining the behavioral lifecycle tests.
Cancellation/process death remains explicit interruption recovery, not a hidden
background write from Drop. Remove the newly introduced clippy allow; keep the
batch's operation in its existing scope instead of adding a generic lifecycle
framework or a wrapper solely to silence argument-count lint.

**Alternatives:** One outer finalizer covers existing and future error paths with
less repeated cleanup. Additional cleanup at individual `?` sites is easy to miss
again. Async work spawned from Drop cannot reliably report completion/failure and
adds shutdown races. The selected boundary still cannot promise a successful
write during database failure; surface that failure rather than masking it.

**Verification:** Fault execution, result persistence and after-result delivery
separately, including failure after a committed action and before a sibling.
With working persistence, no returned invocation leaves an unreached call
running. Preserve original committed effects/results, nested-call ancestry and
full error context. Test recovery for interrupted unfinished records. Do not
replace behavioral tests with a test that merely asserts the guard panics.

### Re-establish the completion evidence

Run focused regressions, the Rust suite, server/WASM checks and the full real
browser suite. For changed context/model-facing behavior, apply prompt_standards
and the real configured-model chat/compaction gate below. Browser assertions
about races must control response ordering; token-gap injection alone does not
prove actual broadcast lag. Exercise the A cases with real sockets, including
lag, slow readers and reconnect state reconciliation. Preserve every existing
assertion's intended coverage; repair fixtures rather than skipping failures.

Update implementation_reference.md with what actually exists and the evidence
obtained. Remove obsolete helpers and stale claims that outputs already share
identity, range reads are bounded, or every error path is finalized. Keep A–C
completion unverified until this correction gate passes; then continue D–F in
order. No runtime test or implementation correction is claimed by this plan edit.

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
2. Implement the [presence plan](multiplayer-presence.md) now, using the group
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
3. Add narrow-screen Chat/Inspect tabs using the existing sm breakpoint and CSS
   visibility. Both panes remain mounted. Verify draft text and one chat socket
   survive navigation, pagination, toggling and responsive switches.
4. Run the complete production-path verification above, update README and
   implementation_reference.md, and remove obsolete viewer/query helpers.

## Acceptance and commit gate

For each slice, re-read [coding standards](../coding_standards.md) and
[prompt standards](../prompt_standards.md), and complete the
[AGENTS.md post-feature/pre-commit checklist](../AGENTS.md#post-feature-and-pre-commit-checklist).
The cases above form the test design for user review before implementing any
LLM-facing changes. Exercise real configured-model chats for grouping/context,
tool and compaction changes; scripts may control only the external backend,
not replace game assembly, persistence or sockets. Run focused Rust tests,
server/WASM builds and npm run test:hydration. Archive/remove from todo only
when all A–F outcomes pass; a missing product decision blocks only its dependent
slice and is reported explicitly.
