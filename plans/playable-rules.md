# Playable rules and party travel implementation

## Deliverable and trace

Rust enforces the declared creation, character/NPC actions, inventory, combat,
time and travel rules through the existing agent tools. Required history/group,
inspection, membership and presence data already exists. The preceding
[NPC interaction](npc-interaction.md) feature supplies speech delivery, response
rounds, interruption and GM-selected intervention; extend those paths here.

| Slice | Declaration | Design |
| --- | --- | --- |
| Creation and item state | [Character creation actions](../user_declarations.md#character-creation-actions), [Game state](../user_declarations.md#game-state) | Rules and action execution |
| Persuasion and notes | [Character actions](../user_declarations.md#character-actions), [NPC actions](../user_declarations.md#npc-character-actions), [GM tools](../user_declarations.md#gm-tools) | Rules and action execution |
| Consent and travel | [Character actions](../user_declarations.md#character-actions) | Player replies and coordinated travel |
| Combat and time | [Game turns and time](../user_declarations.md#game-turns-and-time) | Rules and action execution |

The combat order, time lead and travel choices are explicit proposals under
[Product decisions](../design.md#product-decisions). Their dependent steps wait
for confirmation; the implementation descriptions below are complete proposals.
Storyteller approval tools are wired in the following Storyteller feature, not
stubbed with unconditional approval here.

## Ordered changes

1. In the existing character/store code, replace untyped mechanical sheet access
   with CharacterSheet and Creating/Ready progress from design.md. Add typed
   checked-in tables for backgrounds, traits, bonds, age and their starting items
   from the bundled Cairn SRD. Extend creation tools with choose/roll_background,
   choose_name, roll_traits, roll_bonds and roll_age; retain existing HP/attribute
   rolls. Each roll/result/state change is one action transaction and repeated
   submission returns the prior roll. Support the declared attribute swap once.
   ready_to_begin validates the full creation state; the group's initial ready
   barrier performs RollOmens once and then opens play. Test incomplete/duplicate
   creation and the youngest-character omen rule through real agent chat.
2. Consolidate item holders as specified in design.md. Introduce typed item stats
   for slots, armor, weapon dice and spell references; preserve separate object
   notes. Change take and add give/drop/place to the same transactional transfer
   operation, checking current ownership, ten-slot capacity and consumed approval.
   Render the resulting action/dice via existing history entries. Test concurrent
   attempts to take one item, full inventory, bulky/petty items and repeated
   approval: exactly one holder and no second mutation.
3. Add the concrete pending_reply rows and Transfer/Travel continuation variants.
   agent.rs returns Suspended while preserving invocation/call IDs, tool ordinals
   and budgets. Game::process_world releases the current event and accepts the
   requested character's reply through ordinary chat. Add accept/decline tools
   with explicit action IDs. Response/deadline events resume a continuation once;
   recover these rows/timers at startup. A consent timeout records the declared
   motionless narration and propagates an error, without transferring the item.
   Recheck target ownership/capacity when resuming. Test two real player chats,
   ambiguity requiring another reply, disconnect, timeout and restart.
4. Extend the shared speech path with lie's declared WIL/suspicion outcomes and
   BePersuaded as an NPC-owned save tool.
   Add authorized per-object note-edit tools and feed current notes into each
   GM/NPC invocation. Reuse NPC interaction's hearing rules, proposal selection,
   shared character executor and GM intervention. Explicit human consent for
   transfers/travel uses pending_reply; do not reopen ordinary NPC discussion
   inside an unresolved tool call.
   Reuse look for investigation/open/ask and its refusal-with-
   alternatives flow. Test heard/unheard/private conversation, persuasion,
   the existing intervention and no-response paths without a GM
   recursion cycle; inspect actual supplied context, not prose alone.
5. Implement Rust saves, damage, critical damage/scars, death, armor and recovery
   from the bundled core rules. Add attack, retreat, take_damage, begin_combat,
   end_combat and wait. Store combat/participant order, turn flags and time in
   the same action transaction. Compute can_act server-side before accepting a
   command and publish it afterward; a disabled button is not enforcement.
   Closing a character's game-turn consumes any remaining move/action allowance,
   including Wait. Reset flags only when its next game-turn begins. Interrupt,
   Never mind and unused allowance do not grant out-of-turn permission. Wire
   these checks into the same action admission used by players and NPC follow-ups.
   Add movement's within-location description and approved duration. Advance
   world time to the maximum and queue crossed-hour NPC opportunities through
   WorldEvents. Verify the approved initiative/time policy, second move/action
   rejection, early Wait followed by an attempted action, repeated Interrupt/
   Never mind, out-of-turn requests with unused flags, armor/STR damage, death
   and environmental damage. Do not add the proposed future combat Delay tool.
6. Add travel(Connection ID) using the same pending reply continuation. Collect
   PC/NPC responses, apply the approved stay/join/object policy and ask the GM
   for final arbitration with all responses visible. On approval, call the real
   group split operation, update the travelling group's location and GM arrival
   description, and complete the action atomically. Keep all history parents
   and player agents. Test everyone travels, split party, NPC refusal, objection,
   timeout, stale connection endpoint and two simultaneous travel requests.
   No duplicate questions/results after restart or reply re-delivery.

## Completion

Run real multi-character Bread Thief and a two-location scenario through these
operations. Every state change/roll links to its full recorded turn, and old
inferences reconstruct unchanged. Update implementation_reference.md and tool
reference documentation. Re-read coding_standards.md and prompt_standards.md;
review the test cases with the user before model-facing implementation, exercise
real configured-model chats and complete the
[AGENTS.md checklist](../AGENTS.md#post-feature-and-pre-commit-checklist).
