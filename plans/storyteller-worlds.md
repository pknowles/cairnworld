# Storyteller world initialization

Phases 1 and 2 are implemented and committed; not yet verified. With the device-test settings (4096-token
context, compaction at 2048) real-model runs from a seed do not reliably reach a ready world: the object
creation step took between 13 and 68 model calls across runs, and no run has finished review within 25
minutes (the Storyteller re-creates objects that already exist, and chat summaries sometimes replace its
task with an in-story one). Next round: make the tools' alternatives explicit as `prompt_standards.md` now asks
(an NPC joining a group or starting one; an item at a location or held by an NPC), and fix the
re-creation loop from the real model's inputs. Phases 3 and 4 and the smaller-world checkbox wait for that.

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
