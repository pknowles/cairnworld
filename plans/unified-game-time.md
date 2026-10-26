# Unified game time, activities and short actions

Pending design clarification, implementation and verification.

## Goal and authority

One timing/admission path for PCs and NPCs: interruptible long activities,
short-action resources with next-ready times, explicit EndTurn and individual
initiative. There is no combat-mode switch or maximum-character-time world clock.

Authority: [Game turns and time](../user_declarations.md#game-turns-and-time),
[Game state](../user_declarations.md#game-state),
[Character actions](../user_declarations.md#character-actions),
[GM tools](../user_declarations.md#gm-tools),
[Observed Events](../user_declarations.md#observed-events) and
[World game page](../user_declarations.md#world-game-page).
Implement [Unified game time and activities](../design.md#unified-game-time-and-activities).
Follow [AGENTS.md's checklist](../AGENTS.md#post-feature-and-pre-commit-checklist),
[coding standards](../coding_standards.md) and [prompt standards](../prompt_standards.md).

The preceding NPC interaction supplies real action approval, delivered messages,
responses and world execution. Extend those owners rather than building another
scheduler, event-message model or combat subsystem. This plan supplies timing
before GM editing's movement example; full damage/item rules and cross-location
travel remain in Rules and travel. Do not expose dummy attacks/travel to exercise
cooldowns. Use real supported speech, timed activities, Wait and EndTurn.

## Resolve the explicit design questions first

The linked design lists the unresolved "now"/next-idle calculation, initiative
roll and lifetime, generic attempt costs, dialogue-initiation boundary, Wait
default and same-time ordering. Work through concrete timelines with the user,
then update design before implementation; request user declaration corrections
where needed. This plan is not permission to choose hidden defaults.

The minimum examples are an attack-like resource use plus an unused Move ending
at time 0 and next becoming eligible at 10; a long activity interrupted before its
end; a compatible response leaving it unchanged; and an idle player blocking the
next decision. Use paper/scripted timelines to settle policy, not speculative
GPU prompt changes. Keep the ten-second short interval, EndTurn's `max(existing,
turn_time + 10)` update and immediate tool-call order as declared constraints.

| Implementation approach | Benefit | Cost and decision |
| --- | --- | --- |
| Separate combat rounds and exploration clock | Familiar to many RPGs | Requires mode transitions and duplicates admission; conflicts with the new declaration. Reject. |
| Character-owned activities/resources through existing world execution | Matches the declared state and reuses action/perception/lifecycle code | Requires an agreed next-opportunity calculation and completion handling. Selected. |
| General scheduled-event engine or replayable action workflow | Could represent arbitrary future operations | Adds a framework and replay state beyond the activity data needed here. Avoid. |

## Checkpoint: time state and short-action admission

- Inspect current world/character time columns and constructor defaults. Migrate
  necessary values into the agreed activity/resource representation and update
  all constructors, inspection and current-state context together. Preserve
  immutable captured inputs and historical records. An old schema field is not
  authority to keep a global game clock or implement combat flags alongside this.
- Store character resource ready-times and initiative using the agreed policy.
  Derive the next eligible opportunity from real character state through the
  existing world loop. Do not infer an intention before offering a decision or
  repeatedly wake a character solely because it left Move unused. Waiting and
  long-activity state must participate in the agreed calculation.
- Apply readiness checks at submission and application for player actions, NPC
  follow-ups and intervention actions. A perceived event or browser Interrupt
  offers consideration, not renewed resource allowance. Consume the resource
  with its approved action effect in the same transaction. Rejected actions
  retain existing error semantics; no automatic retry or guessed action.
- Add EndTurn to character tools. It advances every short resource to at least
  the turn timestamp plus ten seconds, preserving any later ready time. It
  ends the short opportunity without an extra forced move/action; it does not
  replace a long activity merely to close an LLM inference. Apply immediate
  actions in call order, not as a simultaneous outcome batch.
- Reuse the declared player-response deadline/AFK mechanism from
  [Player replies and coordinated travel](../design.md#player-replies-and-coordinated-travel)
  when an eligible human blocks progress. Implement the generic deadline and
  consecutive-timeout handling here; Rules and travel adds its Intervention/
  Travel call continuations later. Ordinary turn waiting must not require a fake
  character action or serialized Rust future. A skip uses EndTurn, does not reset
  cooldowns and does not turn five minutes of wall time into five game minutes.
- Publish actual eligibility/current activity through existing UI/game paths.
  Keep the ability to respond to perceived events distinct from permission to
  execute a resource-consuming action. No action queue allowing a player to
  pre-submit out-of-turn operations is introduced.

## Checkpoint: long activities, completion and timed effects

- Represent a current activity with its description, source action/character,
  start/elapsed-time information and end time. Approval starts it and records
  observable beginnings through ordinary GM delivery. Return a started result;
  do not keep a tool/GM invocation open for the activity's duration or pre-apply
  its eventual benefit. Reuse existing recording/content references.
- On reaching the activity end, invoke the GM with its explicit identity, elapsed
  time, current state and reason for resolution. On replacement, abandonment or
  forced interruption, resolve the prior activity with that reason instead.
  Full, partial or absent effects are GM decisions; code ensures an activity is
  resolved once and that an interrupted one cannot later award completion too.
  Keep both causal history and already-applied effects on failure, under existing
  crash rules. Do not build automatic step replay or a rollback system.
- Add explicit-duration Wait and GM ReplaceActivity using this same lifecycle.
  An activity can coexist with compatible responses; merely receiving an event
  or choosing DoNothing does not cancel it. The GM evaluates compatibility and
  can impose an activity such as unconsciousness when continuing is impossible.
  Starting/replacing an activity never resets short-resource ready-times.
- Add AddEffect/RemoveEffect with character/effect IDs, GM text and a duration/
  derived expiry. Use the same due-time processing, current-state refresh and
  recording. At expiry or early removal, exclude the effect from active context
  while retaining its history. Descriptions are GM guidance, not scripts or a
  new programmable rules language. Expiry does not by itself require a new NPC
  response message when nothing observable happened.
- Resume work already waiting between complete steps using existing shutdown
  handling. A crash during inference fails visibly through current cleanup;
  completion must not automatically replay uncertain GM effects. Game-time due
  work uses the world loop; wall-clock human/presence deadlines retain their
  existing earliest-deadline wake-up. Do not add per-character polling loops.

## Dependent features and explicit exclusions

- [GM editing](gm-editing.md) adds actual within-location Move/CombatMove using
  this timing and canonical placement state; it does not add another clock.
- [Rules and travel](playable-rules.md) adds actual attacks/damage and timed
  Travel/Follow through the same resources/activity resolution and group history.
  Cross-group synchronization and mid-travel placement need explicit policy;
  they are not solved by automatically completing activities when groups merge.
- [Storyteller initialization](storyteller-worlds.md) uses the resulting character
  constructors and fields, with all gameplay inactive before BeginWorld.
- Retain natural conversation stopping as user-owned tuning; a dialogue-initiation
  cost does not prove replies terminate. Group drift by hours, time jumps on
  merge, combat-length Delay and out-of-turn action queues remain exploratory.

## Verification and completion

Review model-facing cases and validation with the user before adding LLM tests.
For each case, use production-path state/action assertions for exact times/effects
and captured inputs/history/UI for what characters actually knew and could do.
These detect different failures; do not reproduce scheduling logic in a test
world or count an inference as proof that the intended outcome happened.

| Case | Required outcome; plausible failure to catch |
| --- | --- |
| Short resources and EndTurn | Use one real short resource, skip another, close the turn and attempt to act again. Unused resources cannot produce another opportunity at the same time; later ready-times never decrease. Replies/Interrupt cannot bypass admission. |
| Activity start and completion | A timed activity starts observably, with no completion benefit yet. GM resolves it when due. No tool/GM remains active for its duration, and neither duplicate work nor restart grants a second completion. |
| Optional and forced interruption | Hearing a line and choosing DoNothing/compatible response preserves the activity. An incompatible action or imposed unconsciousness resolves interruption without a later full completion. Actual partial progress remains. |
| Explicit waiting and idle players | Wait has the agreed duration; EndTurn advances a short opportunity. An inactive human receives the configured deadline and skips without blocking indefinitely; replies reset the AFK count. Wall-clock delay does not alter game duration. |
| Timed effects and current context | A real effect reaches GM state immediately, remains before expiry and leaves active context at expiry/explicit removal, with its recorded evidence intact. Same-time effects/completion follow the agreed order. |
| Ordering and scope | Initiative, readiness, group changes and activity ends follow the agreed timeline, including an NPC choosing a response before its previous activity ends. Unrelated groups do not receive private content or invented actions. |

Use scripted model outputs for scheduling/state boundaries and one representative
real chat for activity/response interpretation; do not run speculative repeated
GPU scenarios while policy is unresolved. Follow the five-minute testing budget.
Update implementation_reference.md only after implementation, run the relevant
Rust/SQL/browser checks, re-read both standards and complete AGENTS.md self-review.
Do not mark complete until both checkpoints and their verification pass.
