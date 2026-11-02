# Player replies: asking players in an InterventionCheck

Pending implementation. Taken forward from [Rules and travel](playable-rules.md) step 2
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
(`pending_reply`), a per-membership count of missed deadlines, a way for an agent turn to
wait on a human and later continue, and the question message in a player's chat.

## Steps

1. **Setting and counts.** Add the required `[game] player_response_timeout` (seconds, 300 in
   `default.toml`, no code default: missing is a startup error) to `GameSettings`. Store a
   per-membership count of consecutive missed deadlines, reset by any response.
2. **Saved reply.** Migration for `pending_reply(action_id, character_id, deadline, status,
   response_content_id)` and the `Intervention` continuation on the pending action: the original
   invocation and tool-call ids, the ordinals of the calls still to run in that response, and the
   budget spent. No serialized future; no earlier call re-runs.
3. **The deadline wake.** Reuse the presence deadline's earliest-deadline wake for replies:
   the world loop wakes at the earliest due reply, records `do_nothing` for it, and counts the
   miss. On restart overdue replies are answered first. Three misses in a row end the player's
   connection with the WorldDetail message and the usual departure handling.
4. **Waiting in the agent loop.** `intervention_check` registers a reply for each selected
   player and the GM's call returns `AwaitingReply`. The agent loop unwinds with the invocation
   left recorded and the world queue freed. When every selected character has answered or timed
   out, the continuation resumes once from the stored ordinals: the check's result reaches the GM,
   the remaining calls of that response run and the loop goes on.
5. **The question and the answer.** The question is delivered into the selected player's chat as
   an instruction message (what is about to happen, who is doing it). Only that character's next
   input is taken as the answer; their player agent offers `do_nothing` or the action tools with
   the pending action's id, and the answer becomes a reaction presented to the GM exactly as an NPC's
   proposal is. Other characters and other groups keep playing.
6. **The GM's tool.** `intervention_check` accepts any other character in the group. Remove the
   stopgap that reports a player as "cannot be asked yet".

## Verification

Scripted, through the real Game and Store: two real player chats (an intervening player, one
answering `do_nothing`); a timeout (a persisted wake, not a sleep); three timeouts in a row ending
the connection; a disconnect while waiting; a restart with an overdue reply; a mixed check of an
NPC and a player; the GM's result arriving only after every selected character is settled;
no earlier call re-run on resume; the shared budget spent once. Real model: the player agent
answers the question with a tool, and the GM decides every presented action before narrating
(repeat the intervention scenario 20 times).

## Order

The first two steps and the wake are independent of the agent loop and land first, each with its
scripted checks. The agent-loop change (step 4) is the risk: it needs `agent.rs` to resume an
invocation from stored state, so measure that the GM scenario still passes 20 of 20 before the
player path is switched on.
