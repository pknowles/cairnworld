# Storyteller world initialization

Status, 2026-11-09, branch `pknowles/storyteller-questioner` at `f52e6e3` (code as of `c18d8f1`).
Evidence lives in databases and logs under `target/dev/` and `target/tmp/` (not committed).

### Implemented and committed (phases 1 and 2)
- Stages `meta`, `ask`, `name`, `create_objects`, `review`, `failed`, `complete`. World generation has
  no inference limits. A step interrupted by a stop is closed without an error and runs again.
  `World.failure` is set only at stage `failed`.
- Creation: the locations; a reachability step (connections are requested for locations that cannot
  be reached from the first); NPCs one request at a time until a turn makes none (no cap);
  `group_npcs`; an items request. `[[marks]]` in the summary are only a reminder in each request.
- Tools: `create_object`, `update_object`, `delete_object` (object and ID only), `group_npcs`,
  `give_item`; Questioner `read_object` (one object), `mark_object_reviewed`, `ask_questions`,
  `core_objects_created`. Refusals begin "Not created.", "Not changed.", "Not deleted.",
  "Not grouped." or "Not given.".
- A refused plain-text reply is shown in later requests as its first 400 characters and a note.
- World detail page: phase, inferences and objects for every member, updated over
  `/world/{id}/progress/ws`.
- `generate-world --minutes N` (default 5, 0 for none); running it again on the same database
  continues the unfinished world. `serve` and `generate-world` call `close_interrupted_turns` first.
- Test databases and logs are kept until the next `scripts/run-tests.sh`.

### Evidence (measured)
Code and tests:
- E1. 261 unit tests, the quality script and the browser checks (`npm run test:hydration`, including
  `check_generation_progress.cjs`) pass at the code of `c18d8f1`.
- E2. `max_inferences_per_chat = 32` and `max_inferences_total = 256` entered in commits `ab1a96a` and
  `ba38daa` (2026-09-01). `user_declarations.md` does not ask for them.
- E3. A merge attempt of `pknowles/npcs` (aborted, nothing merged) conflicted in `design.md`,
  `plans/agent-tuning.md`, `prompts.toml`, `src/agent.rs`, `src/game.rs` (20 blocks), `src/store.rs`
  (11), `src/tools.rs`, `src/ui.rs`, `src/web.rs` (7), `user_declarations.md`, `user_exerpts.md` and
  `.sqlx`. Migrations `0017` and `0018` exist twice with different content, and both sides drop the
  group description (theirs `0017`, ours `0019`).

Real model (dev-qwen35; 4096 context, compaction 2048, keep 8 raw messages unless stated):
- E4. Saltgate test world (summary written in the test), build `d87c223` plus the delete-tool text
  trim of `2c9f7bc`. Creation: 15 replies, 4,508 output tokens, none capped; 4 locations,
  3 connections, 5 NPCs in 3 groups, 1 item type, 1 item; 3 refused calls.
- E5. Review of that world, same build: `complete` in 9 min 17 s, 42 inferences, 11 requests refused
  by the engine for length (largest accepted request 3,938 tokens).
- E6. Neither the Saltgate creation nor its review has been run at `c18d8f1`. `d5691ae` (no limits,
  resume, progress, reachability step, CLI) and `c18d8f1` came after E4 and E5.
- E7. Port of Gloom (seed "A smuggler's port town where something is wrong with the tide.", code of
  `d5691ae`, five 5-minute chunks and part of a sixth): stage `review`, 305 inferences, 65 requests
  refused for length, 22 rejected calls; 4 locations, 4 connections, 3 NPCs, 6 item types, 1 item.
  In the review the Questioner made 249 `read_object` calls over 21 distinct objects (the first
  locations and connections 15 or 16 times each), 3 `mark_object_reviewed` calls (item types only),
  9 `core_objects_created` calls (all refused: "Not yet reviewed: location Main Pier; …") and 6
  `ask_questions` rounds; item types went from 2 to 6.
