# NPC interaction: remaining work and acceptance

Partially implemented. Speech delivery, NPC response rounds, individual GM
approvals, expressive speech, causal inspection and Interrupt/Never mind exist.
Remaining: restraint and natural stopping (user-owned tuning), responses to other
perceived events, and tentative/committed intervention. What happens to existing
raised hands when another player acts is an open policy choice.

## Goal and authority

Characters hear speech through their own histories and respond through the shared
character-action path. Responses to completed events use fresh turns; bounded
intervention can affect an original action before the GM resolves it. Humans can
raise a hand while typing without cancelling running work.

Authority: [NPC Interaction and NPC Inference](../user_declarations.md#npc-interaction),
[Character actions](../user_declarations.md#character-actions),
[GM tools](../user_declarations.md#gm-tools),
[World game page](../user_declarations.md#world-game-page),
[Developer view](../user_declarations.md#debugging-and-telemetry), and
[turn/allowance clarifications](../user_exerpts.md#2026-10-20-npc-speech-interruption-and-game-turn-allowance).
Follow [NPC speech and response rounds](../design.md#npc-speech-and-response-rounds).

Each remaining increment follows the
[AGENTS.md checklist](../AGENTS.md#post-feature-and-pre-commit-checklist),
[coding standards](../coding_standards.md) and
[prompt standards](../prompt_standards.md). Review new model-behaviour cases and
validation approaches with the user before adding prompts or LLM tests.

## Implemented foundation

Use the existing operations rather than implementing these again. The
[implementation reference](../implementation_reference.md#web-play-designmd-web-server-and-ui-auth)
indexes speech, response rounds, shared character actions, causal inspection,
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

## Design choices

For intervention, scheduling a reaction after the original turn is too late to
affect it; persisting a suspended GM stack adds continuation state. Selected:
collect bounded child decisions in the current GM turn and apply approved
reactions through existing pending actions, then schedule later dialogue.
The source representation for non-tool perceived events needs a concrete design
before implementation; compare extending the existing cause reference with a
specific recorded event, preserving exact attribution without a workflow engine.
The Interrupt choice remains open as described above.

## Remaining implementation order

| Slice | Production change and finish line |
| --- | --- |
| Perceived-event opportunities | Extend source-linked response opportunities to actual delivered world events, using existing history/delivery paths and authorized perception. Verify one event creates an opportunity, bookkeeping creates none, and private events stay private. Combat events follow their later rules implementation. |
| Tentative/committed intervention | Add committed intent and InterventionCheck. Resolve bounded reaction results before the original decision, then schedule dialogue. Verify warning versus committed retry, source/actor authority, no GM re-entry, shared budget and retained effects on failure. Physical reactions remain with combat rules. |

The following sections specify these pending increments. Reuse existing
Store/Game/worker operations and checked SQL with forward migrations; no generic
workflow engine, parallel executor or file-movement prerequisite is needed.
Keep todo.md's overall order; this cleanup does not defer unfinished scope.

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
speech increment. Verify the prisoner's warning and committed retry in this increment, and
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
