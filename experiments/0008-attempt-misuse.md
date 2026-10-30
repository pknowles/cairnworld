# Characters speaking through `attempt`, and the GM approving it

Found in a played game (Toma used `attempt` of kind `ask` for 7 of 10 actions, his words
in the description; the GM narrated them as speech, and Toma then copied that). Real
model, temperature 0.7. 12 real NPC decision requests captured from the all-real
scenario (`npcs_hearing_the_scene_do_not_speak_through_attempt`), each replayed 8 times
(96 samples per row).

| Offered to the character | `ask` | invalid or text | speak | do nothing |
|---|---|---|---|---|
| original (`attempt` first) | 9 | 10 | 36 | 16 |
| `kind` text says "take literally", `ask` is never words to a character | 4 | 11 | 38 | 21 |
| and `description` says "does, or the question for the GM" | 5 | 7 | 38 | 25 |
| `ask` renamed `ask_gm` and `description` as above | 0 | 11 | 35 | 28 |
| **`speak` offered before `attempt`, no new words** | **0** | **2** | **47** | 23 |
| order, rename and description together | 0 | 1 | 55 | 16 |

The invalid ones are `kind` values that are not in the schema (`speak`, `attempt`) or
text instead of a call. The characters reached for `attempt` because it came first; the
tool order is the change kept. The all-real scenario (creation, real GM scene narration,
real NPCs) failed in 2 of 3 runs before and passed 6 of 6 after.

## The GM approves every ask

`real_model_gm_answers_a_question_and_rejects_any_other_ask`: two real questions and four
things sent as an ask (Toma's speech, Mara addressing the Adventurer, an action, a
question about the story's end), with the NPCs as the askers. The GM approved all four
non-questions in every run, and rejected only the story question once told so. Replays
(12 samples per case) with each of these changes left it approving speech, the NPC
address and the action 12 of 12:

- a sentence in the role saying an ask is a question for the GM
- the same, wording variants, placed in the role, the request, the approve tool and the
  reject tool at once
- showing the request as "X asks you: ..."
- renaming the kind `ask_gm`, and a separate `ask_gm(question)` tool
- the criterion "approve it if it is what its kind says"

Reading the GM's reasoning (thinking switched on for the replay): it takes the
description as the character's action ("Toma is speaking to the Adventurer, a
reasonable action; approve"), judges whether that can happen in the scene, and says it
will then narrate what Toma says. It sees `"kind":"ask"` and does not use it. The
question rule never enters the judgement it is making.

## The player agent

`the_player_agent_says_what_the_player_says_and_stops` types four lines to the real
player agent. In all 3 runs every line failed: it wrote text after its `speak` call
(Toma's and Mara's reactions, or a repeat of the tool result), it rewrote the player's
words ("Mara says: ..."), and for a line addressed to a character by name it made no
call at all. Rewriting the role and end-of-turn text to say what the agent is for did
not change that in 2 runs of each of 2 rewrites.
