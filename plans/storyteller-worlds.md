# Storyteller world initialization

Pending implementation and verification.

## Goal and authority

Creating a game starts generation immediately: the declared Questioner/Storyteller
process develops the world, and the Storyteller builds it by calling the authoring
tools, never by emitting a scenario file. With Developer Mode enabled the owner can
watch it live in the web UI: both agents and their inferences, the world summary on
the world, and each object and NPC as it is created. The Storyteller declares
readiness with BeginWorld after reviewing the actual created world. NPCs speak and
act through their own agents, not scripted lines.

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
| Generate one complete scenario JSON and import it | Reuses the scenario format | Rejected: the declarations have the Storyteller build the world with tools. Scenario import remains only for hand-written scenarios and is never a generation output. |
| Extend existing constructors and field access with scoped authoring tools | Reuses identities, validation, recording and editing | Requires explicit authoring permissions and lifecycle checks. Selected. |
| Add a separate draft-object model or world-generation framework | Could isolate draft data | Duplicates game objects and needs a conversion path. Use ordinary objects inside an initializing world instead. |

## Build order

1. **Generation from world creation.** The create-world form takes the optional
   [seed prompt](../user_declarations.md#world-detail-page); creation stores it,
   makes the owner a member with the usual placeholder Adventurer, whose player
   agent does not run until the world is ready (so the player can do nothing; with
   Developer Mode on they can watch), and queues initialization through ordinary
   world execution. Implement the Questioner/Storyteller conceptual loop
   (Storyteller answers use a higher temperature where the model supports it; this
   is configurable and low priority, dropped if it costs too much quality), the
   world summary and the world/campaign name it produces (the meta-questions are
   a plain list in `prompts.toml`, edited and reloaded like any prompt so they are
   easy to change while experimenting), the declared
   "in-progress or complete" [world status](../user_declarations.md#world-detail-page), the
   admission gating while initializing, and the Developer Mode views of both
   agents, their inferences, the summary and progress. These are the "develop and review"
   checkpoint's loop, progress, failure and visibility bullets. The loop needs no
   authoring tools.
2. **Authoring tools.** After [GM editing and lookup](gm-editing.md) is merged:
   the Create/Update/Delete tools, lookup, mention reminders, focused
   location/NPC development, persisted-state review and BeginWorld, with created
   objects appearing live in Developer Mode. These are the remaining bullets of
   both checkpoints.

Until step 2 a world finishing the loop has a summary and name but no objects and
cannot begin. That is an incomplete world, not a fallback: only the authoring tools
create objects.

### Step 1 data and flow

- The Storyteller and Questioner are one agent each per world
  (`world_role_agent(world_id, role, agent_id)`, like `world_gm`): the Storyteller
  is declared as a single agent and gameplay reuses it. Each run starts on a new
  empty history branch of that agent, so a round begins from the seed prompt and
  latest summary only. Developer Mode lists the branches through the existing
  branch graph, generalized from GM groups rather than copied.
- Every world has `generation_stage`, whatever filled it in: `complete` means
  ready to play (a scenario import is `complete` at once, and BeginWorld will set
  it). Otherwise it selects the next finite step, as `next_npc_step` does: `meta`,
  `ask`, `answer`, `summarize`, `name`, `concept_done`, `failed`. A failed step's
  error stays on its sequence; the stage only marks the world failed.
- `world_generation(world_id, questions, next_question, questioner_branch_id,
  storyteller_branch_id)` is the loop's working data. The round number is the
  count of the Storyteller's branches, not a column. `questions` copies the
  arguments of the Questioner's recorded `ask_questions` call: read them from
  that recording instead, and drop the column, if the store makes that cheap.
- Each stage is one finite step run by the world worker; its outputs and next
  stage commit together, so a restart resumes at the stored stage. `meta`: the
  Questioner answers the `prompts.toml` meta-questions. `ask`: given the seed
  prompt and latest summary it calls `ask_questions` or `world_is_complete`.
  `answer`: the Storyteller answers one question as plain text. `summarize`: its
  text reply is stored as the world summary, then `ask` runs again. After
  `world_is_complete`, `name`: its text reply is stored as the world name, then
  `concept_done` waits for step 2. An empty summary or name reply fails its step
  visibly; it is not replaced or retried. The Storyteller has no tools here; the only
  tools are the Questioner's two, and step 2's editing tools let the Storyteller
  revise the summary and name afterwards.
- Reuse, not copies: split `create_adventurer` into creating the Adventurer and
  placing it in a group (joining a ready world does both; one still initializing only the first;
  entry placement later uses the second); readiness (`generation_stage` is
  `complete`) is one check in `Game::admit`, the single admission point; the sequence, budget, failure and
  Developer Mode handling of an NPC step moves into one function that the
  generation steps share; new code lives in `store/generation.rs` and
  `game/generation.rs`, not the 10k-line `store.rs` and `game.rs`.

| Choice | Alternatives | Decision |
| --- | --- | --- |
| What runs generation | A task spawned per world needing its own shutdown/crash handling; one long inline world event blocking the queue and shutdown | Steps in the world worker, reusing admission, ordering, shutdown and Developer Mode events |
| Agents | A new agent per round, contradicting the single declared Storyteller; one agent compacted, which cannot drop earlier answers | One agent per role with a new branch per run, as the GM does per group |
| Summary and name | A `write_world_summary` tool duplicating the editing plan's field writes; parsing a title out of the reply | The plain-text reply is the field's value; step 2's editing tools revise it |

## Checkpoint: authoring tools and BeginWorld

- Add initialization state to world creation and run it through the existing
  world execution/lifecycle path. Store seed, Storyteller history and current
  stage before starting inference. Keep only the progress needed to select the
  next stage/object and reference committed outputs; use ordinary recordings for
  inference inputs/results, not another transcript or per-tool replay journal.
- Provide Create, Update and Delete variants for Location, Connection, NPC,
  Group, ItemType and Item. Create requires all initial fields; Update names the
  object ID and only fields being replaced. Return assigned IDs, real changes
  and current location/connection/group/NPC/item counts. Use world-scoped typed
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
  Nothing else is flagged or hidden to repair an invalid request. BeginWorld's
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
  to ready-times. BeginWorld must not execute queued NPC activity completions or
  reset cooldowns simply to declare initialization finished.
- Keep NPC responses, GM scene narration, character creation and ordinary game
  actions inactive while initialization is in progress. Players can open the
  world and see status/errors, but cannot start character creation or act.
  Enforce this at operation admission, not only in the browser.
- Implement BeginWorld as the Storyteller's explicit completion tool. Validate
  the declared connected location graph, required fields including NPC
  backgrounds/motives, valid item types and holders, and world-local references
  before changing state. Missing optional narrative ideas are not structural errors.
  A rejection leaves initialization open for edits; success makes the world
  available for character creation. It does not start NPC simulation or opening
  narration without characters joining. Remove BeginWorld from subsequent tool
  surfaces and reject stale calls at the mutation boundary.
- Demonstrate this checkpoint using [Four Brothers](../scenarios/four_brothers.md)
  as the Storyteller input. Inspect persisted locations, NPCs, equipment and notes
  and the context each NPC would receive. This is an authoring check, not a claim
  that all combat, travel or special item mechanics already work.

## Checkpoint: develop and review a generated world

- Implement the declared three conceptual phases. The Questioner first answers
  the supplied meta-questions in one inference, then produces AskQuestions or
  WorldIsComplete in the next. Record this reasoning separately from the world
  state; expose it through normal Developer Mode inspection.
- Run Storyteller answers one question at a time as text to itself, then request
  a world-summary write/update. The next round starts with the input prompt and
  latest summary, not all prior answers. The Questioner receives the current
  world artifacts, including the world summary, not the Storyteller's answer
  history. During conceptual development the summary is the available artifact;
  during construction review include the actual persisted objects as well.
  Questioner outputs are AskQuestions or WorldIsComplete. Neither output starts
  play by itself.
- Develop location/NPC seeds and run the declared focused enrichment on one
  object at a time with world-summary context. Persist the seeds and stage/object
  progress with existing world/object data. Use shared lookup and bounded context
  assembly instead of putting every large object body into every request. The
  Storyteller can revise objects and summary through the same authoring tools.
- Define one explicit markup for names of characters, important items and
  locations in generation text. Parse only that markup and compare it with
  created objects to give non-blocking reminders of uncreated mentions. Do not
  infer mentions from arbitrary prose, force creation, or require a reminder to
  be cleared before BeginWorld. Exercise this formatting through real chat
  before claiming the reminder works.
- Review persisted world state with the Questioner and Storyteller, not merely
  the original proposal. AskQuestions invites further development; WorldIsComplete
  records reviewer satisfaction. The Storyteller remains responsible for its
  final review and BeginWorld. Narrative quality is not a Rust puzzle validator
  or a scripted solution test.
- Use the shared inference limits and failure handling: each agent turn is
  bounded by the per-turn limit, and the whole generation by the existing total
  limit, with no separate round limit. Reaching a limit before BeginWorld is a
  visible unfinished/failed initialization, never automatic readiness.
  A failed world stays stuck with its error and recorded progress visible;
  no retry/resume UI or automatic replay is required by this increment. Preserve
  normal clean-shutdown boundaries and crash handling for active work.
- Show progress/error and both roles in existing world/Developer Mode views.
  Keep summary and private object information out of the normal WorldDetail
  display; the seed remains expandable. Optional random-word injection, a general
  prefab item-type library and random seed-question selection remain experiments,
  not required generation mechanisms. Preparing the types an authored scenario
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
| Review can change the result | Supply a known omission in persisted state; the review sees it rather than only the intended summary. An edit changes the next captured review input. Check AskQuestions and WorldIsComplete without requiring particular prose. |
| Focused development and context | Captured inputs show the meta-question inference, separate question output, individual answers and focused location/NPC work. Later rounds receive current summary/artifacts without earlier answer transcripts; private NPC information stays with authorized roles. NPC creation uses the same activity/resource state as other characters; initialization causes no autonomous action or resource reset. |
| Mention reminders | A marked uncreated mention appears as a reminder; creation updates that reminder. Intentionally leaving it uncreated does not block BeginWorld. Unmarked ordinary prose does not manufacture obligations. |
| Readiness and lifecycle | Joining during generation allows status inspection but no character creation/action/NPC response. Invalid BeginWorld changes nothing; successful BeginWorld opens character creation without starting an empty-world simulation. An inference failure or budget exhaustion remains visible and never opens play. Clean shutdown and crash use the existing lifecycle without replaying committed edits. |

Update implementation_reference.md only for implemented behaviour. Re-read both
standards, run appropriate Rust/SQL/browser checks and representative real-chat
verification within the testing budget, then complete AGENTS.md self-review.
Archive only after both checkpoints are implemented and verified.
