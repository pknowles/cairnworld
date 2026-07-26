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
- No recording wrapper yet - inference calls are not yet persisted anywhere
  (milestone 2).

## Dev CLI (design.md: Dev CLI: chat, fork, replay)

- `src/main.rs` - `cairnworld chat [--model <path>] [--temperature <f32>]
  [--system <text>]`. Bare 1:1 REPL over `Vec<Message>` held in process
  memory; no persistence, no `--kind`/`--fork`/`--prompts` (those need the
  agent/store layers, milestone 4+).

## Configuration

- `src/settings.rs` - `Settings`, loaded via the `config` crate (toml
  feature only) layering `default.toml` (checked in) under `local.toml`
  (gitignored, per-machine overrides). Currently holds only `model`; `--model`
  on the CLI wins over both files if given.
- Models are not checked in; `models/` is gitignored.
