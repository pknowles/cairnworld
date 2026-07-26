# Design

This document is the desired end state: a structured consolidation of
user_declarations.md plus the implementation-shaped decisions needed to build
it. It contains no progress information - build order and status live in
plans/ (see AGENTS.md). Sections marked TODO are known design gaps: expand
them here first, then plan their implementation. Open questions are collected
at the end.

# Architecture overview

One rust binary, one sqlite database file per deployment, an embedded LLM. The
binary has subcommands:

- `cairnworld serve` - run the webserver and game
- `cairnworld chat` - interactive agent REPL (see "Dev CLI: chat, fork,
  replay")
- `cairnworld replay <inference-id>` - re-run a recorded inference, optionally
  with edited prompts
- `cairnworld world export|import <file.json>` - world snapshots for checked-in
  scenarios and repros

Layers, each depending only on those above it:

1. `llm` - inference backends behind one trait; every call recorded
2. `store` - sqlite persistence: chat histories, game objects, inference
   records
3. `agent` - context assembly, the agent loop, tool registry, agent-to-agent
   call tree
4. `game` - game objects, actions, dice, Cairn rules
5. `web` - axum routes, websocket chat, Leptos frontend, dev mode views
6. `mcp` - RMCP server exposing the same tool surface to coding agents

The database is the single source of truth. Agents hold no in-memory chat
state between inferences; each inference reads the (small) context it needs
from the store. This makes NPC "paging" a non-problem - inactive agents simply
occupy no memory - and means a crash or restart loses nothing.

# Inference layer

## Backend trait

A single trait with one method, roughly:

```rust
trait Backend {
    async fn complete(
        &self,
        request: Request,
        on_token: impl FnMut(&str),
    ) -> Result<Response>;
}

struct Request {
    messages: Vec<Message>,      // system / user / assistant / tool roles
    tools: Vec<ToolDefinition>,  // name, description, JSON schema
    sampling: Sampling,          // temperature etc. (Storyteller runs hot)
}

struct Response {
    content: Content,            // Text(String) | ToolCalls(Vec<ToolCall>)
    usage: Usage,                // input tokens, output tokens
}
```

`on_token` fires as generation produces output; mistral.rs and typical
OpenAI-compatible APIs both stream natively, so this is not extra machinery,
just not discarding what the backend already gives us. `complete` still
returns the same fully-assembled `Response` at the end - recording, tool-call
parsing and the agent loop all operate on the complete response exactly as
before. Streaming is purely an additional, optional view onto the same
generation; nothing downstream of the backend has to change to support it.

Backends:

- **mistral.rs, in-process** (first). GGUF quantized model on GPU. Loaded once
  at startup; failure to load is a startup failure (fail fast).
- **OpenAI-compatible HTTP client** (early second). Nearly free to add since
  the request shape is identical, and it buys a lot: comparing small-model
  behaviour against a large API model when debugging prompts, and running the
  test suite on machines without a GPU.

Model choice is configuration (CLI arg / config), not code.

**Model selection.** Stheno v3.3 is a Llama-3-base RP finetune with no native
tool-call template, so it is out as the primary model. Requirements: reliable
tool calling, good conversational/NPC voice, ~8B GGUF, long context.
Candidates, to be settled by a bake-off against our real tool set before
anything is built on top (see plans/):

- **Qwen3 8B** - strongest current small model for tool calling and general
  capability; fewer RP finetunes exist but base instruct voice may be enough.
- **Hermes (NousResearch, Llama 3.1 8B base)** - explicitly trained for both
  function calling and RP/creative voice; likely the best single-model fit.
- **Llama 3.1 8B Instruct** - the baseline with a native tool template and
  the largest finetune ecosystem if a swap is wanted later.

If no single model does both jobs well, different models per agent kind
(tool-heavy GM vs voice-heavy NPCs) is possible behind the backend trait, but
is not designed for until evidence demands it. Constrained/JSON-schema
generation remains a fallback mechanism for weak tool callers.

## Recording is not optional

Every inference in every environment is recorded with token counts and wall
time. This is structural rather than a matter of discipline: the only code
that calls a backend is the context assembly function (see Agent loop),
which builds an input from stored messages and text and records the outcome
in the same operation. Recording is therefore not a separate feature layered
over inference - it is a property of the one place that knows an input's
provenance. A wrapper below the backend cannot do this job: handed an
already-assembled request, it knows only opaque bytes, and can do no better
than storing a copy of them.

# Persistence and recording

## Store choice

`sqlx` with sqlite, WAL mode. Not a full ORM (Diesel's DSL and codegen are
more framework than this project needs), but not raw strings either: sqlx's
`query!`/`query_as!` macros check every SQL statement and its result types
against a real dev database at compile time, mapping rows into plain rust
structs. Type safety where it matters, SQL stays visible, async-native so it
composes directly with axum and the world tasks, and migrations are built in
(`sqlx migrate`). One `SqlitePool` owned by the application.

## Chat schema

- `agent(id, world_id, kind, name)` - one row per agent: each player agent,
  each location GM, each NPC, the Storyteller, transient Questioners.
- `message(id, agent_id, seq, role, content, created_at)` - `content` is
  tagged JSON: plain text, tool calls, or tool results. `seq` orders messages
  per agent.
- `summary(id, agent_id, covers_to_seq, content, inference_id)` - compaction
  products. The live context for an agent is: newest summary + all messages
  with `seq > covers_to_seq`.

## Inference records and reconstruction

Requirement: reconstruct the *verbatim* model input for any inference, without
storing the whole input redundantly every time.

All large strings that feed prompts - role prompts, rule packets, notes/context
packets, compaction instructions - live in one content-addressed table:

- `text(hash, content)` - insert-if-absent; referenced by hash.

An inference is recorded as a recipe of references plus verbatim output:

- `inference(id, agent_id, sequence_id, parent_inference_id, segments,
  sampling, output, error, input_hash, input_tokens, output_tokens,
  duration_ms, model, created_at)`

`segments` is a JSON array describing the input in order, e.g.
`[{text: <hash>}, {summary: <id>}, {messages: [first_seq, last_seq]},
{text: <hash>}]`. Reconstruction resolves the references; `input_hash` is the
hash of the fully assembled input actually sent, so a unit test can reassemble
and verify equality for every recorded inference. This test runs over real
recorded data, which makes any drift between assembly and recording fail
loudly.

A failed inference is recorded, not dropped: exactly one of `output`/`error`
is set, and a failure keeps its full recipe and `input_hash` so the prompt
that produced it reconstructs and replays like any other. Failures are as
interesting as successes when debugging prompts and model behaviour, and an
unrecorded failure is invisible after the fact. A failed output is never fed
back into an agent's context - it is not a `message`, so context assembly
never sees it. Dev mode's chat history renders the union of `message` rows
and failed inferences, so a failure appears inline in the transcript and
opens into its inference view like any other entry.

Because compaction is itself an inference, summaries automatically get the
same record and the same reconstruction guarantee.

Archiving: `inference`, `message`, `summary` and `text` rows are exportable by
date range to compressed JSON for offline retention; out of scope until disk
pressure is real.

## Game state schema

Direct mapping of the game objects in user_declarations.md: `world`,
`character` (PC and NPC in one table, discriminated), `location`, `path`,
`item`, plus `gm_notes`/`storyteller_notes` columns (key/value JSON - see
notes editing below) on each. `world` holds time and `next_action_id`. Exact
columns are an implementation detail; the JSON export format is the stable,
checked-in representation (Bread Thief lives in `scenarios/` as importable
JSON).

Notes are stored as key/value pairs per object, per the "actually this sounds
pretty solid" option in user_declarations.md: the editing tool overwrites a
whole value by key, avoiding line-range or paragraph-index fragility.

# Agent loop

## Context assembly

Every inference input is assembled fresh, in this order:

1. Role prompt - static per agent kind, versioned in the repo as plain text
   files (`prompts/`), loaded at startup, recorded by hash.
2. Context packet - current dynamic state this agent is entitled to see,
   rebuilt each time: e.g. for a GM, its location description, characters
   present with sheets, GM notes, visible Storyteller notes. Never appended to
   history - it is always current, so it never goes stale in the transcript.
3. Latest summary, if any.
4. Raw message tail (`seq > covers_to_seq`).

## The loop

Per user_declarations.md: send context + tools; if the model returns tool
calls, validate (serde against the schema), execute, append results, repeat;
stop on a final text (possibly empty) response.

Structural rules, enforced in rust:

- Agent-to-agent calls form a tree. A `CallContext` carries the stack of agent
  ids and the `sequence_id`; calling an agent already on the stack is an
  error.
- Errors only ever gain context (`anyhow` with `.context()` at each agent and
  tool boundary) and propagate to the top - all the way to the player's chat
  and the recorded sequence. No silent failures, no defaults, production
  included.

## Tools

A tool = name + short description + JSON schema + rust handler. Each agent
kind has a fixed tool set. Two mechanics from user_declarations.md shape the
framework:

- **Action IDs and approve-action.** When a character agent's tool call needs
  GM arbitration, rust assigns the next action id, stores the validated
  arguments in a `pending_action` row, and forwards id + visible arguments to
  the GM. The GM approves by id (optionally with modifiers) or rejects with
  text. Rust executes the stored arguments - the LLM never re-copies them.
- **Rule packets.** The player agent sees a tool's short description; when the
  call reaches the GM, rust attaches the extended rulebook text for that tool
  (content-addressed, so recorded like everything else).

Dice rolls are rust (`rand`), never the model. Every roll is recorded (see
sequences) so a session is fully replayable as data.

## Concurrency

One tokio task per world processes a queue of events (player messages, timers)
strictly sequentially; all agent recursion for one event completes (awaited)
before the next event starts. Per-agent history ordering is therefore trivial,
and there are no locks to reason about. This is deliberately the simplest
model that is correct; if a world with many players ever stalls on it,
that is a measured problem for later. Broadcast messages (GM narration to a
location) fan out from the world task to connected websockets via channels.

# Sequences (debug spine)

Every external trigger - a player message, a timer - opens a `sequence` row.
The `sequence_id` flows through `CallContext` into every inference, tool
execution, and dice roll it causes:

- `sequence(id, world_id, trigger, created_at)`
- `inference.sequence_id`, `inference.parent_inference_id` - the call tree
- `action(id, sequence_id, inference_id, tool, args, result, created_at)` -
  every executed tool call, including dice values

This is what the dev-mode sequence view renders, and summing
`inference.usage` over a sequence gives the cumulative token/latency cost per
player interaction that user_declarations.md calls out as critical.

# Web server and UI

Axum on tokio, with **Leptos** (SSR + hydration via `cargo-leptos`, axum
integration) for the frontend. Chosen now rather than ported to later: the
frontend scope is already known (chat page, world pages, the data-heavy dev
mode browser), and full-stack rust means the message/tool/inference types
flow from the store into components with no duplicated API layer - Leptos
server functions replace hand-written JSON endpoints for everything except
the chat websocket. Accepted costs: the WASM toolchain and slower frontend
compile turnaround.

Pages and endpoints:

- `/` - landing page (log in, name, world list, create world)
- `/world/:id` - detail page (invites, players, dev mode toggle)
- `/world/:id/play` - game page: one chat column with an input box
- `WS /world/:id/ws` - the chat: client sends player text; server pushes chat
  entries, broadcasts, a `can_act` flag (drives the greyed-out send button),
  and token deltas keyed by message id for in-flight inferences

Player-facing chat only ever renders a message once the agent loop has
resolved it to final narration - an in-progress turn may still turn out to
be a tool call, and streaming raw tool-call syntax to a player would leak
mechanics. So for players, streaming buys earlier-starting text rather than
early partial text: the server can start pushing the narration's tokens as
soon as the model itself commits to producing final text (i.e. is no longer
mid tool-call), rather than waiting for the whole message. In dev mode there
is no such restriction (see below) - the raw stream, including tool-call
syntax, is exactly what a developer wants to watch.

## Auth

Google OAuth2 from the start (play testing with friends begins early), via
off-the-shelf crates: `openidconnect` for the Google OIDC flow and
`tower-sessions` (sqlite store) for the session cookie. Accounts are keyed by
email per user_declarations.md; no password storage. Localhost testing is
straightforward - Google accepts `http://localhost:<port>` redirect URIs for
desktop/dev OAuth clients, so one client id in a gitignored config file
serves development. The client secret and db path are the only deployment
configuration.

## Dev mode

One-way per-world flag as declared. The game page splits into two columns;
the right column navigates:

- Agent list → raw chat history (infinite scroll, summaries shown as
  expandable inserts at their compaction points, failed inferences shown
  inline where they occurred)
- Chat entries → sequence view: the call tree of inferences and actions for
  that entry's sequence, with per-node and cumulative token/time costs
- Any inference → inference view: reconstructed verbatim input and verbatim
  output, or the recorded error for a failed one
- Game objects → current state and notes

An in-progress sequence view shows each open inference's raw token stream
live, verbatim, as it generates - tool-call syntax included. This is the
direct, at-a-glance read on generation speed user_declarations.md asks for
("we need to know" when context size or performance blows up), and it uses
the same `on_token` callback as everything else - no separate mechanism.

All views are plain server-rendered data from the tables above; the sequence
spine means no extra bookkeeping exists only for debugging.

# Chat compaction

Config: `compact_at_tokens` (total context trigger) and `keep_tail_chars`
(raw messages preserved after the summary). When an agent's assembled context
exceeds the trigger:

1. Choose the cut point `n` such that messages after `n` total under
   `keep_tail_chars`.
2. Build a compaction input: role prompt + previous summary + messages up to
   `n` + the compaction instruction (what is static and always provided, what
   will be lost, what matters to keep). Newer messages are excluded - the
   agent's history is effectively truncated for this one inference.
3. Run it through the normal recorded inference path; store the `summary` row
   with `covers_to_seq = n`.

Compaction happens lazily, checked before assembling a normal inference, so
there is no background job.

# Dev CLI: chat, fork, replay

The fast iteration loop for prompt and agent work. Everything here is a thin
frontend over the same `agent`/`store` functions the game uses - no parallel
implementation - and every inference made here goes through the normal
recorded path, in a dedicated sandbox world so world telemetry stays clean.

- **`cairnworld chat`** - interactive stdio REPL. In its simplest form it is
  a bare 1:1 conversation with the model for verifying the backend. Once the
  agent layer exists it runs as a real agent:
  `--kind gm|npc|player|storyteller` selects the role prompt and tool set,
  and the full context assembly and compaction machinery is exercised - so a
  REPL session is a faithful stand-in for in-game behaviour, not an
  approximation. Rust-side tool handlers execute for real against the
  sandbox world's state.
- **Fork:** `cairnworld chat --fork <agent-id> [--at <seq>]` copies an
  existing agent's history (from any world, up to an optional seq) into a
  sandbox agent and drops into the REPL at that point. The source world is
  untouched. This is the shortcut for "get me an agent in exactly the state
  where it misbehaved, and let me poke it."
- **Replay:** `cairnworld replay <inference-id> [--prompts <dir>]`
  reassembles the recorded verbatim input via the reconstruction machinery
  and re-runs it, printing old and new output side by side. With `--prompts`,
  segments whose hash matches a current repo prompt file are substituted with
  the edited version. This is the core prompt-iteration loop: find a bad
  exchange in dev mode, edit the prompt file, replay until the output is
  right, commit. Replay depends only on the recording layer, and doubles as
  the living proof that reconstruction works.
- REPL niceties: `/undo` (drop the last exchange and re-prompt - i.e.
  delete-and-retry for interactive prompt testing), `/tools` (show the
  assembled tool definitions), `/context` (dump the exact input that will be
  sent next).

Access paths: humans use the stdio REPL; coding agents get the same verbs -
fork, replay, chat-as-agent, plus the debug-spine queries - through the MCP
server (stdio transport for local Claude Code/Codex; rmcp also offers HTTP if
a remote agent ever needs it). Same functions underneath, two transports.

# MCP for coding agents

An RMCP server (own subcommand or enabled under `serve`) exposing:

- the same game tool surface an agent sees (act as any agent in a dev world)
- read access to the debug spine: sequences, inferences, reconstruction,
  chat histories, game objects

This reuses the tool registry and store queries verbatim - it is a thin
transport, not a second implementation.

# Off-the-shelf dependencies

Fewer lines of ours, chosen once here so nothing gets reinvented mid-build:

- `mistralrs` - in-process GPU inference (GGUF)
- `axum` (+ built-in websockets), `tokio`, `tower` - server
- `leptos`, `leptos_axum`, `cargo-leptos` - frontend (SSR + hydration)
- `sqlx` - compile-time-checked SQL, migrations, sqlite pool
- `openidconnect` + `tower-sessions` (sqlite store) - Google login, sessions
- `serde`/`serde_json` - all message/tool/export payloads
- `schemars` - derives the JSON schema for each tool's argument struct from
  the same type serde validates against, so tool definitions and validation
  can never drift apart
- `rmcp` - official MCP SDK (stdio + HTTP transports)
- `anyhow` - error propagation with accumulated context
- `clap` (derive) - CLI subcommands
- `rand` - dice
- `blake3` - content-addressed text hashing
- `tracing`/`tracing-subscriber` - server logs (game telemetry lives in
  sqlite, not logs)
- `insta` - snapshot tests for context assembly (review prompt-affecting
  diffs explicitly)

Token counting for the compaction trigger uses the loaded model's own
tokenizer through mistral.rs - no separate tokenizer dependency, no
estimation drift.

# Game layer

The framework above exists to serve the game design in user_declarations.md.
Each subsection below is a design pass still to be made - expand it here
before planning its implementation. The listed pointers are the governing
sections of user_declarations.md.

## Actions and rules (TODO)

The full character/NPC/GM/Storyteller tool sets over the framework's tool
registry, rule packets from the Cairn SRD, item transfer invariants
(Give/Take through rust so items cannot duplicate), BePersuaded, save
mechanics. (Tool calls; GM interaction.)

## Turns, time and combat state (TODO)

Per-character time advancing to world time, InCombat/Moved/Acted flags,
rust-defined `can_act` rules (never GM-decided), combat begin/end, initiative,
out-of-combat simultaneity limits. (Turns and time.)

## World initialization (TODO)

Storyteller/Questioner iterative world building: phased meta-questions,
answer-then-summarize loop, initialization tools for locations/paths/NPCs,
per-object Questioner enrichment. (Game setting and story narrative;
Storyteller initialization output and tools.)

## Character creation (TODO)

Player agent guiding Cairn character creation; Storyteller background
negotiation with spoiler scrubbing; the ReadyToBegin/RollOmens sync point.
(Character creation; TODO section of user_declarations.md.)

## Encounter difficulty (TODO)

GM requests guidance, Storyteller sets composition, GM adjusts
attributes/equipment as needed. (GM interaction.)

## End conditions and epilogue (TODO)

UpdateStoryteller event summaries, EndWorld, the epilogue talk-only mode,
ReadyToEnd, final narration. (End Conditions; Epilogue sequence.)

## Multiplayer party travel (TODO)

Group Travel with stay-behind prompts and timeouts. (Character actions.)

## Dynamic storyteller and spells (TODO, future)

NPC/location/path mutation mid-game; Read mind / Command / Erase mind
operating on chat histories. (Dynamic Storyteller; Spell ideas.)

# Open questions

1. **Model.** Final pick between Qwen3 8B, Hermes (Llama 3.1 8B) and Llama
   3.1 8B Instruct happens in the bake-off on evidence from our real tool set
   (see Model selection).
2. **Second backend timing.** The OpenAI-compatible HTTP backend is cheap and
   useful for debugging comparisons; proposal is to add it alongside the
   bake-off, when tool-calling behaviour is being compared across models
   anyway.
3. **user_declarations.md tech stack.** It lists neither Leptos nor sqlx.
   Suggested addition once these decisions settle (design.md must not
   contradict the declarations).
