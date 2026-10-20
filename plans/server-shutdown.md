# Graceful server shutdown and crash cleanup

Routine-shutdown lifecycle and tool-call history closure implemented (see
implementation_reference.md). Still open: players' persistent view of an
interrupted turn, GM-approval execution errors returned to the caller, and
real-model verification. Shared prerequisite before NPC interaction.

## Outcome and authority

Routine shutdown finishes accepted turns before exiting. Crashes and forced
termination are exceptional failed turns: keep committed effects and history,
report the interruption and leave subsequent chat usable. No rollback, automatic
replay or reconstruction of an interrupted agent call stack.

Authority: persistent world/chat and visible error requirements in
user_declarations.md, [the shutdown decision](../user_exerpts.md#2026-10-20-shutdown-and-crashes),
and [shared execution design](../design.md#server-shutdown-and-crashes).
Follow the [AGENTS.md checklist](../AGENTS.md#post-feature-and-pre-commit-checklist),
[coding standards](../coding_standards.md) and
[prompt standards](../prompt_standards.md).

web::serve drains through Game::shutdown; Game::drop only aborts
world workers as a last resort. Startup Store::interrupt_open_inferences marks unfinished records
interrupted, but those labels alone do not ensure valid subsequent model history.
Extend these existing paths rather than add another executor or workflow system.

## Alternatives and selection

| Option | Benefit | Cost/risk and decision |
| --- | --- | --- |
| Stop immediately | Smallest lifecycle code | Breaks active turns during routine restarts. Reject. |
| Finish accepted turns; clean up exceptional crashes | Keeps ordinary restarts safe without serializing active execution | Shutdown waits for work; a crash can leave partial game effects. Select. |
| Continue any interrupted operation after restart | Strongest crash continuity | Requires durable nested-call progress, tool ordinals, delivery stages and budget admission throughout execution. Outside this scope. |

## Shutdown protocol

- Use an explicit running/draining lifecycle owned by Game, with one admission
  boundary shared by browser input and future MCP callers. The shutdown signal
  closes admission atomically: racing requests are either accepted and drained
  or clearly rejected as stopping. Include accepted queued input, not just the
  currently executing request. Do not introduce a durable input queue solely
  for graceful shutdown.
- Stop starting independent automatic work, including new presence narration
  and future NPC steps. Finish the entire active chat-turn: nested calls, sibling
  tools, deferred topology application, result persistence and chat delivery.
  Already admitted foreground input may still perform the nested work it needs.
  Draining must not reject those child operations as new external requests.
- A pausable point has no active model/tool stack or unfinished delivery for
  that turn. Waiting between durable NPC steps qualifies once step progress and
  the discussion budget are saved. Do not wait for the entire NPC conversation
  to end, or for a human answer to a future durable consent request.
- Notify browsers that the server is stopping and close long-lived streams so
  HTTP graceful shutdown cannot wait forever on idle websockets. A disconnected
  browser must not cancel its already accepted turn. Keep Game/backend alive
  while world work drains; then finish admitted inference/deferred jobs, join
  workers and finish HTTP shutdown before releasing resources.
- Do not drain an endlessly replenished queue: reject new external input and
  suppress new independent work after the admission boundary closes. Existing
  durable jobs not yet admitted remain pending for startup. Show outstanding
  turns/jobs while waiting. No automatic timeout that aborts a turn; an explicit
  force-stop is treated as a crash. Use notifications and task joins, not polling.
- Handle Ctrl+C through Tokio's platform signal support and Axum's existing
  graceful shutdown API. Wire other supported shutdown entry points into the
  same path. Game::drop may remain a last-resort abort for abnormal teardown;
  normal shutdown explicitly awaits completion first.

## Crash cleanup

Before admitting play after startup, use existing recording identities to close
interrupted turns. Keep original errors, output, action results and delivered
messages. Never regenerate a finished inference, repeat a roll/mutation or
automatically run remaining sibling tools.

Audit the actual effect/result boundaries of production tools. For a missing
tool reply, use a committed result only when its original source execution
proves that result. Otherwise append a truthful interruption result, explaining
that completion was not confirmed and effects may already have committed.
Do not claim an interrupted tool had no effect merely because its receipt is
missing. Unreached sibling calls receive matching not-executed results.

Handle completed assistant tool-call output whose execution rows were not yet
created, unfinished nested calls and pending action/topology records. Every
retained tool call must have a matching result before another model inference
uses that history. Preserve immutable captured inference inputs and existing
messages; append missing results rather than rewrite old transcripts. Perform
cleanup transactionally and idempotently so another restart adds no duplicates.

Expose the failed turn and original interruption details to its authorized
players and Developer Mode. Missing public narration is not silently generated
or delivered as though the turn completed. The next human input starts a new
turn using committed world state and valid history. This is failure handling,
not automatic continuation of the old turn.

## Failure results and player-visible system errors

Authority: [failure results decision](../user_exerpts.md#2026-10-20-failure-results-in-history)
and [design.md](../design.md#server-shutdown-and-crashes), which state the rules.

Player-visible system errors need a stored home. Alternatives: (1) derive from
existing records, since every sequence has a trigger item
(`message.trigger_sequence_id`) and `sequence.error` holds the system error; (2) a
new error table. Selected (1): no schema, one rule covers player turns, presence
narration, budget exhaustion, setup failures and crashes, and the error stays
tied to the instruction that started the work. Player chat shows the error at the
trigger message, in the player's own branch or a current group's GM branch;
history entry ids share one id space, so the trigger id orders the error and is
the reload cursor. Revisit (2) only if a sequence needs several structured errors.

Done: closing every unanswered call in history, on failure and at startup.
Pending: the GM approval report and caller error; a refused budget as a failed
inference entry; the player-chat error entry.

## Implementation and verification

Implement one complete lifecycle/failure-handling slice before NPC interaction;
do not add generic continuation tables or global durable inference charging.
Use current Game/World task ownership and existing recording/history helpers.
Add only state and schema proven necessary by the shutdown and cleanup paths.

Before writing model-behavior tests or changing model-facing failure content,
present use cases, edge cases and two or three validation approaches for user
review under coding_standards.md. Required evidence includes:

- Shutdown during inference, nested GM tools, result delivery and accepted queued
  input: every accepted turn finishes once; later requests are clearly rejected.
- Idle websockets, disconnected requesters, multiple worlds and pending deferred
  jobs: shutdown completes once admitted work finishes, without starting new work.
- Crash after output recording, after an action commit and between sibling tools:
  original effects/history remain, no mutation is replayed, failed-turn details
  are visible and subsequent real-model chat accepts the completed tool history.
- Startup cleanup interrupted twice: no duplicate results/messages or orphaned
  pending actions. Explicit forced termination follows the same crash path.

Run real process shutdown/restart checks as well as focused Store/Game tests.
Update implementation_reference.md only for implemented behavior, run relevant
Rust/SQL/lint/browser checks, self-review and commit. Remove this prerequisite
from todo only after routine shutdown and post-crash chat both pass. NPC steps
then integrate with these shared admission and shutdown boundaries.
