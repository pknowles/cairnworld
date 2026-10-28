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

## Around "something new and important to say"

Variations of the current role wording that keep that phrase, three runs each:

| Wording | Result |
|---|---|
| drop "are addressed" | 6 lines, 3 of 3 |
| "call one action **only** when you need to act or have something new and important to say" | 6 lines, 3 of 3 |
| "are asked something" in place of "are addressed" | 6 lines, 3 of 3 |
| "before you speak, ask whether it is new and important to the others; if not, call do_nothing" | erratic: 0 lines, 1 line, then 6 lines (nobody answered the player in the first) |

The same phrase as the final message of the NPC's request ("Do you have something
new and important to say? If not, call do_nothing."):

| When it is added | Result |
|---|---|
| after the NPC has spoken since a player character last did | 2 lines, 3 of 3 |
| on every decision | 0, 2 and 1 lines: nobody answered the player in one run |
| only when the last speech the NPC heard was by another NPC | 2 lines, 3 of 3; the player's question was answered each time |

The last row needs no count of what the NPC has said: a player's speech never
gets the question, so a player is always answered normally.

## Outcome: the role and the question on every decision

The scripted GM in the tests above narrated "It is said." after every line; a real GM
adds nothing to speech, so those counts overstate what players see. Re-measured with
the GM calling `narrate_nothing` for speech. NPC lines until the discussion ends,
three runs each (the first line answers the player's question to Toma):

| Role | Question before each decision | Lines per run |
|---|---|---|
| "You are the character…" | none | 21, 22, 23 (ran until the inference budget) |
| "You operate a character in an RPG scene…" | none | 15, 13, 14 |
| "You are the character…" | "Respond as your character would…" | 12, 14, 14 |
| "You operate a character…" | "Respond as your character would…" | 13, 6, 11 |
| "You are the character…" | "Do you have something new and important to say? If not, call do_nothing." | 0, 0, 0: the player was not answered |
| "You operate a character in an RPG scene…" | same | 1, 1, 1 |

The role framing decides whether the question lets an addressed character answer.
Chosen: the "operate" role (without "scene") and the "Do you have…" question as the
last message of every NPC decision, for every character, with no check on who spoke.
Final wording, eight runs, a question put to Toma: 1 line every time (Toma answers,
Mara stays silent). Four runs putting it to Mara: Mara answered every time; two runs
ended there, two with Toma and Mara adding one line each.

## Limits of these tests

Only two NPCs are in the scene, so every NPC line addresses the other NPC and "are
addressed" in the role wording is always true. The wordings that dropped it say
nothing about scenes with three or more NPCs, where it keeps a bystander from
answering something said to someone else. The final-question nudge keeps the role
wording as it is. Untested: three or more NPCs, longer conversations.
