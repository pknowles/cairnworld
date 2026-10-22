# Design

This document is the desired end state: a structured consolidation of
user_declarations.md plus the implementation-shaped decisions needed to build
it. It contains no progress information - build order and status live in
plans/ (see AGENTS.md). Concrete product proposals
requiring user decisions are collected at the end.

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
occupy no memory. Committed history and world effects survive restart; active
turns finish during graceful shutdown, while a crash can interrupt a turn.

# Code quality boundaries

Architecture checks cover the runtime and UI/shared Rust module trees. Shared
wire/model data stays independent of web and game execution; persistence does
not depend on web/MCP or game orchestration, and game operations do not depend
on their web/MCP transports. Modules group real feature responsibilities and data
ownership. Static checks complement review of those responsibilities.

Measure maintained source duplication and per-function cyclomatic/cognitive
complexity, retaining tests in the analysis scope. The configured limits are 5%
duplicated lines for fragments of at least 5 lines and 50 tokens, and 15 for each
complexity measure. Correct violations through coherent ownership and behaviour
preserving refactoring. Report tool coverage gaps and disputed findings rather
than silently changing the scope or limits. The
[code-quality checks plan](plans/code-quality-checks.md) specifies measurement,
verification and implementation order, under the user's
[scope clarification](user_exerpts.md#2026-10-22-code-quality-checks).

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
    messages: Vec<Message>,      // system / user / assistant / tool / narration roles
    tools: Vec<ToolDefinition>,  // name, description, JSON schema
    sampling: Sampling,          // temperature + truncation, per model
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
The configured candidates are selectable by name from configuration. The
choice between them is deliberately open: comparing them on a single tool with
no GM measures the harness, so it waits for real play (see plans/). Each model
carries its own sampling: a shared block sets sensible truncation and a model
overrides it where its family needs different values.

- **Qwen3 8B** - purpose-built tool template. Its GGUF cannot be split across
  CPU and GPU without the fork's fix.
- **Qwen3.5 4B** - a smaller development candidate requested for evaluation.
  Its native GGUF loader reads configuration, tokenizer, and template assets
  from the original Qwen model.
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

One loaded mistral.rs model accepts concurrent requests and schedules its
sequences itself. Cairnworld controls admission policy, not model execution:
`limits.max_concurrent_inferences` is configurable, initially 1 for the target GPU. At model load, the paged KV
cache reserves `max_context_tokens` for every admitted sequence; this fixed
total input-plus-output capacity, not compaction, determines the KV-cache budget.
`max_output_tokens` bounds one generated reply within that capacity. After a
completed turn, its reported input plus output is the starting size of the next
input; reaching `compact_before_next_input_tokens` queues lazy compaction. The
reply is delivered first, but the same agent cannot run another inference until
its queued compaction completes. Foreground player and agent requests take
priority; deferred work uses capacity not needed by them. When the cap is full,
lower-priority work waits rather than increasing player latency. This remains a
policy setting, so measurement can tune it for the available GPU without
duplicating the backend scheduler.

Before the model becomes ready, compute its GPU memory budget from the loaded
model, context limit and concurrency: model/cache allocations, maximum prompt
workspace, allocator retention and safety headroom. Verify that the additional
memory is available and print the breakdown; insufficient capacity fails model
startup. Runtime allocations must stay bounded by those settings. Reserving the
complete working set at launch is preferred where the backend supports it, but
is not required by the bounded-allocation declaration. Other processes can
reduce available memory after this launch check.

Deferred work is persisted before it is admitted. Restarting loads unfinished
jobs again, so a completed reply cannot lose its required compaction merely
because the server exits. A job records the model and sampling of the turn
that created it plus its static prompt/tool recipe, and is written atomically
with that reply. A capacity rejection inside a tool loop creates the same job
against the already-persisted tool history, then retries only after it settles.
The final completion atomically stores its developer-visible notice and removes
the job; a capacity fallback retains the job while it persists each rolling
summary. A crash while it runs leaves it eligible to retry. A later request to the
same agent cannot overtake its earlier deferred work, while unrelated agents
continue through available capacity.

# Persistence and recording

## Store choice

`sqlx` with sqlite, WAL mode. Not a full ORM (Diesel's DSL and codegen are
more framework than this project needs), but not raw strings either: sqlx's
`query!`/`query_as!` macros check fixed-shape application SQL and its result types
against a real dev database at compile time, mapping rows into plain rust
structs. Type safety where it matters, SQL stays visible, async-native so it
composes directly with axum and the world tasks, and migrations are built in
(`sqlx migrate`). One `SqlitePool` owned by the application.

Normal builds use committed `.sqlx` metadata with `SQLX_OFFLINE` enabled by
default. The quality check migrates an isolated `target/sqlx-schema.sqlite` and
validates that metadata with the matching SQLx Cargo CLI. It never prepares
queries against the running game database.
Genuinely dynamic query shapes retain bound parameters and tests for their
supported forms.

## One record for chat history and Developer Mode

Authority: user_declarations.md, **Agents** (an agent is a chat history),
**User Interface / World game page** (the player's chat), and **Debugging and
Telemetry / Developer Mode** (complete histories, compaction points, identical
inference input/output and links between them). The user's reconstruction
clarification in user_exerpts.md requires one authoritative copy of content,
with references wherever that content participates in another view.

Recording runs for every game whether Developer Mode is enabled or not.
Enabling Developer Mode changes visibility, not what evidence is retained.
Player chat, raw agent history, inference inspection and CLI replay read the
same durable record through the same server-side content resolution. There is
no separate debug transcript or second saved copy of an assembled conversation.

| View | Reconstruction from the shared record |
| --- | --- |
| Player chat, including reload and scrolling | Resolve that character's ordered message references and player-visible notices. Apply player visibility rules; raw tools, other agents' private context and reasoning remain developer-only. |
| Developer raw agent history | Resolve all message references in original order up to the selected historical cursor. Show summaries as expandable inserts at their actual compaction points, while preserving the raw messages they cover. Link outputs, tool results, failures and summaries to their sources. |
| Developer inference input | Resolve that inference's ordered recipe: the exact historical role prompt, dynamic context, tools, selected summary and bounded raw-message ranges, with the original formatting inputs and settings. |
| Developer inference output and turn stack | Resolve the original recorded output or error and its causal links. A reply in chat and its inference output refer to the same content; a summary and its compaction output do too. |

An historical cursor denotes recorded event order, not a guessed timestamp or
matching message text. Preserve both the history range a summary covers and
when it became available/was used: those can differ when compaction is deferred.
The recipe identifies which summary each inference actually used. Later
messages, compactions, edits to world notes, configuration changes and restarts
cannot change any earlier view. Historical prompts retain the context actually
supplied; rebuilding the entire world's state at every instant is not required
for this chat/inference guarantee. Object inspection labels current state.

## Chat schema

Authority: Agents, Chat compaction, Debugging and Telemetry and Developer Mode.
All tables below live in the existing SQLite store. IDs are ordinary integer
primary keys. Every insert of a history entry and its typed payload is one
transaction; no payload is published before that transaction commits.

| Record | Fields and ownership |
| --- | --- |
| `recorded_content` | `id, body` where body serializes the existing content enum and reasoning once. No uniqueness constraint on body. |
| `text` | Immutable prompt, tool-schema and formatter artifacts, reused by equality. Ordinary conversation text is in recorded_content, not interned here. |
| `history_branch` | `id, agent_id, next_position`; positions are allocated by UPDATE/RETURNING in the insertion transaction. |
| `history_parent` | `branch_id, agent_id, ordinal, parent_branch_id, through_position`; primary key `(branch_id, ordinal)`. Immutable bounded references. Parents belong to the same agent and predate their child, enforced by composite foreign keys to `history_branch(id, agent_id)` and `parent_branch_id < branch_id`. |
| `agent_history` | `agent_id PRIMARY KEY, branch_id UNIQUE`; the single writable branch of a linear player, NPC, Storyteller or sandbox history. GM heads instead belong to groups. |
| `history_entry` | `id, branch_id, position, kind`; unique `(branch_id, position)`, kind is message, inference or summary. This is the raw history index, not a duplicate transcript. |
| `message` | `id` is also its history_entry ID; `role, content, source_inference_id, source_tool_execution_id, trigger_sequence_id`. `content` is a plain `recorded_content` reference - every message uses its whole content, so no typed selection is needed. Only applicable provenance fields are set. |
| `inference` | Its start has a unique `history_entry_id`; input recipe and outcome remain on inference. Completion updates that row, not the original position. |
| `summary` | Its activation has a unique `history_entry_id`; `inference_id, covered_ranges, reason, retained_messages` stores ordered branch/position ranges of original messages replaced and the compaction metadata. No summary text column. |
| `pending_compaction` | Keyed by branch ID; immutable input head/cut, source invocation, sampling/model and static recipe. Completion inserts a summary activation and deletes the obligation atomically. |

Normal chat completion writes the inference outcome, its assistant message
reference and any due compaction obligation in one transaction. Summary completion
writes the outcome, summary activation and obligation removal together.
Detached replay writes only the outcome. The three producer operations share the
private content insertion code; they do not perform a later copy-and-link step.
History selection resolves one `AgentBranch` containing its world, agent and
branch. Turns, invocation creation, deferred-work admission and capacity recovery
carry that value instead of independently supplied identifiers or an optional
branch selector. Persisted compaction jobs derive the same ownership from their
history branch; replay selects the recorded branch. Group activity is still
validated when topology changes commit. Shared history queries accept the
caller's SQLite connection, including a transaction's existing connection.
Unexpected summary output remains a recorded response with a failed invocation,
not a fabricated successful summary.

Every content reference is a plain `recorded_content` ID: no view needs a typed
sub-selection of a content row, since reasoning is always read alongside its own
content's output (never referenced independently by a different history entry)
and no tool call's argument stands in for narration text (Agent syntax and tool
calls: the GM narrates in plain text after any approval/rejection tool call, not
through a dedicated narration tool). Assistant history uses the inference's
output content ID; GM narration is the GM's own settled reply, so every present
character's delivered copy references that exact same content ID as the GM's
own history entry - not a fresh copy of the text. Tool results have one content
ID shared by their history entry, execution and action. Original JSON arguments
remain intact inside the model output; names, IDs and argument fragments do not
get separate interned rows.

Raw history queries traverse bounded parent branches, then their own entries in
position order. They return typed entries and an opaque cursor containing the
selected head/cut and the next branch/position in that traversal. The cursor is
world-validated. Message-free histories can therefore contain failed inferences;
summary coverage never determines the activation marker's position. The view
updates a running inference in place when its outcome arrives.

Alternatives: unrelated table IDs cannot establish order; a second universal
world event log would duplicate existing payloads/causal records; one typed
history index gives all chat views the same actual order. Branches are plain
parent/range references over that index, not a second history implementation.

## Inference records and reconstruction

Requirement: reconstruct the *verbatim* input and output for every inference,
including compaction, from the shared record defined above.

Prompts, tool definitions, dynamic context and formatting inputs are immutable
recorded content. Reuse their references when unchanged. Preserve a new version
when the actual input changes and cannot be derived exactly from already
retained data. Never regenerate historical context from today's world state.
Necessary source content and causal metadata are not duplicate conversations.
No full assembled prompt, per-view transcript, second copy of model output,
or second copy of summary text is stored for convenience.

An inference owns an ordered recipe of references, effective settings,
model/formatter identity, output content reference or error, measured usage,
lifecycle and causal relationships. The same output reference supplies any
assistant message, summary or narration delivery derived from that inference.
Tool execution links to its original call within that output and its one
recorded result, rather than repeating arguments/results in each view.

`segments` is a JSON array describing the input in order, e.g.
`[{text: <id>}, {summary: <id>},
{messages: {branch: <id>, first_position: <n>, last_position: <m>}},
{text: <id>}]`. Reconstruction resolves the references; `input_hash` is the
hash of the fully assembled application request crossing the backend boundary.
Reconstruction must match that evidence. Dev mode also retains evidence from
the final model-prompt formatting step, described below; the application hash
alone does not prove equality with template-expanded model input.

`input_hash` is an implementation choice for verification, not a user-declared
feature, game identifier, or an ingredient used to reconstruct the prompt.
The recipe reconstructs the request; the checksum detects whether that result
changed. It is retained in production as an integrity check when historical
inputs are inspected or replayed. Tests must also compare directly against the
request observed during inference: a matching checksum cannot prove that the
correct input was recorded in the first place.

An inference is recorded before execution and then completed. For a completed
inference exactly one of its output reference or error is set; running or interrupted work
has an explicit lifecycle state, and a failure keeps its full recipe and `input_hash` so the prompt
that produced it reconstructs and replays like any other. Failures are as
interesting as successes when debugging prompts and model behaviour, and an
unrecorded failure is invisible after the fact. The full failed-inference
record is not a model message. The separate brief interruption-entry proposal
is listed under Product decisions below; it does not permit continuing a failed
invocation. Dev mode's chat history renders messages, non-model-facing
chat notices, and failed inferences in causal order, so a failure appears
inline in the transcript and
opens into its inference view like any other entry.

Because compaction is itself an inference, summaries automatically get the
same record and the same reconstruction guarantee.

Archiving: `inference`, `message`, `summary` and `text` rows are exportable by
date range to compressed JSON for offline retention; out of scope until disk
pressure is real.

## Game state schema

Direct mapping of the game objects in user_declarations.md: `world`,
`character` (PC and NPC in one table, discriminated), `group`, `location`, `connection`,
`item`, plus `gm_notes`/`storyteller_notes` columns (key/value JSON - see
notes editing below) on each. `world` holds time and `next_action_id`. Exact
columns are an implementation detail; the JSON export format is the stable,
checked-in representation (Bread Thief lives in `scenarios/` as importable
JSON).

Notes are stored as key/value pairs per object, per the "actually this sounds
pretty solid" option in user_declarations.md: the editing tool overwrites a
whole value by key, avoiding line-range or paragraph-index fragility.

## World identity and agent topology

Invitation links grant initial entry; membership owns continuing access.
Deleting or exhausting a link never changes an existing membership. The owner
can ban (disable) a member, retaining their association, characters and histories.
Banned members cannot play, inspect the world, or reactivate themselves with
any invitation, including a newly created link. They are hidden from the active
player tree and appear in an owner-only Ban list with a Reactivate control.
Reactivation restores the same membership and characters without an invitation,
a consumed invitation slot, or a newly generated character. The owner cannot
ban themself. No membership depends on an invitation row after acceptance.

The World detail declaration specifies owner reactivation after a ban. The
owner-only Ban list and hiding banned players from the active tree follow the
user's explicit clarification recorded in user_exerpts.md.

This is the durable ownership graph behind the landing page, world detail page,
and agent calls. It follows user_declarations.md's User Interface: accounts are
keyed only by email; players may change a non-unique display name; a world has
one owner; invitation acceptance grants initial access and owner banning disables it while
retaining the player association and characters; every joined player has a
player agent; characters can be PCs or NPCs; and the developer view exposes
the Storyteller, GM, and NPC chats.

- `user(id, email, display_name)` holds unique email and a non-unique display
  name. Neither display name nor any game relationship affects login identity.
- `world_owner(world_id, user_id)` names the single creator/owner without
  making a sandbox chat world a partial user world. `world_member(world_id,
  user_id, access)` records one user's current access to one world; banning
  changes `access` but retains the association and every character. Only the
  owner can reactivate that membership. A membership owns zero or more PCs. Each `player_character`
  relationship owns one PC and that PC's one player-agent history, so a
  replacement or additional character never shares the dead or retired
  character's history.
- Every `agent(id, world_id)` is only a chat history. Its gameplay
  purpose is determined by a relationship that owns it: `world_storyteller`,
  `player_character`, the group's GM history head, or `npc_agent`. A PC belongs to a
  membership through `player_character`; an NPC is played through `npc_agent`.
  Locations contain game state. GM context follows the interacting group,
  including when it moves; location alone does not determine shared history.

The owning relationship determines the role, rather than a separately maintained
`agent.kind`. Construct the world, histories and their owning relationships in
one transaction before exposing them to play.

### Groups and shared GM history

Authority: GM interaction, Game state / Group, Character actions / Travel,
and Developer Mode's GM history DAG. `world_gm(world_id PRIMARY KEY, agent_id
UNIQUE)` owns the world's GM agent. `character_group(character_id PRIMARY KEY,
group_id, description)` replaces character_location. `character_group` is
created with every character. In the development setup, a new Adventurer enters
an existing group at its location; no particular group is required. With the
Storyteller, completion of character creation and the player's readiness lead to
the entry placement described under Storyteller world creation below.
`group(id, world_id, location_id, description, branch_id,
gm_notes, storyteller_notes, retired_at)` owns one GM history branch. A null
retired_at denotes an active group with a writable head; a retired group keeps
its final head for inspection but cannot receive new gameplay entries. Each active
character has exactly one group. Retired groups keep their historical identity
but no character membership or writable head.

`GroupHistory { agent_id, branch_id }` is passed explicitly to context assembly;
agent ID alone cannot select a GM history. Player/NPC calls resolve agent_history.
The call-stack prohibition on re-entering an active agent remains unchanged;
the same GM role cannot recursively invoke itself through a different group.

**Split.** `split_group(group_id, partitions)` accepts a complete, nonempty,
disjoint partition of its character IDs with descriptions. Rust validates exact
membership. Each resulting group gets a new branch whose single parent is the
old head through its settled position. The old group retires. Character and
branch ownership change in one transaction. IDs of resulting groups are returned.

**Merge.** `merge_groups(group_ids, description)` validates distinct nonempty
groups at one location in the same world. Sort source group IDs ascending for
parent order. The resulting group gets a new branch with those bounded parents;
source groups retire. Character membership and the new head commit together.
The resulting GM context includes both histories. No transcript is copied.

**Tool boundary.** Grouping tools produce a typed pending topology change in
`ToolOutcome`; it is applied at the end of the current model-returned tool batch.
The recorded invoking Game Master establishes the world. Group IDs resolve only
within that world into private, validated group values before merge compaction
or mutation. A proposal containing an unavailable or foreign group is rejected
without preparing any of its sources. Compaction and private mutation functions
accept only these resolved values; activity and location constraints are checked
in the commit transaction.
Other calls in that batch execute in returned order against the pre-transition
group, which is the context in which the model generated them. The grouping tool
schema explicitly describes this batch boundary. Each tool execution retains
its outcome independently as it finishes. At batch completion one transaction
validates grouping proposals in call order, creates their result content and
appends all tool-result message references in returned order, then freezes the
source cuts and applies group/head changes. A stale second topology proposal
gets a rejected result; already completed ordinary effects are not rerun.
If execution fails before the boundary, unapplied grouping calls fail and no
head changes; completed ordinary results still get history references during
failure finalization. No half-finished assistant/tool exchange is inherited.

The next GM inference in the same invocation resolves its branch from the
triggering character's current group. For a GM event without a triggering
character, split_group requires an explicit `continue_with_character_id` from
the source group. This binding is recorded on the invocation and visible in its
context, rather than an implicit choice of the first resulting group. A merge
has one resulting head. Source-branch compaction settles before taking cuts;
no database transaction spans inference execution.

**Context order.** Like a git history with merges: every branch reachable from
the head appears once, with the longest prefix any path reaches, in branch
creation order (ancestors always first, since a parent predates its child), and
each branch in position order. Shared ancestors therefore appear once, while
distinct entries with equal text are preserved. Compute this reach from branch
metadata before loading entries; ordinary inference fetches only selected
summaries and the retained raw ranges, not every historical message. A bounded
cut applies only to that branch's own entries. Thus root `[a,b]`, left suffix
`[c,d]`, right suffix `[e]`, merge suffix `[f]` assembles `[a,b,c,d,e,f]`;
wall-clock interleaving never changes it. Labels identify branch/group provenance
without adding another model role. Input recipes record the resulting ordered
ranges, not a pointer to a mutable current head.

**Summaries.** Resolve each summary's covered ranges to original message IDs.
For the selected head, compute the ordered range list above; adjacent ranges
from the same branch coalesce. Match coverage using these range coordinates,
without reading discarded message bodies. A
summary is eligible only if its complete coverage matches a contiguous span of
that stream and it was activated by the selected historical cut. Scan the range list left to
right, replacing the longest eligible span beginning at that position; break
identical-coverage ties by latest activation. Advance beyond the entire span,
so overlapping summaries never repeat their common ancestors. Otherwise emit
the raw message. This same selector handles linear histories.

Before committing a merge, each incoming branch's
unique suffix is eligible for compaction if its raw-message count exceeds
`merge_branch_messages` (default 64). Compact its oldest messages while preserving
`keep_raw_messages` raw messages at the end; if no eligible prefix remains, use
the raw suffix. The count is explicit message units; no
estimated tokens or happy-path tokenization. Shared ancestor material is kept
once. These summaries cover only that suffix through its last settled exchange;
the current grouping tool batch is excluded. Existing summaries may be used
only when wholly inside that suffix. Run these jobs before capturing the final
parent cuts, so their activation entries belong to the inherited history. A summary request never includes messages outside
its coverage. The committed merged context then uses the selector above and ordinary
post-inference compaction can summarize its whole prefix. Exact capacity
rejection follows the ordinary compaction path. All summary jobs and source
cuts are durable before execution, so restart cannot lose the merge's work.

Alternatives: a location-owned history conflates distinct groups and loses
travelling context; copying transcripts loses shared ancestry and duplicates
storage; bounded immutable branches support the declared split/merge behavior
with the existing SQLite content/recipe machinery. The chosen representation
uses no graph database, generic workflow engine or location-handoff histories.

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
   rebuilt each time: e.g. for a GM, its group's location description, group
   characters with sheets, other local groups, GM notes and visible Storyteller
   notes. Never appended to
   history - it is always current, so it never goes stale in the transcript.
3. Selected summaries, marked as summaries of earlier history.
4. Raw history after those summaries: a linear tail for a player or NPC;
   ordered, shared-ancestor-aware branches for a merged GM history.

Each history message carries who sent it and implies who else received it: a
player's own words, this agent's own replies, another agent's message (named),
or a GM narration - which the characters in the interacting group are
understood to have seen. The turn is started by such an event message, not by
the role prompt alone. How these map onto a chat model's message roles is the
backend's concern and follows each model's preference; the assembly only
guarantees the distinctions above survive into the prompt.

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

A malformed or unavailable model tool call is a rejected tool result, persisted
in the same history as a successful result so the agent can correct itself or
explain the limitation. A failure while executing a valid call remains an
application error with its full causal context; it does not masquerade as an
agent-visible result.

Tool schemas are the generation constraint as well as the Rust validation
contract. Every Cairnworld tool explicitly declares an object `properties`
map; a zero-argument operation declares an empty one. The tool boundary rejects
an incomplete schema before inference, rather than repairing it later or
letting a model template interpret a missing map as arbitrary parameters.

- **Action IDs and approve-action.** When a character agent's tool call needs
  GM arbitration, rust assigns the next action id, stores the validated
  arguments in a `pending_action` row, and forwards id + visible arguments to
  the GM. The GM approves by id (optionally with modifiers) or rejects with
  text. Rust executes the stored arguments - the LLM never re-copies them.
- **Rule packets.** The player agent sees a tool's short description; when the
  call reaches the GM, rust attaches the extended rulebook text for that tool
  (stored in `text`, so recorded like everything else).
- **GM narration delivery.** A GM's completed narration is persisted verbatim
  by source reference in each eligible group recipient's history before its
  follow-up inference. It is a distinct narration entry, attributed to the GM
  and understood to have been seen by everyone present, so a follow-up can
  guide the next choice without inventing or repeating the scene. If it fails,
  the narration entry remains durable and the failure still reaches the
  browser.

Dice rolls are rust (`rand`), never the model. Every roll is recorded (see
sequences) so a session is fully replayable as data.

## Concurrency

Authority: Scheduling, Agent syntax and tool calls, Character actions' human
replies. One Tokio task per world processes game events sequentially. An event
runs until its invocation completes, fails or durably suspends for a player
reply. Suspension releases the queue; a later reply/deadline event resumes the
same recorded continuation. Other legal events can run while a reply is pending.
Connection presence uses short-lived state locks; membership writes use short
Store transactions. Both run independently of model work, so a long inference
cannot postpone a disconnect or ban.

Deferred compaction/recap use the existing inference admission policy. A branch's
next inference waits for its own compaction; unrelated branches can use remaining
capacity. Group topology transitions wait for the source histories to settle.
No SQLite transaction is held across an inference. Game mutation
and its result commit together before narration is broadcast. Web and MCP call
this same execution path rather than owning separate world tasks.

# Sequences

The sequence view reads the sequence/invocation/inference/tool_execution/action
relationships specified under Durable execution relationships. Every external
trigger gets one sequence. Nested invocations identify the exact parent tool,
and deferred work identifies its cause. This is the game execution record,
not an additional logging subsystem. Usage totals count each inference once.

# Web server and UI

Axum on tokio, with **Leptos** (SSR + hydration via `cargo-leptos`, axum
integration) for the frontend. Chosen now rather than ported to later: the
frontend scope is already known (chat page, world pages, the data-heavy dev
mode browser), and full-stack rust means the message/tool/inference types
flow through shared serialization types. Raw inspection uses the explicit
JSON endpoints below, also consumed by developer clients; ordinary page forms
use Axum handlers. No second set of equivalent Leptos server-function endpoints
is introduced. Accepted costs: the WASM toolchain and slower frontend
compile turnaround.

Pages and endpoints:

- `/` - landing page (log in, name, world list, create world)
- `/world/:id` - detail page (invites, players, dev mode toggle)
- `/world/:id/characters/:character_id/play` - game page: one chat column for
  the active account's requested owned character, with an input box
- `WS /world/:id/characters/:character_id/ws` - the chat: client sends player text; server pushes chat
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

Runtime environment variables prefixed `CAIRNWORLD__` override the TOML
configuration, with `__` separating nested fields. Development OAuth values
may be exported from a local `secrets/dev.env` file excluded from Git.

## Dev mode

Owner-controlled `world.developer_mode` flag: the owner may enable and later
disable it, each change immediately visible to every active member including
already-connected browsers. The game page splits into two columns; the right
column navigates:

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

### Navigation and visibility

All panels resolve the **One record for chat history and Developer Mode**
relationships above. Opening an inspect link changes the view of existing
recorded content; it never copies the left chat or builds a separate log.


The owner enables Developer Mode on world detail through a warning modal that
explains full internal visibility and requires explicit confirmation. The
modal opens and cancels through native HTML controls, independently of hydration;
confirmation submits the ordinary authorized form.
The owner may later disable the persisted setting. Every active member sees an enabled
indicator on both world detail and play, including already connected browsers.
Every inspection request and live subscription resolves active membership,
the world's enabled setting, and the requested object's world on the server.
Banning ends inspection access as well as play access. Authentication data,
OAuth secrets, and other worlds are never game objects available to inspection.

The left chat remains mounted while the right panel changes. Each player entry,
reply, narration, roll, and compaction summary links to its originating sequence
or inference. The right panel has stable URLs and browser back/forward support:
world overview → sequence → inference; inference → agent history at the relevant
message; history message or summary → inference; object → related agent/history.
On narrow screens the same panes are selectable, preserving draft input and
selection. Direct inspection URLs render the same views and enforce the same
authorization. Lists use stable database cursors, including raw-history infinite
scroll; loading another page never changes or skips the selected record.

The **Turn** view is an ordered execution tree, not just a list of model calls.
It names the triggering event and every actor by game role (player/NPC/Game
Master plus character/group context), then summarizes each inference, tool call
and result, nested agent invocation, action/die, and final response or error in
execution order. Each summary links to its detailed **Inference** view. Sibling
tool calls retain their returned order; opaque call identifiers are not primary
labels. Compaction is linked to its triggering reply and shown as deferred work
when appropriate; it is not silently assigned to the next player turn.

The Inference view presents reconstructed input before output/reasoning, then
errors when present. Model, effective settings, timing, token use, formatter and
integrity evidence live behind the card's top-right technical popover rather
than preceding or duplicating the debugging content. Sequence totals count each inference once; summed
inference duration and wall-clock sequence duration have distinct labels because
deferred work can overlap. Missing usage on a failed attempt is unavailable, not
zero. Raw fields are expandable and copyable without truncation; HTML escaping
must preserve copied text. HTTP JSON is the transport representation for these
views and developer clients, not a pane rendered for humans.

### Browser selection and raw fields

Keep the character game URL and encode inspection in its query string:
`?inspect=turn&id=42&entry=97`, `?inspect=inference&id=9`,
`?inspect=history&agent=3&branch=8&entry=97`, or `?inspect=objects&type=item&id=5`.
A shared `InspectSelection` enum parses/serializes these fields using the browser
URL API; unknown/invalid selections show an error. Navigation uses pushState;
popstate only updates the selection signal and does not push another entry.
Initial hydration reads the URL and fetches the authorized JSON route. This
keeps PlayerChat mounted; the browser's existing history is the only back stack.
Full-page links would discard the draft/socket; adopting a whole client router
for this one island adds another route tree without another routed component.
The small URL-state adapter is the selected option.

Move wire structs for content, inference/turn/history/object responses and
DevEvent into `src/inspection.rs`, exported from the existing library. Gate
Store and backend dependencies to server modules. Both server and WASM use those
structs; the browser never rebuilds model context. Render complete serialized
request, recipe, effective settings and returned content as expandable JSON,
with dedicated message/prompt text fields and exact clipboard text. Object IDs
and typed relationships are links; search filters name/type/ID over paged server
queries. At widths below the existing `sm` breakpoint, Chat/Inspect buttons show
one pane using CSS visibility, leaving both mounted and preserving the draft.

### Durable execution relationships

Authority: Agent syntax and tool calls, Debugging and Telemetry, Developer Mode.
`sequence(id, world_id, trigger, started_at, finished_at, error)` is one external
trigger and its lifecycle. `invocation(id, sequence_id, agent_id, branch_id,
parent_tool_execution_id, cause_inference_id, started_at, finished_at, outcome)`
is one agent loop; branch_id is its starting branch and each inference records
its actual branch. Root invocation has no parent tool; deferred compaction has
its triggering inference as cause. `inference.invocation_id` plus ordinal orders
successive inferences within that invocation.

`tool_execution(id, inference_id, ordinal, result_content_id, status, error,
started_at, finished_at)` uniquely identifies each returned call by
`(inference_id, ordinal)`. Its model call ID/name/arguments resolve from the
original output. Status is running, completed, rejected, failed or unexecuted.
Create rows for every call before executing the batch. Each nested invocation
records this exact parent execution. Each consequential `action` references
its requesting tool_execution, any approving tool_execution, and result content; remove its copied tool/args/result text.
Pending actions retain validated typed arguments only while needed to execute,
and point to the original call. They cannot be approved twice.

The Store transaction is the sole authority for action resolution. It returns
the persisted action ID, decision and result-content reference, whether newly
applied or already resolved. A repeat never re-executes the mutation. The tool
compares the requested decision with that returned decision: matching repeats
report the original result; conflicting requests are recorded tool rejections.
No per-invocation claim flag mirrors durable action state. A later rejection
reason cannot rewrite the original result.

One awaited batch boundary covers tool execution, result persistence and result
delivery. Deliveries put messages into histories, possibly the caller's own, so
they run only after every call of the batch has its result. On failure the
batch preserves already committed results, records the active call's failure
where no result committed, and marks subsequent unreached calls unexecuted. A
delivery failure after commit belongs to the enclosing invocation and sequence;
it does not undo or rerun the action. Failure to persist finalization
is reported with the original failure, with interruption recovery on restart.

An invocation wrapper finishes its own outcome on every ordinary exit, including
schema rejection, execution error and safety-budget exhaustion. Exceptional
process interruption fails its active turn without rolling back committed
effects or replaying actions. Startup closes outstanding tool-call history
truthfully before new model work, preserving original captured inference inputs.
Total tokens/summed inference time aggregate distinct inference IDs;
sequence wall time uses its own start/finish. Pending compaction is shown as
caused deferred work rather than extending the foreground reply's wall time.

### Server shutdown and crashes

Authority: persistent world/chat, visible errors and
[the shutdown decision](user_exerpts.md#2026-10-20-shutdown-and-crashes).
Normal shutdown closes external game-input admission, finishes all accepted
queued and active turns including nested calls and delivery, then finishes
admitted deferred work and joins workers before releasing the backend/database.
Stop starting independent automatic work. Close idle browser streams rather than
wait for their connections forever. Show what shutdown is waiting for; forced
termination is an explicit crash, not an automatic timeout.

Complete chat-turns and boundaries between saved NPC steps are safe stopping
points. Future durable consent waits or not-yet-admitted deferred jobs may remain
pending; no active Rust call stack is saved. A crash fails active turns visibly.
Keep committed actions and messages. For unresolved tool replies, reuse a proven
committed result or record an accurate interruption result without claiming an
unconfirmed action had no effect. Mark unreached calls not executed and close
related pending records. Cleanup is idempotent and must leave valid tool-call
history for the next human turn. No automatic model calls, narration replay or
continuation of interrupted agent stacks occur at startup.

Failures follow the same rule: an error is the result of the thing being
computed, recorded in its place in history (a tool call's result, a failed
inference entry), never an extra entry beside other items. Results an agent can
act on, including rejections, stay in that agent's history and the loop
continues. A system failure fails the turn and every agent's history records it.
The GM's approval result is a side-channel report of the approved action's
outcome; the caller still receives its own result, or the error if the action
errored. Players see only system failures, without Developer Mode: a failed
sequence's `sequence.error` at its trigger message (their own, or one on their
current group's GM branch).

This shared lifecycle applies to player, GM and NPC execution. Admission,
drain order and crash cleanup replace full arbitrary crash continuation.

### Live inspection protocol

Authority: owner visibility toggle, membership access, live Developer Mode and
fail-loudly implementation directives. Keep the existing inspection websocket
and HTTP queries; share the serializable protocol types through `src/lib.rs`.
`Store` publishes `Changed { sequence_id, inference_id }` after relevant commits
and `Token { inference_id, offset, text }` while generating. Offsets are per
inference UTF-8 byte positions and detect lost chunks; tokens are not persisted.

Use the existing per-world broadcast channel and authoritative SQLite access
checks. Subscribe before the initial access query. A single indexed query returns
whether this viewer is an active member and whether Developer Mode is enabled;
reuse that query for HTTP inspection and before each outbound private websocket
frame. Retain viewer/world identity in the connection. Membership/toggle writes
publish `AccessChanged` after committing; it is a wake-up notification, never a
cached authorization decision. On any received event, recheck current access
before deciding what to send. Removed membership closes the connection; disabled
mode sends mode=false, clears cached private client data and permits only mode
notifications. A query failure terminates inspection with its full error.

Authorization takes effect at the access check: a frame already authorized may
finish sending concurrently with a ban or disable, as an authorized HTTP response
may finish. The next frame must pass a new check. Do not hold a transaction or
lock across socket I/O. Generation only publishes events and never waits for a
viewer. A slow send cannot hold up the membership mutation. On broadcast lag,
re-read access and send the current mode before any authorized resynchronization;
a dropped AccessChanged event therefore cannot authorize later private frames.

Alternatives considered against the membership and live-inspection requirements:

| Approach | Benefit | Cost and decision |
| --- | --- | --- |
| SQLite check per private frame, existing broadcast wake-up | One source of truth; ordinary query and socket send; handles lost notifications | One indexed read per frame/viewer. Selected; measure contention with actual streaming viewers during verification. |
| Cached permissions plus a separate watch channel | Avoids per-frame database reads | Duplicates authority and requires every membership path to synchronize cache lifetime and updates. Consider only if measured read cost warrants it. |
| Lock access mutations against socket write admission | Orders write admission against revocation completion | Requires transport readiness/locking machinery, still cannot retract bytes already sent, and adds a stronger boundary than the declarations specify. Not selected. |

Subscribe before loading a selected view. Client increments a request generation
for selection changes; a response applies only to that generation. While fetching,
record matching Changed invalidations; fetch again once if the just-loaded
snapshot was invalidated. Repeat only in response to actual changes. Upsert rows
by durable ID. Inference completion refreshes either a selected turn or that
standalone inference. This reconciles snapshot races without saving a second
world log or assigning unrelated IDs a shared chronology.

On broadcast lag send `ResyncRequired { skipped }`; show a persistent loss notice,
clear partial tokens and reload the authorized selected view. A running
inference starts showing only subsequent chunks with a missing-prefix indicator;
its completed output replaces the partial stream. A disconnected socket shows
the full error and an explicit Reconnect control; reconnect performs the same
subscribe/snapshot handshake. Browser effect cleanup removes handlers and closes
its socket. Bad payloads/serialization fail visibly rather than being ignored.

### Exact model input and verification

An application `Request` and the input consumed by the model are distinct
artifacts. The backend maps narration roles, translates tool calls, applies
effective generation settings, and expands the model's chat template. The
inference panel exposes both the structured request and the final template
input, clearly labelled. Neither is rebuilt using current world notes, prompts,
settings, summaries, templates, or model defaults.

Retain the existing immutable text/summary/message-range recipe for structured
requests. Hash the actual request crossing the backend boundary and require
reconstruction to match. The existing `inference.model` column already
records which configured model served the call, so a later run against the
same game state under a different model is identifiable; no further model
identity is retained. Observe the rendered prompt and token IDs at the actual
template/tokenization boundary used for generation, including special tokens
and the generation prefix. Tests retain these observations temporarily for
direct equality checks; production retains verification checksums and the
source references needed to reconstruct, not duplicate expanded prompts or
token arrays. Observe the normal tokenization pass; do not run another one on
the inference hot path just for inspection.

"Actual model prompt recording" means observing the formatted prompt as the
model runtime consumes it. This is an implementation detail of the declared
verbatim-input inspection and its verification, not a new gameplay feature.
The runtime combines message roles, tool schemas and text using a chat template;
inspecting messages before that formatting step alone cannot reveal formatting
mistakes. The name refers to that actual prompt, not a second model or service.

Reconstruct final template input from immutable recorded inputs and retained
formatter artifacts, and verify it against evidence observed during generation.
First establish experimentally whether the runtime exposes the needed prompt
and tokens, and whether its formatting can be reproduced exactly. A second
handwritten template or test-only renderer is not evidence of equivalence.
There is no approved fallback to storing a full duplicate conversation on every
inference. If exact reconstruction needs additional stored data, measure and
explain that requirement before revising this storage design.
Unavailable legacy evidence is labelled explicitly. A hash mismatch fails the
inspection request with full context rather than showing plausible input.
For a failure before formatting, show the captured structured request and the
failure stage; no model prompt existed to display.

Alternatives considered against the recording declaration and coding standards:

- Store every expanded conversation: simple equality, but duplicates growing
  histories on every inference and violates the requested storage approach.
- Rebuild from current state/templates: small storage, but changes historical
  evidence and cannot support trustworthy debugging.
- Immutable recipes plus boundary evidence: reuses the existing recorder,
  retains small request metadata, and proves equality. This is the chosen
  approach; establish the exact formatter inputs needed before implementation.

Retained model evidence is the existing `inference.model` column — the
configured `[models.<name>]` name that served the call (see
[settings.rs](implementation_reference.md)) — a plain record, not a
content-addressed identity. `inference_prompt.format_id` already references
the recipe text used to render the prompt, and `max_output_tokens` the
resolved output limit; no further settings blob is needed. Reconstruction and
formatting always use the current runtime; tracking historical
formatter/tokenizer revisions across code changes is not a requirement, so
there is no adapter-version dispatch or artifact store to maintain.

### Verification contract

Compare what the browser exposes to an independent capture of the actual
backend request and generation input from the same inference through normal
play. Comparing two calls to the reconstruction function is insufficient.
Cover first entry, tool round trips, sibling and nested calls, narration,
compaction and later tail history, rejected/failed calls, interrupted work,
Unicode/escaping, and changed world context/configuration after inference.
Old inference input must remain unchanged after all later changes and restart.
In the same test, compare player chat and developer raw history at cursors
before and after compaction to the original delivered messages, and compare
summary/inference views to the original model outputs. Verify that appending a
reply, activating a summary and delivering narration to several histories add
references to the original content, not further payload copies. Check both
content identity and exact rendered text: either check alone is insufficient.
Corrupting a referenced message, summary, tool schema, or formatter artifact
must be detected. Verify navigation with deliberately repeated identical text
so linkage cannot pass by matching contents. Authorization tests include a
disabled world, non-owner enablement, another world, and banned membership,
including an already open inspection connection.

All views read these recorded facts on the server. The browser formats and
navigates them; it never assembles an agent prompt.

# Chat compaction

Config: `max_context_tokens` (fixed total input-plus-output capacity),
`max_output_tokens` (the per-inference generated-token limit),
`compact_before_next_input_tokens` (the lazy next-input trigger), and
`keep_raw_messages` (the number of newest raw rows preserved after the summary;
a preference, not a guarantee, as below). A
completed turn's reported input plus output predicts its next input. When that
prediction reaches the trigger, compaction is queued; it must complete before
another inference for that agent, but does not delay the completed reply:

1. Choose the cut point `n` so exactly `keep_raw_messages` raw messages after
   `n` remain.
2. Build a compaction input: previous summary + messages up to `n`, then the
   compaction instruction as the closing request (what will be lost, what
   matters to keep, what is supplied separately). Newer messages are excluded -
   the agent's history is effectively truncated for this one inference.
3. Run it through the normal recorded inference path; store the `summary` row
   with exact covered branch ranges and a separate activation entry.

The trigger uses the completed inference's model-reported `input_tokens` plus
`output_tokens`; it adds no tokenization work. Those are measured units, not
a claim that newly supplied text will fit. The durable background job begins after the
completed reply is delivered. If only the retained raw messages remain, it retires the
job without a history event; a later turn can queue a new one. It never
interrupts the completed game response for context pressure alone.

If an ordinary inference or compaction request is rejected for capacity, the
same durable compaction job runs inline, with two separate fallbacks:

- The agent's own request does not fit and nothing older than the raw messages
  can be summarised, so the raw messages alone overflowed. The job keeps half as
  many raw messages as it tried (down to none) and summarises the rest. Each
  rejection that follows halves again, so a configured count of 32 reaches none in
  at most six rejections. A message count is not a token size and measuring one
  would mean tokenizing, so the rejection itself is the measurement.
- The compaction request does not fit. It tries the ordinary prefix summary; if
  that request cannot run, it retains one more raw message and tries again.

Either fallback logs a warning with the configured and resulting counts, and the
summary records `reason = capacity_recovery` and the count kept, and its
compaction invocation names the rejected inference as its cause, which Developer
Mode links from the compaction entry. They should be rare;
frequent entries mean messages are too long or the configured count too high.
No request is pre-tokenized or predicted. Only Mistral's explicit fixed-KV
capacity rejection starts immediate compaction: it reports the exact requested
token count. A retry must report fewer required tokens than the previous
rejection; otherwise it is a hard error. CUDA out-of-memory is a model/runtime
failure, not evidence that history can be reduced, so it fails directly. If
no raw messages remain to summarise, compaction is also a hard error rather than
a no-op retry.

# Dev CLI: chat, replay

The fast iteration loop for prompt and agent work. Everything here is a thin
frontend over the same `agent`/`store` functions the game uses - no parallel
implementation - and every inference made here goes through the normal
recorded path, in a dedicated sandbox world so world telemetry stays clean.

- **`cairnworld chat`** - interactive stdio REPL for verifying the backend and
  recording path. It is not a player-game transport. Player interaction uses
  the web game's authenticated membership and websocket path.
- **Replay:** `cairnworld replay <inference-id>` reassembles the recorded input
  via the reconstruction machinery and re-runs it, printing old and new output
  side by side. Exact reconstruction uses the original recorded inputs.
  An explicit edited replay substitutes selected prompt/history inputs in a new
  recorded inference and leaves the original intact - the loop
  user_declarations.md asks for
  under Debugging and Telemetry: "reference a specific LLM output message,
  replace the history to match the prompt changes and then re-generate".
  Replay depends only on the recording layer, and doubles as the living proof
  that reconstruction works.
- REPL niceties: `/undo` (select the previous history head and re-prompt,
  preserving the abandoned exchange for inspection), `/tools` (show the
  assembled tool definitions), `/context` (dump the exact input that will be
  sent next).

Access paths: humans use the stdio REPL; coding agents get the same verbs -
replay, chat-as-agent, plus the debug-spine queries - through the MCP
server through the local stdio/socket transport below. Same functions
underneath, with one live execution host.

# MCP for coding agents

Authority: Live Coding Agent Interaction with MCP, Debugging and Telemetry and
An initial proof of concept. The running `serve` process hosts RMCP sessions on
an opt-in Unix socket, `--mcp-socket data/mcp.sock --mcp-user <email>`. The email
must identify an existing account; mode 0600 restricts the socket to the process
owner. It is a local developer transport, not an unauthenticated network endpoint.
`cairnworld mcp --socket data/mcp.sock` copies stdin/stdout bytes to/from that
socket; RMCP's framed protocol terminates inside serve. The proxy never opens
the database or loads another model. The server removes its owned socket path
on shutdown and reports an occupied path instead of replacing it blindly.

Each session binds the configured user once. Every tool resolves membership and
world/Developer Mode at invocation time, through the same Store/Game operations.
The user can operate only characters they own; NPC/GM tools are reached through
the ordinary character-agent loop, not invoked as arbitrary unscoped mutations.
No local proxy changes a user's authorization merely by supplying another ID.

| MCP tool | Existing operation/return |
| --- | --- |
| import_scenario(path) | Parse/validate Scenario and create a fresh owned development world; return world/character IDs. |
| export_scenario(world_id,path) | Authorized existing export of reusable state, excluding private histories. |
| enter(world_id,character_id) | Game::enter via the world task; return sequence and stored entry cursor. |
| chat(world_id,character_id,text) | Game::player_message via the world task; return sequence/result or suspension. |
| history(world_id,agent_id,head,cursor) | The same bounded raw-history query as inspection. |
| inspect_turn / inspect_inference / objects | The same typed Developer Mode queries and authorization. |
| replay(world_id,inference_id,replacements) | Reconstruct originals; explicit text/segment replacements create a fresh recorded sandbox inference, never mutate the source history or execute returned game tools. |

An MCP play session registers a presence connection on enter and releases it
on leave/session close through the same connection events as the web client.
The server can process multiple sessions without a second world executor.
Alternatives: a separate database-writing MCP process violates world serialization;
a second HTTP authentication surface adds machinery for local-only use; the
stdio-to-private-socket transport keeps one execution host and works with local
coding-agent clients. Network MCP access is not part of this implementation.

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

The normal compaction trigger uses the completed model inference's exact usage,
not a second tokenizer pass. Capacity recovery also uses actual inference
outcomes, so there is no separate tokenizer dependency or estimate.

# Game layer

## Membership and presence

Authority: World detail page, World game page, and Game state / Group.
Membership access is `active|banned`. Only the owner changes it; invitation
acceptance returns an existing active membership unchanged and rejects a banned
one before consuming a slot. Reactivation updates that same membership and
creates no characters. Routes are POST `/world/{id}/members/{user}/ban` and
`/reactivate`; WorldDetail renders active members and an owner-only collapsed
Ban list. Both update Store membership and publish AccessChanged after commit.

Each world's `World` owns its action queue, Developer Mode and chat wake-ups,
opening turns, connection IDs and a coalesced set of changed characters. Worlds
are constructed before model loading and when installed, rather than on first
access, so live inspection does not depend on model readiness. The web layer and
`Game` share these same world values. `Game` owns their model-backed worker tasks
and cancels them on drop; workers hold only a weak game reference while idle.
Backend loading finishes before `Game` is constructed. Construction and publishing
readiness share the world-installation lock, so each existing world starts once;
later installations pass their new world directly to worker startup.
Persistence owns no notification channels. The code performing an operation
publishes to its world's channel after committing the durable change. An active
inference retains its IDs and the channel through start, token delivery and
completion; deferred compactions retain that same channel when admitted.
Game::connect/disconnect serialize the durable zero/one transition and connection
set update under the same presence lock. A successful transition adds the
character to the changed set and notifies the world task. These methods never
wait for an ongoing model turn; changed state cannot overflow the action queue.
The world task reads the earliest durable presence deadline and processes it and
changed state between actions. Every event error is logged even if its requester
has disconnected;
narration delivery reads the current set when committing recipient references.
A connection is registered after authentication and deregistered on close,
logout, ban or transport error. Multiple tabs share one character-presence state. Logical
changes are durable as `character_presence(character_id PRIMARY KEY,
last_connected_at, absent_since, generation)`; timestamps are server UTC.
Live connection IDs are process-local and are never restored after restart.

`presence_narration(group_id PRIMARY KEY, last_announced_character_ids,
due_at, generation, waiting_for_return)` records the last narrated online set,
outstanding transition and empty-period phase separately.
Only characters whose creation is ready participate in this protocol: opening
play owns the first narration after `ready_to_begin`, while presence narration
only describes later arrivals and departures.
On first arrival into an already occupied group, queue an immediate GM arrival
turn. On a departure, set due_at to the first departure plus 60 seconds;
subsequent changes within that window share the deadline. Returning before the
deadline changes the final set and removes a cancelled departure. At deadline,
if anyone remains online, narrate the difference and persist the new announced
set with the narration; if nobody remains, retain the difference without running
an inference. First arrival after an empty period sets a new 60-second deadline
and narrates the consolidated old-to-new state then. The world task retains
no duplicate timer map: it queries the earliest active-group deadline on each
wake and waits for it alongside its queue and presence notification. It consumes
the generation-tagged deadline before inference; stale events do nothing and
failed inference is recorded and surfaced without automatic retry. Empty periods
preserve the last narrated set and wait for a return to arm the next window.

At restart, clear all connection sets and timestamp formerly connected characters
as absent at recovery. Retain pending/announced transitions; when a real connection
returns, apply the same empty-group transition above. Split partitions the
announced set and copies pending deadlines; merge unions it and retains the earliest
deadline. Empty results await return; an occupied result with an unannounced
difference arms the configured window. Timer validation and group changes are serialized in the world task; each timer
rechecks the current presence generation and group, so stale events cannot narrate
membership from a retired group. `party_presence()` resolves the caller's group and returns character IDs,
names and online state from this data. It does not mutate group membership.
Narration recipients are NPCs and connected PCs in the interacting group;
offline PCs do not receive it (accepted 2026-09-18, see user_exerpts.md).

All recipient references and the narrated presence snapshot commit in one short
transaction. Generated inference and completed actions remain recorded if delivery
fails. One world chat channel carries only wake-ups or failed sequence IDs;
sockets subscribe once and fetch their own authorized history with a durable
cursor, including after lag or group changes. Failure details are resolved from
the recorded sequence under current membership and group authorization.

## Recap storage and execution

Authority: World detail page's recap after 60 seconds of absence, deferred
priority and durable jobs; Agents / Player agents' separate histories.
The proposed owner is the character (see Product decisions).
`pending_recap(character_id PRIMARY KEY, absence_generation, due_at,
head_branch_id, through_position, static_segments, model, sampling)` is written
with the last-disconnect transition. Return before due_at cancels it. At due_at
it becomes runnable only if the same absence generation is still current.
`character_recap(character_id PRIMARY KEY, inference_id, absence_generation)`
references the completed text output; no second recap payload is stored.

Reuse the existing inference admission with a concrete
`DeferredWork::Compaction(branch_id)|Recap(character_id)` enum and its existing
Notify mechanism. Each variant loads/finishes its own typed obligation; do not
merge different job fields into a sparse generic jobs table. Recap assembly uses
its frozen character history and recorded public character/world context, with
one closing recap request; its output is never appended to model history.
A completed result updates character_recap and deletes that exact generation's
job in one transaction. A newer absence job cannot be removed by an old result.
Return after due_at need not wait for generation; WorldDetail shows the prior
recap and current generation status until completion. Restart reloads both typed
job tables. This reuses real scheduling behavior without treating a recap as a
compaction summary or creating another worker framework.

## Rules and action execution

Authority: Character creation actions, Character actions, GM tools, NPC actions,
Turns and time, and the checked-in Cairn second-edition rules. Rust owns all
rolls and mutations. An action's ToolDefinition and typed argument struct live
with its Rust implementation; character descriptions remain short, and GM
arbitration gets only the relevant rule excerpt, actor/targets, item stats and
current group/location notes. Existing look/say/take and HP/attribute rolls are
extended in place. NPCs use the same character action implementations.

`CharacterSheet` is a serde struct with current/max STR/DEX/WIL, current/max HP,
background, traits, bonds, age and explicit creation progress. Keep creation
progress as a variant `Creating{completed_steps}|Ready`, not magic empty strings.
Use checked-in typed tables for the declared background, traits, bonds and age
rolls and starting equipment. Choosing or rolling a background populates its
items through the same item constructors. Roll functions record the dice and
selected table entry once; replaying an already-completed creation operation
returns its recorded result rather than granting another roll. ready_to_begin
validates every required field. The initial group's ready barrier runs RollOmens
once for the youngest PC and queues the opening GM narration; later entrants get
a separate arrival narration.

Consolidate item_location/item_character into `item_holder(item_id PRIMARY KEY,
location_id, character_id, description)` with exactly one holder FK set.
A transfer validates current holder, visibility, capacity (ten slots, bulky two,
petty zero), and any consent, then changes that row with its recorded action in
one transaction. Full inventory imposes the Cairn HP restriction. Typed item
stats hold damage dice, armor, slots and spell references; object notes hold
fiction rather than substituting for mechanical state.

| Operation | Rust result and GM/NPC responsibility |
| --- | --- |
| Look/investigate/open/ask | GM can answer, request a save/time cost, or reject with a reason. No automatic repeated save. |
| Move/wait | GM supplies destination description and elapsed game seconds; Rust updates position/time or consumes move/action flags. |
| Say/Whisper/lie | Say defaults to all characters in the group; Whisper names its hearers. GM approval may override the audience. Commit the speaker's actual words to recipient histories by source reference, then schedule NPC response opportunities. Lie additionally makes each declared WIL roll and sends the suspicion fact only to those who pass. |
| DoNothing | Explicitly decline an opportunity to respond or intervene; consumes no implied waiting duration. |
| InterventionCheck/Approve/Reject | GM consults selected characters about an unresolved action. Their intentions do not automatically block it. GM can approve a reaction, observe its result, then approve/reject the original action. Tentative actors may pause for an objection; committed actors proceed unless actually prevented. Open-ended dialogue follows after the original action resolves. |
| BePersuaded | NPC initiates a WIL save for the named persuader with its modifier; Rust returns the textual success outcome. No GM round trip. |
| Give/take/drop/place | One holder change after arbitration/consent; no duplicate item on repeated approval. |
| Save/initiative | Rust rolls d20; 1 succeeds and 20 fails; otherwise compare with the relevant modified attribute. Store numbers for player/developer display, textual outcome for agents. |
| Attack/retreat/damage | Attack damage uses weapon dice and armor, HP then STR, critical damage/scars and death per the linked rules; retreat needs DEX and a valid destination. Out-of-combat harm affects the appropriate attribute. |
| BeginCombat/endCombat | GM identifies combatants and state boundary; Rust stores turn order and flags and determines eligibility. |
| SpawnItem/SpawnNPC | GM submits a typed proposal to Storyteller; its approval creates the object through the same validated constructors and records the source. |
| Edit notes | GM/Storyteller tools replace one named JSON value on an authorized object; Storyteller reads its own notes, GM reads both but edits only GM notes. |

`combat(id, group_id, round, current_character_id)` and
`combat_participant(combat_id, character_id, initiative_order)` own the stable
turn order. Character moved/acted flags clear when its new turn begins; wait
can end it early. Closing the game-turn consumes any remaining move/action
allowance; Interrupt and Never mind never reset it or grant a turn. Rust derives
can_act from combat order, death/disabled state,
pending reply eligibility and out-of-combat time. GM cannot set can_act directly.
The proposed initiative order and out-of-combat time window are Product decisions.
World time is the maximum character time. Each character advances by approved
elapsed seconds; time advancement queues hourly distant-NPC opportunities only
for elapsed whole hours, in stable character order, through the world task.

## NPC speech and response rounds

Authority: NPC Interaction, Say/Whisper, GM InterventionCheck, World game page,
Developer view and [the turn/pause clarifications](user_exerpts.md#2026-10-20-npc-speech-interruption-and-game-turn-allowance).

Speech is a completed game event, not a call awaiting an answer. GM arbitration
can determine whether the speaker can speak and override hearers; the character
agent owns the expressive text, including phrasing and descriptions of its own
delivery. Store and show that text unchanged: readers do not manufacture a
"Name says" sentence, strip style or infer identity from text. Speaker identity,
intent, addressee and hearers remain structured metadata. The GM's optional
prefix supplies circumstances of hearing rather than replacing the character's
style. Addressing a character does not make Say private. Whisper supplies
explicit targets; hearing overrides must stay within
the authorized scene; a character cannot be both included and excluded.
Snapshot hearers at commitment, retaining speaker identity and one original
content reference in their histories. All group characters hear ordinary Say,
including PCs whose browser is disconnected. Presence narration retains its
separate connected-PC audience rule. Only actual hearers receive private speech.
Use the committed action result as the shared speech body, with explicit speech
metadata and frozen names for reproducible inspection. Delivery, reload,
context reconstruction and inspection use the same stored expressive text.
Speaker metadata remains available to model context and inspection without a
fixed prose wrapper. New speech uses typed message content containing the authored
text and its recorded attribution/hearing metadata. The backend serializes that
content as JSON in a user-role model message; chat renders the authored prose and
GM prefix, retaining identity as metadata. Legacy speech retains its original
text rendering and request reconstruction. Audience overrides select characters
in the current group; cross-group direct interaction uses the existing merge
operation first.

Once the caller's chat-turn has unwound, hearing NPCs other than the speaker get
response opportunities. Their character facts and heard history supply context;
GM/Storyteller-only notes remain private. NPCs call do_nothing or an offered
character action with eagerness. During collection the call records intent, not an
action. Eagerness is an integer 1–3: low, moderate or strong desire to act now.
Select the highest eagerness; speech-only highest ties may all speak. Their words
were composed against the same history, without hearing one another's replies.
The GM approves/rejects each action separately in any order, through ordinary
approval tools. Finish the selected set before another response round; an earlier
selected utterance does not by itself invalidate its peers. The GM may describe
overlap through each utterance's prefix, only to its actual hearers. Otherwise
choose one tied proposal randomly and record the selection.
Unselected proposals are superseded, not failed or executed tools. After a
committed event, NPCs reconsider against the new history instead of executing
stale replies. All-DoNothing ends the round; unchanged history does not trigger
another round. Recheck actor eligibility, hearers and group topology before
executing any selected action through the ordinary shared character action path.
Collect each NPC decision with one recorded inference and exactly one terminal
do_nothing or real character-action tool call, using the same action schema and
validation as players plus eagerness. Invalid output fails visibly without an
automatic correction loop. There is no separate propose_action interface.
Player authorization stays at the browser boundary; a
private resolved character actor lets PC and selected NPC requests share action
admission without pretending NPCs are players.

After the originating stack unwinds, each selected utterance uses a fresh GM
invocation and ordinary approve_action/reject_action for its presented action ID.
Each approval commits its own utterance and hearing adjustments independently;
a later rejection or failure does not undo earlier speech. Yield to the world
worker between utterances; NPC reply collection waits until the selected set
finishes, but human input can supersede stale remaining intentions. There is no
batch approval tool or all-or-nothing multi-utterance transaction. Repeated resolution
returns that action's recorded outcome without another delivery. GM approval is
mandatory: speaking ability and hearers remain GM decisions. Prefixes and any
additional narration have explicit per-utterance audiences. A combined final GM
reply that has seen several whispers is not broadcast to their union of hearers.
When NPC response rounds are introduced, the GM narrates consequences while
NPC agents supply their own dialogue. This replaces the current GM-voiced NPC
replies and requires real-model prompt verification.

Use concrete durable response rounds in the existing world event loop. A round
records its source tool execution(s), group/history cut, phase, candidate
inference references and selected proposal references. Advance finite steps and
yield to player input between them; do not await the whole discussion inside
Say, an NPC action or the originating world event. A related follow-up is a new
chat-turn linked to the triggering Say/InterventionCheck and proposal execution.
Retain these causal links in Developer Mode, distinct from nested call stacks.
Use an explicit group scene revision for stale-work detection; actual committed
fiction/state changes advance it, inference/proposal bookkeeping does not.
Record round progress in SQLite and wake the worker through its existing
notification mechanism. Do not enqueue work into the worker's own bounded
channel. Foreground events take priority between finite NPC steps.
The causal discussion shares the existing inference/tool budget across follow-ups;
exhaustion is a visible failure, not a fresh budget or a silent dropped response.
During graceful shutdown finish the active NPC step, save its progress and
shared budget, then stop before starting another. Clean restart resumes waiting
work from that boundary. A crash during a step fails its causal discussion
visibly, retaining committed speech/actions and closing outstanding tool history
through shared crash cleanup. Completed steps are not regenerated. There is no
saved agent call stack or per-inference durable budget system.

| Approach | Benefit | Cost / decision |
| --- | --- | --- |
| Nested NPC calls inside Say | Uses the immediate call stack | Re-enters active agents and holds actions open; reject. |
| Concrete response rounds in the world loop | Supports eagerness, stale-proposal cancellation, player input and restart between saved steps | Persist round progress and causal links; selected. |
| New generic workflow engine | Could schedule arbitrary future jobs | Adds an unneeded framework alongside the world loop; reject. |

Interrupt pauses future NPC actions in the character's group at a safe boundary.
Accept the hold even during the requesting character's own foreground turn;
finish that turn normally before acknowledging the safe pause. The hold does
not suspend tools inside that turn or permit re-entry into the player agent.
An in-flight proposal inference may finish recording; it must not publish an
action after the pause is acknowledged. Completed speech/actions remain intact.
The control path can request the pause while the world worker awaits inference;
it cannot merely enqueue a command behind the entire discussion. Active tool
execution commits or fails before acknowledging a safe pause. Human input and
other groups continue through the ordinary world loop. Pause ownership belongs
to each requesting character: Send or Never mind releases that character's hold,
as does its final disconnect or loss of access; other players' holds remain.
Release holds in a retired group or whose character has left that group, and
publish the changed pause state to its viewers. Never mind is an
authenticated button action with no model inference. A pause neither restores
allowance nor authorizes an out-of-turn action. Existing action checks remain
mandatory; full combat enforcement is implemented with the combat rules,
not simulated by a disabled Send button in the NPC experiment.
World owns live per-character holds and watched worker activity. Pause admission
and action-start admission share a short synchronous mutex, released before any
await; acknowledgements wait for an already-active NPC action to unwind. Show
Pausing until then, and catch up the committed chat cursor before showing Paused.
Send reserves a foreground queue slot before atomically enqueueing input and
releasing its character's hold, so NPC work cannot overtake that input.

InterventionCheck collects reaction intentions before an action resolves. NPC
decision calls do not invoke the already-active GM. The GM may approve a proposed
reaction directly, receive its actual result, then decide the original action.
Record these intentions through existing child invocations and pending actions
inside the current turn, not scheduled response rounds. Only subsequent dialogue
uses the world worker's response rounds after the original stack unwinds.
This uses the existing action/roll operations, not a ConditionalApprove tool,
general dependency engine or dynamically generated rule language. Actions that
can be tentative carry committed intent (initially Take, default false): false
means willing to pause for an objection; true means proceeding despite objections,
not guaranteed success or permission to bypass physical opposition or turn rules.
The GM gets this intent in the action request. A prisoner's warning can stop a
tentative Take, but cannot repeatedly veto a committed retry. A shopkeeper may
still physically prevent either attempt using implemented checks/combat rules.

Do not ask an agent already active in the call stack for another inference.
Use its already-recorded intent, or resolve the current call before further
dialogue. Once the bounded reaction/original-action decision finishes, ordinary
conversation runs through scheduled follow-up turns. An idle
human's default non-intervention is not consent to a transfer/travel request
that explicitly requires an answer; those requests use the separate protocol
below. GM routing, response costs and automatic denial require playtesting.
Actual perceived world events may also create NPC opportunities. Use their
recorded sources and authorized recipients, not a dirty flag on arbitrary chat
writes: tool receipts, inference records and private GM notes are not new
perceptions. Expanded action sets and physical/combat reactions follow the
implemented rules; they are not simulated by speech-only tools.

The [NPC interaction plan](plans/npc-interaction.md#design-choices) compares the
alternatives and specifies schema, transaction boundaries, pause protocol,
recovery and complete implementation slices. Full combat remains a separate
rules increment; scheduling controls confer no action allowance.

## Player replies and coordinated travel

Authority: Take/Request, Accept, Travel and Agent syntax's recursion tree.
The action that needs a human response persists
`pending_reply(action_id, character_id, deadline, status, response_content_id)`;
its deadline and intended character are explicit. The corresponding
`pending_action` also holds a typed continuation `Transfer|Travel`, original
invocation/call IDs and the ordered remaining tool-call ordinals. It retains
budget consumption. It never serializes a Rust future or re-executes earlier tools.

The agent loop returns `Suspended(action_id)` up through its call stack; each
waiting invocation remains recorded. The world queue is then free. Only the
requested character's ordinary input is accepted as a response to that action;
their player agent interprets text and calls accept/decline/stay/join/object with
that explicit action ID. Other characters may continue legal independent work.
A response or deadline event updates the pending reply and resumes the stored
continuation once; original completed tool results are reused. Recheck mutable
preconditions before transferring or moving. Restart reconstructs these concrete
continuations from rows and restores outstanding timers.

Travel takes a Connection ID, validates that one endpoint is the source group's
location, and asks the group's other characters to join/stay/object. NPCs answer
through their agents; PCs have the declared one-minute window. The proposed
resolution is in Product decisions. On approval, create the travelling/staying
partition using the group history operation, update the travelling group's
location to the other endpoint, and replace its location description with the
GM's arrival description. Record transfer of location and action completion
atomically. Waiting/objections leave the original location unchanged; rejected
travel can still have an explicitly narrated/recorded movement within the source
location. Group history follows the travellers without a location GM handoff.

## Storyteller world creation

Authority: Game setting and story narrative, Storyteller initialization output
and tools, Character creation and World detail page. `world.phase` is
`initializing|playing|epilogue|complete|initialization_failed`. World creation
stores the optional seed, owner and Storyteller history before queuing work;
only playing/epilogue worlds accept play. Imported scenarios enter playing after
validated installation. `world_build(world_id PRIMARY KEY, phase_index,
round_index, step, question_index, question_inference_id, summary_inference_id)`
is the persisted progress of the three declared phases. Each finished answer or
summary advances one step atomically; a failed inference leaves that step and
full error visible. Resume is explicit after failure. Pending work between saved
steps resumes after a clean restart; a crash during an active turn uses shared
crash cleanup rather than automatically replaying that turn.

The Questioner sees the seed, phase, hard-coded meta-questions, and only the most
recent Storyteller summary. Its structured output is `{questions, satisfied}`.
The Storyteller answers those questions one by one in a fresh round history with
seed and previous summary, then produces a new summary. The next round uses that
summary without replaying previous answers. `world_build.max_rounds_per_phase`
defaults to 3; inference safety budgets still apply. Each phase ends on satisfied
or its round limit. The final named summary is retained by inference reference.
Questioner is an invocation role, not another global autonomous agent.

After the summary, Storyteller tools create locations first, then Connections
with validated endpoint IDs, groups and NPCs one at a time, then items/holders.
Each location/NPC seed gets its own Questioner/Storyteller enrichment using the
world summary. Constructors enforce world-local references and exactly one
character group/item holder. A final validation checks reachability from the
starting location and required object references before phase becomes playing.
Draft objects remain in the initializing world and are not exposed to game play.

Character background negotiation uses three Storyteller proposals stored as
`background_offer(id, character_id, private_content_ref, public_content_ref)`.
Only the public descriptions go to the player agent. A player choice or
counterproposal calls Storyteller with that offer ID; approval commits the
background and relevant private notes. Creation rolls request Storyteller
approval/modifiers through the same action arbitration path. Encounter guidance
returns composition/equipment proposals before those objects are exposed to
players; observed facts are never silently rewritten. Death/replacement checks
real rules state and Storyteller approval; a replacement creates a fresh PC
and agent_history under the existing membership and enters a group through the
normal join flow. The dead character and history remain inspectable.

After character creation is complete and the player confirms readiness, the
Storyteller chooses the exact entry location and group. The character may join
an existing group or enter alone in a new group. Rust validates and applies that
placement before the destination group's GM narrates the entrance. Reopening an
existing character's browser session does not choose a new entry placement.

## Campaign completion

Authority: End Conditions, Epilogue sequence, GM UpdateStoryteller/EndWorld and
World detail page. `update_storyteller(summary, referenced_object_ids)` is an
ordinary nested invocation after consequential events. Typed object trigger
metadata identifies events requiring this update. The Storyteller may call
`end_world(outcome, text)`; Rust also triggers loss when no living PC remains.
The state transition from playing to epilogue happens once with a reference to
the ending narration, delivered to all character histories. NPCs also receive
the declared epilogue notice without proactively taking turns.

In epilogue the tool surface contains conversation and `ready_to_end(text)`;
mutation/travel/combat tools are absent and their server operations reject play
regardless of UI state. Cross-location NPC conversation bypasses travel only in
this phase. `epilogue_choice(character_id PRIMARY KEY, content_ref)` stores each
participating PC's decision; participants are snapshotted when epilogue starts,
so later membership changes cannot silently alter the barrier. Once every
required choice exists, the Storyteller receives them and the ending context in
one final recorded inference. Completion stores its output reference and sets
phase=complete atomically. WorldDetail shows the final account; connected game
pages become read-only and new game entry is refused. Offline/dead participant
handling at this barrier is a Product decision below, not a fabricated timeout.

# Product decisions

The following are concrete proposals pending the user's answers. They are not
implementation instructions until accepted and do not authorize changing
user_declarations.md. All other schema/protocol choices above are implementation
choices derived from the named requirements.

- **Recap:** one recap per character beneath that character on WorldDetail,
  written from its own history after its last connection is absent for 60 seconds.
  The declaration currently says per player despite multiple character agents.
- **Failure context:** full errors remain in the original failed record and
  player/developer display; affected invocations append one brief factual
  interruption message for the next invocation, including unresolved tool IDs.
  No model runs in response to the failure. This reconciles Implementation
  Directives / Chat with immediate propagation and the safety bounds.
- **Combat:** individual stable turns, successes on the initial DEX save before
  failures, higher DEX next, remaining ties randomized once by Rust. This follows
  the game's individual-turn declaration rather than Cairn's simultaneous sides.
- **Travel:** silent PCs join after 60 seconds; stay splits the party; objection
  goes to GM arbitration before movement. No second initiator confirmation.
- **Time window:** propose `max_character_lead_seconds=600` out of combat;
  a character at least that far ahead of the least-advanced connected PC in its
  group waits for catch-up. The GM's elapsed time is bounded nonnegative, with
  long activities gating further action. The declaration leaves this limit open.
- **Epilogue barrier:** propose one choice per PC present in the campaign-end
  participant snapshot, including dead PCs. Disconnection does not auto-consent;
  a world stays in epilogue until those players return and answer. Owner override
  would be an additional feature requiring a declaration.

Capacity recovery follows the currently declared shrinking-prefix summary
algorithm. The older tokenizer proposal in user_exerpts.md does not supersede
that declaration; no capacity-algorithm decision is pending in these plans.
