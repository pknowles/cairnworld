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
