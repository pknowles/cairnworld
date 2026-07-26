# Infrastructure to Bread Thief

Status: in-progress (2026-07-26) - milestones 1-2 complete; milestone 3 next
(see detail below); milestones 4-9 not started

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
Each gets more detail added here as an agent begins it, per the
implementation loop in AGENTS.md - a milestone does not need to be fully
specified up front.

1. **Inference REPL.** Complete. `llm` layer with the mistral.rs backend;
   `cairnworld chat` runs a conversation in the terminal. Detailed below.
2. **Chat history + recording.** `store` layer: `world`/`agent`/`message`
   tables plus content-addressed `text` and `inference` recipe rows; context
   assembly as the single recorded path to the model; `cairnworld replay`.
   Detailed below.
3. **Tool calls + model bake-off.** Tool registry (schemars-derived schemas)
   + agent loop with one toy tool (a dice roll); exercised from the REPL
   against Qwen3 8B, Hermes (Llama 3.1 8B) and Llama 3.1 8B Instruct; add
   the OpenAI-compatible HTTP backend for the comparison. Verifies: the
   chosen model reliably emits valid tool calls - **decision gate for the
   model** (design.md, Model selection).
4. **Compaction + fork.** `summary` table; compaction with artificially low
   thresholds; `chat --fork` and `--prompts` replay substitution. Verifies:
   summaries cover the right ranges, reconstruction still holds across
   compaction, and the prompt-iteration loop works end to end.
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
produces a streamed reply.

### Scope

- One crate, `cairnworld`. design.md's `llm` layer: the `Backend` trait and
  the mistral.rs implementation. No persistence, no tools, no web server.
- `cairnworld chat` in its "bare 1:1 conversation" form (see design.md, Dev
  CLI: chat, fork, replay) - no `--kind`, `--fork`, `--prompts` flags yet,
  since those depend on the agent and store layers.

### Observations (2026-07-26)

Hardware: RTX 3070, 8GB VRAM. Model: Llama 3.1 8B Instruct, Q4_K_M GGUF
(bartowski quant), loaded via `mistralrs` 0.8.1 `GgufModelBuilder` with the
`cuda` feature.

- **VRAM**: ~5.85GB for the model itself (1.08GB baseline -> 6.93GB loaded),
  leaving limited headroom on this 8GB card for KV cache at longer contexts.
- **Speed**: ~9.7 tok/s sustained over a 200-token completion. Usable for a
  REPL, but slow enough that it's worth watching once tool-call round trips
  and longer contexts (milestone 3+) are in the mix.
- **Streaming**: confirmed genuinely incremental, not buffered - tokens
  arrive roughly every ~75ms rather than all at once at the end.
- **Temperature**: confirmed it visibly affects output (0.0 vs 1.3 changed a
  one-word answer). `mistralrs`'s `RequestBuilder` defaults to
  `SamplingParams::deterministic()`, which forces `top_k = Some(1)` (greedy
  decoding) independent of temperature - setting only temperature on top of
  that default is silently a no-op. `MistralRsBackend` starts from
  `SamplingParams::neutral()` before applying temperature.
- **API surface note**: `Model::stream_chat_request` never emits a terminal
  `Response::Done` - that variant is only sent on the non-streaming
  `send_chat_request` path. The last `Chunk` (the one with `finish_reason`
  set) carries the final `usage`, so `MistralRsBackend::complete` assembles
  the final text and usage from accumulated chunks.
- **CUDA/driver note**: check `nvidia-smi`'s reported CUDA version against
  `nvcc --version` on any new dev machine before debugging inference
  failures as a code problem - a driver behind the toolkit fails at
  model-load with `CUDA_ERROR_UNSUPPORTED_PTX_VERSION`.

### Definition of done

- `cargo build --release` and `cargo test` both pass.
- `cairnworld chat --model <path>` runs an interactive multi-turn
  conversation against the real GPU-loaded model from a terminal, with
  visibly incremental (token-streamed) output.
- The `Backend` trait matches design.md's Inference layer section exactly
  (no extra speculative fields or methods).

## Milestone 2 detail: Chat history + recording

Goal: make chat history the primary persisted data, since agents are
defined as "just chat history" (user_declarations.md, Agents) - the game
cannot function past one process lifetime without it. Recording an
inference becomes a small recipe of references into that history plus
telemetry, not a second copy of it. A REPL completion writes its user
message, runs one recorded inference, and writes the assistant message (or,
on failure, an error record) as part of the same flow - there is no separate
"recording" feature bolted on afterward. `cairnworld replay` loads a
recorded recipe, reconstructs the identical request, verifies its hash, and
submits it through the same backend API.

### Scope

- `store` backed by one SQLite database, WAL mode, migrations.
- Real chat schema: identity-only `world(id, name)`; `agent(id, world_id,
  kind, name)`; `message(id, agent_id, seq, role, content, created_at)`,
  unique `(agent_id, seq)`, `content` as tagged JSON. Content-addressed
  `text(hash, content)` for static prompt pieces. `summary`, `sequence`, and
  game-state rows belong to later milestones.
