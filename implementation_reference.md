# Implementation Reference

Index of what exists, mapped to design.md sections. See plans/ for how and
when things were built.

## Inference layer (design.md: Inference layer)

- `src/llm.rs` - the `Backend` trait, `Request`/`Response`/`Message`/
  `MessageContent`/`ToolDefinition`/`Sampling`/`Content`/`Usage` types.
- `src/mistralrs_backend.rs` - `MistralRsBackend`, the mistral.rs
  implementation of `Backend`. Loads a GGUF model via `GgufModelBuilder`
  (`cuda` feature). `complete` drives `on_token` from
  `Model::stream_chat_request`'s `Chunk`s and assembles the final
  `Response`/`Usage` from the accumulated chunks - `stream_chat_request`
  never emits a terminal `Response::Done` (that variant is only sent on the
  non-streaming `send_chat_request` path), so there is nothing to read back.
  Sampling starts from `SamplingParams::neutral()`, not the crate's default
  `deterministic()` (which forces greedy `top_k = 1` independent of
  temperature). It maps native structured tool calls and reasoning deltas to
  the shared types, and applies the recorded `enable_thinking` setting. Tools
  are sent with `strict`, constraining generation to the argument schema.
- `third_party/mistral.rs` - submodule of https://github.com/pknowles/mistral.rs,
  depended on by path. It carries one fix absent upstream: Qwen3 GGUF inference
  omits the device move before the final norm that the Llama path performs, so
  any CPU/GPU layer split fails in rms-norm. Clone with `--recurse-submodules`,
  or run `git submodule update --init` in an existing checkout.
- `templates/hermes-tools.jinja` - the Hermes 3 GGUF ships a bare ChatML
  template that silently drops tool definitions, so its tool surface is unusable
  without it. `[models.hermes]` supplies it automatically.
- `src/context.rs` - the only model-call boundary. It assembles a request from
  stored static prompt text and persisted agent messages, streams through
  the backend, then records either the completed response or the failure using
  the same reference recipe. Its recipe identifies the inference record so a
  compaction result can refer to the exact request that produced it.
- `src/agent.rs` - resolves one recorded chat turn: persists each assistant
  response, runs calls from the invocation-local tool list, persists their
  results, and repeats until final text. It checks compaction only after that
  completed turn, never while its history is still being appended.
- `src/compaction.rs` - triggers one recorded summarisation once a complete
  live context reaches `compact_at_input_tokens`. It preserves exactly
  `keep_tail_messages` newest raw rows, sends only the older range (and a previous summary)
  to the compaction model call, then records the resulting summary.
- `src/tools.rs` - the local `save` tool and the ordinary invocation-local
  lookup used to derive `ToolDefinition`s and run the matching Rust callback.

## Persistence and recording (design.md: Persistence and recording)

- `src/store.rs` and `migrations/0001_recording.sql` - SQLite store, WAL mode,
  identity-only `world`/`agent` rows, ordered `message` history, write-once
  `text` prompt rows, `summary` rows, and `inference` recipes. A recipe refers
  to static text, tools, a summary, and/or an agent message range;
  reconstruction rereads those rows and verifies the assembled input against
  its BLAKE3 hash. The live history selector is exactly newest summary plus
  messages after its `covers_to_seq`; all older message rows remain intact for
  replay and debugging. Pre-1.0 the schema is edited in place rather than
  migrated - changing it invalidates existing database files.

## Dev CLI (design.md: Dev CLI: chat, replay)

- `src/main.rs` - `cairnworld chat [--model <name|path>] [--temperature <f32>]
  [--enable-thinking] [--system <text>] [--database <path>] [--chat-template
  <path>]` creates a sandbox world and agent, then resolves each turn through
  the agent loop. `cairnworld replay [--model <name|path>] [--database <path>]
  <inference-id>` reconstructs and validates the recorded recipe, displays its
  response or error, and records the replay by calling the same context
  boundary. `--kind` remains later work.
- `src/settings.rs` - `[models.<name>]` entries pair a GGUF path with the chat
  template that file needs, so `--model hermes` carries its template
  automatically. `--model` also accepts a path directly, and `--chat-template`
  overrides whatever the entry specifies.

## Configuration

- `src/settings.rs` - `Settings`, loaded via the `config` crate (toml
  feature only) layering `default.toml` (checked in) under `local.toml`
  (gitignored, per-machine overrides). Holds the `[models.<name>]` entries
  described above, `model` naming the default among them, and `limits`,
  including `compact_at_input_tokens` and `keep_tail_messages`.
- Weights are not checked in; `models/` is gitignored.
