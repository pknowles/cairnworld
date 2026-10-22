# NPC speech, response selection and player interruption

Speech delivery, NPC response rounds, individual GM approval of tied replies,
expressive authored speech, Developer Mode causal links, NPC response wording and
the split, merge, access-loss, shutdown and crash scenarios exist. Still open
before player control is NPC restraint (the declaration asks NPCs to be
encouraged to use do_nothing), which is last because it takes the longest to
tune and test. Evidence so far, from a real-model run: wording that says silence
leaves the scene waiting and that speech may be answered made addressed NPCs
respond, but in the run read in full the answers were whispers: an unaddressed
Mara whispered "don't answer him" to Toma, Toma whispered his private reasoning
back to Mara, then repeated it verbatim, and the player heard nothing. Cause
untested; candidates are whisper being the only non-public output and Toma
obeying Mara. Builds on implemented shutdown and failure handling; combat and
InterventionCheck are not implemented.

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
an inference scheduler, serialized world events and shared chat delivery, plus
initial speech-triggered NPC response scheduling.
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

### Existing speech foundation

Shared speech is implemented; see [Web play](../implementation_reference.md#web-play-designmd-web-server-and-ui-auth).
Extend its character action submission, approval, recipient delivery and history
reader for NPCs. Preserve privacy, source identity and offline-PC hearing. Incoming
speech updates PC history without invoking player agents; humans initiate their
replies. NPC decisions use their own facts and actual heard history, excluding
GM/Storyteller-only notes.

### Response rounds and safe player control

- NPCs decide from their own facts and heard history, offering DoNothing or an
  action with eagerness 1–3. The highest wins; speech-only highest ties may all
  speak, having heard none of one another's replies when composing them. Other ties choose one
  recorded random winner. Proposals do not perform actions.
- Approved tied replies finish before the next response round. Losing proposals
  are superseded; new input requires reconsideration, not stale reply playback.
  All-DoNothing ends the discussion. Each selected speech gets ordinary individual
  GM approval; the GM may describe overlap through its per-utterance prefix.
- Human input runs between finite NPC steps. Follow-ups use fresh call stacks,
  one causal budget and links to original events in Developer Mode. World state,
  privacy and action authority remain enforced by the existing production paths.
- Interrupt pauses future NPC actions while the human types; Never mind releases
  that character's hold without inference or changing game-turn allowance.
  Completed effects stand. The pause protocol below defines admission, multiple
  holds, disconnect and topology behavior.
- Clean shutdown saves complete-step progress; a crash fails its active discussion
  visibly without replay. The worker section defines transaction/restart boundaries.

### Tentative actions and GM-selected intervention

The GM sees selected NPC intent before deciding a tentative action; follow-up
dialogue waits for that decision's call stack to unwind. Intervention does not
automatically reject the action. The lifecycle section below specifies this
separate later slice.

## Design choices

The selected options minimize new mechanisms while meeting restart, simultaneous
speech and interruption requirements. These are implementation decisions; routing
policy and automatic denial remain the user's playtesting decisions.

| Question | Options, benefits and risks | Selection and reason |
| --- | --- | --- |
| NPC scheduling | Nested calls need little queue code but re-enter the active GM. Detached tasks allow overlap but race world state and complicate shared budgets. Concrete SQLite rounds in the existing worker require checkpoints but retain one executor. | Concrete rounds; reuse world serialization rather than introduce another scheduler. |
| Speech storage | Copy text to every recipient: easy but duplicates history. Point into arbitrary JSON tool output: avoids a text row but couples readers to proposal layout. Share one committed action-result body and retain speech metadata: small explicit schema and existing content reconstruction. | Shared action-result body; event words are stored once for delivery. The original model arguments remain original inference evidence, not another recipient transcript. |
| Stale decisions | Compare every live field/hash: expensive and easy to omit. Use the GM branch head: existing value, but inference bookkeeping would invalidate its own round. Use a group scene revision: one explicit semantic counter, requiring an audited write boundary. | Group scene revision, changed only by committed fiction/state changes. |
| NPC decision protocol | Reuse the full agent loop: familiar, but permits extra tool batches and adds settling calls. Free text plus parsing: fewer schemas, unreliable control. One constrained terminal decision: one call per NPC, explicit failure on malformed output. | One recorded inference returning exactly one do_nothing, say or whisper call; no second tool executor or text parser. |
| Tied speech approval | A batch tool couples outcomes and mixed private audiences. One shared GM turn with individual tools saves inference but still needs separate delivery and pause handling. One ordinary GM turn per selected utterance reuses single-action arbitration and isolates delivery. | Individual GM turns in any order. Extra GM calls on ties are accepted for simpler ownership, privacy and pause boundaries. No batch tool or atomic multi-utterance write. |
| Pause handling | Queue the control behind NPC work: cannot interrupt promptly. Cancel arbitrary futures: risks half-completed tools. Direct live control with boundary acknowledgement: permits typing while retaining committed work, but acknowledgement can wait for an active action. | Direct pause holds and a watched worker activity value; never cancel a running action. |
| Budgets and restart | Memory-only counters grant a fresh budget after restart. Arbitrary crash continuation requires durable nested-call progress and inference admission. Saving counters between complete steps permits clean restarts with smaller changes, while crashes fail active discussions. | Finish steps on planned shutdown and checkpoint the existing Budget between them; use shared crash cleanup for exceptional interruptions. |
| Shared actions | Fake PlayerAgent for NPCs breaks ownership. A parallel NPC executor duplicates rules. A private resolved character actor plus existing action execution shares rules without giving browser callers NPC authority. | Resolved character actor; player authorization remains at the browser boundary. |
| Intervention lifetime | Schedule the reaction after the original turn: reuses rounds but cannot affect the unresolved action. Suspend and persist the GM stack: supports long waits but adds unnecessary continuation state. Collect child decisions and approve reactions in the active GM turn: reuses invocations and pending actions, with bounded work sharing its budget. | Child decisions in the current turn; only subsequent dialogue uses scheduled rounds. Preserve applied effects on failure, without replay. |
| Interrupt during the caller's turn | Reject: simple but contradicts the declaration. Cancel or suspend the turn: responsive but risks incomplete tools and agent re-entry. Record a hold while the current action finishes: permits immediate input control without cancelling accepted work. | Accept the hold at any time; acknowledge a safe pause after active action work finishes. New input still uses ordinary foreground admission. |
| Speech attribution | Put names into authored prose: no representation change but loses reliable attribution and constrains expression. Preformat metadata into ordinary text: fewer match arms but makes consumers parse presentation. Add a typed speech content variant: touches existing message consumers but keeps identity and authored prose together without conflating them. | Typed speech content, serialized as a compact JSON object for model input and rendered as authored prose in chat. Preserve legacy text messages and their exact reconstruction. |

## Data model and invariants

Speech metadata and initial response state exist in migrations 0004/0005.
Use forward migrations for the expressive-text format and any necessary state
changes; do not edit applied migrations. All new fixed queries use checked SQL and the existing offline
metadata workflow. Keep existing sequence/inference/tool tables as the record of
model work. Do not put prompts, proposed words, eagerness or arguments in a second
work transcript; decode them from the referenced immutable tool output.

| Record | Columns and constraints |
| --- | --- |
| npc_discussion | root_sequence_id primary key/FK sequence; spent_total nonnegative integer; state ready, settled or failed; failure_sequence_id nullable FK, present only for failure. Created when the root turn finishes (so no waiting state) with that turn's spent budget; limits come from the running `Limits`, not a stored snapshot. Stores the existing Budget's checkpoint between complete steps. |
| npc_round | id primary key; root_sequence_id FK discussion; group_id FK; scene_revision; state collecting, selected, executing, settled, superseded or failed. Scheduled rounds describe responses to completed events, not unresolved intervention. Adjust the speech-round indexes to allow one collecting follow-up alongside one selected/executing set per group. The follow-up cannot run until its source sequences succeed and that selected set finishes or is superseded. |
| npc_round_source | round_id and source_tool_execution_id FKs, composite primary key. Speech sources resolve committed Say/Whisper actions. Multiple tied utterances may trigger one round. |
| npc_candidate | round_id and character_id form primary key; decision_invocation_id nullable UNIQUE FK until started; selected NOT NULL boolean initially false; execution_sequence_id nullable FK until that selected action starts, CHECK that it requires selected. Each selected utterance has its own execution sequence. No candidate status column or copied decision-tool FK: derive progress/outcomes from its round, invocation, tool execution and sequence records. |
| sequence_cause | child_sequence_id and source_tool_execution_id FKs, composite primary key. All source executions belong to the same world and earlier sequences; validate this on insertion. Existing invocation parent links still describe nesting, while this table describes separate causally related chat-turns. Player-visible failures follow the cause chain back to the human turn's message; a step failure without its own sequence gets a finished failed "npc discussion failed" sequence caused by the round's sources. |
| group.scene_revision | NOT NULL integer starting at zero. Increment in the transaction applying a fiction/state change, never for inference, tool-record or proposal bookkeeping. |

Candidate progress has one source of truth:

- No decision invocation means the candidate has not started; an unfinished
  invocation means its decision was interrupted/running. A settled invocation
  with one valid terminal decision supplies the proposal or DoNothing through
  the existing inference/tool relations. No duplicate payload or status is saved.
- Selection sets selected on winners in the same transaction that advances the
  round. Others are DoNothing or losing proposals, derived from their decisions
  and the round phase; superseding a round invalidates its unexecuted proposals.
- Execution starts by recording execution_sequence_id; that sequence's completion
  and error determine the execution outcome. A failed decision comes from its
  invocation outcome. Round/discussion failure handling stays at those levels.

Use these relations in the worker queries, not a second persisted candidate
state machine. Runtime types may express the derived phase without independent
optional fields for every possible outcome.

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

Expressive speech is the character agent's complete authored text, including
phrasing and descriptions of its own delivery. Preserve it unchanged for player
live/reload reads, raw inspection and model context. Remove fixed "Name says",
"Name whispers" and "says to X" prose from speech_text/attributed and tool text
that demands bare literal words. Keep speaker/addressee IDs, frozen names,
whisper intent and actual hearers as metadata. Model context must still identify
the source character using structured context rather than rebuilding a fixed
speech sentence; do not parse an actor's name out of authored text. Extend the
existing MessageContent with a Speech variant carrying speaker ID/frozen name,
optional addressee ID/frozen name, kind, prefix and authored text, loaded together
by the shared speech reader. Keep these fields out of ordinary text messages.
The backend presents new speech as a user-role message containing the JSON
serialization of that object. Browser chat renders the authored text with the
GM prefix and exposes attribution as metadata, without manufacturing dialogue.
Inspection shows the same structured content and captured model input. Use the
same content representation across consumers; no new message framework or
name-extraction parser is needed. Update history compaction and token accounting
to consume this content too; speech must remain eligible for summarization, with
its attribution preserved, rather than disappearing from text-only matches.
The GM prefix describes hearing circumstances; it does not overwrite style or
invent the character's words. Preserve old recorded content, names and captured
inference inputs: add an explicit speech text-format discriminator in a forward
migration. Existing utterances retain legacy literal-word rendering for exact
reconstruction, including their existing Text message representation and backend
format; new utterances use typed authored speech without the fixed wrapper.
Keep this distinction in the shared reader, not heuristics that guess from
wording or duplicated formatting paths in each consumer.
Publish world chat/Developer Mode notifications only after commit.

Audit the existing writers that can change NPC context: actual character action
effects, delivered narration/presence, character creation/readiness/fact edits,
and split/merge. (Audited: applied take, speech, a player character joining a
group and split/merge change it. Narration and presence delivery write only
player-agent histories, and readiness edits only the sheet, which NPCs do not
see. Revisit if narration is ever delivered to NPCs.) Their transaction helpers advance affected scene revisions;
new groups initialize their revision and retired groups supersede their rounds.
Private player input, prompt artifacts and decision records do not advance it.
Do not introduce polling, field hashing or catch-all database triggers.
Read-only actions, rejected requests and approval bookkeeping alone do not
advance the revision. Each approved utterance advances affected scene revisions
in its own transaction. Its source identifies whether it belongs to the active
selected set; those commits do not invalidate the remaining tied replies.

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
operations. Submit the selected NPC call's real action name and arguments
through CharacterActor, without a second action schema or translation layer. Its original
say/whisper decision execution is the requesting execution; it can belong to the
earlier decision sequence. Do not fabricate an inference/tool call to imitate a
player. Inspection must resolve the real proposed action name/arguments when
the source execution is an NPC decision, instead of labelling it as an executed
world action.

Offer do_nothing and the real action tools (say, whisper), using the player's
schema/validation plus eagerness 1..3 through with_eagerness. Keep eagerness as
decision metadata; no nested action object or propose_action tool is introduced.
Enforce the range in code and schema. Describe the NPC's own desire to act, not
knowledge of the others' proposed eagerness.
Initially offer Say/Whisper; add Take only in the intervention slice. Exactly one
tool call is required. Free text, unknown tools, multiple calls or invalid
arguments finish the recorded invocation with a visible protocol error. Reuse
context::complete_recorded, existing inference completion and tool-execution
recording; do not add another backend/recording implementation or automatic
correction loop. For a speech-round step, complete a valid decision's result,
invocation and sequence with the Budget checkpoint in one transaction, after
normal inference-output recording. The candidate already references that
invocation from admission. Intervention collection finishes its child invocation;
the existing foreground sequence and budget checkpoint finish only after the
parent GM/player stack completes.
A crash between those writes fails the discussion; shared cleanup closes the
unfinished tool history without generating or executing a new proposal.
Completing the proposal means "intent recorded", not "action
performed". The normal capacity-compaction path and original model output remain
available for inspection.

Execute one selected utterance per finite worker step through ordinary character
action submission and a fresh single-action GM invocation. Use approve_action
and reject_action with the explicit pending action ID, existing hearing
adjustments and ordinary validation. Remove resolve_speech_batch and its
duplicate decision schema, action-list validation and union-audience narration.
GM approval remains mandatory; invalid hearing arguments are returned through
the normal rejection path. The approval and its delivery are independent of
later selected actions, and repeated approval returns the original outcome.

Keep chosen words frozen while the selected tied set is processed in any order.
Each GM request explains they were composed before the other selected replies;
the GM can describe overlap in that utterance's prefix without exposing private
speakers/words to non-hearers or claiming an unapproved reply already happened.
Scope narration and prefix delivery to this
utterance's actual hearers, including rejection handling: no broad-audience
fallback for a rejected whisper, and no combined narration of private replies.
Finish the current GM invocation and its deliveries before the worker boundary.
Only after all chosen replies settle may NPCs reconsider the resulting speech.
Human input, pause, access loss or topology changes can intervene between steps
and invalidate remaining stale work; earlier committed speech stands.

This removes special NPC approval batching, not the ordinary agent loop's
support for multiple tool calls in one model response. Preserve that loop's
rule that all tool results precede deferred history deliveries. Existing
speech_batch rows may remain as historical records; do not delete utterances or
add new grouping/overlap storage for tied NPC replies.

In this slice replace the GM's current NPC dialogue with NPC-owned responses.
The GM continues to approve speech and narrate consequences; it does not compose
NPC replies on their behalf. Its approval request states the facts and the tool
to call but no longer tells it to narrate afterward: with that sentence a real GM
skipped the tool in a run of speech actions or overflowed its context with long
narration (about 20 of 24 actions resolved), without it all 24 of 24 did. This is required prompt/context work, not a pending
policy question. Verify the actual assembled prompts and real-model dialogue.

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
| Speech commits | Record sources and NPC hearer roster; upsert the group's waiting round. It is ineligible until its source foreground sequence succeeds and any originating selected set finishes. Do not schedule the speaker for self-only speech. |
| New heard input while a round is collecting/selected | Supersede its unexecuted proposals and create a round at the current revision, carrying unconsumed heard source references. A new human root owns the new budget; NPC-only follow-ups keep the same discussion root. Never merge budgets by taking a larger allowance. |
| Collect one waiting candidate | Start a fresh causally linked sequence and attach its decision invocation to the candidate in a transaction. Run one inference outside transactions using the shared in-memory Budget, then atomically finish the decision result, invocation, sequence and budget checkpoint. Return to the worker boundary. Intervention collection instead uses child invocations inside its existing foreground sequence. |
| All decisions ready | Validate group/revision, persist selected winners and the round phase, including the random tie result. DoNothing and losers need no additional status writes. All-DoNothing settles without another job. Speech-only highest ties select the whole cohort; mixed/non-speech ties select one. |
| Execute selected work | Admit one selected action under the pause protocol, then attach a fresh execution sequence to that candidate and set the round executing, rechecking scene and ownership. Run ordinary GM arbitration and delivery. Save its sequence outcome/budget and return the round to selected if chosen actions remain, or settled otherwise, before yielding. |
| Revision/group changed before execution | Mark superseded, not failed. Start a fresh round only for still-relevant unconsumed heard input in that group; departed actors keep heard history but do not act in their former group. |
| Selected action rejected | Preserve its recorded rejection; do not try the losing candidates as fallbacks. A new actual event may create a new round. |
| Inference/action failure or budget exhaustion | Preserve original error and committed effects, fail that discussion and its remaining rounds, and notify its actual participant PCs through the existing authorized failure path. Unrelated groups remain usable. |

No transaction, SQLite connection or live-control lock spans model inference.
The revision refers to committed fiction, so the round's recording does not
invalidate it. Track revisions from the selected set's own utterances/deliveries
as its progress, not external invalidation. Revalidate each remaining action
against other intervening changes. Do not depend on revision equality to a value
captured before the first tied reply. Source execution IDs distinguish own
selected work from outside changes. Already completed effects stand when a later
approval is refused, rejected or fails; no multi-utterance rollback is needed.

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

On startup, candidates with interrupted decision invocations, executing rounds
or an unfinished source turn fail their causal discussion and its remaining
rounds through shared crash cleanup. Preserve effects and original errors;
never replay an active step. Unstarted/completed decisions and selected but
unclaimed work may resume only when their discussion has no interrupted step
and its source sequences succeeded. A clean stop between tied utterances leaves
the round selected, with finished execution sequences on earlier candidates and
no sequence on the remaining ones; restart executes only the latter. A crash
in one utterance fails the discussion and does not regenerate or replay peers.
Thus a crash never refunds spent work into a resumed active discussion. No
global per-inference SQL budget or saved agent call stack is needed.
Settle a discussion when its root has finished and no active rounds remain.
Superseding a collecting round and inserting its replacement happen in one
transaction; carry source references and derive the new eligible hearer roster
from committed deliveries, rather than retaining stale candidate identities.
Intervention intentions instead belong to child invocations in the active GM
turn, as specified below. They are not pending response rounds and never become
eligible worker jobs after the original action has resolved.

## Pause protocol

World owns live pause holds keyed by (group_id, character_id), plus a watched
activity enum: Idle, Foreground(group_id, character_id),
NpcDecision(group_id, round_id), or NpcAction(group_id, round_id).
Use a short synchronous mutex to change this state and
claim work; release it before any await. A guard restores activity on every
success/error exit. The durable round remains the recovery record, not a second
live queue. Controls carry no prompts or model work.

- Interrupt authenticates the character/group and accepts the hold even while
  that character's own foreground chat-turn is active. Under the live mutex it
  inserts the hold. During an NPC decision it acknowledges immediately: the
  inference may finish, but execution is gated. If an NPC action is already
  active, or a foreground turn is active in the held group, it waits on the
  watched activity until that complete action/turn unwinds
  before acknowledging. Activity in another group does not delay this group's
  acknowledgement. Show "Pausing" while waiting; never claim it is paused
  while an already-active action in this group can still publish.
  Waiting does not block the worker or hold
  a mutex/connection. The current turn continues normally; the hold gates future
  NPC actions, not tools required to finish that turn. Send cannot re-enter an
  active player agent and retains existing foreground admission. Recheck access
  and that the hold still exists before acknowledging a paused state; Never mind,
  Send or disconnect may have released it while waiting.
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
speech increment. Verify the prisoner's warning and committed retry now, and
verify the shopkeeper's physical opposition when those rules are implemented.

## Perceived world events

Beyond speech, create response opportunities from actual events delivered to an
NPC's history, preserving who perceived what and the actual recorded source. Reuse
the existing source-linked opportunities; do not add a general dirty-history
flag that retriggers on tool receipts, inference records or private GM notes.
For a narration with no source tool call, link its producing inference/event
rather than fabricate a tool execution to fit the current tool-only cause table.
Coalescing events keeps their causes, and DoNothing consumes an opportunity
without creating another. Add new event/action kinds with their implemented
rules, preserving ordinary no-reentry and authorization checks.

## Commit-sized implementation order

| Slice | Production change and finish line |
| --- | --- |
| Speech text and individual NPC approval | Preserve expressive authored text with source metadata and historical reconstruction compatibility. Replace batch approval with ordinary single-action GM turns; each utterance owns its recipients/prefix/narration. Preserve tied intentions, yield between approvals, and remove unused batch code. Verify disjoint whispers, rejection, partial failure, order and unchanged style. |
| Complete NPC response rounds | Finish Developer Mode links to original events and distinguish decision intent from applied action. Audit every fiction/context writer for scene revisions. Finish split/merge/departure/access loss, private-speech, clean-shutdown, other-group and repeated-real-model scenarios; investigate the unused-query warning and GM approval flakes rather than count them as clean gates. No generic continuation engine. |
| Player control | Add watched activity/holds, Interrupt/Never mind, ordered Send release and multi-tab control state. Test real inference-in-progress and action-in-progress barriers, other groups, disconnect and access revocation. No future combat state is fabricated. |
| Perceived-event opportunities | Extend source-linked response opportunities to delivered world events, using existing history/delivery paths and authorized perception. Verify one event causes one opportunity, bookkeeping causes none, and private events remain private. Combat events follow their later rules implementation. |
| Tentative/committed intervention | Add committed intent and InterventionCheck; resolve bounded reaction results before the original decision, then schedule dialogue. Verify warning versus committed retry, correct source/actor authority, no GM re-entry, budget sharing and retained effects on failure. Physical combat reactions remain with the combat rules. |

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
| U3: Tied speech | Highest ties composed from the same history get separate single-action GM turns in any order. Earlier tied speech does not regenerate/invalidate the remaining chosen words. Approved words arrive once before a new response round; prefix is optional. | A/C check original decisions, individual approvals and deliveries; B checks intelligible overlap/style. |
| U4: All DoNothing | No GM approval or follow-up inference after collection completes. | A counts model work; C shows a settled round without actions. |
| U5: NPC conversation | Fresh follow-up turns share one budget; NPCs own dialogue and the GM owns approval/consequences. Evaluate natural stopping over repeated conversations. | B inspects actual prompts and play; A/C verify budget sharing and causal order. |
| U6: Private speech | Only actual hearers other than a self-only speaker get opportunities. Two selected whispers to disjoint audiences have independent words, prefixes and narration. Excluded histories/prompts reveal no private words, style or unseen participants, including when a whisper is rejected. | A includes connected bystanders and revealing GM narration; B/C inspect captured inputs and delivered histories. |
| E1: Human input mid-round | Input runs between finite steps and supersedes stale unexecuted work; committed words stay. | A controls inference boundaries; C confirms no stale action. |
| E2: Split, merge, departure or access loss | Revalidate selection, actor authority and recipients before execution; no actor acts in a retired/wrong group. | A exercises actual mutations; C checks resulting action ownership and deliveries. |
| E3: No NPC hearer or self-only speech | No opportunity or inference is created. In a tied set, hearing another speaker still allows a later opportunity. | A/C inspect work and heard source references. |
| E4: Malformed decision | Free text, multiple calls, noninteger eagerness, 0 or 4 fail visibly and leave valid answered tool history, without a correction loop. 1 and 3 are valid boundary values. | A checks errors and subsequent chat assembly; C checks original output and closed executions. |
| E5: Budget exhausted | Discussion fails visibly without a new allowance or silently dropped reply. | A exhausts the shared limit across NPCs; C checks persisted usage and failure. |
| E6: Individual rejection/failure | Rejection of one tied utterance leaves earlier approvals intact and allows remaining selected actions, not losing alternatives. A system failure ends the discussion, retains prior effects and closes unanswered history. No approvals/no new event ends work. | A tests sequential outcomes and partial delivery; C checks effects and valid tool history. |
| E7: Bad/repeated action approval | Foreign, wrong-actor or unselected action IDs and invalid hearing adjustments are refused through existing approval validation. A repeated approval returns that action's recorded outcome without another delivery. | Extend existing action-path tests for NPC callers; C checks words/recipients, not a new batch invariant suite. |
| E8: Clean shutdown or crash | Clean shutdown finishes/saves the current utterance; restart continues remaining selected actions without replaying earlier ones. A crash fails its active discussion, retains speech and closes unanswered history without replay. | A plus process stop/restart between tied utterances and during approval; C compares effects, IDs and budget. |
| E9: Another group | It is not paused, superseded or given private speech by this discussion. Its accepted input uses the ordinary serialized world worker between finite steps, not concurrent world mutation. | A submits input in both groups; C checks isolation and ordering. |

Later increments retain their own verification: Interrupt/Never mind and multiple
holds, including Interrupt during the caller's active turn followed by release
before acknowledgement (no cancellation, re-entry or false Paused state);
perceived-event delivery without bookkeeping-triggered loops, then
tentative/committed Take and intervention. The prisoner's warning must not veto
a committed retry forever; approved reactions precede the original decision and
do not re-enter the active GM. Verify shopkeeper physical opposition with the
implemented combat rules. These do not expand the current speech completion.

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
