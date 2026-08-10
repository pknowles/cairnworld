# Infrastructure to Bread Thief

Status: in-progress (2026-08-09) - milestones 1-3 complete; Milestone 4's
durable deferred-compaction/scheduling follow-up is in progress; milestone 5
planned, not started; milestones 6-9 not started. The model choice deferred
from milestone 3 is still open.

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
Each gets more detail added here as an agent begins it, per the
implementation loop in AGENTS.md - a milestone does not need to be fully
specified up front.

1. **Inference REPL.** Complete. `llm` layer with the mistral.rs backend;
   `cairnworld chat` runs a conversation in the terminal. Detailed below.
2. **Chat history + recording.** `store` layer: `world`/`agent`/`message`
   tables plus content-addressed `text` and `inference` recipe rows; context
   assembly as the single recorded path to the model; `cairnworld replay`.
   Detailed below.
3. **Tool calls + three working models.** Complete. Schemars-derived tool
   definitions assembled per agent invocation + agent loop with the GM's
   `save`; exercised from the REPL against Qwen3 8B, Hermes 3 (Llama 3.1 8B)
   and Llama 3.1 8B Instruct, each selectable by name from configuration.
   Detailed below.

   Scope changed during the work. The milestone was to produce comparison
   evidence for the model choice, but making all three run at all consumed it:
   three defects had to be found and fixed first, one of which silently
   dropped every tool call from the prompt and invalidated any measurement
   taken before it. Comparing models on one toy tool with no GM would measure
   the harness, so the comparison moves to a milestone where there is real play
   to measure - most likely 7, once Bread Thief is playable. Keeping three
   working models is the durable result; the choice stays open.
