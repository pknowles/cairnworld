# Implementation Reference

Index of what exists, mapped to design.md sections. Plans describe remaining
work; their completion claims do not override the source.

## Current alignment boundary

Developer Mode has the A–C recording/query foundations, but their completion is
not verified. Review of `a7191bd` on 2026-09-17 found several defects; the
[required corrections](plans/developer-inspection.md#required-corrections-before-d)
describe each with reproduced cases, implementation changes, alternatives and
acceptance checks. Status as corrections land:

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
- **Still open:** history traversal loads the whole entry index and validates
  only the current head's ownership; raw paging queries every inference
  before slicing the page; foreign parent ownership is not checked. A
  `CompletedAfter` delivery error leaves unreached sibling tools `running`.

The code still uses location-owned linear GM histories; D introduces groups and
their heads, and E–F complete raw fields/evidence and navigation. Planned
behavior is not implied to exist here.

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
  architectures, including Qwen3.5. Clone with
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
  results, and repeats until final text. A final reply and any due compaction
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
- `src/compaction.rs` - resolves a persisted compaction job as ordinary
  recorded summarisation, preserving exactly `keep_tail_messages` newest raw
  rows. The compaction inference and its chat notice carry the job's
  `sequence_id`, so a summary links back to the reply that queued it. The input
  is the previous summary and that raw range, closed by the compaction
  instruction as a user-role message. If that request is rejected for capacity,
  it retains one more raw row and tries the real summary request again. If no
  older row remains, it fails; it never predicts token use or retries unchanged
  history.
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
  character-owned player-agent, NPC-agent, and location-GM
  relationships; world/character/location/path/item/item-type/spell-type data
  with separate GM and Storyteller JSON notes. Blank player characters are
  named `AdventurerN` from their globally unique `charN` suffix, and a location
  GM's fresh scene packet includes every player character currently there with
  its sheet, and its role prompt tells it to narrate to the characters as a
  group; an arbitration request names the acting character. `install_scenario` creates the
  entire initial relationship graph in one transaction and `export_scenario`
  exports only reusable scenario data, never a player's history or membership.
  The active-character lookup verifies the account, membership, world, and
  requested character together before any play access. `player_location_id`,
  `player_location_gm_agent_id`, and `player_sheet` resolve location, GM, and
  sheet through that character relationship. Until real user databases exist, the checked-in schema
  is rebuilt in place and existing local databases are intentionally replaced;
  later schema evolution requires forward migrations.

- Ordered raw history (design.md: Chat schema; plan slice B). Every chat agent
  (player, NPC, GM, Storyteller) owns one `history_branch` at construction
  (`Store::create_agent`'s single choke point, and `install_scenario`'s direct
  location-GM/NPC/player-agent inserts, all through the shared
  `create_agent_history` helper) plus one `agent_history` row naming its
  current writable branch. `history_entry(branch_id, position, kind)` is the
  one ordered index every message, inference start, summary activation and
  notice appends to (`src/store/history.rs`'s `allocate_position`/
  `append_history_entry`, using `UPDATE history_branch SET next_position =
  next_position + 1 ... RETURNING` in the same transaction as the row it
  backs). `message.id` and `chat_notice.id` are their own history_entry id;
  `inference.history_entry_id` and `summary.history_entry_id` are separate
  unique columns, since an inference's start and completion are the same row
  while a summary's activation is a distinct entry from the message range it
  covers (`summary.covered_ranges`, JSON `[{branch_id, first_position,
  last_position}, ...]` - always length 1 until slice D's branch splits/merges
  exist). `history_parent(branch_id, ordinal, parent_branch_id,
  through_position)` is the (currently always-empty) bounded-parent list a
  future branch split/merge will populate.
  `src/store/history.rs::history_range(HistoryHead{branch_id, cut}, world_id,
  agent_id)` is the shared bounded parent-first traversal every raw and live
  selector is built on: it validates the head's world/agent, walks
  `history_parent` recursively with cycle detection (a visited-branch-id set
  threaded through the recursion) and a missing-parent check independent of
  the schema's own foreign key, loads all entries up to each cut, and removes
  duplicates by scanning accumulated entries. It does not yet subtract intervals
  before reading entries or validate each parent's agent/world ownership.
- `Store::live_segments` walks that traversal, selects the newest summary
  activation (if any), resolves its `covered_ranges` to find the actual
  highest still-covered position **on the branch being read** (distinct from
  the activation's own position in the unified index, which can sit after
  messages the summary does not cover), and returns every message entry past
  that as one `Segment::Messages{PositionRange}` - a position range spans the
  branch's whole unified index, so non-message entries (inference starts,
  notices) inside it are expected gaps, not missing rows; only a `kind =
  'message'` entry missing its `message` row is real corruption.
  `Store::tail_split` and `enqueue_capacity_compaction`/`abandon_pending_compaction`
  work in the same position space. `pending_compaction` is keyed by
  `branch_id` (still 1:1 with an agent in this slice) with a plain `position`
  cut column.
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
  success, writes the summary activation, its notice, and retires the
  `pending_compaction` job atomically; a capacity-rejected attempt still
  records its failed inference in full but leaves the job untouched so
  `compaction::run`'s retry loop can resubmit a smaller recipe. `context.rs`'s
  `run_recipe` is the shared "assemble, start, execute" step under both;
  `complete_recipe`/`complete_summary_recipe` are its two finishing wrappers,
  and `RunOutcome.result` keeps the original `anyhow::Error` (not a
  re-wrapped string) so a caller can still `downcast_ref::<ContextCapacityExceeded>`
  through it.
