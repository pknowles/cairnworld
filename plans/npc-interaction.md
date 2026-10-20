# NPC speech, response selection and player interruption

Implementation pending. This is the next feature, ahead of the coding-agent
interface; it is not a claim that combat or InterventionCheck already exists.

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

## Scope and order

Implement the following complete slices in order. Each commit follows the
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
  0–100. Proposal generation performs no world action and cannot call an active
  GM recursively. Keep GM/Storyteller-only information out of NPC context.
  Use one terminal decision per opportunity: DoNothing/propose record a decision,
  while only the selected proposal reaches the ordinary character action executor.
- Collect against one scene/history version. Highest eagerness wins; if all tied
  winners are speech, commit them as one simultaneous batch with an overlap marker
  appropriate to each hearer's actual audience. Otherwise record one random tied
  winner. Supersede losing proposals; never replay them after the winner changes
  the situation. Every affected NPC reconsiders the new heard event, excluding
  self-only speech. All-DoNothing finishes the round without another inference.
  Finish the simultaneous batch's speech permission/audience checks before any
  utterance commits; do not deliver one winner and label later replies simultaneous.
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
- Restart restores durable response progress and causal budget accounting without
  duplicating speech or completed actions. Discard disconnected pause ownership,
  expose interrupted inference failures through the existing failure path, and
  do not silently retry a failed model decision.

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

| Case | Required outcome / evidence |
| --- | --- |
| Two PCs and two NPCs hear Say | Exact attributed words once per hearer, live and after reload; NPCs speak through their own recorded agents. |
| Whisper and GM audience override | Excluded histories, prompt captures and browser transcripts contain no private speech or revealing marker. |
| DoNothing and competing proposals | No action during proposal collection; one highest selection, superseded losers reconsider after new speech, and all-DoNothing ends work. |
| Equal-eagerness speech and non-speech | Speech-only ties share one simultaneous batch; other ties execute one recorded random winner without asserting a particular identity. |
| Interrupt during inference, Never mind, two players/tabs | Acknowledged pause prevents future NPC actions; input stays usable; only the caller's hold clears; no replayed speech or restored allowance. |
| Player input or group change during a round | Revalidate before execution; no stale response acts in a retired/wrong group and no already-heard message is erased. |
| Tentative Take with intervention | GM sees the selected NPC intent before decision; rejection leaves the item; follow-up dialogue runs after the original stack unwinds. |
| Failure, budget exhaustion, disconnect and restart | Original errors remain visible; no retry loop, duplicate action, renewed budget or abandoned pause freezes play. |

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