4. **Compaction.** `summary` table; compaction with artificially low
   thresholds. Verifies: a summary covers exactly the range it was given,
   reconstruction still holds across compaction, and the live context for an
   agent becomes newest summary plus the messages after it.

   Two flags planned here have been dropped, neither traceable to
   user_declarations.md. `--prompts <dir>` assumed prompts live as files to
   substitute in, when they are assembled from stored text and message rows;
   the declared feature is re-running a stored inference after the code or
   prompts change, which reassembly already does - deferred to milestone
   9/8 in plans/deferred.md, not dropped outright. `--fork` (copying an
   agent's history into a sandbox to poke at) was an unauthorized agent
   addition: no user_declarations.md passage asked for it, and it does not
   belong in plans/deferred.md either, since that file is for declared
   features blocked on a dependency, not invented ones.
   The initial synchronous implementation is followed by durable deferred
   compaction: foreground replies return immediately, persisted jobs survive
   restart, and priority admission runs them in available model capacity.
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
produces a streamed reply.

### Scope

- One crate, `cairnworld`. design.md's `llm` layer: the `Backend` trait and
  the mistral.rs implementation. No persistence, no tools, no web server.
- `cairnworld chat` in its "bare 1:1 conversation" form (see design.md, Dev
  CLI: chat, replay) - no `--kind` flag yet, since that depends on the agent
  and store layers.

### Observations (2026-07-26)

Hardware: RTX 3070, 8GB VRAM. Model: Llama 3.1 8B Instruct, Q4_K_M GGUF
(bartowski quant), loaded via `mistralrs` 0.8.1 `GgufModelBuilder` with the
`cuda` feature.

- **VRAM**: ~5.85GB for the model itself (1.08GB baseline -> 6.93GB loaded),
  leaving limited headroom on this 8GB card for KV cache at longer contexts.
- **Speed**: ~9.7 tok/s sustained over a 200-token completion. Usable for a
  REPL, but slow enough that it's worth watching once tool-call round trips
  and longer contexts (milestone 3+) are in the mix.
- **Streaming**: confirmed genuinely incremental, not buffered - tokens
  arrive roughly every ~75ms rather than all at once at the end.
- **Temperature**: confirmed it visibly affects output (0.0 vs 1.3 changed a
  one-word answer). `mistralrs`'s `RequestBuilder` defaults to
  `SamplingParams::deterministic()`, which forces `top_k = Some(1)` (greedy
  decoding) independent of temperature - setting only temperature on top of
  that default is silently a no-op. `MistralRsBackend` starts from
  `SamplingParams::neutral()` before applying temperature.
- **API surface note**: `Model::stream_chat_request` never emits a terminal
  `Response::Done` - that variant is only sent on the non-streaming
  `send_chat_request` path. The last `Chunk` (the one with `finish_reason`
  set) carries the final `usage`, so `MistralRsBackend::complete` assembles
  the final text and usage from accumulated chunks.
- **CUDA/driver note**: check `nvidia-smi`'s reported CUDA version against
  `nvcc --version` on any new dev machine before debugging inference
  failures as a code problem - a driver behind the toolkit fails at
  model-load with `CUDA_ERROR_UNSUPPORTED_PTX_VERSION`.

### Definition of done

- `cargo build --release` and `cargo test` both pass.
- `cairnworld chat --model <path>` runs an interactive multi-turn
  conversation against the real GPU-loaded model from a terminal, with
  visibly incremental (token-streamed) output.
- The `Backend` trait matches design.md's Inference layer section exactly
  (no extra speculative fields or methods).

## Milestone 2 detail: Chat history + recording

Goal: make chat history the primary persisted data, since agents are
defined as "just chat history" (user_declarations.md, Agents) - the game
cannot function past one process lifetime without it. Recording an
inference becomes a small recipe of references into that history plus
telemetry, not a second copy of it. A REPL completion writes its user
message, runs one recorded inference, and writes the assistant message (or,
on failure, an error record) as part of the same flow - there is no separate
"recording" feature bolted on afterward. `cairnworld replay` loads a
recorded recipe, reconstructs the identical request, verifies its hash, and
submits it through the same backend API.

### Scope

- `store` backed by one SQLite database, WAL mode, migrations.
- Real chat schema: identity-only `world(id, name)`; `agent(id, world_id,
  kind, name)`; `message(id, agent_id, seq, role, content, created_at)`,
  unique `(agent_id, seq)`, `content` as tagged JSON. Content-addressed
  `text(hash, content)` for static prompt pieces. `summary`, `sequence`, and
  game-state rows belong to later milestones.
- `inference(id, agent_id, segments, sampling, output, error, input_hash,
  input_tokens, output_tokens, duration_ms, model, created_at)` - `segments`
  is an ordered recipe (`{text: hash, role}`, `{messages: {agent_id,
  first_seq, last_seq}}` for this milestone; `{summary}`/`{tools}` arrive
  with milestones 3/4), with exactly one of `output`/`error` non-null.
- One assembly function (e.g. `src/context.rs`) is the single place that
  builds a `Request`, computes its recipe and hash, calls the backend, and
  records the outcome - success or failure - referencing the message rows
  it read rather than re-storing them. This realises design.md's "Recording
  is not optional": it is the only code that calls a backend, and the only
  code that knows an input's provenance.
- `chat --database <path>` records every reply through this path. `replay
  --database <path> --model <path> <inference-id>` prints the recorded
  request and old/new outputs, then runs the reconstructed request.

### Data boundary

`Request`, `Response`, and all nested LLM API types have serde derives. An
inference's recorded input is never a serialized copy of the whole request;
it is the recipe above, resolved by reading the referenced `message` rows
and `text` rows fresh. `input_hash` is the BLAKE3 hash of the *reassembled*
request, so a unit test can prove reconstruction is hash-equal without the
schema ever storing the request bytes themselves. A repeated inference over
an unchanged history must add only a new `inference` row (recipe +
telemetry) - never a copy of prior message payloads - which is the
concrete, testable form of "store messages once."

Failed inferences are recorded with their recipe and input hash (so the
exact prompt that produced the failure is reconstructable) and the error
text in place of output/usage. They must be visible in the developer chat
history view. A failed output is never fed back into model context - the
developer view is the union of `message` rows and failed `inference` rows,
not the context-assembly input.

### Steps

This milestone is one commit: the old recording path is deleted and its
replacement built in the same slice, so `RecordingBackend` never coexists
with the new schema. There is no non-disposable existing database - before
rewriting the migration, check for any local `*.sqlite` file that isn't
obviously disposable (e.g. under `/tmp`) and confirm with the user before
deleting it.

1. **Replace the old recording path with the new schema and store.**
   Delete `src/recording_backend.rs` (`RecordingBackend<B>` and its tests)
   and everything referencing it in `src/main.rs` (`mod recording_backend;`,
   the `recording_backend()` constructor helper, and its call sites in
   `run_chat`/`run_replay`). Rewrite `migrations/0001_recording.sql` in
   place (delete-and-replace, not a second migration) with `world`, `agent`,
   `message`, `text`, `inference` as scoped above. `Store::open` opens a
   single SQLite pool, enables foreign keys and WAL, runs migrations,
   returns database errors with context - no automatic fallback.
   Verify: `cargo build` succeeds and `grep -r RecordingBackend` finds
   nothing; a temporary-file test opens a new store and exercises an
   insert/read of each table.

2. **Store operations over real rows.** `append_message`, `put_text`
   (insert-if-absent by hash), `record_inference` (recipe + success-or-error
   outcome), `reconstruct_inference` (resolve every segment, verify content
   hash and `input_hash`, return the deserialized `Request` plus recorded
   output or error).
   Verify: a deterministic multi-turn test via these operations proves a
   repeated inference adds recipe/telemetry only. Corrupt text, a deleted
   message row, reordered `seq`, and a cross-agent reference must each fail
   reconstruction with context. A recorded failure must reconstruct its
   input successfully while reporting its recorded error.

3. **Assembly boundary + REPL wiring.** The context-assembly function
   described above (e.g. `src/context.rs`); `run_chat` drops its in-memory
   `Vec<Message>`, creates a sandbox world + agent, appends the user message
   to the store, and calls the assembly function each turn.
   Verify: multi-turn REPL against the real GPU model; inspect rows to
   confirm each message exists exactly once and each inference row is a
   recipe, not a blob. A fake erroring backend proves the failure path: the
   error propagates to the caller *and* a reconstructable failed-inference
   row exists.

4. **Replay.** `replay <inference-id>` reconstructs via the recipe (this
   works for failed records too - that is the point of recording them),
   prints recorded input and output-or-error, re-runs through the same
   assembly boundary so the replay is itself recorded.
   Verify: replay a mid-conversation inference from step 3's session;
   confirm the replayed record also reconstructs hash-equal.

5. **Document and commit.** Update `implementation_reference.md` - it must
   no longer mention `RecordingBackend`. Run `cargo fmt --check`,
   `cargo test`, `cargo build --release`, and the real chat/replay path by
   hand. Self-review per AGENTS.md, then commit the milestone.

### Definition of done

- No whole-request blobs exist anywhere in the schema: the only copies of
  conversation text live in `message` rows and content-addressed `text`
  rows; `inference` rows hold recipes and telemetry only.
- Every completion through the assembly boundary - success or failure -
  leaves a reconstructable, hash-verified record; a failed inference is
  queryable with its error and reconstructable input.
- A real GPU chat followed by replay has been exercised end to end.
- Documentation matches the code; tests and builds pass; one commit.

## Milestone 3 detail: Tool calls + three working models

Complete (2026-07-30).

Goal: prove the same recorded agent loop can offer a schema-derived tool,
persist the assistant tool call and rust-generated result in its history, then
produce a final response - on every candidate model, not only the one that
happened to work.

The goal changed during the work. It was to produce comparison evidence for the
model choice; what it produced is a harness that runs three models correctly.
Making them run consumed the milestone, and the comparison moves to a milestone
with real play to measure. See "Outcome" at the end of this section.

### Terms used in this plan

- A **tool** is one visible operation offered to a model: its name, short
  description, argument schema, argument type, and Rust code that performs the
  operation. It is not a generic service.
- A **tool list** is the fixed list of tools offered for one agent invocation.
  It is assembled with that invocation's other input. Looking up a returned
  tool name is an ordinary search of this local list; there is no global
  `ToolRegistry`, manager, or controller. The list holds Rust execution code
  and never crosses the wire: it *derives* the serializable
  `Vec<ToolDefinition>` placed in `Request.tools` and recorded in the
  tool-definition segment, and it resolves a returned name back to the local
  operation to run. Only definitions are serialized; the code is never part of
  a request or a record.
- A **tool-call message** is the assistant chat entry containing the model's
  structured call name, arguments, and call id. A **tool-result message** is
  the following tool chat entry containing that id and Rust's result. These are
  persisted messages, not text rendered for debugging.
- A **tool-definition segment** is a reference to the exact JSON tool list sent
  with an inference. It lets replay reconstruct `Request` including its tools
  without storing another complete request blob. It reuses the existing `text`
  table and `Segment` enum - the tool list serializes to JSON, which is text,
  so there is no second store to maintain.

### Scope and design choice

Ordered so each step is verifiable on its own and none builds rows that a
later step immediately produces for real. Persistence and the loop are one
step because a stored call/result history has no source until the loop
exists - splitting them would mean hand-writing histories only to test them,
then writing them again from the loop.

1. **Backend protocol support.** Teach the mistral.rs backend to send the shared tool
   definitions and return the shared structured calls. It
   must assemble streamed tool-call deltas as well as streamed text. CLI
   parsing selects a complete local GGUF configuration rather than accepting
   unrelated optional flags.

   *Reasoning mode.* Qwen3 is a hybrid reasoning model and mistral.rs 0.8.1
   defaults `enable_thinking` to true, so left alone it emits `<think>` blocks
   while Llama 3.1 and Hermes 3 do not. Thinking is a comparison axis (see
   below), so the backend must make it an explicit, recorded input rather than
   an inherited default: `enable_thinking` becomes part of the provider
   configuration, set via `RequestBuilder::enable_thinking(bool)`.

   Reasoning arrives already separated: mistral.rs's `Delta` carries
   `reasoning_content` beside `content` and `tool_calls`, split by its
   incremental tag parser, which handles tags spanning token boundaries and
   partial UTF-8. So the backend never scans or strips accumulated text - it
   reads a field that is already structured, and `Response` gains a
   `reasoning: String` field (empty when absent) to carry it.

   Reasoning is kept, not discarded. It is model output, and
   user_declarations.md requires games be recorded in full with input and
   output reconstructable; dropping it would leave the inference view unable to
   show what the model actually produced. Two distinct rules, deliberately not
   conflated:

   - *Stored and shown.* Persisted with the assistant message and rendered in
     the developer inference view, collapsed by default.
   - *Never re-fed to a model.* Context assembly ignores it, exactly as it
     ignores failed inferences. Reasoning is scratch work: replaying it costs
     the context budget that prompt_standards.md treats as scarcest, and models
     are trained expecting their own prior reasoning to be absent.

   Because the structured field is authoritative, a tool call the model merely
   described inside its reasoning is never mistaken for one it emitted.
   mistral.rs reports total completion tokens rather than a separate reasoning
   count, so the comparison records raw reasoning and total completion tokens
   without inventing a split that the backend did not measure.

   This step is independently verifiable: it turns a `Request` carrying tools
   into wire bytes and a streamed reply back into `Content::ToolCalls`, with no
   persistence involved.

   Verify: backend fixtures cover ordinary text, one call, multiple calls, and
   fragmented streamed call arguments.

2. **Structured messages, the dice operation, and the agent loop.** One step:
   the schema change, the tool, and the loop that produces the histories the
   schema change exists to store.

   *Message content.* Two distinct concepts, so two types - reusing one would
   admit invalid states:

   - `Content` stays what a *model produced*: `Text` or `ToolCalls`. A tool
     result is not something a model can produce, so it must not become a
     `Content` variant; that would make `Response { content: ToolResult(..) }`
     representable, letting a backend claim to have generated a result Rust
     owns.
   - `MessageContent` is what a *chat entry holds*: text, the model's tool
     calls, or a tool result. `Message.content` becomes this instead of a bare
     `String`. Today it is a `String` while `store.rs` already writes
     `Content::Text(..)` as tagged JSON and refuses anything else on read, so
     the stored and in-memory shapes already disagree; this closes that.

   The overlap is real but small (`Text`, and `ToolCalls` when an assistant
   response is appended), and converting a `Response`'s `Content` into a
   `MessageContent` is the explicit, total step where a model output becomes
   history. "Keep a single definition" applies to one concept written twice,
   not to two concepts that share a variant name.

   Reasoning is not a variant of either. A model reasons *and then* answers or
   calls a tool, so it accompanies content rather than replacing it: it rides
   alongside content on `Response` and on the persisted assistant message; an
   empty string represents no reasoning.
   Making it a variant would wrongly imply a reply that is reasoning and
   nothing else.

   *Call and result pairing.* `Content::ToolCalls` holds N calls in one
   assistant message, and each `ToolCall` already owns both its `id` and its
   `arguments`. Each call becomes one `Tool`-role message carrying that call's
   id, appended in returned order, so a response with N calls appends one
   assistant message and N tool messages with consecutive `seq`. Pairing is by
   id, never by position.

   *The tool-definition segment.* Extend the recipe with the segment defined
   above and reassemble the tool list alongside messages and sampling.
   `store.rs`'s `request_for_segments` currently hardcodes `tools: vec![]`
   while `input_hash` covers the whole serialized `Request` including tools -
   so reconstruction breaks the moment a real tool list is sent. Fixing that
   is the concrete target of this step's reconstruction test.

   *The tool.* Add `save` as the only tool offered by the REPL in this
   milestone - the GM's Save from user_declarations.md, not an invented dice
   primitive. A toy `roll_die` would have inverted the declared rule that
   "rolls are made by rust code, not LLMs": the model would ask for a number
   and interpret it, rather than ask for an outcome and receive a verdict. It
   would also have measured the comparison against a tool surface we intend to
   delete.

   Rust rolls d20 and applies the Cairn under-the-attribute rule; the model
   supplies who, which attribute, why, and an optional difficulty. Tool names
   are snake_case at the LLM boundary (user_declarations.md, Tool calls), so
   `Save` is `save` on the wire. The invocation's tool list supplies its
   definition and finds it by name. Its description is model-facing text,
   written and reviewed against prompt_standards.md before the comparison runs.
   The REPL agent's system prompt is fixed for the comparison and recorded with
   it, since whether a model calls a tool at all depends on it.

   The attribute's value is a temporary argument until the `character` table
   exists (milestone 7). It is flagged in the code as such: the model must not
   supply a stat it does not own, since a model that can choose the number can
   choose to pass. Removing it is a breaking schema change that invalidates
   recorded inferences and every test constructing `save` arguments.

   *The loop.* Invoke, persist the returned call, validate and run it, append
   the result, then invoke again for final text - the loop from
   user_declarations.md.

   An invalid call is already preserved as the successful inference's output,
   but appends no invented result and causes no further inference; its
   validation error propagates with context. This is fail-fast for this
   milestone, not the final policy: user_declarations.md's GM rejects actions
   with explanatory text the calling agent can act on, and that flow arrives
   with approve-action in milestone 6. A backend error remains a failed
   inference record through the existing context path.

   Verify: scripted backend responses exercise final text with no tool, one
   valid roll, and multiple valid rolls. The final response must receive and use
   the actual persisted result; reconstructed inputs before and after the roll
   must be hash-equal, over histories the loop actually produced. Missing,
   corrupt, and cross-agent references fail with context, and existing
   text-only histories still reconstruct.

3. **REPL, replay, and llm comparison.** Wire `chat` through this real loop and keep
   `replay` at the existing inference boundary, so each replay reuses the exact
   recorded tool definitions and messages. Run the same fixed prompt set,
   schemas, sampling, system prompt, and trial count on Qwen3 8B, Hermes, and
   Llama 3.1 8B Instruct. Record each run in its sandbox database.

   The output is evidence, not a verdict: per model and per case, what it did
   well and badly, with counts over the trials. Structural tool correctness
   (valid name, schema-valid arguments, matching id/result, and final use of
   that result), latency, and VRAM are measured here. Conversational/NPC voice
   and long-context behaviour matter for the choice but are not exercised by a
   dice roll, so this milestone does not claim to measure them. The user reads
   the situations and stats and picks the model.

### Alternatives considered

- Letting mistral.rs invoke callbacks would make the application miss the
  call/result messages that define the agent's history.
- Putting pseudo-tool JSON in a prompt would create a second unvalidated
  protocol and bypass the model's native tool interface.
- A global registry/service would group unrelated tool operations by technical
  category and impose a separate lifetime without owning game data. A local
  invocation-specific tool list provides the required lookup directly.

### Test design

Happy paths: final text without a tool; one valid dice call followed by final
text; several valid dice calls followed by final text; and replay of each
inference in that exchange.

Edges: unknown name, malformed JSON, zero sides, multiple calls with distinct
ids, a mixture of valid and invalid calls, fragmented streamed call arguments,
empty final text, and backend failure before or during streamed output.

Expected outcomes: valid calls have schema-valid arguments, a Rust-generated
bounded result paired with the visible call id, and final text based on that
result. Invalid calls and backend failures stop immediately with accumulated
context; neither produces a fabricated tool result or a later model call.
Every inference input remains reconstructable, while only backend failures are
stored as failed inference outcomes.

Validation options: scripted backend tests exercise the real loop cheaply and
make history/reconstruction observable; backend fixtures test streamed call
assembly; real candidate runs test model behaviour and performance. Together
they catch a loop that merely appends text, a backend adapter that loses ids or
splits arguments incorrectly, and a model that emits plausible-looking but
unusable calls.

#### What a wrong result looks like, and whether the test catches it

Each case below names the plausible wrong implementation, then the assertion
that fails because of it. A case whose wrong result no assertion catches is not
worth writing.

- *Result not actually generated by the tool.* A scripted call with
  `sides: 1` must produce exactly 1 - the one die whose outcome is fixed, so
  the assertion needs no seeding and no test RNG abstraction. A separate call
  with `sides: 20` asserts the result lies in `1..=20`, over enough repeats to
  catch an off-by-one at either bound. Note the model supplies only `sides`,
  never a result, so "the loop echoed the model's number" is not a reachable
  failure; what these catch is a result that is constant, out of range, or
  derived from the wrong field.
- *Loop appends the result but never re-invokes.* Assert the exchange ends in
  an assistant text message and that two inference rows exist for one user
  turn. Asserting only "a result message exists" would pass.
- *Final response ignores the result.* The scripted second response is
  generated from its own input, so assert the second inference's reconstructed
  input contains the tool-result message with Rust's value. Asserting the final
  text mentions a number would pass on a model that hallucinated the same one.
- *Results not attributed to the originating call.* One response carrying
  several calls with distinct ids and distinct `sides` (e.g. 1 and 20, so the
  d1 result is known exactly). Assert every stored tool-result message carries
  its originating call's id and a value consistent with *that* call's `sides` -
  which fails if results are attributed to the wrong call.
- *Streamed argument fragments concatenated in arrival order but parsed per
  fragment.* A fixture splits `{"sides": 20}` mid-token across chunks; assert
  the assembled call parses to `sides == 20`. Per-fragment parsing errors out.
- *Tool definitions omitted from the hash.* Record an inference, mutate the
  stored tool-definition text, and assert reconstruction fails. If `tools` were
  still hardcoded empty, reconstruction would wrongly succeed - this is the
  assertion that pins the `request_for_segments` fix.
- *Invalid call silently dropped.* Assert both that the error propagates with
  context and that no tool-result message was appended - dropping the call
  would satisfy neither.
- *Backend failure recorded as an empty success.* Assert the inference row has
  an error and no output, and that no assistant message was appended.
- *Reasoning discarded, or leaking back into context.* A scripted response
  carrying both `reasoning` and content must leave the reasoning retrievable
  from the stored assistant message - catching a loop that drops it - while the
  next turn's reconstructed input contains the content and not the reasoning,
  catching the opposite error of replaying scratch work into the context
  budget. One fixture, both failure directions.

#### Tests deliberately not written

- No test seeds the RNG to pin a specific roll. That would pin `rand`'s
  internals, which may change across versions without the behaviour being
  wrong, and it would need a test-only RNG seam in production code. The Cairn
  rule gives certain outcomes without either: d20 must roll *under* the
  attribute, so an attribute of 1 always fails and 21 always passes.
- No test asserts the JSON field ordering of a serialized tool definition. The
  hash covers the bytes actually sent, so ordering is already pinned where it
  matters; asserting it separately would fail on a valid serde change.
- No test asserts the exact wording of `save`'s description. It is reviewed as a
  prompt, and pinning its text would make prompt iteration fail the suite. The
  schema test pins the contract that matters: every argument the model must
  supply is described, and the attribute is constrained to the Cairn set.
- No test reimplements the recipe assembly to compare against it. Reconstruction
  is verified by round-tripping through the real store, per coding_standards.md.

### Model comparison method

Candidates, all Q4_K_M GGUF to match the quant already measured in milestone 1,
one loaded at a time - at ~5.9GB resident on an 8GB card, concurrent loading is
not possible, so runs are sequential:

- **Llama 3.1 8B Instruct** - the milestone-1 baseline.
- **Qwen3 8B** - `bartowski/Qwen_Qwen3-8B-GGUF`, 4.68 GiB.
- **Hermes 3 Llama 3.1 8B** - `NousResearch/Hermes-3-Llama-3.1-8B-GGUF`,
  4.58 GiB.

All three are downloaded in `models/` (gitignored), byte-size verified against
the source and confirmed to carry the GGUF magic.

Reasoning mode is a comparison axis, not a setting chosen in advance - whether
thinking earns its latency on this workload is exactly the kind of question
the runs should answer rather than the plan asserting. It applies only where
the model has the mode: reading the `chat_template` out of each GGUF's metadata
shows `enable_thinking` and `<think>` present in Qwen3's 4614-character
template and absent from Hermes 3's (291 chars) and Llama 3.1's, so forcing the
toggle on those two yields no second data point. That gives four
configurations:

| Configuration | Thinking |
|---|---|
| Llama 3.1 8B Instruct | n/a |
| Hermes 3 Llama 3.1 8B | n/a |
| Qwen3 8B | disabled |
| Qwen3 8B | enabled |

Each configuration is recorded with its `enable_thinking` value so runs are
never ambiguous after the fact. mistral.rs supplies total completion-token
usage, not separate reasoning-token usage, so the report records the raw
reasoning text and total completion tokens alongside latency. It must not
invent a separate reasoning-token count.

Prompt set - six fixed user turns addressed to an agent acting as GM with
`save` available. Because `save` is a real game action rather than a dice
primitive, judging *when* a save is warranted is part of the tool's job, so
these deliberately span the explicit and the implied.

1. An explicit save request naming character, attribute and reason.
2. A described hazard implying a save without naming one ("Rook edges along the
   rotten ledge") - tests whether the tool is reached from a situation.
3. Two characters facing the same hazard in one turn - tests multiple calls and
   id handling.
4. A save whose attribute is unstated ("can Rook resist the smoke?") - tests
   whether the model picks a plausible attribute or invents a field.
5. A two-turn exchange: a save, then a follow-up referring to its outcome -
   tests whether the persisted verdict actually entered context, and whether
   the model narrates the verdict it was given rather than one it preferred.
6. A plain conversational turn with no action at all - tests that offering a
   tool does not derail ordinary chat, and that a model does not call
   unprompted.

Session structure: every trial starts in a fresh sandbox world and agent, so
trials are independent. Prompt 5 is the one deliberately multi-turn case, and
both its turns run in the same session.

Ten trials per prompt per configuration at a fixed temperature of 0.7
throughout - a middle setting that leaves the run-to-run variance the
comparison exists to expose, rather than greedy decoding which would hide it.
The exact value matters less than it being identical across configurations and
recorded with the results.

That is 240 model turns (4 configurations x 6 prompts x 10 trials, with prompt
5 contributing two turns), and more inferences than that: every turn where a
tool is called costs at least two inferences, so the recorded row count will be
substantially higher. Runs are sequential since only one model fits in 8GB, and
the two Qwen3 configurations share one load.

Reported per configuration, per prompt: how often a call was emitted where a
save was warranted and withheld where it was not; how often the name and
arguments were schema-valid, including a sensible attribute where none was
named; how often each result carried its originating call's id; how often the
final text narrated the verdict Rust returned rather than a preferred one;
median and worst latency; total completion tokens, raw reasoning when present,
and peak VRAM. Failures are quoted verbatim from the recorded
inferences rather than summarised, since what a model does badly is the part
that decides the choice.

The report is the deliverable. It ranks nothing and picks nothing: it shows,
per configuration, what worked, what failed, and what it cost, so the quality
and latency trade can be judged directly. It does not cover NPC voice or
long-context behaviour - one save exchange exercises neither.

### Outcome

The comparison above was not run. The method is kept because it still applies
when the comparison happens.

Shipped: structured message content, with `Content` (what a model produced)
separate from `MessageContent` (what a chat entry holds), so a tool result
cannot be represented as model output; a tool-definition segment in the recipe,
so reconstruction rebuilds a `Request` including its tools; the agent loop,
bounded by two configurable limits whose breach is an error reaching the user
rather than an agent; `save` as the one tool, taken from user_declarations.md
rather than an invented dice primitive; and three models selectable by name
from configuration, each paired with the chat template it needs.

Three defects had to be fixed before any model could be judged, all found by
reading what the model actually received rather than what the code implied:

1. mistral.rs dropped every assistant tool call from the rendered prompt: the
   builder stored them under a `function` key while the templates read
   `tool_calls`. Models saw an empty assistant turn followed by a tool result
   with nothing explaining it. Llama 3.1 reissued the same call until the chat
   limit stopped it - 8 calls for one question, 1 after the fix.
2. Qwen3 GGUF inference failed whenever the device mapper split layers across
   CPU and GPU, omitting the device move before the final norm that the Llama
   path performs.
3. The Hermes 3 GGUF ships a chat template with no tool support, so every tool
   definition was silently discarded before the model saw it.

1 and 2 are fixed in the mistral.rs fork; 3 is addressed by `templates/`.

Deferred, with reasons:

- **The model choice.** Comparing three models on one toy tool with no GM
  measures the harness. It moves to a milestone with real play - most likely 7.
- **Rendered-prompt storage.** The verbatim prompt is what exposed defect 1,
  and nothing stores it. Costs measured in experiments/0003; the decision
  belongs with milestone 8's inference view.

Known and unexplained, recorded in experiments/0001:

- Llama 3.1's final reply describes the tool and its parameters instead of
  narrating the outcome.
- Hermes 3 emits a `<tool_call>` block with no closing tag, so it never parses;
  deterministic at temperature 0. It also read "Rook has DEX 11" as
  `attribute_value: 1`.
- The Llama 3.1 template quotes tool results before the model sees them. A
  corrected template fixes the quoting but changes no measured behaviour, so it
  is not adopted.

## Milestone 4 detail: Compaction

Goal: an agent's history stops growing without bound. Agents are just chat
history and there will be one per NPC, so this is needed before there are many
agents, not after.

### Scope

- `summary(id, agent_id, covers_to_seq, content, inference_id)`. The live
  context for an agent becomes its newest summary plus every message after
  `covers_to_seq`.
- A `Summary` recipe segment, so an inference that used a summary reconstructs
  through the same path as text, tools and messages.
- Compaction as an ordinary recorded inference: it goes through the same
  context assembly and is recorded like any other, so it gets an inference
  view for free.
- Two configured limits beside the existing bounds: the reported input-token
  count that makes compaction due, and the exact number of newest raw message
  rows to retain. The latter deliberately names messages rather than a
  converted character or token budget: it is a truthful user-facing quantity
  and requires no speculative per-message token accounting.
- The trigger uses the input-token count reported by the ordinary model
  inference that just completed. It is deliberately a threshold passed by a
  regular request, not a pre-flight target and not a second tokenization pass.

### The one hard rule

user_declarations.md: "for the compaction operation, the LLM should not see
newer chats than those being compacted". The compaction request is built from
the range being summarised and nothing newer. Everything else here is tunable;
this is not.

### Evaluation

Deliberately light. A summary has no ground truth, and an LLM is not
deterministic, so there is nothing to assert equality against. What can be
asserted is structural, and what needs judgement is checked by reading two or
three short synthetic chats:

- Structural, in tests: a summary covers exactly the range it was given; the
  assembled context after compaction is summary plus tail and nothing else; an
  inference that used a summary reconstructs hash-equal; compaction fires at
  the threshold and not before.
- By reading, over synthetic chats with known content: the summary does not
  restate the system prompt or the tool definitions, which are sent every time
  anyway and so are wasted tokens. It keeps what a later turn would need and
  drops what was transient.

No attempt to score summary quality. That waits for real transcripts.

### Decisions already made

Do not re-derive these.

- **The compaction prompt is a string constant in the compaction code**, passed
  as a system message the same way `--system` is today. design.md's `prompts/`
  directory does not exist yet and is not part of this milestone; one prompt
  does not justify creating it.
- **A summary's content is plain text**, stored in the `summary` row, not in
  `text` and not as a `MessageContent`. It is not a chat message and never
  appears in the `message` table.
- **Compaction runs after a turn completes**, not in the middle of an agent
  loop. Checking the threshold mid-loop would compact a history that is still
  being appended to.
- **The split point is a `seq`**, chosen so the messages after it are exactly
  the configured raw tail. If only that tail remains, compaction records an
  inline notice and tries again after a later completed turn.
- **Compaction uses the same model as the agent.** There is no separate
  configuration for it in this milestone.

### Steps

Ordered so each one builds and is verifiable before the next depends on it.

1. **Schema and the summary segment.** Add `summary` to
   `migrations/0001_recording.sql` (edited in place, pre-1.0) and
   `Segment::Summary { summary: i64 }` to the recipe. `request_for_segments`
   resolves it by reading the row, exactly as it does for text and tools.
   `store_summary` writes one; `latest_summary` reads the newest for an agent.
   Verify: a recipe containing a summary reassembles hash-equal; a missing or
   cross-agent summary reference fails with context, matching the existing
   checks for the other segment kinds.

2. **Context selects the tail.** `message_segment` returns one `Segment` and
   takes every message for an agent. It becomes `history_segments`, returning
   the newest summary and the messages after its `covers_to_seq` - two
   segments, or one when the agent has no summary yet. This is the live-context
   rule from design.md's chat schema.
   Verify: with a summary at `covers_to_seq = n`, the assembled request is the
   summary followed by messages `n+1..`, and nothing earlier. An agent with no
   summary assembles exactly as before.

3. **Token accounting.** A completed inference already reports the exact
   model input-token count; compaction consumes that recorded value rather
   than tokenizing a second request.
   Verify: the value is persisted with the inference and compaction adds no
   tokenizer call.

4. **Compaction itself.** Given an agent over its exact input-token threshold,
   choose the split point that leaves exactly the configured number of newest
   raw message rows. Build a request
   from the compaction prompt and the range being summarised - the previous
   summary, if any, plus messages up to the split point - run it through the
   existing `context::complete` boundary, and store the result as a summary
   carrying its `inference_id`. The request is assembled from the parts it
   needs, so nothing is truncated or restored.
   Verify: the recorded compaction inference contains no message newer than
   the split point - the one rule that is not tunable. Compaction fires above
   the threshold and not below. The summary's `covers_to_seq` equals the split
   point. Record an inline, non-model-facing chat notice for each compaction
   or no-eligible-history condition.

5. **Exercise and read.** Run the REPL with thresholds set low enough that a
   short conversation compacts, against a real model. Read the resulting
   summaries.
   Verify: the summary does not restate the system prompt or the tool
   definitions; the conversation continues coherently across the compaction
   boundary; `replay` on both the compaction inference and a later inference
   reconstructs.

### Deferred scheduling follow-up

The synchronous implementation above establishes compaction's data and
context invariants. Before the web milestone uses it, complete the following
replacement so waiting for a summary does not delay a player reply.

1. **Persist intent with the completed reply.** Add one pending-compaction row
   per agent. Writing an assistant's final text response and inserting its
   compaction job is one transaction. The job stores the triggering message,
   its exact reported input-token count, model and sampling. It is only created
   once the threshold has been passed; jobs are not inferred from current
   history on restart.
   Verify: an interrupted process leaves a job that a fresh application
   instance can load, while a below-threshold reply leaves none.
2. **Run and finish a persisted job.** The job reconstructs the same eligible
   compaction range as above. A successful summary, its inline notice, and
   removal of the job commit together. A job with no older eligible history
   commits its explanatory inline notice and removal together. A failed model
   call leaves the job pending and exposes the error to the initiating/developer
   surface; it is not silently discarded.
   Verify: a crash-equivalent uncompleted row is retried, and no summary or
   notice can appear without the corresponding job becoming complete.
3. **Admission policy.** Mistral.rs retains responsibility for batching its
   sequences. Cairnworld limits admissions to
   `limits.max_concurrent_inferences` (default 4): foreground chat work wins
   capacity; queued compaction is admitted greedily only when foreground work
   is absent. A later foreground request to an agent with a pending job waits
   for that agent's earlier compaction, preserving its history order; other
   agents remain independent.
   Verify with a controllable backend that foreground work overtakes deferred
   work, idle capacity runs deferred work, the cap is never exceeded, and a
   same-agent reply cannot use history before its due compaction.

### Nothing is ever removed

Messages are never deleted, edited or moved by compaction. A summary is a new
row that changes what future context assembly *selects*, nothing more: the
database keeps every message, before and between summaries, indefinitely. An
agent stops seeing history before its newest summary; the developer views and
replay still reach all of it.

user_declarations.md says the same: "the database keeps every message forever
and a summary only changes which of them are selected for future context". Its
Before/After lists are what the model sees, not what is stored. The debug
viewer is expected to show summaries inline in the full history, which only
works because none of it is thrown away.

## Milestone 5 detail: Webserver + UI

Goal: the same recorded agent loop, reached over a network instead of a
terminal. A friend logs in with a Google account on another machine and has a
real chat exchange with the model through a browser. Everything the REPL
already exercises - context assembly, tools, compaction - is reused verbatim;
this milestone adds the `web` layer from design.md and nothing to `agent` or
`store` beyond what serving a browser client requires.

### Scope

Per the plan skeleton and design.md's Web server and UI section: Axum +
Leptos (SSR + hydration via `cargo-leptos`), Google OAuth2 login, the landing
page, one world's detail page, and a websocket chat page against one player
agent. This milestone is the transport and page skeleton, not the full
world-detail feature set user_declarations.md describes - the cuts below are
listed with the same weight as the design.md-driven ones, since several of
them remove behaviour user_declarations.md states directly, not just
design.md elaboration.

Out of scope, deferred with reasons:

- **Dev mode.** Design.md's split view depends on the sequence spine
  (milestone 6) and inference/game-object browsing (milestone 8). Building it
  against a GM-less chat would mean building it twice.
- **The GM.** design.md's `game` layer (actions, dice, Cairn rules) is
  milestone 6/7 work. Milestone 5's chat page talks to a bare agent exactly
  like `cairnworld chat` does today - the `--kind gm|npc|player` role
  selection design.md describes for the CLI is also not here yet, since there
  is only one kind of agent to be. This also covers the declared
  GM-narrated join/leave behaviour (arrival narration, and the 1-minute
  disconnect timer with consolidated leave narration and the "who else in my
  party is here?" query) - none of it can exist before the GM does, so it
  is cut here rather than separately.
- **can_act / turn gating.** design.md's websocket protocol includes a
  `can_act` flag for combat/turn-order gating. Nothing produces turns yet
  (milestone 6), so the flag does not exist; the send button is never
  greyed out in this milestone.
- **Streaming token deltas to the browser.** design.md describes pushing
  partial narration once the model commits to final text. The REPL's
  `on_token` callback already proves streaming works end to end; wiring it
  through a websocket is a UI-polish increment on top of a working
  non-streamed round trip, not a precondition for one. Ship the full-message
  round trip first (verifiable: a friend gets a reply), add streaming after
  if the wait is felt in practice.
- **Invitation links, player roster tree, and shortcut Join buttons.**
  user_declarations.md (World detail page) declares these as core to the
  page: unique invite links with optional slot limits, deletable at any
  time; a tree of joined players and their characters; a Join button per
  player to enter with their character; the owner removing joined users.
  None of it exists yet because there is nothing to join *as* - milestone 5
  has no `character` table (that arrives with milestone 7's game state
  schema) and no multiplayer concept beyond one owner per world. Building
  invites onto a single-player world would be built again once characters
  exist to join with.
- **World status and the logged-out recap.** user_declarations.md (World
  detail page) declares an in-progress/complete status and a per-player
  recap written by that player's agent, generated with deferred priority
  once 60 seconds have passed since the player last logged out - a durable
  queued job, not a live in-process timer, so it must survive a server
  restart. Milestone 5 has no deferred-job queue at all yet (compaction's
  is the closest precedent, and even that is milestone 4's in-process
  version); building the recap job now means building the queue twice once
  a real one exists. Deferred rather than faked with a placeholder that
  would need rebuilding.
- **The "Adventurer" auto-created character on join.** user_declarations.md
  (World detail page) declares that joining a world auto-creates a
  placeholder-named character, separate from the player's agent, whose
  stats are filled in through character creation. Milestone 5 has only the
  `agent` row (the chat entity) - no `character` row, no character
  creation, since design.md's Game state schema is milestone 7. Step 4 below
  creates an agent, not a character; this is not the declared join flow, and
  is called out again there so it is not mistaken for it.

### Steps

Ordered so each is independently buildable and verifiable, and so nothing is
built against a mocked version of something the next step builds for real.

1. **Axum skeleton + Leptos wiring, no auth, no chat.** `cairnworld serve
   [--database <path>] [--port <n>]` subcommand in `main.rs`, alongside
   `chat`/`replay`. `cargo-leptos` project structure (client/server feature
   split per its axum-integration convention), one Leptos component: a static
   landing page with placeholder text. `leptos_axum`'s router integration
   serves it.
   Verify: `cargo leptos build` and `cargo leptos serve` succeed; the landing
   page loads in a browser at `localhost:<port>` with hydration active
   (a trivial client-side interaction, e.g. a counter, proves WASM loaded -
   deleted once the real page replaces it).

2. **Google OAuth2 login + session.** `openidconnect` for the Google OIDC
   flow, `tower-sessions` with its sqlite store for the session cookie, both
   against the existing `Store`'s database file. `world` table gains no
   columns here; a new `user(id, email)` table is identity-only, matching the
   `agent` table's existing shape - accounts keyed by email per
   user_declarations.md, no password storage. Client id/secret and redirect
   URI come from a gitignored config file per design.md's Auth section, read
   through the existing `settings.rs` layering.
   Verify: logging in from a real Google account redirects back
   authenticated; the session cookie survives a page reload; an unauthenticated
   request to a page requiring login redirects to `/`; a second browser
   profile logging in with a different Google account gets a distinct
   session and `user` row.

3. **Landing page: name, world list, create world.** `/` renders differently
   logged out (login button only) vs logged in (name field, list of the
   user's worlds, create-world form with the optional hidden-by-default seed
   prompt). Leptos server functions back the name change and world creation -
   no hand-written JSON endpoint, per design.md's stated reason for choosing
   Leptos. World creation with a seed prompt does not run the Storyteller
   (that is future work per user_declarations.md's initial proof of concept
   scope) - it just records the prompt and creates the world row, matching
   the "hard coded setting, no Storyteller" simplification already adopted
   for Bread Thief.
   Verify: creating a world appears in the list without a reload; the name
   change persists across a session; a fresh account sees an empty world
   list.

4. **World detail page → the player's agent.** `/world/:id`, reached by
   clicking a world from the landing page's list per user_declarations.md
   ("Each world takes them to the world detail page"): world name and a link
   into `/world/:id/play`. Status, recap, and the invite/roster/Join-button
   UI are the declared page but are cut per the reasons above, not silently
   - this step builds only the fragment those cuts leave behind. Visiting
   `/world/:id/play` for the first time creates a plain `agent` row for that
   user in that world if one does not already exist (mirrors `run_chat`'s
   sandbox-agent creation, but keyed by user+world instead of created fresh
   every run). This is deliberately *not* the declared join flow - there is
   no invite/Join step (only the world's own owner reaches it in this
   milestone) and no `character` row is created, only the `agent`; the
   declared "Adventurer" placeholder character and character creation are
   milestone 7 work, cut above.
   Verify: visiting `/world/:id/play` twice for the same user reuses the same
   agent id (check the row count, not just the UI); a second logged-in user
   visiting the same world id gets their own distinct agent (a stand-in for
   multiplayer, not the declared join/roster mechanism).

5. **The chat websocket.** `WS /world/:id/ws`: client sends player text;
   server runs it through `agent::complete` exactly as `run_chat` does
   (same static role prompt for now - a single hard-coded "player" prompt,
   since `--kind` selection does not exist yet - same tools, same store, same
   `Budget`), and pushes back the finished assistant message once the loop
   resolves to final text. No token-level streaming yet (deferred above): one
   assistant message per completed turn, matching design.md's rule that
   player-facing chat only ever renders resolved narration, trivially
   satisfied by not streaming at all yet.
   Verify: two browser sessions (different accounts, or the same account in
   two tabs against two worlds) each get replies addressed to their own
   agent's history, never mixed up; killing and reloading the page
   reconnects and shows prior messages (loaded from the store, not kept in
   server memory); a message sent while a previous turn is still resolving is
   either queued or rejected with a visible reason - not silently dropped or
   raced against the in-flight turn (the world-task-per-event-queue model
   from design.md's Concurrency section is the natural fit, but a
   per-agent lock is an acceptable size-appropriate substitute at this
   milestone since there is no multi-agent recursion yet to make ordering
   subtle - note this explicitly in the commit if taken, since design.md's
   world-task queue is still the eventual shape).

6. **Game page: chat history render.** `/world/:id/play` renders the
   player's existing message history (via the same `Store` reads dev mode
   will later reuse) plus the live websocket feed, one scrolling column with
   an input box - matching design.md's stated minimalism ("one big chat
   history, a box at the bottom to enter text and a send button"). Design.md's
   Web server and UI section states player-facing chat only ever renders
   resolved narration, never raw tool-call syntax (no equivalent sentence
   exists in user_declarations.md itself, which only ever describes what the
   *player's agent* sees or scrubs, not the browser rendering) - so this
   step's rendering must already filter to `MessageContent::Text` display
   only, even though there is no GM yet to make tool calls interesting -
   getting the filter right now means milestone 6 does not have to retrofit
   it under time pressure.
   Verify: reloading the page shows the same history a moment later
   produces; a manually inserted tool-call message row (test-only, via the
   store directly) does not render its raw JSON in the page.

7. **Remote smoke test.** Per design.md's Auth section, a real deployment
   needs only the client secret and db path as configuration - verify that
   holds by having a friend connect from another machine on the LAN (or a
   tunnel) using their own Google account, chat, and get replies from the
   real GPU-loaded model.
   Verify: this is the milestone's definition of done, not a separate check -
   see below.

8. **Document and commit.** Update `implementation_reference.md` with the
   `web` layer entry (routes, auth, session store, the websocket handler)
   mirroring the existing per-layer entries' style. Run `cargo fmt --check`,
   `cargo test`, `cargo build --release`, `cargo leptos build --release`.
   Self-review per AGENTS.md, then commit. If the milestone grew large
   enough that step 1-2 (skeleton+auth) and step 3-6 (pages+chat) naturally
   separated into independently-buildable-and-working states, prefer two
   commits over one - but only if both are independently a working `cargo
   leptos serve`, per the worktree sanitation rule that every commit must
   leave the project bisectable.

### Definition of done

- `cairnworld serve` starts the webserver against a real sqlite database and
  a real GPU-loaded model, with no separate code path from `chat`/`replay`
  for context assembly, tool execution, or recording.
- A user neither of the developers has pre-provisioned can log in with their
  own Google account from a separate machine, create a world, and hold a
  multi-turn chat exchange with the model, with replies indistinguishable in
  content from a REPL session against the same model.
- Every message sent through the websocket is recorded exactly as a REPL
  message is - reconstructable, replayable, visible to `cairnworld replay`
  with no special-casing for its origin.
- No dev-mode, GM, action-approval, or streaming-token code exists yet -
  their absence is a deliberate scope cut recorded above, not an oversight
  found in review.
