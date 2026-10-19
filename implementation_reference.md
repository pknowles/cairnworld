# Implementation Reference

Index of what exists, mapped to design.md sections. Plans describe remaining
work; their completion claims do not override the source.

Cargo's package lint table denies warnings and forbids unsafe code for every
application target, including build.rs and tests. `scripts/check-quality.sh`
checks formatting and server/WASM Clippy; dependency lints remain upstream-owned.

## Current alignment boundary

Developer Mode has the A–C recording/query foundations, but their completion is
not verified. Review of `a7191bd` on 2026-09-17 found several defects; the
corrections below have landed, including bounded history ranges
([plan](plans/developer-inspection.md#bounded-history-ranges)). Status:

- **Fixed:** `game.rs::arbitrate` no longer mirrors durable resolution in a
  per-turn `Arc<Mutex<bool>>`. `Store::resolve_pending_action` is the sole
  authority: it returns a `ResolvedAction { id, approved, result_content_id }`
  whether newly applied or already resolved, never erroring on a conflicting
  decision. `approve_action`/`reject_action` compare their requested decision
  against the returned one - a match reports the recorded result, a mismatch
  becomes an ordinary `ToolOutcome::Rejected` naming the existing resolution,
  never a generic error, and never replaces the original result. The
  already-resolved lookup no longer re-derives `tool`/`args`/`character_id`
  through a six-table join (design.md's Chat schema: the caller never needed
  those fields, only the decision and result reference) - it reads
  `action.approved`/`result_content_id` directly. `resolve_tool_call_tx`
  (only needed by the old re-derivation) is removed.
- **Fixed:** the real-browser suite's fixture no longer recreates a parent
  tool_execution row per live-inference step. `web.rs`'s browser-only setup
  records and completes the one real tool-bearing parent call
  (`roll_hit_protection`'s `hp-roll`) once and exposes its
  `parent_tool_execution_id` directly in the fixture JSON; `start_live_inference`
  nests every synthetic child invocation under that same already-recorded
  call instead of fabricating a fresh `tool_execution` row per JS request,
  which collided on `(inference_id, ordinal)` the moment the same parent was
  reused across steps (`check_developer_mode.cjs` calls it 5 times).
  `scripts/check_developer_mode.cjs` passes `fixture.parent_tool_execution_id`
  accordingly. `npm run test:hydration` passes in full, no assertions skipped.
- **Fixed:** `finish_chat_inference` no longer reads its own just-written
  output back and writes a second, distinct `recorded_content` row for the
  reply's assistant message. It calls `insert_message_with_content` directly
  with the existing `output` id (the same primitive narration delivery
  already used), so the inference's `output` column and its own message's
  `content` column are the same row - verified by
  `a_settled_reply_shares_its_content_row_with_the_inference_output`, which
  also confirms two independently produced equal-text replies still get
  distinct rows (no accidental interning). Separately, `resolve_raw_history_entry`
  now selects `inference.id` (not the enclosing `history_entry.id`, a
  different primary key) for a `FailedInference` row, and `ui.rs` renders it
  as a clickable link to that inference's view, matching the existing pattern
  for a summary's source-inference link.
- **Fixed:** `store::history::AgentBranch` is the private-field proof that a
  selected branch belongs to its world and agent. Turns, context assembly and
  live history take it directly; `agent_branch`, `group_branch`, and the
  browser-gated `branch_for_viewer` are its only constructors. `RawCursor`
  validates its fixed head and next key before paging. `reach()` is the single
  recursive traversal definition, whose JSON result feeds the literal raw-page
  query; there is no whole-index load, sentinel cursor or production-only test
  traversal. The browser suite confirms the shared inspection types hydrate.

D (groups and their heads) and E (raw fields/evidence, including the
product-path verification against real model boundary captures) are complete.
F's navigation is in progress: the query-string `InspectSelection` adapter
(pushState/popstate, no history entries on back/forward) and the Close/reopen
fix are done - Close collapses the pane to a thin, always-visible strip on the
edge (never removed from the DOM, no separate reopen control elsewhere);
clicking the strip expands back to the last selection. Exact node
highlighting/history-position links, cursor-driven history and object paging,
and object search are done. At widths below `sm`, Chat and Inspect tabs use
CSS visibility to show one pane without unmounting either; an inspect
selection activates Inspect, and a tab switch preserves the chat draft and
its sole WebSocket. Planned behavior beyond that is not implied to exist here.

## Inference layer (design.md: Inference layer)

- `src/llm.rs` - the `Backend` trait, `Request`/`Response`/`Message`/
  `MessageContent`/`ToolDefinition`/`Sampling`/`Content`/`Usage` types. `Role`
  is `System` (position-0 directive only), `User`, `Assistant`, `Tool`, and
  `Narration` for a GM narration carried in a player agent's history; the
  backend maps `Narration` to a user-role message that names the GM.
- `src/mistralrs_backend.rs` - `MistralRsBackend`, the mistral.rs
  implementation of `Backend`. Loads a GGUF model via `GgufModelBuilder`
  (`cuda` feature). A configured `source_model` supplies the original model's
  configuration, tokenizer, and template when a native GGUF architecture such
  as Qwen3.5 does not carry sufficient metadata itself. `complete` drives
  `on_token` from
  `Model::stream_chat_request`'s `Chunk`s and assembles the final
  response. It logs model loading and inference lifecycle (request submitted,
  first stream chunk, completion or exact stream/model failure) with elapsed
  time and non-sensitive request counts, so a hung or failed model boundary is
  visible immediately.
  `Response`/`Usage` from the accumulated chunks - `stream_chat_request`
  never emits a terminal `Response::Done` (that variant is only sent on the
  non-streaming `send_chat_request` path), so there is nothing to read back.
  Sampling starts from `SamplingParams::neutral()`, not the crate's default
  `deterministic()` (which forces greedy `top_k = 1` independent of
  temperature), then applies the request's `temperature` and any of `top_p`,
  `top_k`, `min_p`, `presence_penalty` that are set. Truncation matters: a
  small model sampled from its full distribution at a normal temperature
  produces incoherent text and, with tools present, spurious tool calls. It
  maps native structured tool calls and reasoning deltas to the shared types,
  and applies the recorded `enable_thinking` setting. Tools
  are sent with `strict`, constraining generation to the argument schema.
- `src/tools.rs` - relationship-local tools distinguish a model-correctable
  rejected call (malformed or unavailable name, returned durably to the
  calling agent) from an actual execution failure, which propagates with its
  complete error context. It also rejects a model-facing tool schema unless it
  is an object with an explicit `properties` map; no-argument tools use `{}`.
- `third_party/mistral.rs` - submodule of https://github.com/pknowles/mistral.rs,
  depended on by path. It carries fixes for Qwen assistant tool-call rendering,
  a Qwen tool grammar with arbitrary parameters, structured paged-KV capacity
  rejections, and passing a stored tool call's `arguments` to the chat template
  as a decoded object (the HF convention; Qwen3.5's template iterates it as
  key/value pairs). Cairnworld's own schemas always explicitly declare their
  properties. Its upstream base provides native GGUF loading for supported
  architectures, including Qwen3.5, and CUDA 13.4 bindings. The Windows CUDA
  implementation enables the same paged-attention kernels, selects ARM64 import
  libraries, and uses explicit 64-bit cache indices and rotary stream handles.
  Dtype capability checks query the selected CUDA devices directly instead of
  parsing `nvidia-smi` output for unrelated devices.
  Numerical tests cover grouped-query attention at head dimensions 64/128/256,
  CUDA-graph replay, scale-update boundaries, and rotary position paths.
  `PromptObserver` supplies the normal generation prompt, token IDs, and retained
  `ChatTemplateFormat` before generation; an observer error aborts the request.
  The retained format preserves its clock, content transformations, and
  tokenizer-specific tool-output escaping for exact reconstruction. Application
  persistence remains in Cairnworld. Build prerequisites are in `README.md`.
  Clone with
  `--recurse-submodules`, or run `git submodule update --init` in an existing
  checkout.
- `templates/hermes-tools.jinja` - the Hermes 3 GGUF ships a bare ChatML
  template that silently drops tool definitions, so its tool surface is unusable
  without it. `[models.hermes]` supplies it automatically. It renders a stored
  tool call's object `arguments` with `| tojson`, matching the other templates.
- `src/context.rs` - the only model-call boundary. It assembles a request from
  stored static prompt text and persisted agent messages, records the bounded
  input recipe before execution, then completes that same inference ID with
  either output or the full failure. The completion carries its `sequence_id`
  so every message, deferred compaction and capacity recovery it produces links
  back to the same triggering event. Its recipe identifies the inference record so a
  compaction result can refer to the exact request that produced it. A
  structured fixed-KV rejection is recovered here by persisting and waiting for
  a compaction job before retrying the same recipe. The retry must report fewer
  required tokens. CUDA out-of-memory remains an ordinary model error because
  it does not establish that compaction can reduce the failed allocation.
- `src/agent.rs` - resolves one recorded chat turn: persists each assistant
  response, runs calls from the invocation-local tool list, persists their
  results, and repeats until its final non-tool completion. A final reply and any due compaction
  job are committed together. `complete_with_call_context` explicitly finishes
  its invocation on success/error. Its InvocationGuard Drop only debug-asserts
  that finish was called; it does not persist cleanup on cancellation.
  `CallContext.parent_tool_execution_id` records the exact call causing nesting.
  `create_tool_executions` inserts the complete batch before execution. Each
  call's execution, result persistence and CompletedAfter delivery run inside
  one awaited block per call, so a failure anywhere in that block - not only
  execution - is caught by the same outer match. A `committed` flag
  distinguishes the two failure shapes: before a result commits, the call
  itself is recorded failed with full context; once committed, the result and
  any completed mutation stand untouched and only the delivery failure
  surfaces as the batch error. Either way the batch aborts and every
  never-entered sibling is marked unexecuted by one `mark_unexecuted_tool_executions`
  call after the loop, never a committed or entered call. `complete_with_call_context`
  now inlines the whole per-invocation loop (previously a separate
  `run_invocation` with a `#[allow(clippy::too_many_arguments)]`); that
  function and the allow are gone.
- `src/inference.rs` - admits foreground work ahead of deferred compaction,
  capped by `max_concurrent_inferences`; mistral.rs batches admitted sequences.
  At execution-host startup it marks unfinished inferences interrupted, without
  inventing usage or duration, then resumes pending jobs and keeps same-agent
  history ordered. Opening a database for replay does not interrupt live work.
  `InferenceScheduler`'s admission-tracking state (`deferred_branches`,
  `wait_for_branch`, `admit_deferred`) is keyed by branch_id, not agent_id: a
  world_gm agent has one branch per group (design.md's `GroupHistory`), so
  agent_id alone cannot resolve which compaction job is pending. The `Backend`
  trait's `before_agent`/`after_agent` take an explicit `branch_id:
  Option<i64>`, `None` for every non-GM agent kind.
- `src/compaction.rs` - resolves a persisted compaction job as ordinary
  recorded summarisation, preserving exactly `keep_tail_messages` newest raw
  rows. The compaction inference and summary carry the job's `sequence_id`, so
  a summary links back to the reply that queued it. The input
  is the previous summary and that raw range, closed by the compaction
  instruction as a user-role message. If that request is rejected for capacity,
  it retains one more raw row and tries the real summary request again. If no
  older row remains on a threshold pass, it retires the no-op job; if recovering
  capacity, it fails. It never predicts token use or retries unchanged history.
  `run()` passes the job's own `branch_id` into `start_invocation`
  explicitly rather than falling back to `branch_for_agent(job.agent_id)`,
  which fails for a world_gm agent (no single `agent_history` row).
  `summary_segments`, `Store::latest_summary_for_branch` and
  `Store::tail_split_for_branch` take that same explicit branch_id; the
  agent_id-keyed `pending_compaction`/`latest_summary` remain only as thin
  wrappers (`latest_summary` is `#[cfg(test)]`-only) over
  `branch_for_agent` for the ordinary one-agent-one-branch case.
  `Store::pending_compactions()` (the unfiltered listing used by `resume()`)
  and `Store::finish_chat_inference`'s enqueue both resolve a branch's
  world_gm agent_id via a `LEFT JOIN "group" ... LEFT JOIN world_gm`, since
  `agent_history` has no row for it; the enqueue's INSERT carries
  `ON CONFLICT(branch_id) DO NOTHING`, matching `enqueue_capacity_compaction`.
- `src/tools.rs` - the ordinary invocation-local lookup used to derive
  `ToolDefinition`s and run the matching Rust callback. Each callback receives
  a `ToolCallContext` with the arguments and the already-created
  `tool_execution_id` for this exact call (not a raw inference id/call-id
  pair) - the model call's own id/name/arguments resolve back to the
  producing inference's output when needed (design.md: "Its model call
  ID/name/arguments resolve from the original output"), never duplicated
  onto the callback context. Game tools are added only with their related
  authenticated game state, never to the sandbox REPL.

## Persistence and recording (design.md: Persistence and recording)

- `src/store.rs` and `migrations/` - SQLite store, WAL mode,
  identity-only sandbox worlds and chat-history agents; OAuth email identities
  with non-unique display names; durable world-owner, membership,
  character-owned player-agent, NPC-agent, and group relationships;
  world/character/location/connection/item/item-type/spell-type data with
  separate GM and Storyteller JSON notes. Blank player characters are named
  `AdventurerN` from their globally unique `charN` suffix. `world_gm(world_id,
  agent_id)` is one GM agent per world; `"group"(id, world_id, name,
  location_id, description, branch_id, gm_notes, storyteller_notes,
  retired_at)` owns one GM history branch each, and `character_group`
  replaces the removed `character_location` - every active character has
  exactly one group. A group's fresh scene packet includes every player
  character currently in it with its sheet, and its role prompt tells the
  world_gm to narrate to the characters as a group; an arbitration request
  names the acting character. `install_scenario` creates the entire initial
  relationship graph - including the world_gm agent and every scenario-declared
  group and its branch - in one transaction, and `export_scenario` exports
  only reusable scenario data, never a player's history or membership. The
  active-character lookup verifies the account, membership, world, and
  requested character together before any play access. `player_location_id`,
  `player_group_history`, and `player_sheet` resolve location, the group's
  `GroupHistory { agent_id, branch_id }`, and sheet through that character
  relationship. `pending_action.gm_agent_id`/`gm_branch_id` record the acting
  character's resolved group GM identity at submission time, used unchanged
  through arbitration and resolution. Until real user databases exist, the
  checked-in schema is rebuilt in place and existing local databases are
  intentionally replaced; later schema evolution requires forward migrations.

- Ordered raw history (design.md: Chat schema; plan slice B). Every chat agent
  (player, NPC, Storyteller) owns one `history_branch` at construction
  (`Store::create_agent`'s single choke point, and `install_scenario`'s direct
  NPC/player-agent inserts, all through the shared `create_agent_history`
  helper) plus one `agent_history` row naming its current writable branch. A
  world_gm agent is the exception: it plays several groups, so it has no
  single `agent_history` row - each group's `history_branch` is created
  directly and owned by `"group".branch_id` instead (design.md's
  `GroupHistory { agent_id, branch_id }`: "agent ID alone cannot select a GM
  history"). `AgentBranch` proves the selected world, agent and branch once;
  `agent::Turn`/`context::segments`/`store::live_segments` carry that proof,
  so a GM call cannot fall back to an unrelated agent-history branch.
  `store::history::branch_owner`
  resolves a branch's owning agent/world directly from
  `history_branch.agent_id JOIN agent` - `spawn_group_branch` always binds a
  world_gm agent's own `agent_id` to every group branch it creates, so this
  needs no fallback through `agent_history`/`group`, unlike before D4.
  `history_entry(branch_id, position, kind)` is the
  one ordered index every message, inference start, and summary activation
  appends to (`src/store/history.rs`'s `allocate_position`/
  `append_history_entry`, using `UPDATE history_branch SET next_position =
  next_position + 1 ... RETURNING` in the same transaction as the row it
  backs). `message.id` is its own history_entry id;
  `inference.history_entry_id` and `summary.history_entry_id` are separate
  unique columns, since an inference's start and completion are the same row
  while a summary's activation is a distinct entry from the message range it
  covers (`summary.covered_ranges`, JSON `[{branch_id, first_position,
  last_position}, ...]` - always length 1 until slice D's branch splits/merges
  exist). `history_parent(branch_id, agent_id, ordinal, parent_branch_id,
  through_position)` is the bounded-parent list `split_group`/`merge_groups`
  (D4, `Store::spawn_group_branch`) populate: a split's resulting branches
  each get one parent (the source), a merge's resulting branch gets one
  parent per source group. Composite foreign keys to
  `history_branch(id, agent_id)` make both branches share one agent (and so
  one world), and `CHECK (parent_branch_id < branch_id)` makes cycles
  unrepresentable, so readers never check either.
  `store::history::BranchHead` is a `HistoryHead` already proven to belong to
  its world/agent - private fields, constructed only by
  `Store::branch_head_for_agent` (an agent's own current head, validated
  against its caller-claimed world), `branch_head_for_branch` (an explicit
  branch, validated against a caller-claimed world/agent - the world_gm group
  case) or `branch_head_from_cursor` (a browser-supplied cursor, validated
  the same way). Holding one proves the check passed, so
  `reach(head)` is one recursive query
  (`WITH RECURSIVE reach(branch_id, cut) AS (...)`) collapsing every path
  from `head` to a branch into that branch's single furthest-reaching cut
  (`GROUP BY branch_id, MAX(cut)`), in ancestor-first order for free via
  `ORDER BY branch_id` (the `CHECK (parent_branch_id < branch_id)` constraint
  guarantees this ordering) - a shared ancestor is read once regardless of
  how many paths reach it, replacing the old per-path recursion and its
  linear-scan deduplication. `history_range(head: BranchHead)` performs no
  validation of its own (holding a `BranchHead` already proves it), loads
  every entry up to each reached branch's bounded cut regardless of kind, and
  is now `#[cfg(test)]`-only: no production caller needs this unfiltered a
  load, since `history_live_segments` and `raw_page` (below) each select
  only what they need directly - it exists to test `reach`'s bounding/dedup
  behavior precisely, which neither of those two (each with its own
  additional filtering) can exercise as exactly.
  `store::history::Store::raw_page(head: BranchHead, after: Option<(branch_id,
  position)>, limit)` is `agent_raw_history`'s keyset-paged raw selector: one
  query joins `reach` against `history_entry`, retaining every inference
  entry so raw history can link each recorded model call (including tool-only
  calls) to its exact inference view, and resumes from `after` via the
  standard `(branch_id > ?) OR (branch_id = ? AND position >= ?)` keyset
  condition rather than reloading and re-slicing every prior page.
  A direct history link names both its branch and entry; `raw_cursor_at`
  validates that entry against the same branch and fixed cut before paging
  from it, so an old/foreign entry cannot be treated as a valid snapshot.
  Each branch section keeps that cursor independent; its intersection sentinel
  invokes the same next-page request as the accessible Load more control.
  `LIMIT limit + 1` reveals whether another page follows. When `after` is
  given, its position is first checked against `head`'s own reach and
  rejected (`ensure!`, not silently treated as end-of-history) if it names a
  branch outside the reach or a position past that branch's bounded cut.
  `store::history::Store::history_live_segments(head: BranchHead)` is the
  other, separate consumer of `reach` and does not call `history_range` at
  all, so it is no longer on the unbounded path: per reached branch it runs
  one query for that branch's newest summary activation within its bounded
  cut (`history_entry JOIN summary`, `MAX` position), then decodes only that
  summary's `covered_ranges` (bounded by branch count, not entry count) to
  find the actual highest still-covered position **on that branch** (distinct
  from the activation's own position in the unified index, which can sit
  after messages the summary does not cover - a branch's own summaries are
  already monotonic, so cross-branch overlap cannot occur; `PositionRange` is
  scoped to one branch_id), then one more query selects only the uncovered
  `kind = 'message'` position range past that. No covered or non-message
  entry is ever loaded. After a merge, this means each parent branch
  contributes its own summary + uncovered tail independently, rather than
  only the single newest summary across the whole merged DAG silently
  absorbing every other parent's raw messages uncompacted
  (plans/developer-inspection.md's D5 "longest-disjoint-summary selector").
  `Store::live_segments(agent_id, branch_id)` is the thin production entry
  point every real inference calls: it resolves the caller's
  `agent_id`/`branch_id` to a validated `BranchHead` and delegates here.
  `Store::tail_split` and `enqueue_capacity_compaction`/`abandon_pending_compaction`
  work in the same position space. `pending_compaction` is keyed by
  `branch_id` (still 1:1 with an agent in this slice) with a plain `position`
  cut column.
  `Store::check_message_range_ownership(agent_id, segments)` validates every
  `Segment::Messages` range's branch in one batched query
  (`history_branch WHERE id IN (...)` via `sqlx::QueryBuilder`, since a
  merge's recipe can carry several distinct branches) before
  `request_for_segments` assembles anything, instead of one `branch_owner`
  lookup per range as the loop reaches each one.
- `Store::finish_chat_inference` and `finish_summary_inference` are the two
  single-transaction completion operations design.md's Chat schema
  describes (`finish_inference` stays the plain outcome-only op, used only by
  detached replay/`cairnworld replay`). `finish_chat_inference` completes the
  inference row, then reads/deserializes that output and inserts a duplicate
  content row for its own assistant message. Tool results instead go through
  `complete_tool_result`. It enqueues compaction only when the
  outcome settled with `Content::Text` and its exact next-context size
  (input+output, no re-tokenizing) reaches the caller's `CompactionPolicy`.
  `finish_summary_inference` completes the inference row and, only on
  success, writes the summary activation and retires the
  `pending_compaction` job atomically; a capacity-rejected attempt still
  records its failed inference in full but leaves the job untouched so
  `compaction::run`'s retry loop can resubmit a smaller recipe. `context.rs`'s
  `run_recipe` is the shared "assemble, start, execute" step under both;
  `complete_recipe`/`complete_summary_recipe` are its two finishing wrappers,
  and `RunOutcome.result` keeps the original `anyhow::Error` (not a
  re-wrapped string) so a caller can still `downcast_ref::<ContextCapacityExceeded>`
  through it.
- `Store::agent_raw_history` resolves `agent_id`/`branch_id`/`after` to a
  validated `BranchHead`, pages it through `raw_page` (above), then resolves
  each returned entry's `message`/`summary`/failed-`inference`
  payload and returns an opaque `RawHistoryCursor{head, next_branch_id,
  next_position}`, so a page boundary landing exactly on a failure or summary
  is neither skipped nor duplicated in the existing paging tests.
  A failed inference resolves its own real `inference.id` (not the
  `history_entry.id` primary key its row shares) as the ID a browser
  inference-view link must name.
  `web.rs`'s `RawHistoryPage` query param carries the cursor JSON-encoded
  plus an optional `branch` (D6) used only when `after` is absent, to page a
  specific one of a world_gm agent's several branches rather than the
  `current_head(agent_id)` a player/NPC/Storyteller agent still resolves by
  default. `Store::gm_branch_graph(viewer_id, world_id, agent_id)` (D6)
  returns every branch that agent owns (`BranchNode{branch_id, parents:
  Vec<BranchParent{branch_id, through_position}>, head:
  Option<BranchHead{group_id, group_name}>}`) - unpaged, since branch count
  only grows with split/merge events. `ui.rs`'s `AgentHistoryView` fetches
  that graph first, then every returned branch's own first history page in
  parallel, combining them into `InspectData::AgentHistory{agent_id,
  branches: Vec<BranchSection>}` once all have arrived; a plain
  single-branch agent (`branches.len() == 1`) renders with no heading,
  identical to the pre-D6 view, while a real DAG renders each branch as its
  own `BranchSectionView` (its own independent "Load more" state) labelled
  by its current group head or "retired", with a "continues from branch ...
  @position" note naming its parents - a shared ancestor branch is one
  `BranchSection`, never duplicated across the children that continue from
  it.
- `src/store/content.rs` - `recorded_content.body` serializes one event's
  whole `MessageContent` and reasoning together (`ContentBody`), written
  fresh on every `content_in` call with **no uniqueness constraint**: two
  independently produced events with equal text get distinct rows (design.md's
  Chat schema; verified by
  `repeated_equal_text_stays_distinct_as_events`). `text` now interns only
  immutable prompt/schema/formatter artifacts (role prompts, tool-definition
  JSON, the compaction prompt) - ordinary conversational content no longer
  routes through it. `message.content` and `tool_execution.result_content_id`
  are bare recorded_content IDs. No
  producer needs a typed sub-selection of a content row, since reasoning is
  always read alongside its own content (never referenced independently by a
  different history entry) and GM narration is the GM's own settled text
  reply, not a dedicated narration tool call - design.md's Chat schema and
  Agent syntax and tool calls sections were corrected to match (an earlier
  draft paragraph describing a `ContentRef`/typed-selection type for a
  hypothetical narration tool was speculative and never had a real producer;
  removed rather than kept unused, per coding_standards.md). `insert_message`
  and the new `insert_message_with_content` (store.rs) are the two ways a
  message row is written - the latter takes an already-resolved content id
  directly rather than re-interning, used by `append_group_narration` so
  every present character's delivered copy shares the GM inference output row
  and carries that output's `source_inference_id` back to its producing Turn
  (verified by `narration_delivered_to_several_recipients_shares_one_content_row`
  and `player_take_is_approved_by_its_location_gm_and_transferred_by_rust`).
  `branch_for_agent_tx`/`recorded_content_tx` are transaction-scoped
  counterparts to `branch_for_agent`/`recorded_content`, required because the
  store's SQLite pool is one connection: any `&self.pool` query issued while
  a transaction is already open on that same connection deadlocks waiting for
  a connection the open transaction holds.
- Message, action and notice provenance. `message.source_inference_id` and
  `source_tool_execution_id` name the inference and, for a tool result, the
  `tool_execution` row that produced an entry; delivered narration carries
  neither. Player input carries `trigger_sequence_id` instead - the sequence
  it began, named directly since no inference exists yet at that point to
  resolve it through. `action` references `requesting_tool_execution_id` (the
  player call, nullable only for the server-driven test path with no
  inference) and `approving_tool_execution_id` (the GM call that arbitrated
  it, when one did); its `tool`/`args`/`result` text columns are gone -
  those resolve transitively through the requesting call's producing
  inference output (design.md: "Its model call ID/name/arguments resolve from
  the original output") and `result_content_id`. `action.approved` records
  whether this resolution actually ran the state mutation.
- GM arbitration (`src/game.rs::arbitrate`) resolves through
  `resolve_pending_action`'s transaction as the sole authority (design.md:
  "They cannot be approved twice"); no in-memory flag mirrors that state. Its
  approve/reject tool closures compare the decision they requested against
  the decision `resolve_pending_action` actually returns - a match reports
  that recorded outcome, a conflicting decision becomes an ordinary tool
  rejection describing the existing resolution.
  `action.pending_action_id` (not a foreign key - the source `pending_action`
  row is deleted on resolve) lets that idempotent-retry lookup find the
  already-resolved row. `pending_compaction.sequence_id` and
  `summary.inference_id` complete the compaction trail. The
  `sequence_inference_ids`, `inference_message_ids` and `inference_actions`
  read helpers walk these links for Developer Mode inspection.
- Execution ancestry (design.md's Durable execution relationships).
  `sequence(started_at, finished_at, error)` finishes on the root,
  player-triggered invocation's return - `game.rs`'s `resolve_player_message`/
  `resolve_enter` capture their entire body (not only
  `agent::complete_with_call_context`'s own result) as one `Result` so a
  setup failure before the agent ever runs still finishes the sequence,
  rather than leaving `finished_at` permanently NULL. `invocation(sequence_id,
  agent_id, branch_id, parent_tool_execution_id, cause_inference_id,
  next_inference_ordinal, finished_at, outcome)` is one agent loop; root
  invocations have `parent_tool_execution_id` NULL, deferred compaction's own
  invocation has `cause_inference_id` set instead and does not extend the
  triggering sequence's wall time. `inference.invocation_id`/`ordinal` order
  successive inferences within an invocation - `ordinal` is allocated via
  `UPDATE invocation SET next_inference_ordinal = next_inference_ordinal + 1
  ... RETURNING next_inference_ordinal - 1` (one point update, matching
  `allocate_position`'s existing idiom) rather than a `COUNT(*)` rescan of
  every prior inference. `tool_execution(inference_id, ordinal,
  result_content_id, status, error)` uniquely identifies each returned call;
  status is running/completed/rejected/failed/unexecuted. Every write that
  finishes one of these lifecycle rows treats a failure to finish as
  secondary to whatever error it was recording - `InvocationGuard::finish`,
  `game.rs`'s two `finish_sequence` call sites, `compaction.rs`'s
  `finish_invocation` call, and `main.rs`'s replay path all log and continue
  rather than let a finish-time error (e.g. an unexpected double-finish race)
  replace the real cause of the turn's own failure.
- Developer Mode inspection. `world.developer_mode` is an owner-controlled
  boolean toggle set by `set_developer_mode`; recording runs regardless.
  `inspecting(viewer_id, world_id)` is the one guard every inspection query
  calls first - it requires an active membership and the toggle on, so a
  disabled world or a removed member is rejected before any data query. The
  world-scoped queries `world_sequences` (cursor-paged), `sequence_tree`,
  `inference_evidence`, `world_agents`, `agent_raw_history` (cursor-paged over
  the ordered history index - see above) and `world_objects` (world,
  character, location, item and path families with their notes - current
  state, never a `Segment`) each reject a record from another world by name.
- `src/store/prompt.rs` - model prompt formatting evidence. Mistral's normal
  tokenization path supplies the rendered prompt, token IDs and original template
  inputs. Its awaited observer retains one shared template artifact plus the
  call's original clock, output limit and independent text/token checksums before
  generation. It stores no assembled prompt or token-array copy. Reconstruction
  uses the bounded request and retained template/time and rejects a text checksum
  mismatch. `RecordedInference` carries the corresponding `PromptEvidenceView`
  (formatter artifact ID, clock, checksums and output limit), so the hydrated
  pane exposes the exact evidence it used without a second query. The GPU
  fidelity test drives a normal `Game` opening and compares its reconstructed
  request/prompt and retained token checksum with values observed at the model
  boundary. The ignored real-model browser check drives the normal creation
  tool round trip, then opens that exact saved tool inference through the
  production play page, chat socket and inspection route, comparing its raw
  request, prompt and evidence with those independently observed values. It
  then changes the current world notes and proves the saved inference still
  reconstructs identically before reopening it in the browser. The
  regular browser fixture replays a reviewed response round captured from the
  configured local model through the same Game-recorded tool/reply/
  deferred-compaction sequence and compares its raw request with the backend
  capture; its separately seeded history is only for live websocket events.
  `inference.model` already
  records the configured model name that served each call; no separate
  model/tokenizer identity or digest tracking is retained. This follows
  design.md's Exact model input and verification decision: the retained
  formatter artifact and boundary evidence reconstruct with the current runtime;
  historical formatter/tokenizer revision dispatch is deliberately out of scope.
- `src/inspection.rs` - the plain data types Developer Mode's server and
  browser both use: model request/response (`Request`, `Response`, `Message`,
  etc., moved from `src/llm.rs`, which re-exports them) and the inspection
  view types (`RecordedInference`, `SequenceNode`, `RawHistoryEntry`,
  `BranchNode`/`BranchParent`/`BranchHead`, `WorldObjects`/`GameObject`/
  `ConnectionObject`, etc., moved from `src/store.rs`, which re-exports them).
  Exposed unconditionally through `lib.rs` (`pub mod inspection`) with no
  sqlx/tokio/mistralrs dependency, so the `hydrate` (browser/wasm) build can
  `use` these types directly instead of hand-mirroring a second, narrower copy
  that could silently drop a field - `src/store.rs` and `src/web.rs` remain
  the only code that builds them from the database or serves them over HTTP.
  `MessageOrigin` records a game-authored **Instruction** separately from a
  conversational message while retaining the model-facing `Role::User` where
  that is the correct chat-template role. Request reconstruction preserves
  `Message::origin`, and both raw history and inference input label instructions
  as such. Input verification hashes exclude this game-side metadata, retaining
  compatibility with existing saved inferences. `Store::player_chat_after` is
  the shared visibility query for live delivery, page snapshots and reconnects;
  it excludes instructions. Both delivery paths use `player_chat_entries` for
  the browser entry mapping. `Game::resolve_enter` stores its
  one-time player-entry instruction before the opening inference, so later
  player turns and compaction process it as ordinary history; only the current
  player system prompt remains input-only. `RecordedInference` carries the
  existing compaction `cause_inference_id` so the browser does not infer that
  meaning from text.
  Opening GM narration, presence narration and action arbitration also record
  their game-authored requests with `append_instruction`.
  `PlayerChat` restores an inspect URL after hydrating the server's closed pane
  layout, so reloading an inference link opens its content visibly.

## Dev CLI (design.md: Dev CLI: chat, replay)

- GPU model loading calls mistral.rs's `validate_cuda_prompt_capacity` before
  publishing the game model as ready. It estimates attention workspace for the
  configured context at full scheduler concurrency, allows another workspace
  for allocator retention/in-flight buffers, adds the existing CUDA maintenance
  safety margin and accounts for reusable pool memory. Insufficient available
  capacity fails loading with required/available byte counts. This is a startup
  capacity check, not a physical reservation or a guarantee against other
  processes consuming memory later. It implements the bounded-allocation launch
  check in the Memory Usage declaration.

- `src/main.rs` - `cairnworld chat [--model <name|path>] [--temperature <f32>]
  [--enable-thinking <bool>] [--system <text>] [--database <path>]
  [--chat-template <path>]` creates a sandbox world and agent, then resolves
  each turn through the agent loop. `--temperature` and `--enable-thinking`
  override the resolved model's configured sampling; the other knobs come only
  from configuration. `chat`, `replay`, and `serve` require the selected GGUF to
  fit entirely on the GPU; `--allow-cpu` explicitly permits a CPU/GPU split.
  `serve [--model <name|path>]` shares the same selection path. `cairnworld
  replay [--model <name|path>] [--database <path>]
  <inference-id>` reconstructs and validates the recorded recipe, displays its
  response or error and verified model prompt when available, and records the replay by calling the same context
  boundary. `cairnworld import-scenario --owner-email <email> --owner-name
  <name> [--database <path>] <scenario.json>` initializes a fresh scenario
  world without loading model settings; `export-scenario [--database <path>]
  <world-id> <output.json>` writes its reusable scenario template. Agent roles
  come from their game relationships, not a `kind` switch.
- `src/scenario.rs` and `scenarios/bread_thief.json` - strict scenario JSON
  loader/validator and the checked-in Bread Thief setup: a shared hut location,
  Mara, Toma, flour and cache items, typed item data, and pre-written notes.
- `src/settings.rs` - `[models.<name>]` entries pair a GGUF path with the chat
  template that file needs, so `--model hermes` carries its template
  automatically. `--model` also accepts a path directly, and `--chat-template`
  overrides whatever the entry specifies. A common `[sampling]` block sets
  `temperature`, `top_p`, `top_k`, `min_p`, `presence_penalty`, and
  `enable_thinking`; `[models.<name>.sampling]` overrides any of those field by
  field, and `Settings::sampling` resolves the pair for a model.
  The interactive editor supports normal terminal history/editing, shows when
  a model is active, and renders recorded tool activity, compaction summaries,
  and notices after each turn.

## Configuration

- `build.rs` - on Windows ARM64 SSR builds, places the installed toolkit's
  `CUDA_PATH/lib/arm64` first in the final package's native-library search.
  This prevents transitive CUDA bindings' x64 defaults from selecting the wrong
  import libraries.
- `src/settings.rs` - `Settings`, loaded via the `config` crate (toml
  feature only) layering `default.toml` (checked in) under `local.toml`
  (gitignored, per-machine overrides), then runtime `CAIRNWORLD__` environment
  variables with `__` separating nested fields. Development OAuth values are
  exported from Git-excluded `secrets/dev.env`; tests use explicit sources and
  do not read machine-local configuration. Holds the `[models.<name>]` entries
  described above, `model` naming the default among them, and `limits`,
  including `max_concurrent_inferences`, `max_context_tokens`,
  `max_output_tokens`, `compact_before_next_input_tokens`, and
  `keep_tail_messages`. The fixed paged KV-cache reservation is
  `max_context_tokens` per concurrently admitted inference. A completed turn
  queues compaction only when its measured input plus output would make the
  next input reach the lazy threshold; that agent's next inference waits for
  it. The real-model compaction test checks that an enduring character goal
  survives summarization across ordinary player turns. This normal path and capacity recovery perform no tokenization. Model startup logs the configured token pool, CUDA memory before and
  after allocation, and their allocation delta; every inference logs its token capacity and actual
  input/output token use for the developer inference view. Temporary CUDA
  workspaces grow within configured context/concurrency bounds and are included
  in the startup capacity estimate alongside allocator and safety headroom;
  fixed KV capacity is only one component of that budget.
  Windows integrated-GPU availability comes from `cuMemGetInfo`, which includes
  dedicated and shared CUDA memory, rather than an estimate from free CPU RAM.
- Weights are not checked in; `models/` is gitignored.
- Cargo's development profile keeps Cairnworld debuggable while compiling its
  dependencies optimized without debug information. This keeps unchanged local
  runs fast and avoids duplicating dependency symbols in every test binary.
- `store::TestDatabase` gives each test a uniquely named on-disk SQLite
  database (a process-wide counter, not a timestamp, since `cargo test` is
  multi-threaded) and removes its `.sqlite`/`-wal`/`-shm` files on drop.

## Web play (design.md: Web server and UI; Auth)

- `src/game.rs` - the membership-scoped game service used by browser events.
  It serializes each world's events, opens a blank Adventurer's player agent
  proactively. Each GM opening, presence, or arbitration trigger is appended
  to its validated group branch before inference, carrying the sequence that
  began it; the stored event is therefore the newest GM history message and is
  visible in Developer Mode. The GM role prompt is one fixed text, while its
  rebuilt scene packet is a separate `Segment::Context` folded into the leading
  system message and never becomes history. The player agent's initial entry
  remains temporary because player chat correctly renders stored user messages
  as player speech. The ordinary agent loop executes any creation-roll
  calls and stores its final completion, including a deliberate empty text
  completion. Empty player-agent completions end the turn without a chat
  entry; Developer Mode names them "No player-facing response." in turns,
  inference detail and raw history. `agent::Settled` retains whether tools
  ran and the final inference ID, so an empty first reply logs durable
  sequence/inference context as anomalous without blocking a player re-prompt.
  The first viewer starts one server-owned
  opening operation per character; concurrent reconnects wait on that same
  operation, then view its durable result rather than queueing game work.
  Completing character creation asks the character's resolved group's world_gm
  (its `GroupHistory { agent_id, branch_id }`) for opening narration in that
  same tool turn, so beginning play never needs a second player message.
  Subsequent player messages expose creation or location-action tools
  according to durable character state.
- Presence (Developer Mode plan D2; design.md's Membership and presence).
  `Game::new` loads all stored worlds before returning. Each `World` owns its
  worker cancellation handle and shared broadcasts, opening turns and presence.
  Web scenario installation is serialized with `GameLoad::finish`, so worlds
  created during model loading or after readiness get their runtime before play.
  Dropping the world aborts its worker; an idle worker holds only a weak game
  reference. `Game::connect`/`disconnect` hold the world's presence lock across
  each durable zero/one transition and update the connection set only after a
  successful write. Changed characters are coalesced in a set and signalled by
  `Notify`, independently of the bounded action queue. The worker drains changes
  and due deadlines between actions; it owns pending timers instead of spawning
  detached timer tasks. Zero-delay deadlines are processed before later actions.
  Event errors are logged with world/character or group identity even when a
  requesting connection has gone. Arrival into an occupied group narrates
  immediately; departures and empty-period arrivals share a deadline. A deadline
  with an unchanged online set clears the transition without inference.
  `handle_presence_deadline` validates the generation before acting. Presence
  narration uses an ordinary recorded GM turn and delivers one content reference
  to connected active PCs in the group. Only creation-ready PCs participate.
  `party_presence()` returns group membership and current connection presence.
  Process-restart reconciliation of stale durable presence remains planned;
  constructing an empty connection set does not implement that recovery policy.
- Group-scoped narration. `WorldState.broadcasts`, `Game::subscribe` and
  `deliver_narration` key live delivery by group. `Store::append_group_narration`
  selects connected active PCs in that same group for durable history, so
  sharing a physical location cannot leak narration on reload. After a topology
  tool batch, the GM's settled reply is delivered to the triggering character's
  resulting group. The isolation test performs real split/action/GM tool rounds
  and checks both live delivery and reloaded histories; it no longer inserts
  group state or sends test narration directly. Presence tests observe an
  event-queue completion barrier instead of fixed sleeps, with explicit deadline
  delivery for departure-window cases. Regression tests also cover more presence
  transitions than a full action queue can hold during inference, failed presence
  writes, and release of idle world ownership. `gm_scene` includes group-local
  description and notes. Existing browser sockets do not yet resubscribe when
  their character's group changes.
- `split_group`/`merge_groups` (Developer Mode plan D4). Two GM tool calls,
  never applied at the point they execute (design.md's "Tool boundary"):
  `agent::complete_with_call_context`'s batch loop collects
  `tools::ToolOutcome::PendingTopologyChange` outcomes in call order and
  applies them all together, once, only after every ordinary call in the
  batch has settled with no error - so every sibling call still to come in
  the same batch executes against the pre-transition group, the context the
  model actually generated them against. `Store::apply_topology_changes`
  validates each proposal against state as it stands at that point (which
  may already reflect an earlier proposal in the same batch) and appends its
  tool-result message in call order alongside the batch's ordinary results;
  a stale or invalid proposal is rejected rather than applied.
  `Store::apply_split` requires a complete, disjoint partition of the
  group's exact current membership and (for a GM event with no single
  triggering character) an explicit `continue_with_character_id`.
  `Store::apply_merge` requires at least two distinct, non-retired groups in
  the same world and location. Both call `Store::spawn_group_branch`, which
  creates one new `history_branch`, one `history_parent` row per listed
  `(parent_branch_id, through_position)` pair, and the new `"group"` row;
  split spawns one branch per partition (one parent each, the source),
  merge spawns one branch with one parent per source group. The source
  group(s) are retired (`"group".retired_at`) and their members'
  `character_group` rows move to the new group(s). Before a merge's
  transaction starts, `agent::compact_merge_sources` compacts each source
  branch whose own unique suffix (`Store::uncovered_message_count`) exceeds
  `limits.merge_branch_messages`, so `apply_merge`'s own fresh
  `next_position - 1` read of the source branch's settled position already
  includes the new summary/notice - no special casing needed in
  `apply_merge` itself. It enqueues via the existing branch-explicit
  `enqueue_capacity_compaction`, then calls `backend.before_agent(store,
  agent_id, Some(branch_id))` rather than running compaction directly, so a
  branch with an unrelated already-pending job (e.g. a token-threshold
  trigger) is waited on through the same admission path a concurrently
  running deferred compaction uses, never resolved twice.
- `src/web.rs` - `cairnworld serve`'s Axum routes. Google OIDC discovers and
  exchanges through `openidconnect`; SQLite-backed `tower-sessions` holds the
  verified account id. The landing page updates only a non-unique display
  name; identity remains the verified email. The world detail page already
  creates, limits, lists, revokes, and accepts invitation links, and lets an
  owner remove an active member while retaining its durable association. It
  renders every retained membership with its characters nested below it; an
  active member can create another blank Adventurer, and every entry link names
  its exact character. Landing,
  world, invitation, and membership routes all resolve access through `Store`
  rather than trusting a client user or agent id. `POST
  /world/:id/developer-mode` is the owner-only toggle; the `GET
  /world/:id/inspect/*` routes (sequences, one sequence, one inference, agents,
  one agent's history, objects) each resolve the signed-in viewer and return
  JSON, deferring the active-membership-and-enabled-mode check to the `Store`
  query. Rendering of these lives in a later Developer Mode step. The World
  detail page hydrates (`ui::DeveloperModeEnableButton` island) so the owner's
  enable control can hold the required warning behind a client-side confirm
  step before it submits the real POST; the indicator ("Developer Mode is on")
  is visible to every active member from plain SSR, and disabling is a plain
  form with no warning, since only enabling exposes new data. `router()`
  builds the complete route table from an `App` and session store without
  binding a listener, so `web::tests` drives it directly with real
  `tower::ServiceExt::oneshot` requests against a real `Store` and session
  middleware - `Google` is a real client built from offline
  `ProviderMetadata::new`, never a network call, since these tests never
  exercise the OAuth handlers. `session_cookie` saves a real session through
  the same store the middleware reads, so a forged cookie names a real,
  loadable session exactly as a browser's would. This proves each
  `/inspect/*` route and the developer-mode toggle reject an unauthenticated,
  non-member, removed-member, developer-mode-off, non-owner, or foreign-world
  request at the HTTP boundary, not only in the underlying `Store` query. The
  authenticated
  `/world/:id/characters/:character_id/play` route loads that character's
  durable player-visible history and its final durable message id, while its
  websocket passes the same resolved membership and snapshot cursor into `Game`
  for each player message. After opening work settles, the socket sends every
  later durable entry before enabling submission, so SSR and a delayed socket
  neither lose nor duplicate a reply. The server emits typed
  activity while it is preparing the first response or resolving a player turn;
  the hydrated page renders that state separately from durable chat entries,
  clears it only on server readiness, reports a closed socket separately, and
  renders server errors as chat notices. It logs each attempted error delivery.
  GM narration is stored in every present character's history under the
  `Narration` role and delivered before the player-agent follow-up so the
  guide does not duplicate it.
  It serves the `cargo-leptos` browser package
  at `/pkg` plus checked-in artwork at `/media`. Browser pages use one valid
  document shell with viewport metadata, stylesheet and, only where needed,
  hydration scripts. `.cargo/config.toml` gives Leptos one Cargo-wide WASM
  output name, so a normal `cargo run` hydration script requests the package
  that `cargo-leptos` actually emits.
- `src/lib.rs` exports Leptos's required islands `hydrate()` entrypoint, which
  initializes the island hydrator before the generated loader invokes each
  island. `npm run test:hydration` builds the frontend and explicitly runs
  `web::tests::browser_checks` (excluded from ordinary unit-test runs because it
  requires the built package and Chrome). Rust supplies the production `GamePage`,
  `GameLoading`, page shell, loader and assets, plus a real store/session/router
  for inspection. `scripts/browser_check.cjs` shares browser startup and error
  handling across the loading, player-chat, Developer Mode and world-detail
  scripts. Its `forward()` proxy relays a real incoming request's method,
  body and content-type verbatim when a script gives it no explicit synthetic
  body (rather than always issuing a GET), and uses `redirect: "manual"` to
  relay the real status and `Location` header instead of `fetch()`'s default
  auto-follow - required for `check_world_detail.cjs`, which exercises the
  developer-mode toggle's real `<form method="post">` submission and its real
  HTTP 303 through to the browser, not a fabricated fixture. Checks
  cover ready-page reload, bounded chat layout, focus and scroll, activity,
  transcript reload/reconnect, disconnects, exact inference text/action
  display while retaining the draft and one usable chat socket, and the
  developer-mode warning modal's open/cancel/confirm flow against real
  production routes. Negative cases
  inject browser exceptions, console errors and missing resources and require
  failing subprocess exits. Node scripts only supply controlled chat events and
  fixture history, not handwritten Leptos markup or inspection JSON. These are
  browser integration checks; the model-driven game and production socket-handler
  acceptance tests remain separate. No OAuth or model is needed for this suite.
- `src/ui.rs` and `src/lib.rs` - the shared, serializable browser chat event
  types and the small Leptos island used by the game page. The server projects
  the durable transcript into the island as opaque children; WASM hydrates only
  live input, transport, and subsequent entries. It keeps the input editable
  while a turn runs, disables only submission, and scrolls the chat on live
  additions. Its snapshot cursor lets the server fill the race between SSR and
  the live socket. This keeps SSR history and browser state from competing
  during hydration. This is the sole browser chat implementation; the page contains
  no parallel handwritten JavaScript loop.
  The loading island displays the complete server startup error when available
  rather than hiding it behind an HTTP status.
  `DeveloperModeEnableButton` is a third island: it renders a real
  `<dialog class="modal">` (daisyUI's modal convention), opened via
  `HtmlDialogElement::show_modal()` on click and closed via `.close()` on
  cancel, holding the warning text and the real toggle form's submit button -
  the SSR markup always includes the dialog and its form (so the toggle works
  without JS), just closed by default; there is still no fetch/socket of its
  own, the form is a normal POST to `/world/{id}/developer-mode`.
  `ChatEntry`/`ServerEvent::Entry` carry an optional `id` and `sequence_id`
  for Developer Mode's chat-entry inspect button (design.md: Developer Mode).
  `web.rs::play_connection` tracks a message-id cursor across the whole
  connection and, after a player turn or a narration broadcast fires,
  `send_new_entries` re-fetches everything past that cursor through
  `Game::player_chat_after` (the same query SSR and reconnect use) rather than
  forwarding the bare broadcast text - a live entry is inspectable as soon as
  it is sent, not only after a page reload re-resolves it. The narration
  broadcast channel itself still only carries text; it exists to wake a
  connection, not to name the row. `message.trigger_sequence_id` records the
  sequence a player message began, since no inference exists yet at that point
  to resolve it through; every other entry resolves its sequence via
  `source_inference_id`.
  When Developer Mode is on, `PlayerChat` renders a two-column layout: the
  chat stays on the left, and clicking an inspectable entry (a `cursor-pointer`
  affordance marks these; a delegated click listener on `#chat` reads
  `data-inspect-sequence` from both live and SSR'd entries so the static
  history needs no reactivity of its own) opens `inspect_pane` on the right,
  a client-side `RwSignal<Option<InspectSelection>>` covering all four views
  user_declarations.md's Developer Mode section calls for; switching between
  them only re-fetches and re-renders the pane, never navigating or touching
  the chat's websocket. Its native URL/history adapter reports a listener or
  URL-serialization failure visibly rather than silently leaving navigation
  partially active; Back and Forward restore the existing pane selection
  without inserting replacement history entries.
  Chat-entry turn URLs retain the durable entry id (`entry=`), and the turn
  visibly retains that selected-entry context rather than relying on repeated
  text to identify it.
  - The turn view (`InspectSelection::Sequence`) fetches
    `/world/{id}/inspect/sequences/{id}` and renders every inference in that
    turn's tree as its actor and generated output. A generated tool call
    contains its Rust execution result and any linked GM decision; it is not
    repeated as a separate action/message summary. Child inference links retain
    the invocation nesting.
  - The inference view (`InspectSelection::Inference`) fetches
    `/world/{id}/inspect/inferences/{id}` and renders the same `InferenceCard`
    standalone, plus a link back to its turn - clicking an "inference N" badge
    anywhere (turn view, agent history) opens this, matching "messages in the
    debug view should link to an inference view."
  - The agent chat history view (`InspectSelection::AgentHistory`,
    `AgentHistoryView`/`BranchSectionView`/`RawHistoryEntryRow`) fetches
    `/world/{id}/inspect/agents/{id}/branches` (D6) for the whole branch DAG,
    then every returned branch's own first page from
    `/world/{id}/inspect/agents/{id}/history?branch={branch_id}` in
    parallel, and renders each branch's raw messages with compaction
    summaries and inference failures inlined at the point they actually
    occurred in the ordered history index (`store/history.rs`'s traversal -
    see "Persistence and recording" above). Each section observes its cursor
    sentinel with `IntersectionObserver` to load its next page, while retaining
    an accessible "Load more" button; both use that section's opaque
    `RawHistoryCursor` (JSON-encoded in the `after` query param) and append
    only to that branch's section. A plain player/NPC/Storyteller agent has exactly one
    branch and renders with no heading, identical to the view before D6; a
    world_gm agent whose groups have split/merged renders every branch as
    its own section labelled by its current group head or "retired", with a
    "continues from branch ... @position" note - a shared ancestor branch
    is one section, never duplicated across the children that continue from
    it. A summary entry's heading names every one of its `covered_ranges` as
    `branch {id} #{first}-#{last}` - each range's own branch_id and
    first_position, not only its last_position, since a summary made after a
    merge can cover ranges from more than one branch.
  - The world object/agent navigator (`InspectSelection::Agents`/`Objects`,
    `AgentsList`/`ObjectsView`) is always reachable from the pane header and
    fetches `/world/{id}/inspect/agents` or a stable cursor page from
    `/world/{id}/inspect/objects`. The object index is ordered by typed
    `(type, id)` keys; the server filters name/type/ID searches before paging,
    and returns typed relationship targets (current holder/type, group,
    location, connection, and members) as direct `?inspect=objects&type=…&id=…`
    links. The browser appends each requested page and preserves the active
    search while doing so; selecting a relationship reloads its exact target
    and highlights it.
  `InferenceCard` shows reconstructed input before output, rendering a GM
  `Current scene packet` as labelled scene fields rather than JSON. Its model
  prompt, token usage and verification evidence live in the top-right `…`
  technical popover. Per-node usage and the cumulative current-Turn total are
  omitted when usage was not recorded rather than showing a misleading zero.
  `ContentBlock` renders model/message content without collapsing it to one
  string. Plain text and scene facts are copyable; a tool call leads with its
  name and arguments, while its execution result and linked GM decision are
  nested beneath it in a Turn. Opaque call IDs remain durable relationships and
  are not rendered. The HTTP endpoints still serialize `RecordedInference` for
  developer clients, but the human pane does not render transport JSON or its
  opaque hashes. Copy controls are compact accessible icons beside the exact
  text they copy.
  Step 6's live updates implement design.md's Live inspection protocol in
  full (Developer Inspection plan slice A). `Store` owns a `dev_events:
  Arc<Mutex<HashMap<world_id, broadcast::Sender<DevEvent>>>>` registry
  alongside the existing per-location chat broadcasts, lazily created on
  first subscribe. `Store::subscribe_dev_events` is unauthenticated by
  itself - the live inspection protocol requires subscribing *before* the
  initial access query, so a membership/mode change racing the subscribe can
  never fall in the gap between "checked access" and "started listening".
  `Store::access_for(viewer_id, world_id) -> Access { active_member,
  developer_mode }` is the one indexed query behind every inspection
  authorization decision - both `inspecting` (HTTP routes) and the websocket
  build on it, so membership and mode stay authoritative from one source
  rather than a cache. `inspection::DevEvent` and
  `RawHistoryPageResponse` are the one serialized shapes used by the store,
  route and hydrated browser, rather than mirrored server/browser types.
  `DevEvent` adds `AccessChanged` (a wake-up published
  after `set_developer_mode`/`remove_member` commit, never itself an
  authorization decision), `Changed { sequence_id, inference_id }` (published
  after `finish_inference` and `resolve_pending_action` commit), and
  `ResyncRequired { skipped }`; `Token` now carries `offset` (the emitted
  UTF-8 byte count before this chunk, tracked by
  `mistralrs_backend.rs::complete_with_recording`), letting a receiver that
  missed earlier chunks tell its accumulated text is short.
  `web.rs::inspect_events`/`dev_events_connection` retains `Store`/viewer/
  world identity and re-runs `access_for` before every private outbound
  frame (plain query, then plain `socket.send`, never a transaction or lock
  held across the send): a removed member's connection closes, a disabled
  mode sends `DeveloperModeChanged { enabled: false }` and suppresses
  everything else, and `RecvError::Lagged` rechecks access, sends the
  current mode, and - only if still authorized - `ResyncRequired` instead of
  continuing silently. A query or send failure logs full context and
  terminates the connection. On the client, `connect_dev_events`
  (hydrate-only, one per `PlayerChat` island) owns its `WebSocket` through a
  `DevSocket` handle (`Rc<RefCell<Option<DevSocket>>>`, matching the
  existing player-chat socket's ownership pattern); `on_error`/`on_close`
  report a visible `dev_error` with a `Reconnect` control
  (`dev_reconnect_nonce`) whose click bumps the nonce, closing the previous
  socket and re-running the same subscribe/snapshot handshake. Final
  teardown on navigation/unload is the browser's own, same as the existing
  player-chat socket - `wasm_bindgen::closure::Closure` cannot be held in
  Leptos's `Send + Sync`-bound `on_cleanup` without `unsafe`, which
  coding_standards.md forbids.
  `inspect_pane` tracks a `generation: RwSignal<u64>` bumped on every
  selection change; `fetch_selection` tags its request with the active
  generation and discards a response that arrives after the pane has moved
  on. A `Changed` event matching the open view refetches immediately, or - if
  a fetch for the current generation is already in flight
  (`fetch_in_flight`) - marks `invalidated_during_fetch` so the just-landed
  snapshot is refetched once more rather than trusted (never a refetch loop).
  `live_tokens` marks a byte-offset gap inline (`"…missing…"`) rather than
  silently rendering a short prefix as whole. `InferenceCompleted` refreshes
  either the open turn or a standalone inference view opened on that same
  inference. `ResyncRequired` shows a persistent `resync_notice`, clears
  `live_tokens`, and reloads the current selection. `DeveloperModeChanged {
  enabled: false }` clears all pane state (`data`/`error`/`resync_notice`/
  `live_tokens`) and the selection itself, not just hiding the pane -
  independent of, and without touching, the chat's own websocket.
  Both the "Input" panel (turn/inference views) and the raw history show the
  exact, untruncated `request.messages`/message content, reasoning, and
  post-template `model_prompt` when recorded. `inspection::DevEvent` and
  `inspection::RawHistoryPageResponse` are shared by the server and hydrated
  browser; inspect views render those shared records rather than maintaining
  narrower client-side wire mirrors. All inspect-pane code is
  `#[cfg(feature = "hydrate")]`-only, since it never renders anything
  server-side.
- `style/app.css`, `package.json`, and `package-lock.json` - Tailwind CSS 4
  with daisyUI's maintained component/theme layer. The default `forest` theme
  establishes the dark palette; the shared Leptos markup uses responsive
  layout primitives for the landing, management and chat pages. The chat's
  bounded central workspace leaves the page structure extensible for future
  developer inspection panels without inventing one early. `npm run
  build:frontend` runs the WASM package build followed by the stylesheet build,
  ensuring cargo-leptos's default blank CSS output cannot overwrite
  `/pkg/cairnworld.css`. Hydrated pages explain that live chat requires
  JavaScript when it is disabled.
- `Cargo.toml` - separates server (`ssr`) dependencies from the WASM
  `hydrate` target. `cargo-leptos build` builds both targets and emits the
  package consumed by the game-page hydration script. `disable-erase-components`
  keeps cargo-leptos's debug frontend rendering compatible with ordinary Cargo
  server/test builds; otherwise they disagree on hydration markers.
