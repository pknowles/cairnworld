# Campaign completion implementation

## Deliverable and trace

A campaign ends in win/loss, allows the declared talk-only epilogue, then records
a final account and closes play. Authority:
[End Conditions](../user_declarations.md#end-conditions),
[Epilogue sequence](../user_declarations.md#epilogue-sequence), and
[World detail](../user_declarations.md#world-detail-page).
Implement [Campaign completion](../design.md#campaign-completion), reusing the
Storyteller phase/state, conversations, rules death state and shared recordings.

## Ordered changes

1. Add typed trigger metadata to story-relevant objects and the GM
   update_storyteller(summary,object_ids) tool. Invoke it after marked item,
   conversation or combat events; its context contains world summary,
   Storyteller notes and the event summary, not the complete GM private transcript.
2. Add end_world(outcome,text). Validate phase=playing, record its narration
   content reference and change to epilogue once. The no-living-PC condition
   enters the same loss path. Snapshot epilogue participants in
   epilogue_participant(world_id,character_id), deliver ending narration to all
   characters, append the NPC epilogue notice and queue the player-agent question.
3. Build the epilogue tool surface from conversation and ready_to_end only.
   Recheck phase in action mutation methods so forged/stale tools cannot act.
   Conversation may reach a named NPC at another location only in epilogue.
   NPCs respond when addressed; they do not get unsolicited action turns.
4. Add epilogue_choice(character_id,content_ref). ready_to_end stores the player's
   summary once, then checks the persisted participant barrier. Follow the
   offline/dead participant policy confirmed in design.md; do not invent a timer
   or owner override. When the barrier is satisfied, queue one final Storyteller
   inference with all choices and the ending context.
5. Store the final output reference and phase=complete atomically. WorldDetail
   resolves the epilogue text and completed status. Existing game sockets receive
   read-only status and can_act=false; new game entry is refused, while authorized
   Developer Mode remains available through world inspection routes.

## Verification and completion

Exercise win and all-PC-dead loss, duplicate end requests, cross-location NPC
conversation and attempted mutation during epilogue. Disconnect/restart before
and after the final choice and final inference; no choices or completed output
are duplicated/lost. One real multi-player model session must produce a final
account consistent with recorded player choices and actual events. Reloaded
completed worlds show the same account and cannot resume play.

Update implementation_reference.md; re-read coding_standards.md and
prompt_standards.md, obtain the required LLM-test review, run real paths and
complete the [AGENTS.md checklist](../AGENTS.md#post-feature-and-pre-commit-checklist).
