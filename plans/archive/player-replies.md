# Player replies: asking players in an InterventionCheck

Completed and verified 2026-11-04 (mechanism revised: no continuation, see Mechanism). Taken forward from [Rules and travel](playable-rules.md) step 2
and the generic deadline and consecutive-timeout handling in
[Unified game time](unified-game-time.md), because the GM's `intervention_check` cannot
ask player characters today, the GM names them anyway (about 45% of takes), and the
declarations ask them.

## Goal and authority

A GM's InterventionCheck asks the selected player characters as well as NPCs. Each
selected player is asked in their chat what is about to happen and who is doing it; their
player agent answers `do_nothing` or the action they intervene with, or, if they do not
answer in time, `do_nothing` is applied for them. Authority:
[GM tools](../user_declarations.md#gm-tools) (InterventionCheck, "asking players if they
wish to intervene"), "Players are expected to respond in a timely manner" (5 minute
configurable timeout, DoNothing as the idle response, 3 in a row means AFK) and
[Player replies and coordinated travel](../design.md#player-replies-and-coordinated-travel),
which already specifies the protocol. Follow the
[AGENTS.md checklist](../AGENTS.md#post-feature-and-pre-commit-checklist), the
[coding standards](../coding_standards.md) and the
[prompt standards](../prompt_standards.md).

Travel's Follow/stay/object protocol uses the same machinery later and is not part of
this plan.

## What exists and what does not

Exists: the persisted earliest-deadline wake used by connection presence
(`next_presence_deadline`, `PresenceDeadline`), NPC decisions inside a GM call
(`ask_npc`, `create_reaction_action`), `PresentedActions` (the GM must decide every
presented action before narrating), restart cleanup of interrupted turns.

Does not exist: the `[game] player_response_timeout` setting, any saved reply
(`pending_reply`), a per-membership count of missed deadlines, a GM turn that ends
waiting on a human and starts again on their answer, and the question message in a
player's chat.

## Mechanism

No continuation, ordinals or unwinding (design.md, Player replies and coordinated travel).
`intervention_check` asks every selected character at once: it stores a `pending_reply` and
appends the question to the chat of each selected player, then runs the selected NPCs'
decisions (recorded in any order), then ends the GM's turn with a result naming whom it waits
for. Calls after it in that response are not run and say so. Each reply settles by the
player's next input (their player agent calls `do_nothing` or an action tool with the pending
action's id) or by its deadline (`do_nothing`, counted as a miss). The settle that leaves no
open reply for the question appends one message carrying all the answers to the GM's branch
and runs the GM again on the same pending action, with the NPC and player reactions presented
as now. The actor's own `attempt` result had already been recorded as "waiting", so the
deferred outcome reaches the actor through the same delivery as the immediate one.

## Steps

1. **Setting and counts.** Add the required `[game] player_response_timeout` (seconds, 300 in
   `default.toml`, no code default: missing is a startup error) to `GameSettings`. Store a
   per-membership count of consecutive missed deadlines, reset by any response.
2. **Stored replies and the wake.** Migration for `pending_reply(action_id, character_id,
   deadline, status, response)`. Reuse the presence deadline's earliest-deadline wake:
   the world loop wakes at the earliest open reply deadline, records `do_nothing`, counts the
   miss. On restart overdue replies are answered first. Three misses in a row end the player's
   connection with the WorldDetail message and the usual departure handling.
3. **The check asks everyone at once.** `intervention_check` accepts any other character in the
   group. Players get a `pending_reply` and the question in their chat before the NPCs'
   decisions run. A waiting result ends the GM's turn (a tool outcome that completes and ends the
   turn; later calls in the response are closed as not run). Remove the "cannot be asked yet"
   stopgap.
4. **The answer.** While a player has an open reply, their next input is its answer: the player
   agent is offered `do_nothing` and the action tools bound to that action id; an action becomes
   a reaction presented to the GM exactly as an NPC's proposal is. Other characters and groups
   keep playing.
5. **The join.** When the last reply of a question settles (an answer, a deadline, or the
   question having only NPCs), one message with all answers goes into the GM branch and the GM
   runs again on the same pending action in a fresh budget. The result the actor would have
   seen (failure reason, speech outcome, narration) is delivered through the same code as the
   immediate path.

## Verification

Scripted, through the real Game and Store: two real player chats (an intervening player, one
answering `do_nothing`); a timeout (a persisted wake, not a sleep); three timeouts in a row ending
the connection; a disconnect while waiting; a restart with an overdue reply; a mixed check of an
NPC and a player; the GM's second turn starting only after every selected character is settled and
carrying all answers; calls after the check not run. Real model: the player agent answers the
question with a tool, and the GM decides every presented action before narrating (repeat the
intervention scenario 20 times).

## Order

Steps 1 and 2 are independent of the check and land first, each with its scripted checks. Steps 3
to 5 are one vertical slice: measure that the existing NPC-only intervention scenarios still pass
before the player path is switched on.