- E8. Valley of Echoes (seed "someone is imprisoned in a high tower by 7 demons…"): question stage and
  naming took 14 minutes at 4096. Its review reached `complete` only in a run with 8192 context,
  compaction 4096 and inference limits of 400 and 1000 set by environment variables; that run says
  nothing about 4096. Result: 7 locations, 8 connections, 11 NPCs, 2 item types, 2 items.
- E9. Salvage Bay (same seed as E7, code of `2c9f7bc`): failed in review at the 32-inference turn
  limit; 12 locations (two named `Order of the Dry Shore`), 1 connection, 3 NPCs, 7 item types, 0 items.
- E10. At `80bf832` this plan said runs from a seed did not reliably reach a ready world, creation took
  13 to 68 model calls, and no run had finished review within 25 minutes.
- E11. One earlier Saltgate creation and review from a finished summary took 158 s in total; its
  object counts are not recorded.
- E12. Questioner review request (inference 133 of the Valley run): 3,168 tokens; tool definitions
  16%, role/summary/object list 39%, question-phase chat 42%. The object list is one line per object
  (about 10 tokens each; 33 lines, 316 tokens).
- E13. The engine refuses a request whose token count is above its fixed 4,096-token cache and reports
  the count. Reported counts match the prompt length alone (3,439 tokens plus 12,157 characters of
  results gave 6,692), not prompt plus the 1,024-token reply allowance.
- E14. Storyteller tools: 3,725 characters for five tools now; about 4,000 for three before. The
  Questioner's first request: 1,671 tokens before, 1,584 now.
- E15. Question count: 8 to 19 per call in replays; a schema maximum of 5 changed nothing (one call
  still had 10).
- E16. Connections lacked `name` on the first call in 12 of 17, 13 of 15 (required in schema),
  22 of 23 (request says "named") and 14 of 15 (argument text) created objects; a separate connection
  request gave 5 refusals against 3. The refusal and retry corrects it.
- E17. Repetition loop of the Questioner: with its long failed reply in the history, 11 of 12
  samples looped; with that reply removed, 0 of 6; with it shortened to 400 characters and a note,
  0 of 20 (three variants of the note).
- E18. Refused calls in comparable creation runs: 36 before the NPC and group interface changes, 4 after.
- E19. `generate-world` with `--minutes 2` stopped with exit 2; the next run printed "Continuing world 1"
  and carried on from the question stage into creation.
- E20. The reachability request ("These locations cannot be reached from …") has never been sent on the
  real model; the scripted test covers it.

### Guesses (not established)
Each is a guess. The lead is the observation that suggested it.
- G1. The Questioner re-reads without marking because its earlier reads leave its history. Lead: E7
  (249 reads, 3 marks, 65 refusals) and one run in which 13 consecutive refusals removed retained
  messages one at a time.
- G2. The review is slower now than in E11 because of one-object reads, more objects (a group per NPC,
  items) and refusal churn. Lead: E5 against E11, where the earlier world's size is unknown.
- G3. `d5691ae` or `c18d8f1` caused E7. Lead: E4 and E5 passed on the build before them; E7 failed on
  `d5691ae`; the worlds differ.
- G4. A small model treats a connection as an unlabelled edge, so it leaves out `name`. Lead: E16;
  in earlier runs, when connections had no name field, the model added names in 5 of 8 first calls.
- G5. Locations were made from factions because every marked name reads as a place. Lead: E9.
- G6. Item types grew in the review because Questioner questions asked whether factions should be item
  types. Lead: one question in E7 (inference 85) says so.
- G7. A doubled batch after compaction exists. Lead: seen once before this work; not reproduced.
- G8. The question-phase chat, a third of the review request (E12), is not needed in the review. Lead:
  it is the largest single part of that request; whether the Questioner uses it is not checked.

