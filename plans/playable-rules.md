# Playable rules and party travel implementation

## Deliverable and trace

Rust enforces the declared creation, character/NPC actions, inventory, damage
and travel rules through the existing agent tools. Required history/group,
inspection, membership and presence data already exists. The preceding
[NPC interaction](npc-interaction.md) feature supplies speech delivery, response
rounds, interruption and GM-selected intervention; extend those paths here.

| Slice | Declaration | Design |
| --- | --- | --- |
| Creation and item state | [Character creation actions](../user_declarations.md#character-creation-actions), [Game state](../user_declarations.md#game-state) | Rules and action execution |
| Persuasion and notes | [Character actions](../user_declarations.md#character-actions), [NPC actions](../user_declarations.md#npc-character-actions), [GM tools](../user_declarations.md#gm-tools) | Rules and action execution |
| Player replies and travel | [Character actions](../user_declarations.md#character-actions), [GM tools](../user_declarations.md#gm-tools) | Player replies and coordinated travel |
| Damage and timed travel | [Game turns and time](../user_declarations.md#game-turns-and-time) | Unified game time and activities; Rules and action execution |

The preceding [Unified game time](unified-game-time.md) feature supplies activity
lifecycle, resource readiness, EndTurn, initiative, timed effects and generic
human-response deadlines. Its unresolved timing choices and the remaining travel
choices under [Product decisions](../design.md#product-decisions) must be settled
before their dependent steps. Do not implement combat modes, turn flags or a
global time/lead-window system alongside it.
World authoring tools already come from [Storyteller initialization](storyteller-worlds.md).
Creation and encounter approval integration is wired in the following
[Storyteller gameplay](storyteller-gameplay.md) feature, not stubbed with
unconditional approval here.
The preceding [GM editing](gm-editing.md) feature supplies persisted relative
placement and within-location Move/CombatMove. Extend those operations for damage
and travel rather than implementing movement or scheduling again.

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
2. Extend the timing feature's existing human deadlines/AFK handling to calls
   waiting for player replies, and use it for InterventionCheck. Reuse the
   required `[game] player_response_timeout` setting and consecutive-timeout
   counting. Add the concrete pending_reply rows and the
   `Intervention` continuation. agent.rs returns AwaitingReply while preserving
   invocation/call IDs, tool ordinals and budgets. Game::process_world releases
   the current event and accepts the requested character's reply through ordinary
   chat. The deadline is a persisted wake-up that records the idle response (do_nothing for an intervention); nothing
   counts down in memory, and restart answers overdue replies first, using the
   earliest-deadline mechanism connection presence already has. Allow
   InterventionCheck to select player characters as well as NPCs: each gets the
   question in their chat and answers through their agent. Preserve the shared
   per-membership timeout count/reset and AFK departure behaviour. Test two real player
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
   Include Gesture among the `attempt` kinds and reuse its GM-request path and refusal-with-
   alternatives flow. Test heard/unheard/private conversation, persuasion,
   the existing intervention and no-response paths without a GM
   recursion cycle; inspect actual supplied context, not prose alone.
5. Implement Rust saves, damage, critical damage/scars, death, armor and recovery
   from the bundled core rules. Add actual attack, retreat and take_damage through
   unified timing's admission/resources and GM approval. Attack consumes the short
   action resource with its approved effect. Use the agreed resource costs for
   other operations; do not add begin_combat/end_combat or reimplement EndTurn,
   Wait, ReplaceActivity or timed effects. Compute can_act at the shared boundary
   and publish it; a disabled button is not enforcement. No intervention,
   Interrupt/Never mind or unused resource grants extra permission.
   Extend NPC interaction's tentative/committed intervention to physical contests:
   committed intent does not bypass a shopkeeper's actual obstruction, initiative
   or consumed allowance. Resolve checks and approved reaction effects before
   deciding the original action, without re-entering an active GM. Exercise the
   declared shopkeeper example; ConditionalApprove and dynamic executable rules
   remain unimplemented ideas pending playtesting, not prerequisites here.
   Reuse GM editing's placement and movement, and timing's activity interruption
   when damage makes continuing impossible. Verify second-action rejection,
   skipped resources after EndTurn, optional versus forced activity interruption,
   repeated Interrupt/Never mind, armor/STR damage, death and environmental harm.
   Distant NPC opportunities use the agreed derived-time policy; do not restore
   hourly processing from a global maximum clock. Delay remains a future idea.
6. Add Travel/Follow(Connection/action references) using the waiting-for-replies
   machinery from the second step with a `Travel` continuation. Collect
   PC/NPC responses, apply the approved stay/join/object policy and ask the GM
   for final arbitration with all responses visible, as with InterventionCheck.
   Approval starts timed travel and applies the real group split operation; it
   does not instantly arrive. Resolve completion or interruption through the
   activity lifecycle. Apply destination/arrival description only when reached,
   and record actual partial placement on interruption under the agreed travel
   policy. A silent or away PC goes with a strict majority of the party,
   otherwise (including a tie) stays. Short opportunities use the common
   skip/EndTurn handling. Keep all history parents
   and player agents. Test everyone travels, split party, NPC refusal, objection,
   timeout, stale connection endpoint, two simultaneous travel requests, and
   interruption before arrival without a later duplicate completion.
   No duplicate questions/results after restart or reply re-delivery.

## Completion

Run real multi-character Bread Thief and a two-location scenario through these
operations. Every state change/roll links to its full recorded turn, and old
inferences reconstruct unchanged. Update implementation_reference.md and tool
reference documentation. Re-read coding_standards.md and prompt_standards.md;
review the test cases with the user before model-facing implementation, exercise
real configured-model chats and complete the
[AGENTS.md checklist](../AGENTS.md#post-feature-and-pre-commit-checklist).