- `Store::agent_raw_history` pages `history_range`'s output directly: it
  resolves every `message`/`chat_notice`/`summary` entry (skipping a
  successful or still-running `inference` entry, which renders nothing of its
  own - only a *failed* inference becomes a `RawHistoryEntry::FailedInference`)
  and returns an opaque `RawHistoryCursor{head, next_branch_id,
  next_position}`, so a page boundary landing exactly on a failure, summary or
  notice is neither skipped nor duplicated in the existing paging tests. It
  queries each inference outcome across the full history before slicing, and
  failed rows currently mislabel their history-entry ID as an inference ID.
  `web.rs`'s `RawHistoryPage` query
  param carries the cursor JSON-encoded; `ui.rs`'s `AgentHistoryView` has a
  "Load more" button that fetches and appends the next page.
- `src/store/content.rs` - `recorded_content.body` serializes one event's
  whole `MessageContent` and reasoning together (`ContentBody`), written
  fresh on every `content_in` call with **no uniqueness constraint**: two
  independently produced events with equal text get distinct rows (design.md's
  Chat schema; verified by
  `repeated_equal_text_stays_distinct_as_events`). `text` now interns only
  immutable prompt/schema/formatter artifacts (role prompts, tool-definition
  JSON, the compaction prompt) - ordinary conversational content no longer
  routes through it. `message.content` and `tool_execution.result_content_id`
  are bare recorded_content IDs; chat_notice instead stores Rust-generated text. No
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
  directly rather than re-interning, used by `append_location_narration` so
  every present character's delivered copy shares the GM inference output row
  (verified by `narration_delivered_to_several_recipients_shares_one_content_row`).
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
- GM arbitration (`src/game.rs::arbitrate`) shares an in-memory `Mutex<bool>`
  between approve/reject tool closures, with claim/release helpers. Store also
  checks the durable resolution in its transaction. The two boundaries differ:
  the local flag rejects any second call, whereas Store accepts matching repeats
  and errors on conflicting decisions. Existing tests cover each behavior, but
  this duplicates authority. The correction plan replaces the local flag with
  a typed Store result and tool-level conflict rejection.
  `action.pending_action_id` (not a foreign key - the source `pending_action`
  row is deleted on resolve) lets that idempotent-retry lookup find the
  already-resolved row. `pending_compaction.sequence_id` and
  `chat_notice.source_inference_id` complete the compaction trail. The
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
  mismatch. The GPU fidelity test compares the reconstructed prompt and token IDs
  with those observed during actual generation. Historical tokenizer retention,
  complete causal navigation and the Developer Mode interface remain in the
  active Developer Mode plan.

## Dev CLI (design.md: Dev CLI: chat, replay)

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

