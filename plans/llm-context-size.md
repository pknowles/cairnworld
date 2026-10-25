# Model context size: find and justify what is in it

Status: not started. Nothing below has been decided or tried.

## Goal

Know what every part of the context each agent is sent costs, and whether each
part earns its place. The model has a fixed context of 4096 tokens in the
real-model tests, and prompt_standards.md says every addition has a cost. This
plan finds the waste; it does not choose the remedy.

Follow the [AGENTS.md checklist](../AGENTS.md#post-feature-and-pre-commit-checklist),
[coding standards](../coding_standards.md) and
[prompt standards](../prompt_standards.md). Any change to what a model receives
is model-facing: replay the captured requests on the real model before and after
and keep a change only if the measured behaviour does not get worse.

## Measurements so far

One GM request after approving a spoken line, as rendered for the model
(`experiments/0004-gm-narration-validity.md` has the captures): 12,380 characters.

| Part | Characters | Share |
|---|---|---|
| Tool definitions (nine tools) | 7,344 | 59% |
| Scene packet | 2,907 | 23% |
| Standard tool-calling text from the chat template | 817 | 7% |
| GM role | 374 | 3% |
| Everything else (the action request, the tool call and result) | about 850 | 7% |

Per tool, characters in total / of which description text: `split_group` 1,805 /
881, `approve_action` 1,105 / 479, `merge_groups` 858 / 361,
`intervention_check` 799 / 318, `narrate_actor_and_others` 768 / 223,
`reject_action` 567 / 158, `narrate_group` 588 / 141, `narrate_characters` 564 /
113, `narrate_nothing` 273 / 96. Description text is under 40% of the tool block.

GM requests late in a real-model run reached 3,500 to 3,700 input tokens against
the 4,096 limit. The player agent's and NPCs' contexts have not been measured.

## Problems to investigate

- How much of each tool definition is generated metadata rather than written
  text: the schema URL, the title, `additionalProperties`, `strict`, and an
  `int64` format on every id. About 940 characters across nine tools. Which of
  these the model or the constrained argument generation actually needs is not
  known.
- Whether every description sentence is needed. `split_group` is 881 characters
  of description for a rarely used tool. Several narrate tools repeat that they
  end the turn, which the role also says.
- Whether all nine tools need to be present in every action turn, or whether the
  ones that apply only sometimes (`split_group`, `merge_groups`,
  `intervention_check`) have a cost that is worth paying each time.
- Whether tools that overlap (`approve_action` and `reject_action`;
  `split_group` and `merge_groups`) cost more as separate definitions than the
  clarity they give the model.
- Whether the scene packet's contents are all used by the GM. It carries every
  NPC's full background, motive, ambition and private notes, the item list and
  each player's sheet on every request, including requests where none of it is
  relevant. Some notes contain dialogue written as if spoken
  ("Mara says: ..."), which the GM role says not to put in a character's mouth.
- How many times the same workflow instruction is stated. "End your turn with a
  narrate tool" now appears in the role, the action request and every approval
  result; whether each repetition changes behaviour has only been measured for
  the speech wording.
- How the history the GM keeps across many actions grows, and what it costs
  before compaction applies. Late-run requests are close to the limit.
- Whether player-agent and NPC contexts carry the same kind of overhead. Not
  measured.
- Whether the chat template's standard tool-calling text can be changed, and at
  what risk to tool-call reliability.

## Verification

- A repeatable measurement of the size of each agent's request by part, taken from
  the requests the real chat path produces, so a change can be seen in numbers.
- Replays of the captured failing requests (the speech turn, the opening, a take,
  split and merge) on the real model, a fixed number of samples before and after,
  with the same pass criteria as the device suite: no invalid output.
- The device suite still passes within the five-minute budget.
