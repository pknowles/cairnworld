# 0001 - Infrastructure to Bread Thief

Status: draft (2026-07-25)

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
