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
- `src/recording_backend.rs` - `RecordingBackend<B>` owns a `Backend` and a
  `Store`.  It is the CLI's only model-call boundary: it passes streaming
  tokens through unchanged, then records each successful completed request,
  response, usage, model string, and elapsed duration.

## Persistence and recording (design.md: Persistence and recording)

- `src/store.rs` and `migrations/0001_recording.sql` - SQLite store, WAL mode,
  migration, content-addressed `text` records and `inference` records.
  Milestone 2 records the bare REPL request as one canonical JSON text segment;
  agent/message/summary segments remain deliberately deferred until their
  corresponding persistence layer in milestone 4.  Reconstruction rechecks
  the text BLAKE3 hash, record input hash, sampling, and usage columns before
  returning the deserialized request and output.

## Dev CLI (design.md: Dev CLI: chat, fork, replay)

- `src/main.rs` - `cairnworld chat [--model <path>] [--temperature <f32>]
  [--system <text>] [--database <path>]` is a bare 1:1 REPL over
  `Vec<Message>` held in process memory, with each completion recorded.  The
  `cairnworld replay [--model <path>] [--database <path>] <inference-id>`
  command reconstructs and validates its request, displays the old request and
  output, then runs the same recording path again.  `--kind`, `--fork`, and
  `--prompts` remain milestone 4 work.

## Configuration

- `src/settings.rs` - `Settings`, loaded via the `config` crate (toml
  feature only) layering `default.toml` (checked in) under `local.toml`
  (gitignored, per-machine overrides). Currently holds only `model`; `--model`
  on the CLI wins over both files if given.
- Models are not checked in; `models/` is gitignored.
