# Storyteller world initialization

Pending implementation and verification.

## Goal and authority

Turn a scenario in Storyteller context into a persistent world, then support the
declared Questioner/Storyteller process that develops that scenario itself.
The Storyteller declares readiness with BeginWorld after reviewing the actual
created world. NPCs speak and act through their own agents, not scripted lines.

Authority: [world development](../user_declarations.md#game-setting-and-story-narrative),
[focused development](../user_declarations.md#storyteller-initialization-output-and-tools),
[Storyteller and Questioner tools](../user_declarations.md#storyteller-and-questioner-tools),
[game objects](../user_declarations.md#game-state), and
[World detail](../user_declarations.md#world-detail-page).
Follow [the design](../design.md#storyteller-world-creation),
[AGENTS.md's checklist](../AGENTS.md#post-feature-and-pre-commit-checklist),
[coding standards](../coding_standards.md) and [prompt standards](../prompt_standards.md).

The preceding [GM editing plan](gm-editing.md) supplies shared field editing,
lookup and bounds. Existing scenario installation and object constructors supply
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
| Generate one complete scenario JSON and import it | Reuses the scenario format | A single output must describe all objects; later edits need another interface. Keep import for development; use the declared incremental tools for the Storyteller. |
| Extend existing constructors and field access with scoped authoring tools | Reuses identities, validation, recording and editing | Requires explicit authoring permissions and lifecycle checks. Selected. |
| Add a separate draft-object model or world-generation framework | Could isolate draft data | Duplicates game objects and needs a conversion path. Use ordinary objects inside an initializing world instead. |

## Checkpoint: author an existing scenario

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
  Reject deletion while other records reference the object; the Storyteller can
  explicitly update/delete those records first. Do not silently cascade-delete
  an NPC, inventory or location contents to repair an invalid request.
- Use the GM editing feature's whole-field and exact-text operations for
  Storyteller notes and the editable, spoiler-bearing world summary. Storyteller
  access excludes GM notes. Keep summary distinct from short object notes and
  preserve the original input prompt as the initialization record.
- Create each NPC with its public description, private background, motives,
  supported character stats, group and equipment. The NPC's own current context
  includes its private background and relevant character fields; that private
  background is not copied to other NPCs or players. Author knowledge and motives
  as context, not canned future messages or a new clue/knowledge subsystem.
  Record incidental scenery and discovery instructions in descriptions/notes
  where appropriate.
- Keep NPC responses, GM scene narration, character creation and ordinary game
  actions inactive while initialization is in progress. Players can open the
  world and see status/errors, but cannot start character creation or act.
  Enforce this at operation admission, not only in the browser.
- Implement BeginWorld as the Storyteller's explicit completion tool. Validate
  the declared connected location graph, required fields including NPC
  backgrounds/motives, item-type use, holders and world-local references before
  changing state. Missing optional narrative ideas are not structural errors.
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
- Use the shared inference budgets and failure handling. Reaching a safety limit
  before BeginWorld is a visible unfinished/failed initialization, never automatic
  readiness. Round limits must not substitute for explicit completion.
  A failed world stays stuck with its error and recorded progress visible;
  no retry/resume UI or automatic replay is required by this increment. Preserve
  normal clean-shutdown boundaries and crash handling for active work.
- Show progress/error and both roles in existing world/Developer Mode views.
  Keep summary and private object information out of the normal WorldDetail
  display; the seed remains expandable. Optional random-word injection, prefab
  item types and random seed-question selection remain experiments, not required
  generation mechanisms.
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
| Authoring and permissions | Scripted production-path calls create, partially update, edit long text and delete an unreferenced object. Foreign IDs, dangling references, incomplete NPCs, invalid holders and writes/reads of GM notes fail unchanged with recorded errors. |
| Review can change the result | Supply a known omission in persisted state; the review sees it rather than only the intended summary. An edit changes the next captured review input. Check AskQuestions and WorldIsComplete without requiring particular prose. |
| Focused development and context | Captured inputs show the meta-question inference, separate question output, individual answers and focused location/NPC work. Later rounds receive current summary/artifacts without earlier answer transcripts; private NPC information stays with authorized roles. |
| Mention reminders | A marked uncreated mention appears as a reminder; creation updates that reminder. Intentionally leaving it uncreated does not block BeginWorld. Unmarked ordinary prose does not manufacture obligations. |
| Readiness and lifecycle | Joining during generation allows status inspection but no character creation/action/NPC response. Invalid BeginWorld changes nothing; successful BeginWorld opens character creation without starting an empty-world simulation. An inference failure or budget exhaustion remains visible and never opens play. Clean shutdown and crash use the existing lifecycle without replaying committed edits. |

Update implementation_reference.md only for implemented behaviour. Re-read both
standards, run appropriate Rust/SQL/browser checks and representative real-chat
verification within the testing budget, then complete AGENTS.md self-review.
Archive only after both checkpoints are implemented and verified.
