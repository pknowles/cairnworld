# Code quality and correctness by construction

## Outcome and authority

Invalid states are unrepresentable rather than checked for, and a reproducible
command fails on warnings, formatting, forbidden dependencies, duplication and
complexity. Authority: [coding_standards.md](../coding_standards.md) (RAII and
complete-at-initialization, no optional values, fail fast, single definition,
group by feature, reference rather than copy, performance) and the user's
request for Clippy, archtest, jscpd-rs and bca checks. Player latency:
[Scheduling](../user_declarations.md#scheduling).

Tool identities: [ArchTest](https://github.com/tdymel/arch_test) checks module
access rules; [jscpd-rs](https://github.com/vv-bogdanov/jscpd-rs) provides the
jscpd clone detector; [big-code-analysis](https://github.com/dekobon/big-code-analysis)
provides bca function metrics/checks. Use these tools, not similarly named crates.

Runs after [bounded history ranges](developer-inspection.md#bounded-history-ranges),
which introduces the `BranchHead` validated-handle pattern this plan extends.

## Findings

Review of `8f32f75`, 2026-09-24. Clippy is clean and 102 Rust tests pass with 9
opt-in skipped; the rest is from reading the code and has not been measured.

- **Gates missing.** `cargo fmt --check` reports about 190 diffs. No crate root
  sets `deny(warnings)`/`forbid(unsafe_code)`, and there is no check script.
- **Store is one type for all persistence.** `src/store.rs` is ~5,500 lines of
  production code with ~97 public methods spanning membership, presence,
  groups, inference records, compaction jobs and inspection, grouped by
  technical layer rather than feature.
- **Duplicate functions caused by the one-connection pool.** `branch_for_agent`
  / `branch_for_agent_tx`, `world_gm_agent_id_tx`, `text_in`, `content_in`
  exist because a `&self.pool` query inside an open transaction deadlocks.
- **Duplicate SQL.** The "owner's first character's starting group" query is
  copied four times (`store.rs` ~1085, ~1680, ~3181, ~3194).
- **SQL is unchecked.** design.md's dependency list says sqlx provides
  compile-time-checked SQL; all ~232 queries are runtime strings.
- **Single SQLite connection** (`store.rs` `max_connections(1)`) despite WAL:
  game turns, presence writes, logins and Developer Mode paging all queue
  behind each other.
- **Static prompt round trip per inference.** `context::segments` serializes
  and writes the role prompt and tool definitions, and `request_for_segments`
  reads them back and re-parses the tool JSON, on every turn.
- **References stored as JSON.** `inference.segments`,
  `summary.covered_ranges` and `pending_compaction.static_segments` hold text,
  summary and branch ids with no foreign keys, so reconstruction re-validates
  ownership at read time.
- **Re-checked authority.** Seven inspection reads start with
  `self.inspecting(viewer, world)`; history readers re-validate branch
  ownership; ~100 `Option<` uses in Store's production code, several of which
  select between two different operations (`branch_id: Option<i64>`,
  `after: Option<cursor>`) or mark fields that only exist for one caller
  (`Completion::output_content_id` is `None` only for compaction).
- **Lazily created world state.** `Game` holds `worlds` and `openings` maps
  created on first access; `Store` owns the Developer Mode broadcast map,
  mixing live pub/sub with persistence.
- **Presence events can be dropped.** `Game::connect`/`disconnect` send with
  `let _ = try_send` into a 64-slot queue that only drains between model turns.
  A world task's turn error is lost when the requester has gone
  (`let _ = reply.send(...)`).
- **Remaining wire mirrors.** `ui::DevEvent` mirrors `store::DevEvent`, and a
  `ui.rs` struct mirrors `web::RawHistoryPageResponse`.
- **Tests reimplement production paths.** `compaction.rs` has a
  `#[cfg(test)]` `CompletedTurn`/`after_turn` trigger that production never
  runs; ~17 `#[cfg(test)]` Store helpers write raw SQL (e.g.
  `enqueue_compaction_for_test`).
- **Hand-rolled inference admission.** `inference.rs` implements a
  priority-aware admission loop over `Mutex` + `Notify`.

Tracked elsewhere: one sequential task per world blocks other groups during a
model turn until [playable rules](playable-rules.md) adds durable suspension.

## Design refactors

Each item: problem, alternatives, recommendation. The recommendations share one
principle: validate or construct once, hold the result in a value whose type
proves it, and let the schema or compiler reject what used to be checked.

### Validated handles instead of repeated checks and `Option` parameters

| Option | Pros | Cons / risks |
| --- | --- | --- |
| Keep per-method checks | No churn | Every new reader must remember the check; `Option` parameters keep two operations in one signature |
| Authorization only in web handlers | One place | Store/game callers (CLI, MCP, tests) get no protection; checks drift from the queries |
| Validated handle types (recommended) | Holding the value proves the check passed; signatures state which operation; fewer queries | One-time migration of call sites |

Recommendation: values with private fields and only validating constructors,
e.g. `WorldInspection` (from the viewer's access check; inspection reads are
its methods), `BranchHead` (from the bounded-history slice), and a group GM
handle. Replace `Option` parameters that select an operation with separate
constructors, and `Option` fields present for one caller with separate result
types. Triage every remaining `Option` in production code: keep it only where
absence is real data (e.g. nullable `sequence.finished_at`).

### Persistence grouped by feature, one function per operation

| Option | Pros | Cons / risks |
| --- | --- | --- |
| Split `impl Store` across feature files | Mechanical, low risk | Still one type with every operation; `_tx` twins remain |
| Feature modules own their queries as functions over `&mut SqliteConnection` (recommended) | One function works inside or outside a transaction, so no `_tx` twins; persistence lives with its feature; Store shrinks to the pool | Larger move; call sites pass a connection or transaction explicitly |
| Repository per table | Familiar | Groups by type, which the standards reject |

Recommendation: the second option. Store keeps pool ownership and transaction
start; groups, presence, membership, inspection, compaction jobs and inference
records move next to their features. The four copies of the starting-group
query become one function.

### Compile-time checked SQL

| Option | Pros | Cons / risks |
| --- | --- | --- |
| `sqlx::query!`/`query_as!` with committed offline `.sqlx` data (recommended) | Schema/query/type mismatches fail the build; in-place schema edits are caught everywhere | Needs sqlx-cli and `cargo sqlx prepare` after schema changes; SQLite nullability inference needs `as "col!"` overrides on some joins |
| Keep runtime strings and change design.md | No work | Errors surface at runtime, only on paths tests reach |
| Query builder / ORM | Typed composition | Large dependency and rewrite; more of our code, not less |

Recommendation: the first option, converting each module as it moves (previous
item). `cargo sqlx prepare --check` joins the check script. This makes the code
match design.md's existing dependency choice (user-confirmed 2026-09-25).

### Connection model

| Option | Pros | Cons / risks |
| --- | --- | --- |
| Keep one connection | Simple, fully serialized | All reads wait behind writes; forces `_tx` twins |
| Larger single pool | WAL concurrent reads | A deferred transaction that later writes can fail with `SQLITE_BUSY` instead of waiting |
| Read pool (N, read-only) plus write pool (1, `BEGIN IMMEDIATE` via `begin_with`) (recommended) | Concurrent reads; writes serialized in-process, never busy; a write needs a write transaction by type | Two pools to configure; reads inside a write use its transaction (already enforced by the connection-parameter functions above) |

Recommendation: the third option, after the feature-module change removes the
twins. Measure Developer Mode paging concurrent with a game turn before and after.

### Static recipe constructed once

| Option | Pros | Cons / risks |
| --- | --- | --- |
| Keep the per-turn write/read round trip | The request is built only from stored records | A write, a read and a JSON parse per turn |
| Build the request in memory, record separately | No round trip | Two paths that can diverge from what was recorded |
| Intern the role prompt and tool definitions once when the agent's recipe is constructed (recommended) | The recipe value holds the committed ids and their immutable text/parsed tools, so the request is built from the record by construction; no per-turn I/O | Recipe construction moves to agent/role setup |

Recommendation: the third option. Database reads remain only for reconstructing
past inferences. The existing request-reconstruction device test must still pass.

### Relational inference segments

| Option | Pros | Cons / risks |
| --- | --- | --- |
| Keep JSON columns plus read-time checks | No schema change | Integrity depends on every reader; not queryable |
| Child tables (`inference_segment`, `summary_range`, `compaction_static_segment`) with foreign keys, message ranges keyed by `(branch_id, agent_id)` (recommended) | Dangling or foreign references are unwritable; reconstruction drops its checks; queries such as "which inferences included this message" become possible | Schema and reader/writer rewrite; more rows per inference |

Recommendation: the second option, while the pre-user schema can still be
rebuilt in place.

### Worlds as owned objects; presence as state

| Option | Pros | Cons / risks |
| --- | --- | --- |
| Patch the channel: `send().await` or unbounded | Small | `send().await` makes connect wait behind a turn (design forbids); unbounded still turns state into a queue of stale events |
| `World` value constructed complete for each world (at startup and on install), owning its task, event queue, group broadcasts, connections, Developer Mode broadcast and a coalesced presence-changed set plus `Notify` (recommended) | Presence cannot be dropped or overflow, since the handler reads current state (design already calls the event a wake-up); no lazy maps; Store stops owning pub/sub; the world task records every event error itself | Moderate move of `game.rs` state; a task per world at startup |
| Actor framework | Structured | New dependency for what tokio already provides |

Recommendation: the second option. `Game` holds `HashMap<world_id, Arc<World>>`
built complete. Ignored results that remain (a broadcast with no receivers, a
reply to a requester that has gone) are named explicitly and the underlying
error is still recorded.

### Wire types and tests on the real path

No alternatives needed: move `DevEvent` and the raw history page response into
`src/inspection.rs` and delete the mirrors. Delete compaction's test-only
trigger and drive those cases through `complete_recipe`/`finish_chat_inference`
with the existing scripted backend. Keep `#[cfg(test)]` Store helpers only for
read-only assertions; seed state through production operations.

### Inference admission

| Option | Pros | Cons / risks |
| --- | --- | --- |
| Keep the current loop | Works; small | Our code for a scheduling primitive |
| tokio `Semaphore` plus a foreground-waiting gate | Standard primitive | Priority still needs custom code |
| mistral.rs native scheduling/priority | Declared preference ("less code for us to maintain") | Unknown whether our fork supports priority |

Recommendation: check mistral.rs's scheduler for priority support first; if it
has none, keep the current loop. Lowest priority.

## Ordered changes

1. One mechanical `cargo fmt` commit over the tree. Add scripts/check-quality.sh
   running `cargo fmt --check`, server Clippy with `--all-targets -- -D warnings`
   and Clippy for the no-default-features hydrate WASM library with
   `-D warnings`. Set `deny(warnings)` and `forbid(unsafe_code)` on crate roots.
   Keep dependency build-script diagnostics distinct; add no allow attributes.
2. Move the remaining wire mirrors into `src/inspection.rs`.
3. Put tests on the real path (compaction trigger, `#[cfg(test)]` writers).
4. `World` objects and presence as state.
5. Persistence by feature with connection-parameter functions, checked SQL and
   validated handles, one feature module per commit. Remove the `_tx` twins and
   the duplicated queries as each module moves.
6. Read/write connection pools.
7. Relational inference segments and constructed-once static recipes (both
   change recipe storage; one slice).
8. Put request_builder and original-version prompt rendering in
   src/model_prompt.rs, taking immutable Request/artifact values. Backend and
   reconstruction call that same module; store/prompt.rs persists/loads
   evidence and no longer imports mistralrs_backend.
9. Add architecture.json with MayNotAccess rules: UI/shared wire types may not
   import Store/Game/runtime; llm/content/wire data may not import web/MCP;
   persistence may not import web/MCP/Game; game operations may not import
   web/MCP. Run cargo archtest from the shared library root. Prove the rules
   reject a forbidden import in a temporary fixture.
10. Add .jscpd.json scanning src and scripts (5-line/50-token fragments, 5%
    maximum duplicate-line ratio) with reports under target/quality.
    Consolidate identical operations into their feature's existing function.
11. Add bca.toml with function cyclomatic and cognitive complexity limits of 15.
    Split complex functions by their real responsibilities. No suppressions or
    accepted-violation baseline; if a metric demands a worse design, report the
    case before changing its threshold. The thresholds are proposals.
12. Pin tool versions in the documented install commands, record reports
    before/after, update README with commands and scopes. The script exits
    nonzero on missing tools and violations.
13. Investigate mistral.rs admission priority.

## Verification and completion

Each step is its own commit with focused tests for moved/refactored operations,
the full Rust suite and the browser suite. Removed runtime checks are replaced
by a test proving the schema or type rejects the invalid state. Measure the
per-inference database work and concurrent Developer Mode paging before step 5
and after steps 6 and 7. Prompt/model-facing changes (step 7's recipe, step 8's
formatter) additionally require a real configured-model chat and prompt review.
Update design.md (after user sign-off where marked) and implementation_reference.md,
re-read coding_standards.md and prompt_standards.md and complete the
[AGENTS.md checklist](../AGENTS.md#post-feature-and-pre-commit-checklist).
