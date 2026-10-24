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
