# GM turns start from a recorded event

Completed and verified 2026-09-26. The normal game path and the configured
real-model merged-history test verify that each GM trigger is saved as the
newest event, Developer Mode shows the event and reply, and the action is
resolved before narration.

## Problem and evidence

Every GM turn after a GM branch's first sees its own past as the newest input,
not the event it must act on. Observed by printing each request in
`merge_groups_unions_membership_and_bounds_both_source_branches` (temporary
probe, removed). The GM's second arbitration request was:

```
System: You arbitrate this location ... The current scene packet is {...}
User: Adventurer47 takes action 2: a `look` request ...     <- this turn's event
Assistant: ToolCalls(approve_action {"action_id":1}, ...)    <- previous turn
Tool: Action 1 approved.
```

Cause: `context::segments` places a turn's `static_messages` before
`live_segments`, and the three GM turn kinds in `game.rs` (`arbitrate`,
`narrate_presence_transition`, `opening_narration`) pass their triggering event
as a static `User` message. The event is therefore placed above all stored
history and is never recorded in it. Later turns also show tool calls and
narration with no visible cause, and each turn kind swaps in a different system
prompt on the same GM branch.

This likely explains the failure `fbe2108` attributed to the model: in
`real_model_assembles_context_after_a_split_and_merge`, the GM narrated without
approving about 3 times in 10. The newest context it saw was an approval that
had already happened. That link is a hypothesis until verification below
re-measures it.

## Requirement

design.md's Context assembly, from
[Agent syntax and tool calls](../user_declarations.md#agent-syntax-and-tool-calls)
and [GM interaction](../user_declarations.md#gm-interaction): input is role
prompt, then context packet, then summaries, then raw history. "The turn is
started by such an event message", and each history message carries who sent
it. The code diverges from design; design stands and the code changes. The
[prompt standards](../prompt_standards.md) apply: this is a context fix
("missing, stale, or contradictory context"), not a new instruction.

## Intent

Whatever starts a GM turn is the newest message the GM sees, and stays in its
history so later turns, summaries, merges and Developer Mode show why the GM
acted. The GM keeps one role prompt for its relationship with the group; what
differs per turn is the event and the current packet.

## Options

| Option | Pros | Cons / risks |
| --- | --- | --- |
| Record each triggering event as a history message on the GM branch before the turn (recommended) | Matches design; the event is naturally last; later turns see cause and effect; compaction and merges carry events like any other message; Developer Mode shows the trigger; uses the existing `message.trigger_sequence_id` | One more stored message per GM event (compaction already bounds this); each event needs sender wording |
| Keep events static but assemble them after history | Few lines changed | Still diverges from design; later turns still see unexplained approvals and narration; the trigger exists only in the inference recipe |
| Record only arbitration events | Covers the observed failure | Presence and opening turns keep the same defect; two ways of starting a turn |

Also considered:

- **Keep three GM system prompts versus one.** One GM role prompt per the design
  ("static for the relationship"). Instructions that differ per event ("narrate
  this to the characters present") belong in the event text, and tool
  availability already differs by turn. Keeping three prompts leaves the model
  with a role that changes under a single continuous history.
- **Scene packet inside the system text versus its own segment.** The packet is
  design item 2: dynamic, never appended to history. Keeping it inside the role
  string makes the role a new stored text every turn, and blocks
  [code quality's constructed-once recipe](code-quality.md#static-recipe-constructed-once).
  Make it its own segment, rendered into the leading system message the same
  way summaries already are.

The player agent's opening turn ("The player has entered the world...") runs
against an empty history, so its position is correct today. It still becomes a
recorded event for the same reason (one way to start a turn, visible trigger),
unless recording it would show it to the player. Check
`player_chat_after`'s filtering before choosing.

## Recommendation and steps

Depends on the validated branch identity from
[bounded history ranges' remaining corrections](developer-inspection.md#remaining-corrections),
so events append through `AgentBranch` rather than a new raw-`branch_id`
variant of `append_message_with_sequence`.

1. Add a Store operation that appends a named event message to an
   `AgentBranch` with its `trigger_sequence_id`, in the transaction that begins
   the turn.
2. Record arbitration ("Adventurer47 takes action 2: ..."), presence change and
   opening events through it, and remove them from `static_messages`.
3. Replace the three GM role strings with one GM role prompt. Pass the scene
   packet as its own dynamic segment (recorded like other recipe text) and
   render it after the role text in the leading system message.
4. Remove `static_messages`' remaining use as an event carrier; it holds role
   text only. Update the comment in `resolve_enter` accordingly.
5. Withdraw the model-selection.md note blaming the model unless verification
   still reproduces the failure.

## Verification

- A regression test on the recorded request of a second GM turn on one branch
  (the split/merge fixture): the last message is that turn's event, the
  previous event appears in history before its tool calls, and the system text
  is identical across turn kinds apart from the packet. Revert the fix and
  confirm the test fails.
- Real configured-model runs: `real_model_assembles_context_after_a_split_and_merge`
  20 times before and 20 after, reporting how many turns narrate with the
  action still pending. Also run a normal Bread Thief chat with a take request
  and a presence narration, and inspect the recorded prompts in Developer Mode.
- Complete the prompt review checklist in prompt_standards.md, and the
  [AGENTS.md checklist](../AGENTS.md#post-feature-and-pre-commit-checklist).
  Update design.md only if the event wording needs a new rule, and
  implementation_reference.md.
