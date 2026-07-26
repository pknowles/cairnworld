# Implementation Reference

Index of what exists, mapped to design.md sections. See plans/ for how and
when things were built.

## Inference layer (design.md: Inference layer)

- `src/llm.rs` - the `Backend` trait, `Request`/`Response`/`Message`/
  `ToolDefinition`/`Sampling`/`Content`/`Usage` types.
- `src/mistralrs_backend.rs` - `MistralRsBackend`, the mistral.rs
  implementation of `Backend`. Loads a GGUF model via `GgufModelBuilder`
  (`cuda` feature). `complete` drives `on_token` from
  `Model::stream_chat_request`'s `Chunk`s and assembles the final
  `Response`/`Usage` from the accumulated chunks - `stream_chat_request`
  never emits a terminal `Response::Done` (that variant is only sent on the
  non-streaming `send_chat_request` path), so there is nothing to read back.
  Sampling starts from `SamplingParams::neutral()`, not the crate's default
  `deterministic()` (which forces greedy `top_k = 1` independent of
  temperature).
- `src/context.rs` - the only model-call boundary. It assembles a request from
  content-addressed static text and persisted agent messages, streams through
  the backend, then records either the completed response or the failure using
  the same reference recipe.

## Persistence and recording (design.md: Persistence and recording)

- `src/store.rs` and `migrations/0001_recording.sql` - SQLite store, WAL mode,
  migration, identity-only `world`/`agent` rows, ordered `message` history,
  content-addressed `text`, and `inference` recipes. A recipe refers to static
  text and an agent message range; reconstruction rereads those rows, verifies
  text and assembled-input BLAKE3 hashes, and checks stored sampling and usage.
  An inference contains exactly one response or error, so failed calls preserve
  their reconstructable input without entering the agent's message history.

## Dev CLI (design.md: Dev CLI: chat, fork, replay)

- `src/main.rs` - `cairnworld chat [--model <path>] [--temperature <f32>]
  [--system <text>] [--database <path>]` creates a sandbox world and agent,
  then persists each user and assistant message. `cairnworld replay [--model
  <path>] [--database <path>] <inference-id>` reconstructs and validates the
  recorded recipe, displays its response or error, and records the replay by
  calling the same context boundary. `--kind`, `--fork`, and `--prompts` remain
  later work.

## Configuration

- `src/settings.rs` - `Settings`, loaded via the `config` crate (toml
  feature only) layering `default.toml` (checked in) under `local.toml`
  (gitignored, per-machine overrides). Currently holds only `model`; `--model`
  on the CLI wins over both files if given.
- Models are not checked in; `models/` is gitignored.
