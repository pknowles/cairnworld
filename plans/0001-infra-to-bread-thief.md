# 0001 - Infrastructure to Bread Thief

Status: in-progress (2026-07-26) - milestones 1-2 complete, milestones 3-9 not started

## Goal

Build the framework layers of design.md, in small independently-verifiable
milestones, up to a human playing the Bread Thief scenario end to end with
full debug inspection.

## Scope

design.md sections: Inference layer, Persistence and recording, Agent loop,
Sequences, Web server and UI, Chat compaction, Dev CLI, MCP - plus the
minimal slice of the Game layer that Bread Thief needs (per the "initial
proof of concept" simplifications in user_declarations.md: no Storyteller,
hard-coded notes, stat-roll-only character creation).

## Milestones

Ordered so debugging tools exist before the systems that will need them.
Each may get its own detailed plan when it begins.

1. **Inference REPL.** `llm` layer with the mistral.rs backend;
   `cairnworld chat` runs a conversation in the terminal. Verifies: model
   loads, generation quality/speed, sampling params work. Detailed below.
2. **Recording + replay.** `store` layer; the recording wrapper; `text` +
   `inference` tables; `cairnworld replay`; reconstruction unit test over
   real recorded REPL chats. Verifies: any inference's input can be rebuilt
   verbatim (hash-equal) and re-run.
3. **Tool calls + model bake-off.** Tool registry (schemars-derived schemas)
   + agent loop with one toy tool (a dice roll); exercised from the REPL
   against Qwen3 8B, Hermes (Llama 3.1 8B) and Llama 3.1 8B Instruct; add
   the OpenAI-compatible HTTP backend for the comparison. Verifies: the
   chosen model reliably emits valid tool calls - **decision gate for the
   model** (design.md, Model selection).
4. **Chat history + compaction + fork.** `agent`/`message`/`summary` tables;
   context assembly; compaction with artificially low thresholds;
   `chat --fork` and `--prompts` replay substitution. Verifies: summaries
   cover the right ranges, reconstruction still holds across compaction, and
   the prompt-iteration loop works end to end.
5. **Webserver + UI.** Axum + Leptos, Google OAuth2 login, one world, one
   player agent, websocket chat page. Verifies: a friend can log in from
   another machine and chat against the real model.
6. **Multi-agent + actions.** Player agent → GM call tree, `CallContext`,
   sequences, action IDs with `pending_action`/approve-action, narration
   broadcast. Verifies: the attack-approval flow from user_declarations.md
   end to end with two real agents.
7. **Bread Thief.** Game state schema, JSON import/export, the simplified
   scenario (scenarios/bread_thief.md) with the minimal action set: Move,
   Say, Attack, Save, TakeDamage, BeginCombat/EndCombat, Give/Take, Look.
   Verifies: a human can play the scenario to any of its endings.
8. **Dev mode.** The split view: chat histories, sequence view, inference
   view, game object browser. Verifies: every Bread Thief playtest
   interaction is fully inspectable down to verbatim model input.
9. **MCP.** RMCP server over the existing tool surface and debug queries.
   Verifies: a coding agent can drive a playtest and pull sequences without
   custom scripts.

Milestones 7 and 8 may swap or interleave in practice - playtesting without
the inference view will get painful fast, and that pressure is fine to
follow.

## Milestone 1 detail: Inference REPL

Goal: prove the `llm` layer works end to end - a model loads on GPU and
produces a streamed reply - before anything else in design.md is built on
top of it. This is the first code in the repo; there is no existing `src/`
to integrate with.

### Scope

- New Cargo workspace at the repo root, with one crate, `cairnworld`.
- design.md's `llm` layer only: the `Backend` trait and the mistral.rs
  implementation of it. No recording wrapper yet (that is milestone 2 - see
  design.md, "Recording is not optional"; it does not apply until the store
  exists to record into). No tools, no chat history persistence, no web
  server.
