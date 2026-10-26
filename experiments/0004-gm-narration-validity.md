# How often the GM ends a turn with a valid narrate call

The GM ends each turn with one of four narrate tools (plans/npc-interaction.md).
Qwen3.5-4B, device-test model, configured sampling. A flow is a character
creation finish with the GM opening and two GM actions; a GM-only run is the
opening and two `look` actions through real arbitration without the player
agent. The count is rejected tool calls plus refused plain-text replies per
run. Runs shared the GPU with other work at times, which affects timing only.

## Failures seen

- The model answers a long scene description as plain prose instead of calling
  a narrate tool (opening, and a `look` result).
- The model sends `approve_action` and a narrate call in one response; a
  terminal call must be alone, so it is rejected and the GM runs again.

## Measurements

| Variant | Runs | Invalid outputs per run | Runs with any invalid |
|---|---|---|---|
| Role: "then end your turn with one narrate tool that says who perceives what" | 8 flows | 0 1 0 3 2 0 0 0 | 3 of 8 |
| Role: "narration reaches the characters only through a narrate tool, which ends your turn: call it alone, once your other calls have returned" | 12 flows | 0 0 0 0 0 0 0 1 0 0 2 1 | 3 of 12 |
| Same wording, Mara's scenario dialogue note kept (GM-only) | 10 | 0 0 2 0 0 1 0 0 0 0 | 2 of 10 |
| Same wording, Mara's dialogue note blanked (GM-only) | 10 | 1 0 1 1 0 1 1 0 3 1 | 7 of 10 |
| Same wording plus backend `tool_choice: required` (GM-only) | 10 | 18 16 5 32 39 14 29 20 20 9 | 10 of 10 |

## Conclusions

- The interface wording helps somewhat; it does not remove the failures.
- Blanking the scenario dialogue did not help (n=10, suggestive only).
- `tool_choice: required` is much worse: the grammar lets the model emit
  several calls in one response (`narrate_nothing`, `narrate_characters` and
  `narrate_group` together), every one of which is rejected, and inferences took
  up to 115 s. Not used.

## Later findings (replaying one captured GM request)

The GM request after approving a spoken line, replayed on the real model, 10
samples per variant:

| Variant | Plain text | Tool call |
|---|---|---|
| As is | 3 | 7 (all `narrate_group`) |
| Without the "End your turn with a narrate tool" sentence in the approval result | 5 | 5 |
| Only `narrate_group` and `narrate_nothing` offered | 6 | 4 |
| Without Mara's scenario dialogue note | 3 | 7 |
| Approval result adds "Their words are already conveyed to them." | 1 | 9 |
| Result adds "narrate only what else they perceive, or narrate nothing." | 4 | 6 |

- The model never chose `narrate_nothing` for a plain spoken line; it writes new
  narration, sometimes as a long scripted scene instead of a call.
- `tool_choice: required` in this mistral.rs does not constrain generation: 6 of
  12 samples errored with "required but no tools were called". A stop sequence
  at `</tool_call>` strips the tag and breaks parsing.
- The model sends `approve_action` and a narrate call together and repeats the
  identical pair after a rejection, so a narrate call must be allowed as the
  last call of a response whose earlier calls succeeded.
- A scene request that said "No player character is here yet" while the packet
  listed the player contradicted itself; it now names the player as about to
  arrive.

## Speech policy (the fix)

Reading the whole rendered prompt showed the GM was only ever told to narrate
what is seen and done; that spoken words are delivered as said appeared only as a
clause inside the `narrate_nothing` tool description. Replaying the captured
request after a spoken line, 10 samples each:

| Where "speech is already conveyed" is stated | `narrate_nothing` | Plain text | Other |
|---|---|---|---|
| Nowhere | 0 | 3 | 7 `narrate_group` |
| GM role only | 0 | 0 | 10 `narrate_group` |
| GM role and the approval result | 10 | 0 | 0 |
| Approval result only | 6 | 1 | 3 `narrate_group` |

Adopted: the role says spoken words reach their hearers as said, so narrate only
what else happens or call `narrate_nothing`; the approval result for speech says
the words are already conveyed so there is nothing to narrate. The GM still has
every narrate tool. The device suite then passed with no invalid output (178 s).

## Narrating a non-speech action (look, ask, open)

A look at an inventory got approval and no narration. The GM's history held a run
of approve-then-`narrate_nothing` turns, and the approval result for a non-speech
action read "Outcome: approved" without saying the characters had been told
nothing. The GM's decision after approval, replayed on the real model with that
history, 8 samples each:

| Action | as it was | role sentence scoped to speech | result says nobody has been told |
|---|---|---|---|
| look | `narrate_nothing` 8 | `narrate_nothing` 8 | narrates 8 |
| ask | `narrate_nothing` 8 | `narrate_nothing` 8 | narrates 8 |
| open | `narrate_nothing` 8 | `narrate_nothing` 8 | narrates 8 |
| investigate | narrates 8 | narrates 8 | narrates 8 |
| speak | `narrate_nothing` 8 | 8 | 8 (unchanged) |

Adopted: only the result change ("The characters have not been told the outcome:
narrate it."). Rewriting the request text as plain language changed none of these
choices. The narrations also invented a character's inventory: the GM's packet
held the location's items but not what each character carries. With each
character's inventory in the packet, and the result above, the GM's look at an
inventory narrated 8 of 8, naming only the flour sack the character held.

## Merging the near-identical tools (`attempt`, `speak`)

`look`/`investigate`/`open`/`ask` became `attempt` with a `kind`, and `say`/`whisper`
became `speak` with `to` and `private`. The player's tool text fell from 3,721 to
2,526 characters. The first device-suite runs passed 4 of 8, which prompted
repeating the failing scenarios:

- GM answering a player's question (8 runs each): `kind` ask 5 correct, `kind` look 5
  correct. The kind does not matter. The misses were the GM narrating with another
  narrate tool than the test demanded; the check now reads the text of whichever
  narrate tool the GM called.
- NPC answering a question (10 runs): 4 failed. The cause was in the model's own
  output: it passed two names in `to` on a public speech and the "at most one
  addressee" rule failed the NPC's step. Public speech now accepts several names
  (it singles out an addressee only when exactly one is named). Afterwards 10 of 10
  passed, none private.
- Full device suite afterwards: passed (218 s).

- The structured public addressee was then dropped (user declaration: the addressee is up to the character agent, in the text). 	o now only lists a private speech's hearers and is ignored on a public one. Device suite afterwards: passed (232 s).
