# NPCs answering each other without end

Two NPCs, Mara and Toma, real model, a mechanical GM that approves and narrates.
A player character asks Toma a question; the run stops at 6 NPC lines or when
both choose `do_nothing`. Three runs per row.

## Wording of the NPC role

| Wording | Runs reaching 6 lines |
|---|---|
| previous: "Call one action, or do_nothing to stay silent." | 3 of 3 |
| current: act when you need to, are addressed or have something new; `do_nothing` to give others a turn | 2 of 3 (the third stopped at 5) |
| answer once, then do_nothing and let the others reply | 3 of 3 |
| call an action only when it adds something not yet said | 3 of 3 |
| `do_nothing` description says to choose it once you have answered | 3 of 3 |

The inputs show why: each reply addresses the other NPC by name or answers it
directly, so "when you are addressed" is always true, and speaking is what gives
the other NPC its turn. An NPC also repeats itself word for word.

## A last message after the NPC has spoken

Appended as the final message of the NPC's request, only for a decision made after
the NPC has already spoken since a player character last did:

| Last message | Result |
|---|---|
| nothing | 6 lines, 3 of 3 |
| "Since a player character last spoke, you have spoken N times." | 6 lines, 3 of 3 |
| the same plus "Two replies are enough for one question: after that, call do_nothing." | 2 lines, 3 of 3 |
| "Is anyone waiting for an answer from you? If not, call do_nothing." | 2 lines, 3 of 3 |
| the same question on every decision, including the first | 0 lines, 3 of 3: nobody answered the player |
| the question or the rule placed in the system message instead | 6 lines, 3 of 3 each |

So the nudge works only as the last thing the NPC reads, and only after it has
already spoken. Asked before it has spoken, the question silences it.