### Open
- Review of a world made from a real seed has not completed at 4096 on the current code (E7).
- Each NPC starts in its own group and each group has its own review mark.
- The Storyteller's tools are about 1,000 tokens, 35% to 40% of creation requests; `update_object`
  repeats `create_object`'s fields (750 characters); the kind list appears in three tools.
- `give_item` is not in `user_declarations.md`.
- The summary-writing side inference has no capacity recovery (one request one token over the window
  failed a world).
- A real-model stop and relaunch of `serve` was not run; a unit test and E19 cover it.
- Merge of `pknowles/npcs` (E3) is not done.

### Not built
- ReadyToBegin entry placement by the Storyteller (declared); `place_waiting_adventurers` is TEMPORARY.
- Phases 3 and 4 (extended summary and detail objects); the smaller-world checkbox.

## Goal and authority

Creating a game starts generation immediately: the declared Questioner/Storyteller
process develops the world, and the Storyteller creates its objects by calling the authoring
tools, never by emitting a scenario file. With Developer Mode enabled the owner can
watch it live in the web UI: both agents and their inferences, the world summary on
the world, and each object and NPC as it is created. The Questioner leads the
four declared phases (world summary, core objects, extended summary, detail
objects) and ends creation with DetailObjectsCreated after reviewing every created
object; there is no BeginWorld tool. NPCs speak and act through their own agents,
not scripted lines.

