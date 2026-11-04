# Playable rules and party travel implementation

## Deliverable and trace

Rust enforces the declared creation, character/NPC actions, inventory, damage
and travel rules through the existing agent tools. Required history/group,
inspection, membership and presence data already exists. The preceding
[NPC interaction](archive/npc-interaction.md) feature supplies speech delivery, response
rounds, interruption and GM-selected intervention; extend those paths here.

| Slice | Declaration | Design |
| --- | --- | --- |
| Creation and item state | [Character creation actions](../user_declarations.md#character-creation-actions), [Game state](../user_declarations.md#game-state) | Rules and action execution |
| SplitItem/CombineItems and morale | [Character actions](../user_declarations.md#character-actions), [GM tools](../user_declarations.md#gm-tools) | Rules and action execution |
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
2. Done by [Player replies](archive/player-replies.md), which implements what follows; kept here
   as the record of the original scope. Extend the timing feature's existing human deadlines/AFK handling to calls
   waiting for player replies, and use it for InterventionCheck. Reuse the
   required `[game] player_response_timeout` setting and consecutive-timeout
   counting. Add the concrete pending_reply rows; the asking turn ends and a new
   turn of the same agent starts on the answers (no stored continuation).
   Game::process_world accepts the requested character's reply through ordinary
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
   Add SplitItem(item ID, description) and CombineItems(input IDs, description)
   through the same action/GM approval path. Approval names two existing ItemType
   IDs for a split (the same type may be used twice), or one for a combination
   plus the result's holder/placement through the existing holder references.
   Check live world-local inputs/types and authorization, then consume inputs and
   instantiate outputs atomically. Keep consumed records and history; filter them
   from active inventory and action targets. Split outputs retain the original
   holder; combined outputs use the explicitly approved holder. Reuse normal
   authorization for inputs and the result instead of forcing preparatory
   transfers or inferring a holder from argument order. The GM judges physical
   access and consent through the existing action path. Return
   new item IDs in the action result, and return those same IDs on repeated approval.
   Use the prepared full- and half-sack types in Bread Thief: one full becomes two
   halves; two halves become one full. The prepared half type has no initial item.
   Make both types discoverable through existing GM lookup before approval needs
   their IDs. Test split/combine round-trip, an unavailable input or foreign type
   changing nothing, and repeated approval producing no duplicate outputs. Check
   a combination with accessible inputs at different holders, an unauthorized
   result holder, and consumed input IDs still reconstructing in old history.
   No new crafting framework
   or arbitrary fractional item ledger.
4. Implement the shared Rust attribute-save operation before its callers: GM Save
   takes a character, attribute, short reason and applicable difficulty modifier,
   and returns the recorded roll/outcome. Characters do not invent or resolve the
   roll. Use it for Bread Thief's DEX stealth check before approving the theft:
   success permits feasible quiet removal; failure alerts Toma through narration
   and leaves his response to his agent. Then extend the shared speech path with
   lie's declared WIL/suspicion outcomes and BePersuaded as an NPC-owned save tool,
   reusing the same Rust roll calculation with their declared policies.
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
5. Extend the shared Rust saves with damage, critical damage/scars, death, armor and recovery
   from the bundled core rules. Add actual attack, retreat and take_damage through
   unified timing's admission/resources and GM approval. Attack consumes the short
   action resource with its approved effect. GM approval supplies the Cairn
   weakness/advantage flags and applicable modifiers; Rust calculates and rolls
   damage and applies armor, HP/STR loss, critical-damage saves and resulting
   conditions/death. Return the result to the GM and damaged character; the GM
   narrates without giving bystanders the numbers. Attack damage must not be
   applied again through GM TakeDamage or note/field edits. TakeDamage stays
   environmental harm, sharing the underlying damage calculations without giving
   the GM control over attack results. Repeated approval must not roll or damage
   twice. Use the agreed resource costs for
   other operations; do not add begin_combat/end_combat or reimplement EndTurn,
   Wait, ReplaceActivity or timed effects. Compute can_act at the shared boundary
   and publish it; a disabled button is not enforcement. No intervention,
   Interrupt/Never mind or unused resource grants extra permission.
   Retreat uses the shared DEX save before the GM permits the described safe
   Move/run; failure does not grant a consequence-free escape. Apply any allowed
   movement through the existing movement/timing path.
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
   Add the GM's MoraleCheck using the shared WIL save and Cairn's morale triggers.
   Record each NPC result for that NPC and the GM. A failed NPC gets the declared
   flee instruction before its next ordinary turn; keep this internal instruction
   out of perceived-event delivery. PCs have no roll or effect. NPC-only handling
   belongs in Rust, not extra GM instructions about the exception. Test successful
   and failed NPC checks and a mixed PC/NPC request, including the failed NPC's
   next actual inference input. Do not build another response scheduler for morale.
   Distant NPC opportunities use the agreed derived-time policy; do not restore
   hourly processing from a global maximum clock. Delay remains a future idea.
6. Add Travel/Follow(Connection/action references) using the waiting-for-replies
   machinery from the second step with `Travel` replies. Collect
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

For Bread Thief, verify a voluntary resolution, actual portion transfers after
SplitItem, recombination without creating food, stealth success/failure, a failed
attempt followed by harder negotiation, and confrontation that can end in combat
or retreat. Use scripted Store/Game paths for dice/state boundaries and captured
real-model chats for tool use and perception; do not prescribe either NPC's deal
or treat recovering all flour as the only valid ending. Combat checks must show
that Rust, not GM prose, determines damage and that morale failures reach the
NPC's next turn as instructions. These are acceptance cases for the existing
steps, not a separate scenario-specific tool suite.
