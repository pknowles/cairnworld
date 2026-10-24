# NPC interaction: remaining work and acceptance

Partially implemented. Speech delivery, NPC response selection, individual GM
approvals, expressive speech, causal inspection and Interrupt/Never mind exist.
Remaining: restraint and natural stopping (user-owned tuning), responses to other
perceived events, and tentative/committed intervention. What happens to existing
raised hands when another player acts is an open policy choice.

## Goal and authority

Characters perceive events through their own histories and respond through the shared
character-action path. Responses to completed events use fresh turns; bounded
intervention can affect an original action before the GM resolves it. Humans can
raise a hand while typing without cancelling running work.

Design the complete flow together: something happens -> each observer receives
a history message -> each may respond -> selected responses use ordinary
character actions -> their outcomes can be perceived in turn. Content and action
rules vary; delivery, perception controls and response scheduling remain shared
across current and future features. Say and Take below exercise that mechanism;
they are not the list of event kinds it accepts. Combat supplies its own rules
later, and InterventionCheck collects intentions before the original outcome,
without introducing another perception or action execution system.

Authority: [NPC Interaction and NPC Inference](../user_declarations.md#npc-interaction),
[Observed Events](../user_declarations.md#observed-events),
[Character actions](../user_declarations.md#character-actions),
[GM tools](../user_declarations.md#gm-tools),
[World game page](../user_declarations.md#world-game-page),
[Developer view](../user_declarations.md#debugging-and-telemetry), and
[turn/allowance clarifications](../user_exerpts.md#2026-10-20-npc-speech-interruption-and-game-turn-allowance).
Follow [Character perceptions and NPC responses](../design.md#character-perceptions-and-npc-responses).

Each remaining increment follows the
[AGENTS.md checklist](../AGENTS.md#post-feature-and-pre-commit-checklist),
[coding standards](../coding_standards.md) and
[prompt standards](../prompt_standards.md). Review new model-behaviour cases and
validation approaches with the user before adding prompts or LLM tests.

## Implemented foundation

Use the existing operations rather than implementing these again. The
[implementation reference](../implementation_reference.md#web-play-designmd-web-server-and-ui-auth)
indexes speech, saved NPC responses, shared character actions, causal inspection,
shutdown/crash handling and player control. It holds the current implementation
details; this plan holds remaining work and acceptance requirements.

Implemented mechanics:

- Say/Whisper, group-local GM hearing adjustments and prefixes, expressive
  authored speech with typed attribution, offline-PC hearing and legacy request
  reconstruction.
- CharacterActor and shared pending-action/GM execution for players and NPCs.
- One recorded NPC decision: DoNothing or real Say/Whisper with eagerness 1–3.
  Tied speech is approved individually through ordinary GM turns.
- Finite steps in the existing world worker, scene revisions, budget checkpoints,
  causal links, clean-shutdown boundaries and crash failure without replay.
- Developer Mode inspection of causes and invalid tool calls.
- Per-character raised hands, Interrupt/Never mind, multi-tab control state and
  other-group progress.
- Scripted coverage of speech privacy, selection, stale input, topology/access
  changes, shutdown/crash and player controls. Preserve that coverage; it does not
  establish successful natural stopping by the real model.

## Invariants to preserve during remaining work

- Proposals have no effect until approved. Repeated resolution cannot apply an
  action or deliver speech twice; one proposal requests at most one actual action.
  Recheck world/group, authority and eligibility at submission and commitment.
  Browser callers cannot supply arbitrary NPC actors.
- NPC context contains its own facts and heard/perceived events, not another
  character's history or private GM/Storyteller notes. Snapshot actual hearers at
  speech commitment, including the speaker; addressing someone does not make Say
  private. GM hearing overrides remain group-local.
- Shared content references retain authored text, source IDs/frozen names,
  addressee, whisper intent and prefix. Live/reload/model/inspection/compaction
  paths preserve it; legacy rendering and historical requests stay reproducible.
  Tool receipts are not duplicate utterances. Publish only after commit.
- Narration and prefix follow each utterance's own audience. Rejected or
  undelivered speech cannot fall back to group-wide narration. All results in an
  ordinary tool batch precede deferred history deliveries.
- Highest eagerness wins. Highest speech ties may all speak, composed before
  hearing peers, approved individually in any order. Their own deliveries do not
  invalidate remaining chosen words; external input can supersede stale work.
  Finish the selected set before another collection. Other action ties choose
  one recorded random winner when those action types are implemented.
- One world worker mutates state and admits foreground input between finite NPC
  steps. Exclude groups waiting for humans during work selection. Use existing
  notifications, not polling or jobs sent into the worker's own bounded channel.
- Scene revisions follow committed fiction/context changes, not inference or tool
  bookkeeping. Extend the writer audit when new perceptions/actions are added.
  Retired groups and changed authority cannot leave old proposals executable.
- Fresh follow-up stacks share the root Budget and exact recorded causes. Save
  progress/spent budget at complete-step boundaries; new turns/restarts do not
  grant another allowance. Clean shutdown finishes accepted work/current steps.
  Crashes fail interrupted work visibly and close unanswered history, preserving
  effects without replay, rollback or serialized agent stacks.
- Malformed NPC decisions fail visibly without an automatic correction loop.
  Invalid tool calls remain recorded and visibly flagged even if the normal
  agent loop later succeeds; an ordinary GM rejection is a gameplay outcome.

## Raised hands: current behaviour and open choice

Interrupt raises a hand in the player's current group. No new NPC step starts
there while a hand is raised; an already-running step finishes. Humans and other
groups can continue. Never mind lowers only its caller's hand without inference
or allowance changes, and is hidden/inert without a hand. Send or final disconnect
lowers the player's hand. Retired-group hands do not affect current groups;
splits and merges are not restricted by hands.

Current code also lowers every hand in a group when another player's message is
queued. The latest World game page declaration explicitly leaves this undecided:
keep the original hand so the player can finish typing, or clear it because the
conversation moved on. Queueing text is not the same as a successful game action.
Do not present the current implementation as settled user intent. Once the user
decides, align design.md, code, regression expectations and implementation
reference together. This open choice does not require rebuilding Interrupt.

## User-owned tuning

The user is handling response choice, restraint and natural stopping. Do not
force an addressed NPC to answer, remove Whisper, increase budgets or introduce
a fallback merely to make a test pass. Context/prompt changes follow the prompt
standards; separate measured outcomes from hypotheses.

Evidence from real-model runs, read from the recorded contexts and replayed with
variants:

- The role text used to call say and whisper interchangeably. All 36 replayed
  decisions were whispers, so a question put to Toma was answered privately or not
  at all. Neither persona content, tool order, the say and whisper descriptions nor
  the addressee field changed that. Removing the whisper tool made Toma answer
  openly in 6 of 6. Stating what each tool does (say is open, whisper is private)
  made Toma answer openly in 5 of 6 replays and in 9 of 10 end-to-end runs.
- NPCs never call do_nothing once a dialogue is under way: 0 of 105 decisions in
  five complete discussions between Mara and Toma. Each discussion ran 21
  utterances (about 2.3 minutes with a scripted GM) and ended by exhausting the
  shared budget of 64 inferences, so the discussion is marked failed. The
  NPC-answer device test passes because it stops at the first reply. About ten
  context and wording variants, replayed on recorded late-round contexts, did
  not produce a single do_nothing: an instruction to stay silent, a trailing
  question, thinking mode, a different do_nothing description, a turn marker,
  merging recent speech into one message, a note that characters talk in turns,
  and removing the character's own earlier calls. Only with the whisper tool
  removed did an unaddressed Mara stay silent (5 of 6, first round only).

These are recorded observations, not universal model guarantees. Preserve useful
captured context and conclusions. A first-response test proves only that a reply
reached the player; it cannot close the natural-stopping requirement.

## Ideas to explore — not to be implemented

These are discussion notes, not implementation requirements or approved changes
to the current scheduler, prompts, time rules or acceptance tests. Do not build
them as part of this plan without a later explicit decision.

- Explore out-of-combat action timing as a resource governing when characters
  can act again, including NPC reactions. Initiating a conversation/action
  advances the character's action clock; its action availability does not reset
  until the group's game time catches up. Costs are ordinary RPG time costs,
  not a separate conversation-energy currency.
- The user's proposed group time is implicitly the earliest character time in
  that group. Characters therefore get a fair opportunity to advance, rather
  than fast actors repeatedly acting while others remain behind. This is a
  group-time idea, not a change to the world's clock definition.
- Timing does not replace conversation tuning. Conversations should tend to end
  when their subjects are resolved, reach a standstill, or give way to combat.
  What produces that behaviour still needs exploration; do not assert that the
  clock alone guarantees termination or add a forced end-conversation tool.

## Design choices

For message delivery and responses, compare these approaches against the coding
standards:

| Approach | Benefits | Costs / decision |
| --- | --- | --- |
| Add a separate observation type/table and response path for each action kind | Keeps each kind isolated | Repeats routing, privacy and scheduling. Reject: the delivered history message already is the observation. |
| Make every message describe one action with generic actor/target columns | Removes speech-specific joins | Human input, instructions and narration need not describe one action; Whisper has multiple listeners. Reject as a universal representation. |
| Share history delivery and response scheduling, preserving necessary content-specific metadata | One recipient record and one response path; supports speech and narration without duplicate prose | Requires general message provenance and a forward migration. Selected. |

Speech attribution can remain structured without making every future action use
speech's fields or acquire its own observation tables. The history entry records
receipt; its provenance records the cause; saved NPC work records pending
decisions. These are distinct responsibilities, not three copies of an event.

For intervention, scheduling a reaction after the original turn is too late to
affect it; persisting a suspended GM stack adds continuation state. Selected:
collect bounded child decisions in the current GM turn and apply approved
reactions through existing pending actions, then schedule later dialogue.
The Interrupt choice remains open as described above.

## Remaining implementation order

| Slice | Production change and finish line |
| --- | --- |
| Narration tools and shared delivery | Replace plain-text GM completion across action/opening/presence paths with the four narration tools. Complete recipient validation, per-text provenance, shared-detail configuration and shared history delivery together. The group, actor-and-others and addressed forms work through live/reload/context/inspection paths; an invalid narration call delivers nothing. Existing speech responses keep working; general NPC response eligibility remains pending until the next checkpoint. |
| Message-driven NPC responses | Complete narration delivery to NPC histories and generalize pending response sources/causal inspection to received messages. Verify Say/Whisper and Take together, including standalone narration, own-action exclusion, privacy, stale decisions and lifecycle/budget boundaries. Include necessary migrations and terminology cleanup with this behaviour; no rename-only prerequisite or new scheduler. |
| Distinct GM request tools | Expose Look/Investigate/Open/Ask through the shared action path, retaining actual intent in recorded requests. Verify real-model routing and ordinary GM approval/results without adding unsupported world mechanics. |
| Tentative/committed intervention | Add committed intent and InterventionCheck. Resolve bounded reaction results before the original decision, then schedule dialogue. Verify warning versus committed retry, source/actor authority, no GM re-entry, shared budget and retained effects on failure. Physical reactions remain with combat rules. |

The following sections specify these pending increments. Reuse existing
Store/Game/worker operations and checked SQL with forward migrations; no generic
workflow engine, parallel executor or file-movement prerequisite is needed.
Keep todo.md's overall order; these are checkpoints within the same NPC work,
not deferred items. Each is a coherent end-to-end change with its applicable
acceptance checks and AGENTS.md self-review. Schema/reader refactors belong with
the first checkpoint that needs them, not in separate scaffolding commits.

## Shared perception delivery and NPC responses

### Contract and scope

An observation is the message already delivered to a character's history.
Perceptions offer an opportunity to respond regardless of originating tool or
content kind; a character's own action does not invite its own response.
Look descriptions, narration following speech and presence
changes follow the same rule. Whether an NPC speaks or chooses DoNothing is its
decision; do not add tool-specific exclusions to reduce model chatter.

Do not insert an additional observation message or Rust-generated
Take sentence beside the GM's narration. Speech retains verbatim authored words
and structured attribution. Take uses the existing GM narration in this
increment. Future actions may expose appropriate results directly, such as
combat mechanics or dice rolls, through the same perception delivery path.
Internal approval receipts are not perceived events. A rejected action has no
automatic public failure announcement, but any narration the GM actually gives
its observers can be perceived and answered.

Use the relevant group as the default audience and apply validated, group-local
GM inclusion/exclusion before delivery. Whisper starts with its explicit
listeners. Preserve offline-PC speech hearing and the existing connected-PC
narration rule; include NPCs in narration as design.md already requires. Do not
expand narration to every group at the same location in this slice. Speech prefixes
describe hearing and must follow the same restricted audience. Keep action
permission separate from who perceives the outcome.

Each recipient gets one history entry referencing shared content. The delivered
entries are the record of who perceived it, including after later group changes.
Delivery and registration of pending NPC responses commit together. Action
mutation and immediate delivery share the existing transaction where applicable;
later generated narration has its own delivery transaction. Never keep a database
transaction open across inference or pretend later narration was atomic with an
earlier action. Publish wake-ups only after commit.
The worker processes the pending response work, not a repeated scan of delivered
messages. No new message-reprocessing or deduplication layer is required.

### Implementation detail for the checkpoints

Use the checkpoints above to bound commits. The detailed work below specifies
their common end state, not one mandatory giant commit. Preserve existing speech
behaviour while adding the narration tools, then verify speech and Take through the
same message-driven response mechanism. No temporary alternate delivery path or
rename-only checkpoint is needed.

- Replace the speech-only message-to-action join with an optional action
  reference on the existing message record. Keep its meaning distinct from
  `source_tool_execution_id`, which answers a tool call. Retain producing
  inference provenance for narration; a narration need not correspond to exactly
  one action. Reuse action/tool records rather than copying their arguments.
  Use forward migrations, preserving existing databases and recorded requests.
- Remove `speech_message` after migrating its links and updating all readers.
  Remove `speech_batch` if its remaining group/sequence references can be
  resolved from existing records without losing historical facts. Do not require
  removal of the speech metadata row just to reduce table count: preserve
  speaker/addressee IDs and frozen names, whisper intent, prefix and text format
  in the smallest coherent representation. Do not add a target column intended
  to cover all future action kinds. A new action kind must not require its own
  observation or response tables.
- Keep prefix with speech metadata. Shared perception delivery does not make
  every content field generic: non-speech actions do not acquire a prefix
  argument or storage field. Preserve speech prefixes across all readers and
  historical request reconstruction. Extend content-specific metadata only
  when that action kind has a demonstrated need.
- GM narration is one terminal tool call, in the model's ordinary tool-call
  format, in place of its plain-text final answer. The GM picks exactly one of
  four tools, which differ only in who receives what:
  - `narrate_group(text, exclude_character_ids)`: one text for the group's
    normal audience, the standard case. The optional exclusions are validated in
    the authorized group and can only subtract from that audience.
  - `narrate_actor_and_others(actor_text, others_text, exclude_character_ids)`:
    the actor receives `actor_text`; everyone else in the normal audience,
    except the exclusions, receives `others_text`. The actor is the character
    whose action or arrival the GM request describes: the requester of a
    presented action, including NPC and reaction actors, or the character
    entering a scene. It requires one; document that binding in the actual
    model context and reject the call when the request names none.
  - `narrate_characters(character_ids, text)`: one text for exactly the listed
    characters. The list is required, nonempty and validated.
  - `narrate_nothing()`: the GM judges the outcome already conveyed, so it has
    nothing to add. The classic case is speech, which is delivered verbatim and
    not re-narrated as who said what.
  An answer never carries more than two texts: no list of entries and no mixing
  of forms. Each form takes its own arguments, so none is an optional override of
  another. The narration call is the last thing the GM does: it is the only call
  in its response and follows the action decisions whose results it needs. It
  ends the turn. Record its result in the GM's history so the history stays
  valid, but run no closing inference.
- Validate the arguments with the existing tool-argument machinery and its
  `prompts.toml` descriptions, then validate pending-action resolution and every
  recipient before any delivery, and commit all delivered texts with the pending
  NPC work together. An invalid narration is answered, not repaired behind the
  GM's back: a call that is malformed, names foreign recipients, narrates before
  its action is resolved, mixes with other calls or repeats is a rejected tool
  call, so the GM receives the error as the call's result and runs again; a
  plain-text answer receives an instruction message saying to narrate with one
  of the tools. Nothing is delivered for the invalid attempt, and earlier
  committed actions remain. A rare recovery in a live game is acceptable, but it
  is an error and not normal: log it at error level, flag it in Developer Mode
  like any rejected call, and treat it as a failure in every real-model test.
  The usual inference limits bound the retries. Update every GM invocation,
  including opening and presence, to finish with a narration call.
- Narrate the initial scene to the group before any player is in it, using
  `narrate_group`, so the NPCs have it in their histories. When a player
  character then joins the group, the GM narrates the entrance with
  `narrate_actor_and_others`: the entering character is the actor and receives
  the scene as `actor_text`, and the others receive a brief arrival as
  `others_text`. This replaces the current single opening narration. For a
  new character's first turn, the player agent asks the player what they want
  to do after that narration. Open question, not built: the scene narration
  offers the NPCs a response opportunity with nobody present, so they may talk
  to each other. Add no special handling or filter for it; observe what
  happens and report it.
- Store the model's unmodified output as the GM's history. Record each delivered
  text once, linked to its producing inference and to which text of the call it
  is; recipient histories reference that shared text, not the call. Historical
  plain-text narration keeps its references and request reconstruction. Do not
  manufacture observer narration for an outcome that is already conveyed,
  and do not duplicate verbatim speech as narration.

  Worked example: Mara (character 7) climbs a rock and sees guards beyond a wall,
  and character 9 cannot perceive her movement. The GM calls
  `narrate_actor_and_others` with `actor_text` "From the rock, you see three
  guards beyond the wall.", `others_text` "Mara climbs the rock for a better
  view." and `exclude_character_ids` [9]. Mara receives only the first text,
  character 9 receives nothing and no NPC response opportunity, and the other
  eligible members receive only the second. Ordinary narration to everyone is
  one `narrate_group` call with no exclusions; do not automatically exclude the
  actor in that case.

- Implement the declaration's global shared-detail setting, default on, and
  supply it as a fact in GM context. It guides routine narration to players;
  explicitly private actor descriptions and chosen audiences always stay private.
  Do not automatically copy private information into observer or NPC histories.
  Alternatives compared: plain-text final answers parsed into an audience leave
  the audience to text conventions; a JSON batch in the final text adds a format
  the model is not trained to emit and a second parsing path; ordinary
  narration tools leave a closing response to define. Terminal tool calls reuse
  the tool-call format the model is trained on, the existing argument validation
  and the existing recorded-call machinery, at the cost of one terminal outcome
  that ends the turn. The user asked for exactly one of a few structures and for
  tool-like calls. Exercise the four forms through captured real-chat contexts
  before claiming that they improve routing; expected model behaviour is not a
  measurement.
- Consolidate recipient validation and history delivery for speech and GM
  narration. Speech approval controls who hears verbatim speech; the narration
  tools choose who receives their separate text. Do not add general
  narration audience fields to approve_action as a competing routing mechanism.
  Audit player and NPC action paths together: exclusions must restrict actual
  delivery, not merely a receipt from approve_action. Resolved recipient
  IDs travel with the narration; prefix remains speech-only.
  Do not re-resolve approval-time names against a later roster or depend on a
  mutable "last audience" on the GM. Use the existing resolved-action/narration
  data flow, retaining a recorded snapshot wherever later reconstruction needs
  it. Standalone group/presence narration keeps its existing default audience;
  this does not require inventing an approval call for narration-only work.
  Preserve the rule that
  all tool results precede deferred deliveries. Do not deliver speech twice
  under different names; any additional GM narration is distinct content and
  must retain the speech's privacy boundary. Apply the same rule to non-speech
  narration: the two texts of one call must not leak restricted outcomes to the
  union of their audiences. Each text delivers only to its validated audience;
  do not attempt to redact generated prose afterward.
- Change pending response sources from speech-tool-only references to the
  actual delivered message IDs. Use those entries and their history ownership
  to identify NPC recipients; do not reconstruct a past audience from current
  membership or join through speech tables. Keep original source IDs when
  coalescing unanswered messages, and revalidate current membership before
  acting. A character's own action does not invite its own response, regardless
  of action kind. Distinct perceptions can still offer that character a response
  opportunity. Source registration may
  cover several NPCs, but each decision's causal links name only the messages
  delivered to that NPC. Do not attach private sources merely because another
  NPC in the same group received them. Coalesce pending work without merging
  audiences or identifying distinct messages by matching their text.
- Extend the existing causal relationship to reference a source message as
  well as its existing direct tool-call cause. Use foreign keys and an
  exactly-one-source constraint, retaining direct proposal-tool causes for
  selected execution. Update causal readers and Developer Mode navigation
  together: NPC response -> received message -> originating action/inference.
  Narration without an originating character action links to its final producing
  inference and which text of its narration call it is, not a fabricated tool execution, action or approval.
- Register an opportunity as part of delivering any perception. Human input,
  instructions, inference bookkeeping, tool receipts, summaries and DoNothing
  results do not independently schedule responses. Distinguish what a character
  perceives from its agent's internal conversation, rather than checking an
  allowlist of action names or adding per-kind scheduling callbacks. This is a
  delivery-path operation, not a scan for arbitrary history changes, a second
  event bus or a separate speech/combat response subsystem.
- Retain the existing finite-step world worker and necessary saved pending work:
  source messages, decision invocations, selected proposals, scene revision and
  shared budget. New received messages during collection supersede stale
  proposals without losing unanswered sources. Own tied deliveries preserve
  remaining selected words; external changes can invalidate them. A boolean
  dirty flag or checking the last history entry alone cannot represent these
  states. Do not replace working scheduling with recursive calls or detached
  per-NPC tasks.
- Preserve admission after the originating turn has unwound and completed
  successfully. Registering a received message does not immediately start an
  NPC inference inside its producer's tool stack. Resolve readiness from that
  message's producing inference/sequence or referenced action sequence, including
  standalone narration, rather than retaining the speech-only readiness join.
  A failed originating turn leaves its committed content intact but fails its
  pending follow-ups visibly under the existing failure policy. Carry the
  originating discussion's root and spent budget when registering narration
  sources; do not grant a new budget because a source is narration instead of
  speech. Standalone presence work uses its own recorded originating sequence.
- Rename misleading `round`/`reopen_round` terms while updating their actual
  operations, schema and readers. Name stored data for pending NPC responses;
  names such as `replace_pending_npc_responses` must describe the function body.
  Do not split a function merely to satisfy a naming rule or perform a broad
  mechanical rename before the structure is settled. There are no combat-style
  turns granting each NPC a slot to speak.
- Expose `look`, `investigate`, `open` and `ask` with distinct model-facing names
  and concise descriptions, sharing the existing description-argument schema,
  character-action submission and GM arbitration implementation. Look means
  observation, not arbitrary actions/questions; Ask requests information or
  clarification from the GM, while speaking to a character uses Say/Whisper.
  Preserve each actual tool name and arguments through validation, pending and
  resolved action handling, model context and inspection. Do not silently alias
  everything to Look or duplicate the executor. Apply the same shared path when
  these actions are offered to NPCs; do not add separate NPC variants. This
  interface change does not invent unsupported state mutations, saves or time
  mechanics. Review descriptions under prompt_standards.md and exercise routing
  through the real chat path rather than checking only generated schemas.

Review remaining scheduling fields and branches against the retained behaviours
above; remove unused state and duplicate paths. Preserve progress at completed
step boundaries, restart without replay, and visible crash failures. Line/table
counts are evidence for investigation, not proof that required state is waste.
Update implementation_reference.md only after the code exists.

### Acceptance for this slice

Retain U1–U6 and E1–E9 below. Extend existing scenarios rather than adding a
separate suite for every delivery kind:

| Case | Expected outcome and evidence |
| --- | --- |
| Speech and Take share delivery | Approved Say reaches each hearer unchanged; an approved Take changes ownership and its GM narration reaches PCs/NPCs once. NPC context contains that same narration and can produce an ordinary GM-approved response. No extra generated event sentence or second response path. |
| Distinct general GM requests share implementation | Observe surroundings, investigate a clue, attempt to open something and ask for clarification use the appropriately named tools. Scripted production-path checks verify all four reach ordinary GM arbitration with their actual names/arguments and normal authorization; a representative real chat checks that Look is not presented as the generic action. Narration audiences depend on what is perceived, not which name was used. Do not claim unsupported world changes occurred. |
| Requester and observers see different information | Exercise the worked `narrate_actor_and_others` example: the actor receives only the detailed text; other eligible members receive only the brief text; an excluded character receives neither. Check history, captured prompts, reload and pending NPC work. `narrate_group` without exclusions includes the actor normally. Foreign exclusions fail the call before any delivery. Shared-detail on/off guides routine narration, but explicit private delivery stays private in both modes. Cover player, NPC and reaction actors, an entering character as the actor, and a narration-only scene invocation without an actor, for which the actor tool is rejected. |
| Eligibility is independent of tool kind | An actually delivered perception offers its NPC recipient a response opportunity. An NPC's own action does not itself invite another response from that actor; apply this equally to speech and other actions. A scripted DoNothing ends the pending work; no tool-specific suppression is used. Speech's prefix remains available without imposing it on other action kinds. |
| Perception overrides and privacy | Exclude an in-group NPC from Take narration: it receives neither text nor a response opportunity. Included NPCs receive both. Whisper and its additional narration stay private; an NPC decision's causes do not include another NPC's private sources. Invalid final narration audiences visibly fail the turn before any delivery; they are not tool-call rejection records. Invalid speech approval arguments remain recorded rejected tool calls. |
| Narration tools and narration without a character action | Group/presence uses `narrate_group` and links replies to received messages and the actual producing inference/text. Exercise all four tools: a standard group narration, actor-and-others, addressed characters and nothing. A mixed response, two narration calls, plain text, an actor tool with no actor, unresolved actions or foreign recipients deliver nothing for that attempt and return the error to the GM, which runs again; prior action effects remain. Any such error fails a real-model test. The narration call needs no closing inference or synthetic action record. No narration-only opportunity is stranded by speech-only queries. |
| Input arrives during a decision | Two delivered perceptions are retained; stale unexecuted proposals do not run, and the next decision sees the received messages. DoNothing consumes the pending sources; bookkeeping and unchanged history cannot restart the conversation. |
| Migration and all readers | Existing speech, IDs/frozen names, prefixes, authored/legacy rendering and historical request reconstruction survive migration. Model context, live chat, reload, compaction and Developer Mode show the same received content and its true cause. |
| Worker and lifecycle | Preserve eagerness/tied selection, other-group progress, raised hands, shutdown between complete steps and crash failure without replay. Pause a producer after delivery: no NPC decision starts before its successful completion, and narration retains the same causal budget. Fail the producer: committed messages remain and follow-ups fail visibly. Disconnecting a browser cannot cancel a queued player turn; check completion and its reply, not only stored input. |

Use scripted Store/Game paths for routing, permission, source links and migration
checks. Exercise changed tool descriptions/context through the real chat path
with captured backend input, reviewing the cases with the user first as required
by coding_standards.md. Do not start conversational tuning or require a specific
NPC to reply. Keep the five-minute test budget; use existing lifecycle checks
rather than repeated GPU scenarios. Finish the AGENTS.md self-review before
committing each checkpoint. Complete the shared-delivery, response and tool-name
checkpoints before proceeding to intervention; do not mark the overall increment
complete while any of its acceptance requirements remain pending.

## Intervention lifecycle

This later slice resolves bounded reactions before the original action, rather
than automatically rejecting the original and postponing every reaction.
Add committed to Take's arguments (default false) and later actions that can be
tentative. Carry it to the GM in the recorded request. Tentative means willing
to pause for an objection; committed means proceeding despite objections, not
physical force, guaranteed success or extra action allowance. The character agent
sets it from the human's expressed intent, not as an automatic retry mechanism.

InterventionCheck takes the original action ID and selected characters. Validate
their world, scene and authority. NPCs return reaction intentions through short
child decision invocations. These do not execute actions or ask the active GM
for approval again. Never invoke an agent already active on the stack; its
original intent is already available. Human non-intervention remains the default;
blocking for human consent belongs to the later explicit-consent protocol.
Use the existing child invocation/tool records and pending actions for these
intentions, applying the same decision validation and eagerness selection rules
as responses. No npc_discussion/npc_round/npc_candidate rows are needed to hold
an unresolved InterventionCheck. Collect against the pre-reaction histories,
return selected intentions, and close that child collection before GM approval.

Return reaction IDs and intentions to the current GM. Let that GM approve a
reaction through the existing action executor, receive its real result, then
approve/reject the original request. Scope approval authority to that original
request and its validated reactions, not arbitrary actions in the world. A Say
reaction may be approved and heard before a tentative Take is rejected. A
committed retry cannot be repeatedly stopped by the same verbal warning alone;
actual physical prevention can still require implemented checks or combat.
Approval is immediate; no ConditionalApprove tool, dependency language or dynamic
rule interpreter is introduced. Save those speculative ideas for playtesting.

The NPC whose reaction was approved is not called again to get GM approval; its
decision has finished and the GM applies the recorded intent directly. Finish
the bounded reaction and original tool results before scheduling open-ended
dialogue in fresh turns. All nested work shares the original budget, and the
causal record links reactions to InterventionCheck and the original action.
DoNothing does not deny the original; neither does merely proposing a reaction.
Actual system errors still fail the turn and retain committed effects.

Combat, attacks, action allowance and contested physical outcomes depend on
[Rules and travel](playable-rules.md); do not invent simulated combat in this
NPC increment. Verify the prisoner's warning and committed retry in this increment, and
verify the shopkeeper's physical opposition when those rules are implemented.

## Combat boundary

Interrupt changes scheduling, never character eligibility or allowance. Route
all resulting requests through the existing shared action admission boundary;
do not grant a turn because a hand is raised or because an action flag is unused.
Combat order/flags, Move, Wait and damage are not implemented in the current
runtime. Their complete implementation remains in [Rules and travel](playable-rules.md),
with the accepted rule that closing a game-turn consumes remaining allowance and
only a new game-turn resets it. This experiment does not expose dummy combat,
attack, movement or travel tools, or claim combat-abuse tests have passed.

## Verification and completion

Use existing browser and configured-model game paths; do not make MCP a gate.
Before writing model-behavior tests, review these cases and validation approaches
with the user as required by the coding standards:

Use these as complementary observations, not separate suites per table cell:

- A: scripted model output through production Store/Game paths checks selection,
  delivery, authorization and state changes deterministically.
- B: real-model conversations and captured production prompts check NPC-owned
  dialogue, knowledge boundaries and natural stopping. No prescribed wording.
- C: stored decisions, recipient histories and Developer Mode causal links prove
  which original events/inferences/actions produced the result and detect replay.

One scripted production-path scenario should normally assert its world/player
outcome (A) and inspect the relevant evidence (C) together. Combine compatible
cases in one conversation, such as addressed speech, competition, a tied reply
and stopping. Add isolated cases for invalid inputs or failures that prevent
continuing that conversation. Do not repeat an outcome test merely to assert
its implementation's status enum, column values, query order or candidate order.

Reserve real-model validation (B) for NPC-owned dialogue, same-history tied
replies, private knowledge and natural stopping across representative
conversations. Argument rejection, selection arithmetic and transaction rollback
are deterministic production-path checks; running the model adds no coverage.
Re-run broad checks at a complete-slice checkpoint or after a relevant failure,
not after every state transition or cosmetic edit.

| Case | Expected outcome | Verification |
| --- | --- | --- |
| U1: Say addresses one NPC | All actual NPC hearers get an opportunity; an addressed NPC may choose DoNothing. Selected speech retains the actor's authored style, without a fixed reader-added sentence, and gets ordinary GM approval. | A/B compare authored text through live/reload/context paths; C links speaker and source. Include text that omits the speaker's name: the captured backend input must still identify the speaker and addressee. Verify historical request reconstruction remains unchanged. |
| U2: Different eagerness | Highest wins; losing proposals are superseded and their actors reconsider newly heard speech. | A contrasts 1/2/3; C checks only the winner acted. |
| U3: Tied speech | Highest ties composed from the same history get separate single-action GM turns in any order. Earlier tied speech does not regenerate/invalidate the remaining chosen words. Approved words arrive once before collecting follow-up responses; prefix is optional. | A/C check original decisions, individual approvals and deliveries; B checks intelligible overlap/style. |
| U4: All DoNothing | No GM approval or follow-up inference after collection completes. | A counts model work; C shows a settled round without actions. |
| U5: NPC conversation | Fresh follow-up turns share one budget; NPCs own dialogue and the GM owns approval/consequences. Evaluate natural stopping over repeated conversations. | B inspects actual prompts and play; A/C verify budget sharing and causal order. |
| U6: Private speech | Only actual hearers other than a self-only speaker get opportunities. Two selected whispers to disjoint audiences have independent words, prefixes and narration. Excluded histories/prompts reveal no private words, style or unseen participants, including when a whisper is rejected. | A includes connected bystanders and revealing GM narration; B/C inspect captured inputs and delivered histories. |
| E1: Human input mid-round | Input runs between finite steps and supersedes stale unexecuted work; committed words stay. | A controls inference boundaries; C confirms no stale action. |
| E2: Split, merge, departure or access loss | Revalidate selection, actor authority and recipients before execution; no actor acts in a retired/wrong group. | A exercises actual mutations; C checks resulting action ownership and deliveries. |
| E3: No NPC hearer or self-only speech | No opportunity or inference is created. In a tied set, hearing another speaker still allows a later opportunity. | A/C inspect work and heard source references. |
| E4: Malformed decision | Free text, multiple calls, noninteger eagerness, 0 or 4 fail visibly and leave valid answered tool history, without a correction loop. 1 and 3 are valid boundary values. | A checks errors and subsequent chat assembly; C checks original output and closed executions. |
| E5: Budget exhausted | Discussion fails visibly without a new allowance or silently dropped reply. | A exhausts the shared limit across NPCs; C checks persisted usage and failure. |
| E6: Individual rejection/failure | Rejection of one tied utterance leaves earlier approvals intact and allows remaining selected actions, not losing alternatives. A system failure ends the discussion, retains prior effects and closes unanswered history. No approvals/no new event ends work. | A tests sequential outcomes and partial delivery; C checks effects and valid tool history. |
| E7: Bad/repeated action approval | Foreign, wrong-actor or unselected action IDs and invalid hearing adjustments are refused through existing approval validation. A repeated approval returns that action's recorded outcome without another delivery. | Retain existing NPC action-path coverage; C checks words/recipients, not a new batch invariant suite. |
| E8: Clean shutdown or crash | Clean shutdown finishes/saves the current utterance; restart continues remaining selected actions without replaying earlier ones. A crash fails its active discussion, retains speech and closes unanswered history without replay. | A plus process stop/restart between tied utterances and during approval; C compares effects, IDs and budget. |
| E9: Another group | It is not held back, superseded or given private speech by this discussion. Its accepted input uses the ordinary serialized world worker between finite steps, not concurrent world mutation. | A submits input in both groups; C checks isolation and ordering. |

Interrupt/Never mind and multi-tab/other-group checks are existing regression
coverage, not a future implementation slice. Extend them for an agreed policy
change or a demonstrated gap. Remaining increments verify perceived-event
delivery without bookkeeping-triggered loops, then tentative/committed Take and
intervention. The prisoner's warning must not veto a committed retry forever;
approved reactions precede the original decision without GM re-entry. Verify
shopkeeper physical opposition with the implemented combat rules.

Check deterministic selection, authorization and transaction outcomes through the
real Store/Game paths. Inspect captured model input and Developer Mode causal links,
and use relevant real-model chats for changed model-facing behaviour. Natural
stopping and conversational tuning are user-owned; do not begin repeated tuning
runs without the user's direction. Do not validate wording by
transcript-specific string matching. Record any proposed routing/denial policy
change as a decision for the user rather than quietly changing the declarations.

Update implementation_reference.md only for implemented behavior. Run appropriate
Rust, offline SQL/schema, lint and browser checks, perform self-review, and commit
each complete slice. Follow AGENTS.md's five-minute testing budget. Mark complete
and archive only after remaining mechanics, applicable verification and tuning
acceptance are resolved; combat's separate plan keeps its own completion gate.
A removed requirement is not a completed requirement.