Authority: [world development](../user_declarations.md#game-setting-and-story-narrative),
[focused development](../user_declarations.md#storyteller-initialization-output-and-tools),
[Storyteller and Questioner tools](../user_declarations.md#storyteller-and-questioner-tools),
[game objects](../user_declarations.md#game-state), and
[World detail](../user_declarations.md#world-detail-page).
Follow [the design](../design.md#storyteller-world-creation),
[AGENTS.md's checklist](../AGENTS.md#post-feature-and-pre-commit-checklist),
[coding standards](../coding_standards.md) and [prompt standards](../prompt_standards.md).

The preceding [Unified game time](unified-game-time.md) feature defines character
activities, resource ready-times, initiative and timed effects; its constructors
own initial timing values. The [GM editing plan](gm-editing.md) supplies shared
field editing, lookup and bounds. Existing scenario installation and object constructors supply
relationships and history ownership; reuse their validation rather than build a
second world model. Full combat/travel rules are not a prerequisite for authoring
the existing object fields. No placeholder rule effects or automatic Storyteller
approvals are introduced to make a generated scenario appear fully playable.

Background negotiation, creation-roll approvals, entry placement, encounter
guidance and replacements remain in [Storyteller gameplay](storyteller-gameplay.md),
after their rules dependencies. Campaign completion remains a separate plan.

## Design choices

| Approach | Benefit | Cost and decision |
| --- | --- | --- |
| Generate one complete scenario JSON and import it | Reuses the scenario format | Rejected: the declarations have the Storyteller create the world's objects with tools. Scenario import remains only for hand-written scenarios and is never a generation output. |
| Extend existing constructors and field access with scoped authoring tools | Reuses identities, validation, recording and editing | Requires explicit authoring permissions and lifecycle checks. Selected. |
| Add a separate draft-object model or world-generation framework | Could isolate draft data | Duplicates game objects and needs a conversion path. Use ordinary objects inside an initializing world instead. |

## Order of work

1. **Generation from world creation.** The create-world form takes the optional
   [seed prompt](../user_declarations.md#world-detail-page); creation stores it,
   makes the owner a member with the usual placeholder Adventurer, whose player
   agent does not run until the world is ready (so the player can do nothing; with
   Developer Mode on they can watch), and queues initialization through ordinary
   world execution. Implement the Questioner/Storyteller conceptual loop
   (Storyteller answers use a higher temperature where the model supports it; this
   is configurable and low priority, dropped if it costs too much quality), the
   world summary and the world/campaign name it produces (the example questions
   and meta-questions are plain lists in `prompts.toml`, edited and reloaded like
   any prompt; each world samples a few at random, the declared way to introduce
   variety), the declared
   "in-progress or complete" [world status](../user_declarations.md#world-detail-page), the
   admission gating while initializing (players can enter the world and see
   status, but cannot act or start character creation), and the Developer Mode
   views of both agents, their inferences, the summary and progress. These are the
   "develop and review" checkpoint's loop, progress, failure and visibility
   bullets. The loop needs no authoring tools.
2. **Core objects (phases 1 and 2).** Built before [GM editing and lookup](gm-editing.md)
   merged, so the tools replace whole fields by name; moving to its shared editing
   and lookup is part of merging the two: the Storyteller's Create/Update/Delete tools and lookup, the
   Questioner's ReadObject, MarkObjectReviewed and CoreObjectsCreated, mention
   reminders and focused location/NPC development, with created objects appearing
   live in Developer Mode. A world can be run with only its core objects, so until
   step 3 CoreObjectsCreated validates the world and makes it ready. This departs
   from the declarations (DetailObjectsCreated marks the world ready) and needs
   the user's sign-off.
3. **Extended summary and detail objects (phases 3 and 4).** CoreObjectsCreated
   instead starts the extended-summary cycle; then ExtendedWorldSummaryComplete,
   detail object creation and review, and DetailObjectsCreated as the point that
   marks the world ready. A UI option to skip these for a smaller world is
   undecided.

Until step 2 a world finishing step 1 has a summary and name but no objects and is
never ready. That is an incomplete world, not a fallback: only the tool that ends
the last implemented phase makes a generated world ready.

### Step 1 data and flow

- The Storyteller and Questioner are one agent each per world
  (`world_role_agent(world_id, role, agent_id)`): the Storyteller is declared as a
  single agent and gameplay reuses it. Each has exactly one chat for the whole
  world, compacted as necessary like any agent's; a fresh history per round was a
  mistake (it made one agent hold several chats, which the declarations do not
  have) and is removed. Developer Mode shows them as ordinary agents with
  ordinary chats, with no role-specific inspector code. The Storyteller never sees
  a stale world summary: the declarations require one version only (see
  "Replacing the summary" below). The Inspect panel is hosted by the world's own page (the
  shared `Inspector`), so a world with nothing to play yet needs no player page.
- Every world has `generation_stage`, whatever filled it in: `complete` means
  ready to play (a scenario import is `complete` at once, and DetailObjectsCreated
  will set it). Otherwise it selects the next finite step, as `next_npc_step` does: `meta`,
  `ask`, `name`, `create_objects`, `review`, `failed`. A failed step's
  error stays on its sequence; the stage only marks the world failed.
- `world_generation(world_id, spent)` is the loop's working data; `spent` is
  restored into the shared `Budget` each step, as NPC discussions do with
  `Budget::restore`, so the total limit bounds the whole generation. The two chats
  are found through `world_role_agent`.
- Each stage is one finite step run by the world worker; its outputs and next
  stage commit together, so a restart resumes at the stored stage. `meta`: the
  Questioner is given the seed, example questions and meta-questions from
  `prompts.toml` and answers the meta-questions. `ask`: one Questioner turn. It
  asks with `ask_questions`, whose call runs the Storyteller (answers one at a
  time, then a new summary as a side inference) and returns what changed (the summary was
  rewritten; the Questioner's context holds it), as declared; it calls the terminal `world_summary_complete` once
  the summary has a solid foundation. Its role text
  carries the declared priority order (interest, depth and internal connections;
  multiple player pathways; contradictions and weak coupling; then detail) and
  switches to critical-review questions as the summary matures.
  After `world_summary_complete`, `name`: the Storyteller's text reply is stored as the world name, then
  `create_objects` follows, in which the Storyteller creates the summary's core objects. An empty summary or name reply fails its step
  visibly; it is not replaced or retried. The Storyteller has no tools while
  answering; step 2's editing tools let it revise the summary and name afterwards.
- Reuse, not copies: split `create_adventurer` into creating the Adventurer and
  placing it in a group (joining a ready world does both; one still initializing only the first;
  entry placement later uses the second); readiness (`generation_stage` is
  `complete`) is one check shared by the two admission paths; new code lives in
  `store/generation.rs` and `game/generation.rs`, not the 10k-line `store.rs` and
  `game.rs`. An NPC step and a generation step share only `Budget::restore` and the
  sequence calls: the NPC step is built on claims and response collections, so no
  common step runner was extracted.

**Replacing the summary.** [The declarations](../user_declarations.md#game-setting-and-story-narrative)
require the Storyteller's context never to hold more than one world summary, so
that several versions cannot overload or confuse it. The current summary is
supplied as per-request context. The difficulty is the Storyteller's own reply
that wrote it: kept in the chat it becomes a stale summary in the next round.

| Option | Cost |
| --- | --- |
| The summary-writing request and reply never enter the Storyteller's chat: a recorded side inference over the chat (as compaction already is), whose reply is stored only as the world summary and shown in Developer Mode as an inference | A turn variant that appends nothing; the chat does not remember writing it, which per-request context makes irrelevant |
| Mark the summary exchange superseded when the next summary is saved, and have context assembly skip superseded entries (still visible in Developer Mode) | Changes the history model and every context builder for a single case |
| Replace the old exchange with a stub through the compaction mechanism | Compaction text comes from an inference, not a given string; awkward |

Decided: the first. The Storyteller's other requests (answers, name) stay in its
chat and are compacted like any chat's.

| Choice | Alternatives | Decision |
| --- | --- | --- |
| What runs generation | A task spawned per world needing its own shutdown/crash handling; one long inline world event blocking the queue and shutdown | Steps in the world worker, reusing admission, ordering, shutdown and Developer Mode events |
| Agents | A new agent per round, contradicting the single declared Storyteller; a new chat per round in one agent, which gives an agent several chats (rejected: the declarations give every agent one chat) | One agent per role with one compacted chat |
| Summary and name | A `write_world_summary` tool duplicating the editing plan's field writes; parsing a title out of the reply | The plain-text reply is the field's value; step 2's editing tools revise it |

## Checkpoint: authoring tools and readiness

- Add initialization state to world creation and run it through the existing
  world execution/lifecycle path. Store seed, Storyteller history and current
  stage before starting inference. Keep only the progress needed to select the
  next stage/object and reference committed outputs; use ordinary recordings for
  inference inputs/results, not another transcript or per-tool replay journal.
- (Built as three tools with an `object` kind, `create_object`, `update_object` and `delete_object`,
  addressed by ID: the user's 2026-11-05 decision, so the tools fit a 4096-token context.) Provide Create,
  Update and Delete for Location, Connection, NPC, ItemType and Item. There is no tool for a group: the
  declarations say NPCs "must have a group - either existing or created with their first NPC", and a group
  goes when its last NPC does. Create requires all initial fields; Update names the
  object ID and only fields being replaced. Return assigned IDs and the real
  change. Use world-scoped typed
  arguments and shared constructors/validation. Return full validation errors as
  rejected tool results; execution failures propagate normally.
- Create in reference dependency order: locations, connections/groups, then
  NPCs and item types/items with existing holders. This is an ordering of needed
  references, not a mandatory extra inference for each record. Every character
  belongs to one group, and every item has one valid location or character holder.
  An ItemType may exist with no instances (for example the result type of a
  later SplitItem); initialization must not create items merely to give every
  type one. Runtime transformations belong to
  [Rules and travel](playable-rules.md), not the authoring tools.
  Delete only flags the selected object as deleted. Never physically remove
  its record, owned links, agent/history or other related records. Filter deleted
  objects out of normal discovery, counts, model context, NPC scheduling and
  action/tool targets; Developer Mode can explicitly include them and inspect
  their retained relationships and history. What a delete does to dependents
  (block, cascade or leave alone) is per object type as the design states, with
  the NPC's optional item location/description or delete-items flag and the
  Location's delete-groups flag as tool arguments; there is no DeleteGroup tool.
  Nothing else is flagged or hidden to repair an invalid request. The readiness
  validation operates on the remaining active world.
- Use the GM editing feature's whole-field and exact-text operations for
  Storyteller notes and the editable, spoiler-bearing world summary. Storyteller
  access excludes GM notes. Keep summary distinct from short object notes and
  preserve the original input prompt as the initialization record.
  Reuse its shared-subnote create/read/edit/link/unlink operations for facts that
  concern several objects. Link one Storyteller subnote rather than copy its body
  into each object; its full read exposes the related objects. Linking a character
  is relevance metadata, not delivery of knowledge to that character. Its private
  background must still contain what it personally knows. Shared-subnote storage and
  scenario import/export already exist from that feature; do not add another
  Storyteller-only note representation.
- Create each NPC with its public description, private background, motives,
  supported character stats, group and equipment. The NPC's own current context
  includes its private background and relevant character fields; that private
  background is not copied to other NPCs or players. Author knowledge and motives
  as context, not canned future messages or a new clue/knowledge subsystem.
  Record incidental scenery and discovery instructions in descriptions/notes
  where appropriate.
- Reuse the timing feature's constructor state for each NPC. Do not author an
  InCombat flag, moved/acted booleans or a global world time. An initial activity
  or effect, if supplied, uses the shared typed lifecycle rather than text edits
  to ready-times. Readiness must not execute queued NPC activity completions or
  reset cooldowns simply to declare initialization finished.
- Keep NPC responses, GM scene narration, character creation and ordinary game
  actions inactive while initialization is in progress. Players can enter the
  world (the play page shows the status, not a redirect) and see status/errors,
  but cannot start character creation or act. Enforce this at operation
  admission, not only in the browser.
- The Questioner's phase tools follow the declarations. `world_summary_complete`
  (step 1) saves the summary, takes the title and prompts core object creation.
  After that the Questioner lists the created top-level objects (not those
  reached through another, such as an NPC's items) with their Reviewed flag,
  reads them one at a time with ReadObject, and records its progress with
  MarkObjectReviewed. Modifying an object, or anything it contains such as an
  NPC's item, resets that object's flag, so a stale review can never satisfy
  completion and items need no flags of their own. CoreObjectsCreated verifies
  every flag, then inserts the canned message to the Storyteller that the
  extended summary begins and gives the Questioner the same, so it first asks
  questions that produce a good extended summary and reviews it (step 3);
  ExtendedWorldSummaryComplete saves that summary and prompts detail objects;
  DetailObjectsCreated verifies every flag, validates the declared connected location graph, required fields
  including NPC backgrounds/motives, valid item types and holders, and
  world-local references, then sets `generation_stage` to `complete`. Missing
  optional narrative ideas are not structural errors. A rejection leaves the
  phase open for edits. Success makes the world available for character creation
  without starting NPC simulation or opening narration before characters join.
  Each phase tool is offered only in its phase and stale calls are rejected at the
  mutation boundary.
- Demonstrate this checkpoint using [Four Brothers](../scenarios/four_brothers.md)
  as the Storyteller input. Inspect persisted locations, NPCs, equipment and notes
  and the context each NPC would receive. This is an authoring check, not a claim
  that all combat, travel or special item mechanics already work.

## Checkpoint: develop and review a generated world

- Implement the declared four phases. The Questioner first answers
  the supplied meta-questions in one inference, then produces AskQuestions or
  WorldSummaryComplete in the next. Record this reasoning separately from the world
  state; expose it through normal Developer Mode inspection.
- Run Storyteller answers one question at a time as text to itself, then request
  a world-summary write/update. Every round shows the input prompt and the one
  latest summary; no earlier summary version is ever in its context. The Questioner receives the current
  world artifacts, including the world summary, not the Storyteller's answer
  history. During conceptual development the summary is the available artifact;
  during construction review include the actual persisted objects as well.
  Questioner phase outputs are AskQuestions or that phase's completion tool; only
  DetailObjectsCreated makes the world ready.
- Develop location/NPC seeds and run the declared focused enrichment on one
  object at a time with world-summary context. Persist the seeds and stage/object
  progress with existing world/object data. Use shared lookup and bounded context
  assembly instead of putting every large object body into every request. The
  Storyteller can revise objects and summary through the same authoring tools.
- Define one explicit markup for names of characters, important items and
  locations in generation text. Parse only that markup and compare it with
  created objects to give non-blocking reminders of uncreated mentions. Do not
  infer mentions from arbitrary prose, force creation, or require a reminder to
  be cleared before DetailObjectsCreated. Exercise this formatting through real
  chat before claiming the reminder works.
- Review persisted world state with the Questioner and Storyteller, not merely
  the original proposal. AskQuestions invites further development; each phase
  completion tool records reviewer satisfaction with that phase. The Questioner
  has no access to the Storyteller's answers or history, only the world's
  objects. Narrative quality is not a Rust puzzle validator or a scripted
  solution test.
- Use the shared inference limits and failure handling: each agent turn is
  bounded by the per-turn limit, and the whole generation by the existing total
  limit, with no separate round limit. Reaching a limit before DetailObjectsCreated is a
  visible unfinished/failed initialization, never automatic readiness.
  A failed world stays stuck with its error and recorded progress visible;
  no retry/resume UI or automatic replay is required by this increment. Preserve
  normal clean-shutdown boundaries and crash handling for active work.
- Show progress/error and both roles in existing world/Developer Mode views.
  Keep summary and private object information out of the normal WorldDetail
  display; the seed remains expandable. Random selection of questions and
  meta-questions is part of step 1 as declared. Optional random-word injection and
  a general prefab item-type library remain experiments, not required generation
  mechanisms. Preparing the types an authored scenario
  actually needs is ordinary world content, not dependent on that library.
- Measure assembled authoring-tool and review contexts against the configured
  model capacity, following [the context-size plan](llm-context-size.md). Do not
  assume every tool definition and every world object body fits together; agree
  any context strategy with the user as that plan requires.

## Verification and completion

Review model-facing cases with the user before adding LLM tests, as required by
coding_standards.md. Use production tools, recording, constructors and game paths;
combine cases rather than require repeated whole-world GPU runs by default.

| Case | Outcome and evidence |
| --- | --- |
| Existing scenario becomes world state | Author Four Brothers through real Storyteller calls. Inspect objects and captured contexts: witnesses have relevant private memories, suspects have motives, and equipment/conditions are recorded. NPC dialogue is generated from that context rather than stored as a script. |
| Authoring and permissions | Scripted production-path calls create, partially update, edit long text and flag an NPC as deleted. Its record, membership/agent links and history remain stored and inspectable when Developer Mode includes deleted objects, but normal discovery/context/scheduling/actions exclude it. An NPC carrying items is blocked unless the call places the items at a location or deletes them too; a Group is flagged when its last NPC is removed; unrelated objects remain unchanged. Foreign IDs, dangling active references, incomplete NPCs, invalid holders and writes/reads of GM notes fail unchanged with recorded errors. |
| Review can change the result | Supply a known omission in persisted state; the review sees it rather than only the intended summary. An edit changes the next captured review input. Check AskQuestions and each phase completion tool without requiring particular prose. |
| Focused development and context | Captured inputs show the meta-question inference, separate question output, individual answers and focused location/NPC work. Later rounds receive the current summary/artifacts and no earlier summary version; private NPC information stays with authorized roles. NPC creation uses the same activity/resource state as other characters; initialization causes no autonomous action or resource reset. |
| Review state | Reviewing an NPC lists its items inside the NPC with no flag of their own. Editing one of its items resets the NPC's flag; CoreObjectsCreated is refused until it is reviewed again. An object listed unreviewed keeps the phase open. |
| Mention reminders | A marked uncreated mention appears as a reminder; creation updates that reminder. Intentionally leaving it uncreated does not block DetailObjectsCreated. Unmarked ordinary prose does not manufacture obligations. |
| Readiness and lifecycle | Entering during generation allows status inspection but no character creation/action/NPC response. DetailObjectsCreated with unreviewed objects or an invalid world changes nothing; a successful one opens character creation without starting an empty-world simulation. An inference failure or budget exhaustion remains visible and never opens play. Clean shutdown and crash use the existing lifecycle without replaying committed edits. |

Update implementation_reference.md only for implemented behaviour. Re-read both
standards, run appropriate Rust/SQL/browser checks and representative real-chat
verification within the testing budget, then complete AGENTS.md self-review.
Archive only after both checkpoints are implemented and verified.

# Work Summary Backups

The following are quotes added by the User in case the context is lost from compaction. Take with a grain of salt; this is WIP thinking output.

## 26/22/09 01

Written for: you, as the status of world generation and what remains before merge. It cannot merge yet. No full generation has reached a ready world.

Done since my last report (257 unit tests pass, quality script clean)

80bf832. After the Storyteller rewrites the summary, a second short call asks it to say, in about 20 words, what its answers changed. That reply is what the Questioner gets back. Measured: 29–37 words against 55 for "one short sentence". This follows your updated declarations, which I committed with it. The summary stays in the system context, as your edit says.
484546a. I had accidentally committed my temporary replay test with a blanket git add -u. It is removed. I now add files by name.
Creation and review now reach a ready world from a finished summary. The real-model test took 158 seconds on the Saltgate world. The first pass left out two NPCs, the review's missing-names note caught them, and the Storyteller created them. That is one world only.
Your questions

Per-question rewrites: dropped.
NPC groups (implicit single-NPC groups plus a grouping call): not built. Baseline data supports it: in my replays 34 of 44 NPC calls gave no group fields at all, so models mostly ignore "give a group when creating". My replay of the new interface was inconclusive because every reply hit the output cap on locations before reaching any NPC. I won't build it until creation replies fit.
Your point about cut-off handling: agreed, it is a fail-fast error. The cut-off call stays visible in the model's history and its result says why. But the fix is for replies not to overrun. I measured, and wording does not control that. With "3 to 5", "about 5" or one-sentence fields, replies still reach the cap with 6–9 calls. A call costs about 140 tokens in this format, so the cap fits about 7 calls and a small world needs about 12. The structural answer is the staged creation your declaration already describes ("NPCs one at a time"): small focused requests driven by code.
Open findings, nothing lost from earlier

Questioner won't converge. It asks 7–13 questions per round despite "up to five", and loops by repeating its question list about 1 time in 6. The question phase took over 23 minutes in the last full run and never finished.
The summary-writing side inference has no overflow recovery. In an earlier run it overflowed the context by one token and killed the world generation. Not fixed.
The doubled batch after compaction was never reproduced.
Item tool alternatives (location_id against held_by_id) have not been reviewed against the prompt standards.
Merge with the other agent's d9dec09 (they already dropped group description, with migration 0017 on their branch; their version should win) is untouched.
Entry placement: store/review.rs still has the marked-temporary first-group placement for new characters. Your new ReadyToBegin declaration (the Storyteller picks the group and location) is not built.
Reliability: several seeds reaching a ready world, then a played world, are still needed.
My self-review gaps

My prompt measurements use small samples on one or two worlds.
Creation passed on one world only.
Staged creation is a design change, so I want your go-ahead before building it.
Next unless you redirect: build staged creation (locations and connections, then each NPC in its own request) and re-measure the implicit-groups idea on it.
