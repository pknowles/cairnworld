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
| Player replies and travel | [Character actions](../user_declarations.md#character-actions), [GM tools](../user_declarations.md#gm-tools) | Player replies and coordinated travel |
| Combat and time | [Game turns and time](../user_declarations.md#game-turns-and-time) | Rules and action execution |

The combat order, time lead and travel choices are explicit proposals under
[Product decisions](../design.md#product-decisions). Their dependent steps wait
for confirmation; the implementation descriptions below are complete proposals.
World authoring tools already come from [Storyteller initialization](storyteller-worlds.md).
Creation and encounter approval integration is wired in the following
[Storyteller gameplay](storyteller-gameplay.md) feature, not stubbed with
unconditional approval here.
The preceding [GM editing](gm-editing.md) feature supplies persisted relative
placement and the out-of-combat within-location Move path. Extend its existing
action/time operations for combat and travel rather than implementing Move again.

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
2. Add waiting for player replies and use it for InterventionCheck. Add the
   required `[game] player_response_timeout` setting (300 seconds in default.toml,
   no code default). Add the concrete pending_reply rows and the
   `Intervention` continuation. agent.rs returns AwaitingReply while preserving
   invocation/call IDs, tool ordinals and budgets. Game::process_world releases
   the current event and accepts the requested character's reply through ordinary
   chat. The deadline is a persisted wake-up that records the idle response (do_nothing for an intervention); nothing
   counts down in memory, and restart answers overdue replies first, using the
   earliest-deadline mechanism connection presence already has. Allow
   InterventionCheck to select player characters as well as NPCs: each gets the
   question in their chat and answers through their agent. Count consecutive timeouts per membership, reset by any player response, and at 3
   end the connection with the AFK message on WorldDetail. Test two real player
   chats, a player who intervenes, one who answers do_nothing, a timeout, three
   timeouts in a row ending the connection, disconnect, restart with an overdue reply, and a mixed NPC and player check.
3. Consolidate item holders as specified in design.md. Introduce typed item stats
   for slots, armor, weapon dice and spell references; preserve separate object
   notes. Replace the `take` tool with the `transfer` tool (`kind` take, give,
   drop or place; design.md) and make all four kinds one transactional holder
   change checking current ownership and consumed approval. `approve_action`
   gains an optional location description, required for drop and place and
   stored as the item's description at its new location. `committed` and
   InterventionCheck apply to every kind. There is no acceptance step: a clear
   refusal by the receiver arrives as an intervention and the GM rejects. A
   committed drop need not be rejected. The ten-slot capacity check may be added
   here or later; it is not required for this step. Render the resulting
   action via existing history entries. Test concurrent attempts to take one
   item, give to an NPC who objects and one who does not, drop and place with
   and without a description, committed drop, and repeated approval: exactly one
   holder and no second mutation. Exercise routing of the four kinds on the real
   model.
   A player-character receiver or holder is asked through InterventionCheck
   (previous step) and has the configured response time to object. Test a give
   to a player who objects, one who stays silent until the timeout, and one who
   consents.
4. Extend the shared speech path with lie's declared WIL/suspicion outcomes and
   BePersuaded as an NPC-owned save tool.
   Reuse [GM editing and lookup](gm-editing.md) for authorized object-field/note
   access and current GM state; do not expose private notes to NPCs or implement
   another editing interface. Reuse NPC interaction's hearing rules, proposal selection,
   shared character executor and GM intervention. Explicit human consent for
   travel uses pending_reply; do not reopen ordinary NPC discussion
   inside an unresolved tool call.
   Reuse the `attempt` tool's GM-request path and its refusal-with-
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
   Extend NPC interaction's tentative/committed intervention to physical contests:
   committed intent does not bypass a shopkeeper's actual obstruction, initiative
   or consumed allowance. Resolve checks and approved reaction effects before
   deciding the original action, without re-entering an active GM. Exercise the
   declared shopkeeper example; ConditionalApprove and dynamic executable rules
   remain unimplemented ideas pending playtesting, not prerequisites here.
   Reuse GM editing's within-location description and approved duration. Advance
   world time to the maximum and queue crossed-hour NPC opportunities through
   WorldEvents. Verify the approved initiative/time policy, second move/action
   rejection, early Wait followed by an attempted action, repeated Interrupt/
   Never mind, out-of-turn requests with unused flags, armor/STR damage, death
   and environmental damage. Do not add the proposed future combat Delay tool.
6. Add travel(Connection ID) using the waiting-for-replies machinery from the
   second step with a `Travel` continuation. Collect
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
