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
call at all.

**After the action.** With thinking switched on, the agent's reasoning at that point is
"Now I need to continue the scene by having Toma respond... Since this is a roleplay
scenario, I should provide the GM's narration". The role said "Guide the player through
the current scene". The request after a `speak` call was replayed 10 times for each of
`speak`, `ask`, `look` and `take`: the original role, a role saying the game tells the
player what happens next, and the same with an end-of-turn line each produced text of
its own in 40 of 40, and never an empty reply. A model that does not produce an empty
reply cannot be told to; ending the turn after an action is deterministic, so the
code does it (`Tool::ending` on the action tools).

**The first step.** "Toma, I will defend you if Mara tries to take it." got a text reply
in character in 12 of 12 samples without thinking (the other three lines: `speak` 12 of
12). The role is what changed it: "The player types what their character does or says,
and you carry it out with the action tools." made all four lines `speak` 10 of 10, as
did two longer variants. The check now passes 3 of 3 runs (every line failed before).
Questions to the agent itself were replayed 8 times each, old role against new: "What
do you think Toma wants?" is answered in text in both; "Ask the GM what the hut is built
from." is an `ask` in both; "What should I do next?" was a text answer and now calls
the read-only `party_presence` tool first (8 of 8; what it writes afterwards was not
measured). The listener scenario in the device suite, a player asking what the other
player said, still gets a text answer.

## Correction: the replay sampling

The GM and player-agent replays above were captured from scripted scenes, which use
temperature 0, so each "12 of 12" was one deterministic answer repeated. The runs
through the real game (temperature 0.7) are what carry weight: the all-real NPC
scenario, the player-agent check, the GM ask check and the device suite. With the production
sampling (40 samples), the GM's first answer to a take named an invalid set of characters
(a player or the actor) 18 times: the check asks NPCs only, and the GM sees one list of
characters. The GM's second step put speech controls on the take in 11 of 60 intervention
turns, with the complete approval schema. Both end as invalid output in the device suite.
Letting the check skip players was tried and dropped: the declarations ask players
(user_declarations.md, InterventionCheck and "Players are expected to respond in a timely
manner"), so a silent skip is not what is declared.

## Speech options on an approval

The GM step after the intervention check, replayed 60 times at the production sampling
with each form of `approve_action`. Counted: speech options placed on the take (not on
Toma's spoken warning, which they apply to), which the tool rejects as invalid output.

| Form | On the take | Used on the warning |
|---|---|---|
| flat `include`, `exclude`, `prefix` ("Only for speech; leave out...") | 5 of 60 | 19 of 60 |
| the same, positive wording ("For speech: ...") | 6 of 60 | 38 of 60 |
| the same, a sentence in the tool description | 6 of 60 | 35 of 60 |
| both wording changes | 10 of 60 | 38 of 60 |
| options nested in one `hearing` object | 11 of 60 | 42 of 60 |
| options renamed `speech_include` and so on | 13 of 60 | 58 of 60 |
| nested and renamed | 15 of 60 | 28 of 60 |
| two tools: `approve_action` (id only) and `approve_speech` (id and the options) | **0 of 60** | 24 of 60 through `approve_speech`, the rest through `approve_action` |

Wording and naming do not reduce the misuse, and anything that makes the options more
visible raises it. The two-tool form removes it; the request is 55 tokens longer.
