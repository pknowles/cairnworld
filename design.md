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
- `cairnworld chat` - interactive agent REPL (see "Dev CLI: chat, replay")
- `cairnworld replay <inference-id>` - re-run a recorded inference, optionally
  with edited prompts
- `cairnworld import-scenario` / `export-scenario` - reusable checked-in
  scenario templates

Layers, each depending only on those above it:

1. `llm` - inference backends behind one trait; every call recorded
2. `store` - sqlite persistence: chat histories, game objects, inference
   records
3. `agent` - context assembly, the agent loop, per-invocation tool lists,
   agent-to-agent call tree
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
    reasoning: String,           // chain-of-thought, empty when the model emits none
    usage: Usage,                // input tokens, output tokens
}
```

`on_token` fires as generation produces output; mistral.rs streams natively, so
this is not extra machinery, just not discarding what the backend already gives
us. `complete` still
returns the same fully-assembled `Response` at the end - recording, tool-call
parsing and the agent loop all operate on the complete response exactly as
before. Streaming is purely an additional, optional view onto the same
generation; nothing downstream of the backend has to change to support it.

The backend is **mistral.rs, in-process**: a GGUF quantized model on GPU,
loaded once at startup. Failure to load is a startup failure (fail fast).
Model choice is configuration (CLI arg / config), not code.

**Model selection.** Stheno v3.3 is a Llama-3-base RP finetune with no native
tool-call template, so it is out as the primary model. Requirements: reliable
tool calling, good conversational/NPC voice, ~8B GGUF, long context.
All three candidates run and are selectable by name from configuration. The
choice between them is deliberately open: comparing them on a single tool with
no GM measures the harness, so it waits for real play (see plans/).

- **Qwen3 8B** - purpose-built tool template. Its GGUF cannot be split across
  CPU and GPU without the fork's fix.
- **Hermes (NousResearch, Llama 3.1 8B base)** - trained for both function
  calling and RP voice, but its GGUF ships no tool template, so one is supplied
  from `templates/`.
- **Llama 3.1 8B Instruct** - native tool template, largest finetune ecosystem.
  Its template quotes tool results and puts tool definitions in the first user
  message alongside the question.

If no single model does both jobs well, different models per agent relationship
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

## Inference scheduling

One loaded mistral.rs model accepts concurrent requests and schedules their
sequences itself. Cairnworld controls admission policy, not model execution:
`limits.max_concurrent_inferences` defaults to 4. Foreground player and agent
requests take priority; deferred work such as compaction uses capacity not
needed by them. A completed reply is delivered before its deferred work runs.
When the cap is full, lower-priority work waits rather than increasing player
latency. This remains a policy setting, so measurement can tune it for the
available GPU without duplicating the backend scheduler.

Deferred work is persisted before it is admitted. Restarting loads unfinished
jobs again, so a completed reply cannot lose its required compaction merely
because the server exits. A job records the model and sampling of the turn
that created it and is written atomically with that reply. Completing a job
atomically stores its result, its developer-visible notice, and removes the
job; a crash while it runs leaves it eligible to retry. A later request to the
same agent cannot overtake its earlier deferred work, while unrelated agents
continue through available capacity.

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

- `agent(id, world_id)` - one row per chat history. Its gameplay role is
  defined by the relationship that owns it, not a duplicated kind string.
- `message(id, agent_id, seq, role, content, created_at)` - `content` is
  tagged JSON: plain text, tool calls, or tool results. `seq` orders messages
  per agent.
- `summary(id, agent_id, covers_to_seq, content, inference_id)` - compaction
  products. The live context for an agent is: newest summary + all messages
  with `seq > covers_to_seq`.
- `chat_notice(id, agent_id, after_message_id, content)` - an inline event in
  a user/developer chat view. It is deliberately not selected for agent
  context.
- `pending_compaction(agent_id, after_message_id, input_tokens, sampling,
  model)` - durable deferred work caused by an already completed reply. It is
  not a derived cache: its presence is the instruction to compact after a
  restart.

## Inference records and reconstruction

Requirement: reconstruct the *verbatim* model input for any inference, without
storing the whole input redundantly every time.

All large strings that feed prompts - role prompts, rule packets, notes/context
packets, compaction instructions - live in one table:

- `text(id, content)` - written once, never updated, referenced by id.

The requirement is that an inference does not store a second copy of the whole
conversation, which the recipe below already achieves. Deduplicating identical
strings is not a goal: it would save almost nothing here and a content hash as
the primary key makes one shared row able to rewrite history for every
inference that references it.

An inference is recorded as a recipe of references plus verbatim output:

- `inference(id, agent_id, sequence_id, parent_inference_id, segments,
  sampling, output, error, input_hash, input_tokens, output_tokens,
  duration_ms, model, created_at)`

`segments` is a JSON array describing the input in order, e.g.
`[{text: <id>}, {summary: <id>}, {messages: [first_seq, last_seq]},
{text: <id>}]`. Reconstruction resolves the references; `input_hash` is the
hash of the fully assembled input actually sent, so a unit test can reassemble
and verify equality for every recorded inference. This is the one hash that
earns its place: it runs over real recorded data and makes any drift between
assembly and recording fail loudly.

A failed inference is recorded, not dropped: exactly one of `output`/`error`
is set, and a failure keeps its full recipe and `input_hash` so the prompt
that produced it reconstructs and replays like any other. Failures are as
interesting as successes when debugging prompts and model behaviour, and an
unrecorded failure is invisible after the fact. A failed output is never fed
back into an agent's context - it is not a `message`, so context assembly
never sees it. Dev mode's chat history renders the union of `message` rows
and non-model-facing `chat_notice` rows in chronological order.
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

## World identity and agent topology

This is the durable ownership graph behind the landing page, world detail page,
and agent calls. It follows user_declarations.md's User Interface: accounts are
keyed only by email; players may change a non-unique display name; a world has
one owner; invitation acceptance grants and owner removal revokes access while
retaining the player association and characters; every joined player has a
player agent; characters can be PCs or NPCs; and the developer view exposes
the Storyteller, GM, and NPC chats.

- `user(id, email, display_name)` holds unique email and a non-unique display
  name. Neither display name nor any game relationship affects login identity.
- `world_owner(world_id, user_id)` names the single creator/owner without
  making a sandbox chat world a partial user world. `world_member(world_id,
  user_id, access)` records one user's current access to one world; removal
  changes `access` but retains the association and characters exactly as
  declared. `member_player_agent(member_id, agent_id)` gives that membership
  its singular player chat history.
- Every `agent(id, world_id)` is only a chat history. Its gameplay
  purpose is determined by a relationship that owns it: `world_storyteller`,
  `member_player_agent`, `location_gm`, or `npc_agent`. A PC belongs to a
  membership through `player_character`; an NPC is played through `npc_agent`.
  Locations contain game state and do not own agents beyond their
  location-scoped GM.

The relation, rather than a string `agent.kind`, is the source of truth. A
`kind` can say an agent is a GM without saying which location it governs, allow
two GMs for one location, or leave a labelled agent unused. Putting nullable
role foreign keys on `agent` has the inverse problem: it makes mutually
exclusive ownership implicit and permits invalid combinations. Small explicit
relationship tables (one Storyteller per world and one GM per location) own
real game data, make cardinality constraints direct, and avoid construction
cycles: create a world, its agents, then their relationships in one transaction.

This accommodates the declaration's open, playtestable design: there may be a
GM for each location, including locations occupied by PCs or NPCs; a later
playtest may consolidate them without changing agent histories or player
identity. It does not pre-decide Storyteller iteration, invitation mechanics,
or dynamic NPC/location creation beyond the relationships those declared
features require.

## Character tool identifiers

Every character has an immutable, globally unique numeric `tool_id`, rendered
to models and tools as `charN`. This `charN` handle is the only character
identifier sent in an LLM-facing tool argument; names are separate
player-facing text and may change. Allocation chooses uniformly from the unused
two-digit range 10 through 99. Once it is exhausted, it chooses from the unused
three-digit range, and widens again only when required. SQLite's single
application connection serializes allocation, so selecting from unused values
cannot collide or require retrying.

The numeric suffix avoids duplicate "Adventurer" and renamed-character
problems. It also keeps `charN` handles short for small models without exposing
allocation order.

# Agent loop

## Context assembly

Every inference input is assembled fresh, in this order:

1. Role prompt - static for the relationship the agent is serving, versioned in
   the repo as plain text files (`prompts/`), loaded at startup, recorded in
   `text`.
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

A tool is one visible operation: its name, short description, JSON schema, and
the Rust code that validates and performs it. Each agent relationship builds
its own fixed tool list from the features it offers. When an agent loop needs to find
 an operation by the model-supplied name, it uses that list directly as a small
 lookup table; there is no central tool registry, manager, or service with a
 separate lifetime. Two mechanics from user_declarations.md shape the tools:

- **Action IDs and approve-action.** When a character agent's tool call needs
  GM arbitration, rust assigns the next action id, stores the validated
  arguments in a `pending_action` row, and forwards id + visible arguments to
  the GM. The GM approves by id (optionally with modifiers) or rejects with
  text. Rust executes the stored arguments - the LLM never re-copies them.
- **Rule packets.** The player agent sees a tool's short description; when the
  call reaches the GM, rust attaches the extended rulebook text for that tool
  (stored in `text`, so recorded like everything else).

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
  output, or the recorded error for a failed one. Where a model emitted
  reasoning, it is shown collapsed beside the output - recorded like any other
  model output, but never re-fed into a later context
- Game objects → current state and notes

An in-progress sequence view shows each open inference's raw token stream
live, verbatim, as it generates - tool-call syntax included. This is the
direct, at-a-glance read on generation speed user_declarations.md asks for
("we need to know" when context size or performance blows up), and it uses
the same `on_token` callback as everything else - no separate mechanism.

All views are plain server-rendered data from the tables above; the sequence
spine means no extra bookkeeping exists only for debugging.

# Chat compaction

Config: `compact_at_input_tokens` (reported completed-inference input trigger) and
`keep_tail_messages` (exact newest raw rows preserved after the summary). When
an agent's assembled context
exceeds the trigger:

1. Choose the cut point `n` so exactly `keep_tail_messages` raw messages after
   `n` remain.
2. Build a compaction input: role prompt + previous summary + messages up to
   `n` + the compaction instruction (what is static and always provided, what
   will be lost, what matters to keep). Newer messages are excluded - the
   agent's history is effectively truncated for this one inference.
3. Run it through the normal recorded inference path; store the `summary` row
   with `covers_to_seq = n`.

The trigger uses the model-reported `input_tokens` already recorded for the
completed inference. It adds no tokenization work and therefore describes the
input that caused compaction, rather than estimating the newly appended reply.
Compaction happens after a completed turn, so there is no background job. If
only the retained tail remains, it records an inline notice and retries after a
later turn; it never interrupts the game for context pressure alone.

# Dev CLI: chat, replay

The fast iteration loop for prompt and agent work. Everything here is a thin
frontend over the same `agent`/`store` functions the game uses - no parallel
implementation - and every inference made here goes through the normal
recorded path, in a dedicated sandbox world so world telemetry stays clean.

- **`cairnworld chat`** - interactive stdio REPL. It remains a bare 1:1
  conversation for verifying the backend and recording path. Real game play
  through the terminal targets an existing player membership, so its role,
  tools, and context come from the same relationships as browser play rather
  than a `--kind` switch.
- **Replay:** `cairnworld replay <inference-id>` reassembles the recorded input
  via the reconstruction machinery and re-runs it, printing old and new output
  side by side. Reassembly uses the current code and the current prompt files,
  so editing a role prompt and replaying shows what the new prompt would have
  produced for that exact exchange - the loop user_declarations.md asks for
  under Debugging and Telemetry: "reference a specific LLM output message,
  replace the history to match the prompt changes and then re-generate".
  Replay depends only on the recording layer, and doubles as the living proof
  that reconstruction works.
- REPL niceties: `/undo` (drop the last exchange and re-prompt - i.e.
  delete-and-retry for interactive prompt testing), `/tools` (show the
  assembled tool definitions), `/context` (dump the exact input that will be
  sent next).

Access paths: humans use the stdio REPL; coding agents get the same verbs -
replay, chat-as-agent, plus the debug-spine queries - through the MCP
server (stdio transport for local Claude Code/Codex; rmcp also offers HTTP if
a remote agent ever needs it). Same functions underneath, two transports.

# MCP for coding agents

An RMCP server (own subcommand or enabled under `serve`) exposing:

- the same game tool surface an agent sees (act as any agent in a dev world)
- read access to the debug spine: sequences, inferences, reconstruction,
  chat histories, game objects

This reuses the same tool lists and store queries verbatim - it is a thin
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
- `blake3` - hashes an assembled request so reconstruction can be verified
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

The full character/NPC/GM/Storyteller tool sets over the framework's
per-invocation tool lists, rule packets from the Cairn SRD, item transfer
invariants
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

1. **Model.** All three run; the pick waits for a scenario worth measuring,
   since one tool and no GM measures the harness rather than the models (see
   Model selection).
2. **user_declarations.md tech stack.** It lists neither Leptos nor sqlx.
   Suggested addition once these decisions settle (design.md must not
   contradict the declarations).