- The `cairnworld chat` subcommand, in its "bare 1:1 conversation" form (see
  design.md, Dev CLI: chat, fork, replay) - no `--kind`, `--fork`, `--prompts`
  flags yet, since those depend on the agent and store layers.

### Out of scope (do not build yet)

- Recording / sqlite / any persistence.
- Tool calling of any kind.
- The OpenAI-compatible backend (deferred to the milestone 3 bake-off per
  design.md's Open questions).
- Multi-turn history beyond what the REPL process holds in memory for the
  current session (no save/resume - that needs the store).

### Steps

1. **Workspace scaffold.**
   - `cargo init --name cairnworld` at the repo root (binary crate; a
     `crates/` split can happen later if `llm` needs to be a separate crate
     for reuse - not needed yet, keep it one crate per "less is more").
   - Add `clap` (derive feature) for CLI parsing, `anyhow` for errors,
     `tokio` (full or the specific features: `rt-multi-thread`, `macros`,
     `io-std`) for the async runtime, `tracing` + `tracing-subscriber` for
     logs.
   - Verify: `cargo build` succeeds with an empty `main.rs` that just prints
     something, before adding any model code.

2. **Add the mistralrs dependency and confirm it builds.**
   - Add `mistralrs = "0.8"` (check crates.io for the current patch version
     at implementation time; pin the minor version, do not use a wildcard).
   - mistral.rs needs a CUDA (or Metal/Accelerate, depending on hardware)
     feature flag to use the GPU - check the crate's current feature names
     in its Cargo.toml / docs.rs page (they change between versions) and
     enable the one matching the target machine's GPU. Building without a
     GPU feature will silently run on CPU, which is not what we want to
     verify - if the feature is missing, generation will be very slow and
     that is the signal something is wrong.
   - Verify: `cargo build --release` succeeds (release mode matters here -
     inference in debug mode is misleadingly slow and would give a false
     read on step 5's speed check). This step compiles llama.cpp-equivalent
     kernels and can take a long time on first build; that is expected, not
     a hang.

3. **Define the `Backend` trait per design.md.**
   - In `src/llm.rs`: the trait (`complete` taking an `on_token: impl
     FnMut(&str)` callback alongside `Request`, returning the full
     `Response` at the end - see design.md's Inference layer), `Request`,
     `Response`, `Message`, `ToolDefinition` (unused fields fine to leave in
     now since the shape is fixed by design.md - do not add fields
     design.md doesn't ask for), `Sampling`, `Content`, `Usage`, exactly as
     sketched in design.md.
   - `Sampling` needs at minimum `temperature`; add nothing beyond what
     mistral.rs's request builder actually accepts, to avoid an unused-field
     smell.
   - Verify: `cargo check` - the trait compiles with no implementation yet.

4. **Implement the mistral.rs backend.**
   - One struct, e.g. `MistralRsBackend`, holding the loaded model (from
     `TextModelBuilder` or equivalent - confirm the exact builder type name
     against the installed crate version's docs, since the API shown in
     search results may have moved on by patch version).
   - Model loading happens once, in an async constructor
     (`MistralRsBackend::load(model_id_or_path: &str) -> Result<Self>`),
     called at startup - not lazily on first request. If loading fails, the
     error must propagate out of `main` and exit the process (fail fast per
     coding_standards.md) - do not catch and retry, do not fall back to a
     smaller model.
   - Implement `Backend::complete` by translating `Request` into whatever
     shape mistral.rs's chat API wants. mistral.rs exposes a token stream for
     generation (check its current streaming API - it may be a
     `Stream`/channel of partial responses rather than a raw callback);
     drive `on_token` from that stream as tokens arrive, and assemble the
     final `Response` (including `usage` - mistral.rs exposes token counts
     directly, do not estimate them by hand, and design.md explicitly rules
     out a separate tokenizer dependency) once the stream ends.
   - Model choice: any 8B-class GGUF model already on hand works for this
     milestone - it only proves the backend and REPL mechanics, not model
     quality (that is milestone 3's bake-off); user_declarations.md no
     longer names a specific model, so pick whichever is most convenient to
     download/test with first. Model id/path is a CLI flag or config value
     (`--model`), not hardcoded, so milestone 3 can swap it freely.
   - Verify: a `#[tokio::test]` (or a small `examples/` binary if loading a
     multi-GB model in `cargo test` is too slow/heavy for CI - decide based
     on actual load time observed in step 5) that loads the model, sends one
     fixed prompt with an `on_token` callback that appends to a `String`,
     and asserts both that the callback's accumulated text and the final
     `Response`'s text are non-empty and equal (proves the stream and the
     final response agree, not just that one of them produces output).

5. **Wire up `cairnworld chat`.**
   - `clap` subcommand `chat` with a `--model <path-or-id>` argument
     (required - no hardcoded default path, since that would silently
     depend on one developer's machine layout) and a `--temperature <f32>`
     argument (optional, sensible default e.g. 1.0).
   - Loop: read a line from stdin, append as a `User` message to an
     in-memory `Vec<Message>`, call `Backend::complete` with an `on_token`
     that writes each token to stdout immediately (no newline, flush after
     each write) so the reply appears incrementally as it generates - this
     is the terminal proof of the same mechanism the browser will use over
     the websocket in milestone 5. Print a trailing newline once the stream
     ends, append the final `Response`'s text to the same `Vec`, repeat
     until EOF or an explicit `/quit`.
   - No system prompt yet beyond an optional `--system <text>` flag for
     manual testing - the real role-prompt assembly is agent-layer work
     (milestone 4), not this milestone's concern.
   - Verify by hand: run `cargo run --release -- chat --model <path>`,
     have a short back-and-forth, confirm (a) text visibly appears
     incrementally rather than all at once, and (b) replies use
     conversation context from earlier turns (proves `Vec<Message>` history
     is actually being sent, not just the latest line).

6. **Record generation speed and quality observations.**
   - This milestone's stated design.md verification is "model loads,
     generation quality/speed, sampling params work" - capture this as a
     short dated note appended to this plan (per plans/README.md, plans are
     allowed to go stale but a note here is cheap and directly informs
     milestone 3's bake-off): tokens/sec observed, VRAM used, and whether
     temperature changes visibly affect output variety.
   - This is observation, not a gate - there is no target number to hit at
     this milestone. If speed is unacceptably slow, that is itself a finding
     to flag to the user before continuing, not something to silently work
     around (e.g. by lowering quantization without saying so).

### 2026-07-26 observation note (milestone 1)

Hardware: RTX 3070, 8GB VRAM. Model: Llama 3.1 8B Instruct, Q4_K_M GGUF
(bartowski quant), loaded via `mistralrs` 0.8.1 `GgufModelBuilder` with the
`cuda` feature.

- **VRAM**: ~5.85GB for the model itself (1.08GB baseline -> 6.93GB loaded),
  leaving limited headroom on this 8GB card for KV cache at longer contexts.
- **Speed**: ~9.7 tok/s sustained over a 200-token completion. Usable for a
  REPL, but slow enough that it's worth watching once tool-call round trips
  and longer contexts (milestone 3+) are in the mix - flagging per the "if
  speed is unacceptably slow" note above rather than silently accepting it.
- **Streaming**: confirmed genuinely incremental, not buffered - tokens
  arrive roughly every ~75ms rather than all at once at the end.
- **Temperature**: confirmed it visibly affects output (0.0 vs 1.3 changed a
  one-word answer). This required a fix: `mistralrs`'s `RequestBuilder`
  defaults to `SamplingParams::deterministic()`, which forces `top_k =
  Some(1)` (greedy decoding) independent of temperature - setting only
  temperature on top of that default is silently a no-op. `MistralRsBackend`
  now starts from `SamplingParams::neutral()` before applying temperature.
- **API surface note**: `Model::stream_chat_request` never emits a terminal
  `Response::Done` - that variant is only sent on the non-streaming
  `send_chat_request` path (confirmed by reading `mistralrs-core`'s
  `pipeline/sampling.rs`). The last `Chunk` (the one with `finish_reason`
  set) carries the final `usage`, so `MistralRsBackend::complete` assembles
  the final text and usage from accumulated chunks rather than reading back
  a `Done` response. This deviates from the initial reading of design.md's
  sketch ("`complete` still returns the same fully-assembled `Response`...")
  but the resulting `Response` shape returned to callers is unchanged.
- **CUDA/driver note**: the dev machine's `/usr/local/cuda` (nvcc 13.2) was
  ahead of the installed NVIDIA driver (initially supporting only CUDA
  13.0), which made mistral.rs's compiled kernels fail at model-load with
  `CUDA_ERROR_UNSUPPORTED_PTX_VERSION`. Resolved by updating the driver.
  Worth checking `nvidia-smi`'s reported CUDA version against `nvcc
  --version` on any new dev machine before debugging inference failures as
  a code problem.

### Definition of done

- `cargo build --release` and `cargo test` both pass.
- `cairnworld chat --model <path>` runs an interactive multi-turn
  conversation against the real GPU-loaded model from a terminal, with
  visibly incremental (token-streamed) output.
- The `Backend` trait matches design.md's Inference layer section exactly
  (no extra speculative fields or methods).
- A dated observation note is appended to this plan per step 6.
- Nothing outside this milestone's scope was built (check against "Out of
  scope" above before committing).
- Status line at the top of this file is updated to `in-progress` when
  started and `complete` when done, per plans/README.md.

## Milestone 2 detail: Recording + replay

Goal: make recording structural before adding another inference feature.  A
REPL completion must pass through one wrapper that persists the exact request
representation sent to `Backend::complete`, its complete response, token usage,
model identity, and elapsed time.  `cairnworld replay` must load that record,
reconstruct the identical request, verify its hash, and submit it through the
same backend API.

### Scope

- `store` backed by one SQLite database, configured with WAL and migrations.
- Content-addressed `text` rows and `inference` rows only.  `agent`,
  `message`, `summary`, `sequence`, and game-state rows belong to later
  milestones; this slice records a complete backend request as one text
  segment, rather than pre-empting their eventual segment types.
- `RecordingBackend<B>` owns a backend and a store, implements `Backend`, and
  is the only backend value constructed by the CLI.  Timing includes the whole
  inner completion, including token streaming.
- `chat --database <path>` records every reply.  `replay --database <path>
  --model <path> <inference-id>` prints the recorded request and old/new
  outputs, then runs the reconstructed request.

### Data boundary

`Request`, `Response`, and all nested LLM API types gain serde derives.  The
recorded input is `serde_json::to_vec(&Request)` and its BLAKE3 hex hash.  The
text table stores that exact UTF-8 JSON once, keyed by its hash; the inference
row stores a JSON segment list with the single `{ "text": hash }` reference.
Reconstruction resolves the segment, verifies the stored text hash and
`input_hash`, then deserializes the request.  This is deliberately a narrow,
lossless recording boundary for the current bare REPL.  Milestone 4 replaces
the single request segment with role/context/summary/message segments while
keeping the same reconstruction proof.

### Steps

1. **Persist the current backend contract.**
   - Add serde derives to the LLM request/response types, including the tagged
     content and role enums, so recorded JSON is explicit and round-trippable.
   - Add `sqlx` (SQLite, Tokio Rustls runtime, migration support), `blake3`,
     and a small async-stream helper only if the chosen CLI plumbing actually
     needs it.  Do not introduce an ORM or a second serialization format.
   - Verify: `cargo check` succeeds before implementing the database wrapper.

2. **Create the store and migration.**
   - Add an embedded SQL migration creating `text(hash, content)` and
     `inference(id, segments, sampling, output, input_hash, input_tokens,
     output_tokens, duration_ms, model, created_at)`.  Fields that require an
     agent/sequence in the final schema are absent for this milestone rather
     than nullable placeholders.
   - `Store::open` creates the parent directory when one was supplied, opens a
     single SQLite pool, enables foreign keys and WAL, and runs migrations.
     It must return database errors with context; no automatic memory fallback.
   - Verify: a temporary-file integration test opens a new store and confirms
     its migration is usable by inserting and reading one content-addressed
     text value.

3. **Add recording and reconstruction.**
   - `RecordingBackend<B>` serializes the request before the inner call,
     streams tokens unchanged, measures monotonic elapsed time, and commits
     the request text plus the completed response/usage as one inference
     record only after a successful completion.  Failed calls are returned
     with context and are not misrepresented as completed inferences.
   - Store the configured model string in every record.  Preserve exact
     request bytes in the content-addressed text row; do not regenerate JSON
     when calculating the record hash.
   - `Store::reconstruct_inference` resolves every segment, validates both
     content-address and complete-input hashes, and returns the deserialized
     `Request` plus the recorded output.
   - Verify: a deterministic fake backend records a request, invokes its
     token callback, and proves reconstruction equals the original request
     and the persisted output/usage/duration/model are correct.  Corrupting a
     stored hash must fail reconstruction, proving this is not a tautological
     readback test.

4. **Route both CLI paths through the abstraction.**
   - `chat` gains `--database`, defaulting to `cairnworld.sqlite` only for the
     database (the model remains explicitly configured as in milestone 1).
     It opens the store before loading the model and constructs only a
     `RecordingBackend` for calls.
   - Add `replay <inference-id>` with the same database/model/temperature
     options.  It reconstructs and verifies before loading the model, prints
     old output and streams the new output, then records the replay as a new
     inference through the same wrapper.
   - Verify by hand with the GPU model: make a REPL request, use its database
     row id with `replay`, and confirm the replayed record can itself be
     reconstructed.  Inspect SQLite rows to establish that both calls have
     token counts, duration, input hash, and model identity.

5. **Document and commit the completed slice.**
   - Update `implementation_reference.md` with the store, recording boundary,
     CLI flags, and the deliberate single-segment limitation.
   - Append a dated verification note to this plan with the actual command,
     database path type (temporary/local), and outcome.  Review the scope
     against milestone 3 to ensure tools, agents, and HTTP backends were not
     added early.
   - Run `cargo fmt --check`, `cargo test`, `cargo build --release`, and the
     real-model chat/replay path.  Re-read coding and prompt standards for the
     self-review, then make one self-contained commit.

### Definition of done

- No CLI code can invoke a concrete model backend without first wrapping it in
  `RecordingBackend`.
- Every successful REPL and replay completion creates a record containing the
  exact serialized request, complete output, usage, duration, model, and
  hashes.
- Reconstruction validates hashes and returns a request byte-for-byte equal to
  what the recorder received; the test uses a real SQLite store and catches a
  deliberately corrupt record.
- A real GPU chat followed by replay has been exercised end to end.
- Documentation is updated, tests/build pass, and this increment is committed.

### 2026-07-26 verification note (milestone 2)

- `CUDA_COMPUTE_CAP=86 cargo test --offline` passed all three tests, including
  the real GPU-backed streamed mistral.rs completion (355 seconds), the
  recorder stream/reconstruction test, and corruption detection against SQLite.
- A release REPL against the local Llama 3.1 8B Q4_K_M GGUF wrote inference 1
  to a disposable `/tmp/cairnworld-milestone2-e2e.sqlite` database.  Its row
  recorded the model path, 47 input tokens, 3 output tokens, 2,769 ms, and a
  64-character BLAKE3 input hash.
- `cairnworld replay ... 1` reconstructed and hash-validated that request,
  printed the saved output, streamed a second `recorded` completion with the
  same 47/3 token counts, and created the replay's own record.  This exercises
  both CLI paths through `RecordingBackend` rather than a separate replay path.
