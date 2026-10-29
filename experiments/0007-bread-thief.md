# Bread Thief: competing claims

2026-10-29. Scenario authoring checks requested by the user. These are independent
role-play samples, not measurements of Qwen's reliability or scripts for NPCs.

## Problem and revision

The original adventure already gave Toma an injured leg. Its playable conversion
also omitted much of the positioning and opposition around taking the sack.
An initial revision made him healthier and better defended but kept an elaborate
cache and negotiation setup. That addressed difficulty more than coherence.

The revised situation has one conflict: Mara owes Toma the flour for completed
mill repairs, but withheld it after a flood spoiled the other village stores.
Toma took it; both his household and the village households need food. Both
acknowledge the debt. The cache, constable and scripted exchanges are removed.

## Hypothetical probes

Separate agents received only their assigned character's situation and the
questions, without the author's preferred resolution. The supplied facts,
personalities and motives correspond to the NPC backgrounds in the scenario.
Each hypothetical was independent, not a conversation continued from the
previous answer. A third agent played the adventurer without a preferred ending.

| Hypothesis / situation | Observed choice |
| --- | --- |
| Toma has a reason to refuse a bare demand to hand over the sack | He refused, citing his completed work and payment. |
| Asking Toma what happened reveals the competing claim | He explained the unpaid wages and distrust of another promise. |
| Asking for charity is persuasion, not automatic surrender | He cited his own household's hunger and asked how much he was being asked to leave. |
| Supporting Toma's claim can end the dispute without recovery | He accepted the adventurer's support and chose to carry the sack home. |
| A grab is opposed without requiring a scripted attack | He tried to reclaim and shield the sack; he did not immediately use the axe. |
| A concrete contribution can make a compromise worthwhile | He accepted three actual days of player rations for leaving half the flour. |
| Mara can acknowledge the debt without abandoning her responsibility | She confirmed the work and promise, but argued that other hungry households also did not cause the flood. |
| Mara need not support violence just to recover the flour | She opposed the proposed assault and tried to talk. |
| The adventurer has a meaningful choice rather than an obligatory recovery | The player probe asked questions, defended the earned payment, then asked about voluntary sharing; if refused, it would leave the sack with Toma. |

Mara suggested sharing after the adventurer defended Toma's claim. Her opening
answer did not propose a settlement. This does not establish that NPCs will
never suggest a compromise themselves. No instruction forbidding that, forcing
agreement, or prescribing these answers was added to the scenario.

The player probe found a real cost rather than a consequence-free solution:
sharing reduces Toma's earned food; keeping it leaves less for other households.
That supports the premise, not a claim that every player will find the decision
equally difficult. A voluntary compromise may simply work.

## Application boundary

`Store::npc_scene` supplies background, motive and ambition, but not GM notes.
The conflict and each character's reasons therefore belong in those NPC-facing
fields. GM notes alone cannot establish what an NPC knows or values.

`bread_thief_npc_fields_survive_import_into_npc_context` imports the actual
playable JSON through Store and checks the resulting production NPC scenes
against the authored fields. It does not assert a predetermined NPC decision.
The existing scenario import/export check covers the complete reusable graph.

The old hut mechanics fixture is separate from the authored scenario. Tests of
action execution should not require the playable encounter to retain an old
injury, cache or particular motive wording.

Validation: the three scenario-filtered tests passed (0.13 seconds of test
execution), including the NPC-field check and the current scenario's full
import/export check. The captured NPC packets contained the wage dispute and
each character's own reasons. A separate import/export through the existing
application executable also preserved all authored values, with the existing
schema defaults for omitted optional fields. The scenario Rust file passed
its formatting check. The complete non-device binary suite then passed: 237
passed, 7 ignored, 0 failed, in 22.90 seconds. These checks ran in the shared
working tree while another agent's changes were also present; they do not
claim to validate that agent's work against its separate requirements. No
production Rust or prompt template was changed here.

Qwen gameplay has not been measured by these role-play probes. Captured
production context and actual replies are needed before attributing any future
model behaviour to a scenario fact or claiming it follows the same choices.