- `inference(id, agent_id, segments, sampling, output, error, input_hash,
  input_tokens, output_tokens, duration_ms, model, created_at)` - `segments`
  is an ordered recipe (`{text: hash, role}`, `{messages: {agent_id,
  first_seq, last_seq}}` for this milestone; `{summary}`/`{tools}` arrive
  with milestones 3/4), with exactly one of `output`/`error` non-null.
- One assembly function (e.g. `src/context.rs`) is the single place that
  builds a `Request`, computes its recipe and hash, calls the backend, and
  records the outcome - success or failure - referencing the message rows
  it read rather than re-storing them. This realises design.md's "Recording
  is not optional": it is the only code that calls a backend, and the only
  code that knows an input's provenance.
- `chat --database <path>` records every reply through this path. `replay
  --database <path> --model <path> <inference-id>` prints the recorded
  request and old/new outputs, then runs the reconstructed request.

### Data boundary

`Request`, `Response`, and all nested LLM API types have serde derives. An
inference's recorded input is never a serialized copy of the whole request;
it is the recipe above, resolved by reading the referenced `message` rows
and `text` rows fresh. `input_hash` is the BLAKE3 hash of the *reassembled*
request, so a unit test can prove reconstruction is hash-equal without the
schema ever storing the request bytes themselves. A repeated inference over
an unchanged history must add only a new `inference` row (recipe +
telemetry) - never a copy of prior message payloads - which is the
concrete, testable form of "store messages once."

Failed inferences are recorded with their recipe and input hash (so the
exact prompt that produced the failure is reconstructable) and the error
text in place of output/usage. They must be visible in the developer chat
history view. A failed output is never fed back into model context - the
developer view is the union of `message` rows and failed `inference` rows,
not the context-assembly input.

### Steps

This milestone is one commit: the old recording path is deleted and its
replacement built in the same slice, so `RecordingBackend` never coexists
with the new schema. There is no non-disposable existing database - before
rewriting the migration, check for any local `*.sqlite` file that isn't
obviously disposable (e.g. under `/tmp`) and confirm with the user before
deleting it.

1. **Replace the old recording path with the new schema and store.**
   Delete `src/recording_backend.rs` (`RecordingBackend<B>` and its tests)
   and everything referencing it in `src/main.rs` (`mod recording_backend;`,
   the `recording_backend()` constructor helper, and its call sites in
   `run_chat`/`run_replay`). Rewrite `migrations/0001_recording.sql` in
   place (delete-and-replace, not a second migration) with `world`, `agent`,
   `message`, `text`, `inference` as scoped above. `Store::open` opens a
   single SQLite pool, enables foreign keys and WAL, runs migrations,
   returns database errors with context - no automatic fallback.
   Verify: `cargo build` succeeds and `grep -r RecordingBackend` finds
   nothing; a temporary-file test opens a new store and exercises an
   insert/read of each table.

2. **Store operations over real rows.** `append_message`, `put_text`
   (insert-if-absent by hash), `record_inference` (recipe + success-or-error
   outcome), `reconstruct_inference` (resolve every segment, verify content
   hash and `input_hash`, return the deserialized `Request` plus recorded
   output or error).
   Verify: a deterministic multi-turn test via these operations proves a
   repeated inference adds recipe/telemetry only. Corrupt text, a deleted
   message row, reordered `seq`, and a cross-agent reference must each fail
   reconstruction with context. A recorded failure must reconstruct its
   input successfully while reporting its recorded error.

3. **Assembly boundary + REPL wiring.** The context-assembly function
   described above (e.g. `src/context.rs`); `run_chat` drops its in-memory
   `Vec<Message>`, creates a sandbox world + agent, appends the user message
   to the store, and calls the assembly function each turn.
   Verify: multi-turn REPL against the real GPU model; inspect rows to
   confirm each message exists exactly once and each inference row is a
   recipe, not a blob. A fake erroring backend proves the failure path: the
   error propagates to the caller *and* a reconstructable failed-inference
   row exists.

4. **Replay.** `replay <inference-id>` reconstructs via the recipe (this
   works for failed records too - that is the point of recording them),
   prints recorded input and output-or-error, re-runs through the same
   assembly boundary so the replay is itself recorded.
   Verify: replay a mid-conversation inference from step 3's session;
   confirm the replayed record also reconstructs hash-equal.

5. **Document and commit.** Update `implementation_reference.md` - it must
   no longer mention `RecordingBackend`. Run `cargo fmt --check`,
   `cargo test`, `cargo build --release`, and the real chat/replay path by
   hand. Self-review per AGENTS.md, then commit the milestone.

### Definition of done

- No whole-request blobs exist anywhere in the schema: the only copies of
  conversation text live in `message` rows and content-addressed `text`
  rows; `inference` rows hold recipes and telemetry only.
- Every completion through the assembly boundary - success or failure -
  leaves a reconstructable, hash-verified record; a failed inference is
  queryable with its error and reconstructable input.
- A real GPU chat followed by replay has been exercised end to end.
- Documentation matches the code; tests and builds pass; one commit.