- `src/settings.rs` - `Settings`, loaded via the `config` crate (toml
  feature only) layering `default.toml` (checked in) under `local.toml`
  (gitignored, per-machine overrides). Holds the `[models.<name>]` entries
  described above, `model` naming the default among them, and `limits`,
  including `max_concurrent_inferences`, `max_context_tokens`,
  `max_output_tokens`, `compact_before_next_input_tokens`, and
  `keep_tail_messages`. The fixed paged KV-cache reservation is
  `max_context_tokens` per concurrently admitted inference. A completed turn
  queues compaction only when its measured input plus output would make the
  next input reach the lazy threshold; that agent's next inference waits for
  it. This normal path and capacity recovery perform no tokenization. Model startup logs the configured token pool, CUDA memory before and
  after allocation, and their allocation delta; every inference logs its token capacity and actual
  input/output token use for the developer inference view.
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
  proactively, and gives every agent turn a user-role event message - the
  player's entry, or the request that starts a GM's opening narration or
  arbitration - so tool-capable templates have a valid first user turn without
  displaying or persisting a fake player message. The ordinary agent loop executes any creation-roll
  calls and stores its final text. The first viewer starts one server-owned
  opening operation per character; concurrent reconnects wait on that same
  operation, then view its durable result rather than queueing game work.
  Completing character creation asks the resolved location GM for opening
  narration in that same tool turn, so beginning play never needs a second
  player message. Subsequent player messages expose creation or location-action
  tools according to durable character state.
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
  the chat's websocket.
  - The turn view (`InspectSelection::Sequence`) fetches
    `/world/{id}/inspect/sequences/{id}` and renders every inference in that
    turn's tree (`SequenceNodeCard`, which embeds `InferenceCard`): agent,
    model, parent link, output text/tool calls, reasoning, actions performed,
    and the messages it produced.
  - The inference view (`InspectSelection::Inference`) fetches
    `/world/{id}/inspect/inferences/{id}` and renders the same `InferenceCard`
    standalone, plus a link back to its turn - clicking an "inference N" badge
    anywhere (turn view, agent history) opens this, matching "messages in the
    debug view should link to an inference view."
  - The agent chat history view (`InspectSelection::AgentHistory`,
    `AgentHistoryView`/`RawHistoryEntryRow`) fetches
    `/world/{id}/inspect/agents/{id}/history` and renders the linear raw
    messages an agent's own inferences saw, with compaction summaries and
    inference failures inlined at the point they actually occurred in the
    ordered history index (`store/history.rs`'s traversal - see "Persistence
    and recording" above), plus a "Load more" button that fetches the next
    page via the opaque `RawHistoryCursor` (JSON-encoded in the `after` query
    param) and appends. This is linear for every agent including the
    location GM's - the declared DAG with one head per Group needs Groups,
    which do not exist yet (plans/todo.md).
  - The world object/agent navigator (`InspectSelection::Agents`/`Objects`,
    `AgentsList`/`ObjectsView`) is always reachable from the pane header and
    fetches `/world/{id}/inspect/agents` or `/world/{id}/inspect/objects`,
    presenting every field verbatim rather than a simplified projection
    (no search/filter yet).
  `InferenceCard` shows per-node token usage (prompt/completion/total) and a
  cumulative total folded over every node in the current turn view; both are
  omitted when usage was not recorded rather than showing a misleading zero.
  A `CopyButton` component sits next to every exact-text field worth copying
  verbatim (outcome text, each request message, the post-template model
  prompt) - it writes to the clipboard via `navigator.clipboard.write_text`
  and flips to a checkmark for a moment, skipped entirely when the field is
  empty.
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
  rather than a cache. `DevEvent` adds `AccessChanged` (a wake-up published
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
  post-template `model_prompt` when recorded. `SequenceNodeView`,
  `InferenceSummary`, `RawHistoryEntryView`, `AgentInfoView` and
  `WorldObjectsView` are hand-written mirrors of `store::SequenceNode`,
  `RecordedInference`, `RawHistoryEntry`, `AgentInfo` and `WorldObjects`
  respectively (`serde` ignores fields the pane does not render yet, such as
  `segments`/`tools`/`sampling`). Tool-call IDs and some object relationships
  are also omitted from browser types. The existing library can host shared
  serialization types; the active plan removes unnecessary manual mirroring. All
  of this inspect-pane code is `#[cfg(feature = "hydrate")]`-only, since it
  never renders anything server-side.
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
