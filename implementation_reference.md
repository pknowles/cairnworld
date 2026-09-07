# Implementation Reference

Index of what exists, mapped to design.md sections. See plans/ for how and
when things were built.

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
  stored static prompt text and persisted agent messages, streams through
  the backend, then records either the completed response or the failure using
  the same reference recipe. Its recipe identifies the inference record so a
  compaction result can refer to the exact request that produced it. A
  structured fixed-KV rejection is recovered here by persisting and waiting for
  a compaction job before retrying the same recipe. The retry must report fewer
  required tokens. CUDA out-of-memory remains an ordinary model error because
  it does not establish that compaction can reduce the failed allocation.
- `src/agent.rs` - resolves one recorded chat turn: persists each assistant
  response, runs calls from the invocation-local tool list, persists their
  results, and repeats until final text. A final reply and any due compaction
  job are committed together.
- `src/inference.rs` - admits foreground work ahead of deferred compaction,
  capped by `max_concurrent_inferences`; mistral.rs batches admitted sequences.
  It resumes pending jobs at startup and keeps same-agent history ordered.
- `src/compaction.rs` - resolves a persisted compaction job as ordinary
  recorded summarisation, preserving exactly `keep_tail_messages` newest raw
  rows. The input is the previous summary and that raw range, closed by the
  compaction instruction as a user-role message. If that request is rejected
  for capacity, it retains one more raw row and tries the real summary request
  again. If no older row remains, it fails; it never predicts token use or
  retries unchanged history.
- `src/tools.rs` - the ordinary invocation-local lookup used to derive
  `ToolDefinition`s and run the matching Rust callback. Game tools are added
  only with their related authenticated game state, never to the sandbox REPL.

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
  sheet through that character relationship. A recipe refers
  to static text, tools, a summary, and/or an agent message range;
  reconstruction rereads those rows and verifies the assembled input against
  its BLAKE3 hash. The live history selector is exactly newest summary plus
  messages after its `covers_to_seq`; all older message rows remain intact for
  replay and debugging. `request_for_segments` carries the summary inside the
  leading system message as a marked block, or as that message when a recipe
  has no role prompt, and rejects any assembled request that is not a
  well-formed conversation - one system message only in first place, and a
  real user turn - so a malformed request fails at assembly with the agent id
  rather than deep in a model's chat template. Until real user databases exist, the checked-in schema
  is rebuilt in place and existing local databases are intentionally replaced;
  later schema evolution requires forward migrations.

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
  rather than trusting a client user or agent id. The authenticated
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
  island. `scripts/check_hydration.cjs` runs the compiled loading island in
  headless Chrome against a local ready response, while
  `scripts/check_player_chat.cjs` uses Playwright with installed Chrome to
  verify the compiled chat island stays within the game pane, retains focused
  text while Send is unavailable, scrolls to a newly added entry, shows server
  activity outside the transcript, reloads its durable SSR transcript,
  reconnects, receives a durable entry produced after its SSR cursor, and
  reports disconnects. Neither needs OAuth or a model.
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
  package consumed by the game-page hydration script.
