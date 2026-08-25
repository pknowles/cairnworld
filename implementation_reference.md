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
  response. It logs model loading and inference lifecycle (request submitted,
  first stream chunk, completion or exact stream/model failure) with elapsed
  time and non-sensitive request counts, so a hung or failed model boundary is
  visible immediately.
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
  results, and repeats until final text. A final reply and any due compaction
  job are committed together.
- `src/inference.rs` - admits foreground work ahead of deferred compaction,
  capped by `max_concurrent_inferences`; mistral.rs batches admitted sequences.
  It resumes pending jobs at startup and keeps same-agent history ordered.
- `src/compaction.rs` - resolves a persisted compaction job as one recorded
  summarisation. It preserves exactly `keep_tail_messages` newest raw rows and
  transactionally writes the summary and inline notice while retiring the job.
- `src/tools.rs` - the ordinary invocation-local lookup used to derive
  `ToolDefinition`s and run the matching Rust callback. Game tools are added
  only with their related authenticated game state, never to the sandbox REPL.

## Persistence and recording (design.md: Persistence and recording)

- `src/store.rs` and `migrations/0001_recording.sql` - SQLite store, WAL mode,
  identity-only sandbox worlds and chat-history agents; OAuth email identities
  with non-unique display names; durable world-owner, membership,
  membership-player-agent, player-character, NPC-agent, and location-GM
  relationships; world/character/location/path/item/item-type/spell-type data
  with separate GM and Storyteller JSON notes. `install_scenario` creates the
  entire initial relationship graph in one transaction and `export_scenario`
  exports only reusable scenario data, never a player's history or membership.
  The active-membership lookup is the single player-world authorization
  boundary. A recipe refers
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
  overrides whatever the entry specifies.
  The interactive editor supports normal terminal history/editing, shows when
  a model is active, and renders recorded tool activity, compaction summaries,
  and notices after each turn.

## Configuration

- `src/settings.rs` - `Settings`, loaded via the `config` crate (toml
  feature only) layering `default.toml` (checked in) under `local.toml`
  (gitignored, per-machine overrides). Holds the `[models.<name>]` entries
  described above, `model` naming the default among them, and `limits`,
  including `max_concurrent_inferences`, `compact_at_input_tokens`, and
  `keep_tail_messages`.
- Weights are not checked in; `models/` is gitignored.

## Web play (design.md: Web server and UI; Auth)

- `src/game.rs` - the membership-scoped game service used by browser events.
  It serializes each world's events, opens a blank Adventurer's player agent
  proactively, and gives the agent a static entry event so tool-capable Llama
  templates have a valid first user turn without displaying or persisting a
  fake player message. The ordinary agent loop executes any creation-roll
  calls and stores its final text. A reconnect waits for that durable event
  rather than creating another one; subsequent player messages expose creation
  or location-action tools according to durable character state.
- `src/web.rs` - `cairnworld serve`'s Axum routes. Google OIDC discovers and
  exchanges through `openidconnect`; SQLite-backed `tower-sessions` holds the
  verified account id. The landing page updates only a non-unique display
  name; identity remains the verified email. Landing, world, invitation, and membership routes all
  resolve access through `Store` rather than trusting a client user or agent
  id. The authenticated `/world/:id/play` route loads that membership's
  durable player-visible history, while its websocket passes the same resolved
  membership into `Game` for each player message. On every connection it sends
  an authoritative typed history snapshot, then readiness; later entries and
  errors are typed events. It serves the `cargo-leptos` browser package
  at `/pkg` plus checked-in artwork at `/media`. Browser pages use one valid
  document shell with viewport metadata, stylesheet and, only where needed,
  hydration scripts. `.cargo/config.toml` gives Leptos one Cargo-wide WASM
  output name, so a normal `cargo run` hydration script requests the package
  that `cargo-leptos` actually emits.
- `src/lib.rs` exports Leptos's required islands `hydrate()` entrypoint, which
  initializes the island hydrator before the generated loader invokes each
  island. `scripts/check_hydration.cjs` runs the compiled loading island in
  headless Chrome against a local ready response, while
  `scripts/check_player_chat.cjs` opens the compiled chat island's WebSocket
  and verifies a typed ready event enables the rendered input. Neither needs
  OAuth or a model.
- `src/ui.rs` and `src/lib.rs` - the shared, serializable browser chat event
  types and the small Leptos island used by the game page. SSR receives the
  initial durable transcript; Leptos hydrates that same island in WASM and
  owns the input, send state, websocket transport, and rendering of player,
  agent, narration, and visible error entries. This is the sole browser chat
  implementation; the page contains no parallel handwritten JavaScript loop.
  The loading island displays the complete server startup error when available
  rather than hiding it behind an HTTP status.
- `style/app.css`, `package.json`, and `package-lock.json` - Tailwind CSS 4
  with daisyUI's maintained component/theme layer. The default `forest` theme
  establishes the dark palette; the shared Leptos markup uses responsive
  layout primitives for the landing, management and chat pages. The chat's
  bounded central workspace leaves the page structure extensible for future
  developer inspection panels without inventing one early. `npm run
  build:frontend` runs the WASM package build followed by the stylesheet build,
  ensuring cargo-leptos's default blank CSS output cannot overwrite
  `/pkg/cairnworld.css`.
- `Cargo.toml` - separates server (`ssr`) dependencies from the WASM
  `hydrate` target. `cargo-leptos build` builds both targets and emits the
  package consumed by the game-page hydration script.
