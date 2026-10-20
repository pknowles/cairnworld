# NPC speech, response selection and player interruption

Slice 1 (shared speech) is implemented; the response rounds, player control and
intervention slices are pending. Builds on the implemented server shutdown and
failure handling, and precedes the coding-agent interface; it is not a claim that
combat or InterventionCheck already exists.

## Outcome and authority

Players hear one another's actual speech, NPC agents decide their own responses,
and a conversation can continue without nesting Say calls. Players can pause
future NPC replies and either send input or choose Never mind. GM-selected
intervention can resolve a tentative Take before a separate discussion begins.

Authority: [NPC Interaction](../user_declarations.md#npc-interaction),
[Character actions](../user_declarations.md#character-actions),
[GM tools](../user_declarations.md#gm-tools),
[World game page](../user_declarations.md#world-game-page),
[Developer view](../user_declarations.md#debugging-and-telemetry), and
[the turn/pause clarifications](../user_exerpts.md#2026-10-20-npc-speech-interruption-and-game-turn-allowance).
Follow [NPC speech and response rounds](../design.md#npc-speech-and-response-rounds).

The current game has recorded NPC identities/history branches, group topology,
an inference scheduler, serialized world events and shared chat delivery. Say
currently reaches GM arbitration and narration without invoking NPC agents.
Use those production paths; MCP, a new job framework, complete character creation,
inventory normalization and full combat are not prerequisites for this experiment.

## Feature requirements

The requirements below describe the complete feature. The concrete design and
commit order follow them. Each commit follows the
[AGENTS.md checklist](../AGENTS.md#post-feature-and-pre-commit-checklist),
[coding standards](../coding_standards.md) and
[prompt standards](../prompt_standards.md). Before writing new LLM tests or
model-facing changes, expand the verification cases below into the required
use-case/edge-case/validation review and obtain the user's review.

### Shared speech and character actions

- Keep browser access and character ownership checks at the player membership
  boundary. Share existing speech delivery and action validation. When adding NPC
  execution in the response slice, resolve npc_agent and the same world/character/
  history ownership and reach that same executor. Do not fabricate PlayerAgent
  values for NPCs, duplicate take/say, or add unused NPC constructors in advance.
- Extend Say with an explicit addressed character and add Whisper's explicit
  targets. Addressing is not privacy. GM approval defaults Say to everyone in the
  group and can supply an audience override; reject contradictory include/exclude
  arguments and invalid scene/world references. Snapshot recipients at commitment.
- Store the approved original words once, with speaker identity derived from
  the producing character/action, and append references to actual hearers' player,
  NPC and GM histories. PCs in the group hear Say even without a browser open.
  Excluded characters receive neither private words nor revealing overlap labels.
- Render attributed speech through the same live/reload history reader, with no
  duplicate echo in the speaker's chat. The GM can narrate consequences but must
  not replace an NPC's own response. Preserve ordinary narration's separate
  connected-PC audience rule and all existing history reconstruction.
  Incoming speech updates PC chat/context without automatically invoking player
  agents; their replies start from human input. NPC opportunities use actual hearers.

### Response rounds and safe player control

- Add concrete durable response-round state alongside existing world work. It
  retains source tool-execution references, group/history cut, collection/selection/
  execution phase, candidate inference references and selected proposal references.
  Derive words/arguments from original recorded outputs, not copied transcripts.
- An NPC decision receives its own character facts and heard history and produces
  DoNothing or a proposed implemented character action plus integer eagerness
  1–3 (low, moderate or strong desire to act now). DoNothing has no eagerness.
  Proposal generation performs no world action and cannot call an active
  GM recursively. Keep GM/Storyteller-only information out of NPC context.
  Use one terminal decision per opportunity: DoNothing/propose record a decision,
  while only the selected proposal reaches the ordinary character action executor.
- Collect against one scene/history version. Highest eagerness wins; if all tied
  winners are speech, submit them together for GM approval and commit the approved
  utterances as one batch, delivering their messages in any order. Their proposals
  were generated from the same history without hearing one another; finish this
  selected set before starting another response round. The GM may describe
  overlap through prefix, but no automatic marker is required. Otherwise record
  one random tied
  winner. Supersede losing proposals; never replay them after the winner changes
  the situation. Every affected NPC reconsiders the new heard event, excluding
  self-only speech. Selected tied speech is not cancelled merely because another
  utterance in that selected set commits. All-DoNothing finishes the round
  without another inference.
  Finish the simultaneous batch's speech permission/audience checks before any
  utterance commits. A separate external event or topology change still requires
  the usual pre-execution revalidation.
- Follow-up actions are fresh chat-turns after the originating call stack unwinds.
  Link their sequence to the exact triggering Say/InterventionCheck and selected
  proposal execution in Developer Mode. Distinguish causal follow-ups from nested
  invocations. Extend existing recording/query reconstruction rather than adding
  a second transcript or independent NPC executor.
- Advance one finite piece of work through the existing world loop, yielding to
  player input between pieces. Coalesce newly heard events, recheck the stored
  scene/history version before executing a proposal, and invalidate stale work
  after intervening speech, player actions, split/merge or departure. Already-heard
  words remain in history. The causal discussion shares the existing inference/
  tool budget across follow-ups; do not reset it per NPC or silently drop errors.
- Add Interrupt and Never mind controls, authorized against the current character
  and group. Interrupt signals the world state while inference is running; a
  command queued behind the whole discussion cannot implement this. Pause future
  NPC execution at a safe boundary, preserving completed tool results. In-flight
  proposal inference may finish recording, but no action publishes after pause
  acknowledgement. Do not abort committed actions or pause unrelated groups.
- Each character owns its pause hold. Send, Never mind, final disconnect or loss
  of access clears its hold; another player's hold stays active. Release holds
  for retired groups or characters that left and publish the updated control state.
  Other players may still submit
  legal input. Never mind needs no model/tool call and changes no game-turn flags.
  Resume revalidates proposals against current state instead of playing old replies.
- Planned shutdown finishes the active step and saves its progress/budget before
  stopping; restart resumes waiting work between steps. A crash during a step
  fails its discussion through shared crash cleanup, retaining committed speech
  and effects without replay. Discard disconnected pause ownership.

### Tentative actions and GM-selected intervention

- Add InterventionCheck(action_id, character_ids) to the real GM tool set. Validate
  the original pending action and selected actors against the GM's world/scene.
  NPCs return short proposals; they do not start a discussion or recursively ask
  the GM to approve those proposals while that GM is still deciding the action.
- Return the selected proposal(s) to the GM before its Approve/Reject decision.
  Do not automatically reject every intervention; that policy remains for
  playtesting. A rejected tentative Take can queue the selected NPC dialogue only
  after the original call completes. An already executed Take is not undone.
- Humans who do not interrupt default to no unsolicited intervention; this is
  not approval of explicit consent requests. Do not introduce a suspended human
  dialogue inside Take or reuse the future transfer/travel continuation here.
  Selected non-speech NPC proposals later use the same ordinary action checks.

## Design choices

The selected options minimize new mechanisms while meeting restart, simultaneous
speech and interruption requirements. These are implementation decisions; routing
policy and automatic denial remain the user's playtesting decisions.

| Question | Options, benefits and risks | Selection and reason |
| --- | --- | --- |
| NPC scheduling | Nested calls need little queue code but re-enter the active GM. Detached tasks allow overlap but race world state and complicate shared budgets. Concrete SQLite rounds in the existing worker require checkpoints but retain one executor. | Concrete rounds; reuse world serialization rather than introduce another scheduler. |
| Speech storage | Copy text to every recipient: easy but duplicates history. Point into arbitrary JSON tool output: avoids a text row but couples readers to proposal layout. Share one committed action-result body and retain speech metadata: small explicit schema and existing content reconstruction. | Shared action-result body; event words are stored once for delivery. The original model arguments remain original inference evidence, not another recipient transcript. |
| Stale decisions | Compare every live field/hash: expensive and easy to omit. Use the GM branch head: existing value, but inference bookkeeping would invalidate its own round. Use a group scene revision: one explicit semantic counter, requiring an audited write boundary. | Group scene revision, changed only by committed fiction/state changes. |
| NPC decision protocol | Reuse the full agent loop: familiar, but permits extra tool batches and adds settling calls. Free text plus parsing: fewer schemas, unreliable control. One constrained terminal decision: one call per NPC, explicit failure on malformed output. | One recorded inference returning exactly one DoNothing/propose_action call; no second tool executor or text parser. |
| Simultaneous speech | Sequential ordinary approvals can publish half a batch. Infer approval from NPC intent bypasses the GM. One GM batch-resolution tool admits all winners before a single transaction. | resolve_speech_batch for NPC speech; ordinary player Say and this tool share the same speech commit helper. |
| Pause handling | Queue the control behind NPC work: cannot interrupt promptly. Cancel arbitrary futures: risks half-completed tools. Direct live control with boundary acknowledgement: permits typing while retaining committed work, but acknowledgement can wait for an active action. | Direct pause holds and a watched worker activity value; never cancel a running action. |
| Budgets and restart | Memory-only counters grant a fresh budget after restart. Arbitrary crash continuation requires durable nested-call progress and inference admission. Saving counters between complete steps permits clean restarts with smaller changes, while crashes fail active discussions. | Finish steps on planned shutdown and checkpoint the existing Budget between them; use shared crash cleanup for exceptional interruptions. |
| Shared actions | Fake PlayerAgent for NPCs breaks ownership. A parallel NPC executor duplicates rules. A private resolved character actor plus existing action execution shares rules without giving browser callers NPC authority. | Resolved character actor; player authorization remains at the browser boundary. |

## Data model and invariants

Use two forward migrations: speech metadata with the first slice, response state
with the second. All new fixed queries use checked SQL and the existing offline
metadata workflow. Keep existing sequence/inference/tool tables as the record of
model work. Do not put prompts, proposed words, eagerness or arguments in a second
work transcript; decode them from the referenced immutable tool output.

| Record | Columns and constraints |
| --- | --- |
| speech_batch | id primary key; world_id, group_id, sequence_id foreign keys. A batch contains one or more actual utterances. Simultaneity is derived from multiple utterances in that batch, not a second independent flag. |
| speech | action_id primary key/FK action; batch_id FK; speaker_character_id FK; speaker_name snapshot, NOT NULL; kind say or whisper; prefix, the GM's optional short phrase shown to every hearer before the words (at most MAX_PREFIX_CHARS); addressed_character_id nullable FK with its addressed_name snapshot (both or neither; Say's optional addressee, which affects only how the line reads and who is expected to answer, never who hears it). The approved action's result_content_id is the one Text body containing the exact words. Frozen names keep old requests reconstructable after renaming. |
| speech_message | message_id primary key/FK message; speech_action_id FK speech. A message remains an ordinary conversation entry with shared content; this link identifies speech without rebuilding message's existing origin CHECK or treating speech as an answered tool call. |
| npc_discussion | root_sequence_id primary key/FK sequence; limits JSON snapshot; spent_total nonnegative integer; state waiting_root, ready, settled or failed; failure_sequence_id nullable FK, present only for failure. Stores the existing Budget's checkpoint between complete steps. |
| npc_round | id primary key; root_sequence_id FK discussion; group_id FK; scene_revision; mode speech or intervention; state collecting, selected, executing, settled, superseded or failed; original_request_tool_execution_id FK only for intervention. A partial unique index permits at most one collecting/selected speech round per group. An executing round may create that next round, which cannot run until its source sequence succeeds. |
| npc_round_source | round_id and source_tool_execution_id FKs, composite primary key. Speech sources resolve committed Say/Whisper actions; intervention sources identify the exact InterventionCheck execution. Multiple tied utterances may trigger one round. |
| npc_candidate | round_id and character_id form primary key; decision_invocation_id nullable UNIQUE FK until started; decision_tool_execution_id nullable UNIQUE FK until a valid decision completes; execution_sequence_id nullable FK until selected work starts. A simultaneous cohort shares one execution sequence. State is waiting, running, ready, selected, executing, done, superseded or failed. These references represent lifecycle stages, with CHECKs matching the state. Eagerness/action values remain in the original decision output. |
| sequence_cause | child_sequence_id and source_tool_execution_id FKs, composite primary key. All source executions belong to the same world and earlier sequences; validate this on insertion. Existing invocation parent links still describe nesting, while this table describes separate causally related chat-turns. |
| group.scene_revision | NOT NULL integer starting at zero. Increment in the transaction applying a fiction/state change, never for inference, tool-record or proposal bookkeeping. |

Add unique indexes on pending_action.requesting_tool_execution_id and on non-null
action.requesting_tool_execution_id. A recorded proposal can request at most one
actual action. Validate existing rows before adding the indexes; do not delete
duplicates or weaken the invariant if migration exposes a defect.

Speech publication creates the approved action result, speech rows, recipient
messages/links and scene revision in one transaction. Reuse each action result's
content reference in every recipient history. Tool receipts remain distinct from
public speech; existing readers must not show a receipt as a second utterance.
Snapshot the audience once. Include each speaker's own history and all other
actual hearers. The trusted GM has the words in its arbitration request and the
delivery outcome in its approval result, not an extra speech-message copy.
Ordinary PC/NPC contexts never inherit another character's private
history or the GM's complete history. GM overrides are group-local in this
increment; direct interaction with another group first uses the existing merge
path. Reject out-of-group IDs rather than silently filtering them.

Centralize speech decoding in Store so player live/reload reads, raw inspection
and request_for_segments all derive speaker attribution and words from the same
speech record. Never consult a character's current name to rebuild old context.
Publish world chat/Developer Mode notifications only after commit.

Audit the existing writers that can change NPC context: actual character action
effects, delivered narration/presence, character creation/readiness/fact edits,
and split/merge. Their transaction helpers advance affected scene revisions;
new groups initialize their revision and retired groups supersede their rounds.
Private player input, prompt artifacts and decision records do not advance it.
Do not introduce polling, field hashing or catch-all database triggers.
Read-only actions, rejected requests and approval bookkeeping alone do not
advance the revision. A speech batch advances each affected group's revision
once, after checking the expected revision for the whole batch. Decode durable
rows into lifecycle-specific Rust enums; avoid structs full of independent
Option fields and repeated existence checks. SQL nulls describe stages before
an invocation or execution exists, not partially initialized runtime objects.

## Action and decision interfaces

CharacterActor exists (slice 1): action submission works on the acting character,
and Store::player_actor validates player membership to produce it. Its private
fields hold the world and character. Add NPC construction using the npc_agent
relationship and recorded selection; resolve its current AgentBranch when needed
for inference. Callers cannot supply the GM or group. Recheck current ownership,
active membership for a PC, source execution's actor/world, and current group at
action submission and commitment. Selected NPC requests additionally prove the
candidate/round selection and revision. Do not expose this constructor through
the browser or through arbitrary model-supplied actor IDs.

Reuse create_pending_action's existing character boundary.
Retain the existing pending action, GM arbitration and execute_approved_action
operations for ordinary actions. Decode a selected NPC proposal into the same
typed CharacterActionRequest that player tools construct. Its original
propose_action execution is the requesting execution; it can belong to the
earlier decision sequence. Do not fabricate an inference/tool call to imitate a
player. Inspection must resolve the nested proposed action name/arguments when
the source execution is propose_action, instead of labelling it as an executed
world action named "propose_action".

The decision tools are do_nothing with an empty object schema and propose_action
with {action: typed supported character action, eagerness: integer 1..3}.
Enforce the range in argument validation as well as the schema. Describe the
NPC's own desire to act; it does not know the other NPCs' proposed eagerness.
Initially offer Say/Whisper; add Take only in the intervention slice. Exactly one
tool call is required. Free text, unknown tools, multiple calls or invalid
arguments finish the recorded invocation with a visible protocol error. Reuse
context::complete_recorded, existing inference completion and tool-execution
recording; do not add another backend/recording implementation or automatic
correction loop. Complete a valid decision's result and candidate state in
one transaction, after the normal inference-output recording has completed.
A crash between those writes fails the discussion; shared cleanup closes the
unfinished tool history without generating or executing a new proposal.
Completing the proposal means "intent recorded", not "action
performed". The normal capacity-compaction path and original model output remain
available for inspection.

For a selected speech cohort, start one fresh GM invocation with the explicit
round ID, selected proposal execution IDs, actors, exact words and current scene.
Expose resolve_speech_batch(round_id, decisions), where decisions contains one
Approved(Hearing: include/exclude/prefix) or Rejected(reason) for every selected proposal ID.
GM approval is mandatory, including for apparently ordinary NPC speech. There
is no automatic approval path; who can speak and who hears are GM decisions.
Reject missing, duplicate, foreign or unselected IDs. Its one transaction applies
the whole admitted speech batch through the same helper used by player Say;
denied proposals publish no words. This tool's result applies immediately and
is idempotent by the round and original request executions. GM final free text
is not another copy of the NPC speech. Ordinary Say also delivers its actual
speech at approval rather than broadcasting a paraphrase of the GM's final text.
Repeated batch resolution returns the original recorded outcome. Keep the round
executing until its entire GM invocation finishes, even after speech commits;
follow-up rounds remain ineligible until that source sequence succeeds. A later
GM failure preserves speech but fails the discussion and stops automatic replies.

Extend the existing speech-write transaction to accept the admitted utterances
and create one speech_batch row for that set; ordinary Say supplies a set of one.
Validate all proposals and hearing adjustments before writing any utterance.
Reuse current per-utterance attribution, prefix and recipient delivery, including
delivery after all tool results in a call batch. Batch identity groups the
approvals; it does not require concurrent database writes, inference or special
presentation. Per-hearer prefixes must not reveal words or speakers they did
not hear.

In this slice replace the GM's current NPC dialogue with NPC-owned responses.
The GM continues to approve speech and narrate consequences; it does not compose
NPC replies on their behalf. This is required prompt/context work, not a pending
policy question. Verify the actual assembled prompts and real-model dialogue.

The implementation should expose these transaction boundaries as ordinary
Store/Game operations, rather than a generic scheduling interface:

| Operation | Responsibility |
| --- | --- |
| Resolve character actor | Produce an authorized player or selected-NPC actor proof; existing shared action submission rechecks mutable ownership and group state. |
| Commit speech batch | Validate all admissions, create canonical utterances and recipient links, advance revision and record response opportunities atomically. |
| Claim/finish NPC decision | Persist invocation ownership before inference; finish the decision, candidate state, sequence and Budget checkpoint afterward. |
| Select NPC round | Validate revision and persist winners, including the random tie result, without executing them. |
| Claim/finish NPC execution | Persist the new execution sequence and selection ownership; afterward finish the sequence, round outcome and Budget checkpoint together. |
| Advance NPC round | Game runs one of those finite steps and returns to the existing world loop. |
| Request/release NPC pause | World changes live holds and publishes authenticated control state; no database transaction spans acknowledgement waiting. |

## Worker transitions and transaction boundaries

One world worker owns execution. Do not spawn per-NPC world mutation tasks or
send NPC work into its own bounded WorldEvent channel. SQLite is the pending
round source; the existing Notify wakes the worker when work becomes eligible.
At a boundary, handle presence deadlines and queued foreground events first,
then the lowest-ID eligible unpaused round. Within a collection choose the
lowest character ID still waiting; this is collection order, not eagerness.
Wait on channel/Notify/deadline when no work is eligible, with no timer polling.

| State/event | Required operation |
| --- | --- |
| Speech commits | Record sources and NPC hearer roster; upsert the group's waiting round. It is ineligible until its source foreground sequence has finished successfully. Do not schedule the speaker for self-only speech. |
| New heard input while a round is collecting/selected | Supersede its unexecuted proposals and create a round at the current revision, carrying unconsumed heard source references. A new human root owns the new budget; NPC-only follow-ups keep the same discussion root. Never merge budgets by taking a larger allowance. |
| Collect one waiting candidate | Start a fresh causally linked sequence and claim running plus its decision invocation in a transaction. Run one inference outside transactions using the shared in-memory Budget, then atomically complete the decision/candidate, sequence and budget checkpoint. Return to the worker boundary. Intervention collection instead uses child invocations inside its existing foreground sequence. |
| All decisions ready | Validate group/revision, persist the highest selection and recorded random tie result. Mark DoNothing candidates done and losing proposals superseded. All-DoNothing settles without another job. Speech-only highest ties select the whole cohort; mixed/non-speech ties select one. |
| Execute selected work | First admit live activity under the pause mutex as described below, then claim its fresh sequence and executing state in SQL, rechecking scene and ownership. A speech cohort uses one batch resolution; a Take uses the shared ordinary executor. Finish sequence and outcome before selecting more work. |
| Revision/group changed before execution | Mark superseded, not failed. Start a fresh round only for still-relevant unconsumed heard input/selected intervention intent in that group; departed actors keep heard history but do not act in their former group. |
| Selected action rejected | Preserve its recorded rejection; do not try the losing candidates as fallbacks. A new actual event may create a new round. |
| Inference/action failure or budget exhaustion | Preserve original error and committed effects, fail that discussion and its remaining rounds, and notify its actual participant PCs through the existing authorized failure path. Unrelated groups remain usable. |

No transaction, SQLite connection or live-control lock spans model inference.
The revision refers to committed fiction, so the round's own recording does not
invalidate it. A simultaneous cohort finishes all admission decisions before
any of its words commit. Already completed effects are never rolled back because
a later model completion or notification fails.

Add checkpoint/restore over the existing Budget's limits and spent counter;
keep spending rules in one implementation. Retain the existing per-trigger
limits (currently 64 total inferences by default); no larger NPC-only allowance
is introduced. More frequent ties may consume that budget faster, which is a
playtesting observation rather than grounds to silently increase it.
At source-sequence completion save
the counter and make its rounds eligible in the same transaction. Each finite
step restores and shares that Budget across nested calls, then saves it with
step completion. A new sequence never grants another allowance. During planned
shutdown finish the active step and save its checkpoint; start no further NPC
steps, even if accepted foreground work creates more response opportunities.

On startup, running candidates, executing rounds or an unfinished source turn
fail their causal discussion and its remaining rounds through shared crash
cleanup. Preserve effects and original errors; never replay an active step.
Waiting/ready candidates and selected but unclaimed work may resume only when
their discussion has no interrupted step and its source sequences succeeded.
Thus a crash never refunds spent work into a resumed active discussion. No
global per-inference SQL budget or saved agent call stack is needed.
Settle a discussion when its root has finished and no active rounds remain.
Superseding a collecting round and inserting its replacement happen in one
transaction; carry source references and derive the new eligible hearer roster
from committed deliveries, rather than retaining stale candidate identities.
An intervention round can execute selected work after its original sequence
finishes just as a speech round can; it never becomes eligible mid-GM call.

## Pause protocol

World owns live pause holds keyed by (group_id, character_id), plus a watched
activity enum: Idle, Foreground(character_id), NpcDecision(round_id), or
NpcAction(round_id). Use a short synchronous mutex to change this state and
claim work; release it before any await. A guard restores activity on every
success/error exit. The durable round remains the recovery record, not a second
live queue. Controls carry no prompts or model work.

- Interrupt authenticates the character/group and rejects the request while
  that character's own foreground chat-turn is active. Under the live mutex it
  inserts the hold. During an NPC decision it acknowledges immediately: the
  inference may finish, but execution is gated. If an NPC action is already
  active, it waits on the watched activity until that complete action unwinds
  before acknowledging. Show "Pausing" while waiting; never claim it is paused
  while an action can still publish. Waiting does not block the worker or hold
  a mutex/connection. Recheck access before returning the acknowledgement.
- Claiming another NPC action checks holds under the same mutex that records
  active work, eliminating the check/start race. If held, leave work selected
  and unclaimed. Otherwise set NpcAction with its activity guard, release the
  mutex, then claim the SQL execution. A pause arriving during that SQL claim
  waits for the activity to unwind; a failed claim drops the guard. Never hold
  the live mutex across a database await. NPC-origin topology changes
  also reject changes affecting a paused group; a different group's work cannot
  bypass the hold by merging it. Foreground human actions use ordinary admission.
- Never mind clears only the caller character's hold and wakes the worker.
  Any of that character's tabs can do this; another player's hold remains.
  A duplicate clear is harmless. Release a character's hold on final disconnect,
  access loss or leaving that group; retired-group holds cannot survive.
- Send reserves a foreground channel slot without holding the live mutex, then
  enqueues the message and clears its own hold together under that mutex. The
  worker sees the queued human input before admitting more NPC work. Do not
  clear first and leave a race where an NPC speaks before the input is queued.
- Controls return authenticated can_interrupt/own_hold/group_paused state to all
  affected tabs via the existing wake-up/read path. The acknowledgement includes
  the committed chat cursor; catch up earlier committed speech before showing
  the paused state. This is a scheduling pause, not an action-allowance reset.

## Intervention lifecycle

InterventionCheck records a round bound to the original request execution and
the check execution. Resolve only the GM-selected NPCs, using child invocations
on the existing GM stack; their terminal decision tools cannot invoke the GM.
This short collection uses the current Budget and returns the chosen proposal
references/content to the GM. It does not execute or publish them. Humans without
an interrupt supply no unsolicited intervention; explicit consent remains the
separate later protocol.

The original action must then settle through Approve/Reject. Follow-up work is
ineligible until that foreground stack has unwound. A rejected tentative Take
with an unchanged scene can execute the chosen response afterward. If approval
or another event changed the scene, supersede the old proposals and ask the
selected NPC cohort to reconsider the actual outcome under the same budget;
never say stale words as though the item were still available. All-DoNothing
creates no follow-up. Do not automatically reject on an intervention, keep the
original Take pending for dialogue, or invent rollback of a completed Take.
When reconsideration is needed, supersede the intervention round and create a
replacement bound to the original check, original action outcome and GM-selected
roster. Do not broaden that roster to every NPC in the group. A selected speech
cohort uses batch resolution; a selected non-speech action uses ordinary
arbitration. Repeated checks for one pending action supersede earlier unexecuted
intervention selections; they do not queue duplicate reactions.

## Commit-sized implementation order

| Slice | Production change and finish line |
| --- | --- |
| Shared speech | First migration, Say/Whisper audience admission, canonical speech commit and the one reader used by live/reload/context/inspection. Exercise two PCs, privacy and speaker rename; existing non-speech actions keep their original path, now submitted through CharacterActor. No unused NPC constructors. **Implemented.** Two choices differ from the first draft: the GM learns of speech through its approve_action result (a message between its call and result would break strict chat templates) rather than a history reference, and the speaker's own copy is written after the speaker's tool result for the same reason (deliveries run once the whole batch has its results). GM narration of speech goes only to the speaker and the PCs who heard it, so it cannot describe a private word to anyone else. |
| NPC speech rounds | Second migration, terminal Say/Whisper decisions, selection, GM batch resolution, finite worker steps, causal inspection links, Budget checkpoints and shared shutdown/crash-cleanup integration. These form one coherent slice: do not commit unused round tables or a dispatcher unable to execute its declared actions. Verify multiple NPCs, tied speech, natural stopping, stale rounds, budget exhaustion, clean restart and visible mid-action crash failure before marking the slice complete. |
| Player control | Add watched activity/holds, Interrupt/Never mind, ordered Send release and multi-tab control state. Test real inference-in-progress and action-in-progress barriers, other groups, disconnect and access revocation. No future combat state is fabricated. |
| Tentative-action intervention | Add InterventionCheck, shared NPC Take execution, non-speech tie selection and original-action/follow-up handoff. Verify rejection-before-discussion, approval invalidating old intent, wrong-world/source rejection and completed-action retention after failure. |

Place NPC persistence/domain rows in src/store/npc.rs and worker/model-decision
code in src/game/npc.rs (a child module can use Game's private operations).
Keep shared actor/action/speech entry points with existing Store/Game methods;
do not make a file-movement prerequisite or introduce a manager/registry. Use
temporary commits for incomplete internal checkpoints if needed; final commits
must each pass the repository's complete-slice gate.

## Combat boundary

Interrupt changes scheduling, never character eligibility or allowance. Route
all resulting requests through the existing shared action admission boundary;
do not grant a turn because a pause is held or because an action flag is unused.
Combat order/flags, Move, Wait and damage are not implemented in the current
runtime. Their complete implementation remains in [Rules and travel](playable-rules.md),
with the accepted rule that closing a game-turn consumes remaining allowance and
only a new game-turn resets it. This experiment does not expose dummy combat,
attack, movement or travel tools, or claim combat-abuse tests have passed.

## Verification and completion

Use existing browser and configured-model game paths; do not make MCP a gate.
Before writing model-behavior tests, review these cases and validation approaches
with the user as required by the coding standards:

Slice 2 validation methods:

- A: scripted model output through production Store/Game paths checks selection,
  delivery, authorization and state changes deterministically.
- B: real-model conversations and captured production prompts check NPC-owned
  dialogue, knowledge boundaries and natural stopping. No prescribed wording.
- C: stored decisions, recipient histories and Developer Mode causal links prove
  which original events/inferences/actions produced the result and detect replay.

| Case | Expected outcome | Verification |
| --- | --- | --- |
| U1: Say addresses one NPC | All actual NPC hearers get an opportunity; an addressed NPC may choose DoNothing. A selected NPC speaks through its own agent and GM approval. | A/B; C links response to its speaker and source. |
| U2: Different eagerness | Highest wins; losing proposals are superseded and their actors reconsider newly heard speech. | A contrasts 1/2/3; C checks only the winner acted. |
| U3: Tied speech | Highest ties composed from the same history reach one GM approval invocation; approved words arrive once in any order before another response round. Prefix is optional and respects each utterance's audience. | A/C check shared input cut, approvals and deliveries; B checks intelligible dialogue. |
| U4: All DoNothing | No GM approval or follow-up inference after collection completes. | A counts model work; C shows a settled round without actions. |
| U5: NPC conversation | Fresh follow-up turns share one budget; NPCs own dialogue and the GM owns approval/consequences. Evaluate natural stopping over repeated conversations. | B inspects actual prompts and play; A/C verify budget sharing and causal order. |
| U6: Private speech | Only actual hearers other than a self-only speaker get opportunities. Excluded histories, prompts, narration and prefixes reveal no private words or unseen participants. | A includes connected bystanders and revealing GM narration; B/C inspect captured inputs and delivered histories. |
| E1: Human input mid-round | Input runs between finite steps and supersedes stale unexecuted work; committed words stay. | A controls inference boundaries; C confirms no stale action. |
| E2: Split, merge, departure or access loss | Revalidate selection, actor authority and recipients before execution; no actor acts in a retired/wrong group. | A exercises actual mutations; C checks resulting action ownership and deliveries. |
| E3: No NPC hearer or self-only speech | No opportunity or inference is created. In a tied set, hearing another speaker still allows a later opportunity. | A/C inspect work and heard source references. |
| E4: Malformed decision | Free text, multiple calls, noninteger eagerness, 0, 4 or 400 fail visibly and leave valid answered tool history, without a correction loop. 1 and 3 are valid boundary values. | A checks errors and subsequent chat assembly; C checks original output and closed executions. |
| E5: Budget exhausted | Discussion fails visibly without a new allowance or silently dropped reply. | A exhausts the shared limit across NPCs; C checks persisted usage and failure. |
| E6: GM rejects one or all tied proposals | Only approved speech is published; no losing proposal becomes a fallback. Zero approvals ends work unless another actual event intervenes. | A tests partial/all rejection; C checks action outcomes and recipient histories. |
| E7: Bad/repeated batch resolution | Missing, duplicate, foreign or unselected IDs and invalid hearing adjustments are refused before publication. A repeated successful resolution returns its recorded outcome without another delivery. | A exercises the real approval transaction; C checks unchanged words, recipients and row counts. |
| E8: Clean shutdown or crash | Clean shutdown finishes/saves the active step; waiting work resumes once. A crash fails its active discussion, retains effects and closes unanswered history without replay. | A plus actual process stop/restart; C compares original effects, IDs and budget. |
| E9: Another group | It is not paused, superseded or given private speech by this discussion. Its accepted input uses the ordinary serialized world worker between finite steps, not concurrent world mutation. | A submits input in both groups; C checks isolation and ordering. |

Later slices retain their own verification: Interrupt/Never mind and multiple
holds (slice 3), then tentative Take/intervention and random non-speech ties
(slice 4). These are not extra capabilities to implement in slice 2.

Check deterministic selection, authorization and transaction outcomes through the
real Store/Game paths. Inspect captured model input and Developer Mode causal links,
and play repeated real-model Bread Thief conversations for natural stopping,
NPC independence, latency and player intervention. Do not validate wording by
transcript-specific string matching. Record any proposed routing/denial policy
change as a decision for the user rather than quietly changing the declarations.

Update implementation_reference.md only for implemented behavior. Run appropriate
Rust, offline SQL/schema, lint and browser checks, perform self-review, and commit
each complete slice. Mark complete and archive only after the entire scoped
feature passes these checks; combat's separate plan keeps its own completion gate.
