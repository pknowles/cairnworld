# Which context can go without changing what the model does

Real model (Qwen3.5-4B, temperature 0.7). Requests captured from the production
path in a scripted two-player scene (`context_size_by_agent_and_part` prints the
parts of each request), then replayed 10 or 15 times per variant and case. A
case is a player line: speak to Toma, ask the GM, look, take the flour sack, and
walk around the back with Mara (a split). Input tokens are what the model
counted. Each cell below is the tool or tools the model called; every cell was
the same on all runs unless a count is shown.

Baseline input tokens: GM request 3,390 (limit 4,096), GM after its approval
3,480, NPC 1,500, player agent 1,270.

## Variants

| Variant | GM tokens | NPC | Player | Choices changed |
|---|---|---|---|---|
| remove `$schema`, `title`, `format`, `default` from tool schemas | -345 | -100 | -100 | none |
| ... and `additionalProperties` | -50 more | -20 | -20 | NPC chose `speak` instead of `attempt` 10/10: kept |
| offer no `split_group`, `merge_groups`, `intervention_check` | -845 | | | none; rejected, agents must see every tool |
| `approve_action` without `include`/`exclude`/`prefix` for non-speech | -160 | | | none |
| `split_group` without `group_id` and `continue_with_character_id`, short text | -165 | | | none |
| drop empty fields from the scene packet | -46 | | | none (not worth it) |
| all of the above except `additionalProperties` and empty fields | -1,100 | | | none |

The GM after its approval (narration) gave the same tools on all five cases with
the variants combined: `narrate_nothing` for speech, `narrate_group` otherwise.

## Findings

- Schema metadata is a tenth of every request and changes nothing: removed in
  `Tools::tool` for all agents (the rendered tool lists are identical to the
  variant's after the change).
- The GM never called `split_group` when told two characters walk away
  (15 of 15 `narrate_group`, with the tool offered). This is the missing-groups
  limitation in plans/agent-tuning.md, now measured.
- The scene packet is now 5,200 characters (about a third of the GM request), as
  the Bread Thief scenario grew; empty fields are under 150 characters of it.
- The same request with a different random player name gave other choices: the GM
  rejected the take in one capture and approved it in another (10/10 each), and
  the player agent answered "What do I know about Toma?" with `attempt` in one
  and `speak` in the other. Compare variants only on one capture.

## Slimmer GM tools, and what the intervention check taught

Applied: `split_group` without `group_id` and `continue_with_character_id` and with short
text, and `approve_action` offering `include`/`exclude`/`prefix` only for speech. On three
fresh captures (the fixed hut scenario, six samples each) the GM after approval chose
`narrate_group` 24 of 24 with both the old and the new tools, at 2,420 against 2,715
input tokens (-11%). A capture of the larger Bread Thief scenario flipped to
`narrate_characters` with both changes together and to `narrate_group` with either one
reverted; that is a knife edge on one request, not a regression, and it went away on
the fixed scenario.

The intervention-check device scenario failed in about 5 of 26 runs with the GM calling
a narrate tool while the take was still undecided ("action 1 is not yet approved or
rejected"). Reverting either change above made no difference (8 of 12 and 4 of 12 failed).
The cause was in what the GM reads: approving Toma's reaction returned "... Their words
are already conveyed to them, so there is nothing to narrate. End your turn with a
narrate tool." while action 1 still waited for a decision. The result now ends with
"Still to decide: action 1." while presented actions remain, and with the narrate
instruction only when none do. 0 of 14 runs failed afterwards.

## The scene packet

Production-assembled GM requests on the fixed hut scenario (about 3,570 characters of
packet, 2,450 input tokens), 8 samples per case, GM before and after approval:

| Packet | Input tokens | Choices |
|---|---|---|
| compact JSON, as sent | 2,454 (speech) / 2,337 (look) | `approve_action`; then `narrate_nothing` (speech) or `narrate_group` |
| JSON without empty fields | -55 | identical, every sample |
| plain text outline, same facts | -49 | `ask`: answered in plain text instead of a narrate tool, 8 of 8 |

Empty fields (`""`, `null`, `[]`, `{}`) were dropped in the GM packet only: 55 tokens, a
little over 2%, nothing lost. Plain text saved no more and broke the tool call, so the
JSON stays. The rest of the packet is authored scenario text (location, notes, each
character's background, motive and ambition), which is most of its weight; trimming that
changes what the GM knows and needs judging against what the GM uses, not formatting.
The NPC and player scenes carry the same kind of empty fields; they are not measured yet.

## What the GM is sent about each character, and shorter tool text

The GM is sent no background, motive or ambition for any character: it adjudicates what
happens and each character's own agent plays those. That cut the request by 150 tokens
(2,290 to about 2,140) with the same tool choices.

Shortening the GM tool descriptions as well (dropping "Ends your turn." from the narrate
tools, shorter approve, reject, intervention, split and merge text) saved another 50 to
60 tokens, but the intervention-check device scenario then failed 7 of 20 runs (the GM
narrated before deciding every presented action), against 0 of 32 with the original
text. Each group of trimmed tools passed 10 of 10 alone and the combination did not, and a
one-step replay of the GM's decision (40 samples) could not see the failure. Kept: the
original text. Tool text is only worth cutting if the full scenario is run enough times
to show it does not change the GM's order of work.

## The request restated for every action

Each presented action reached the GM as "X takes action N: a `tool` request with arguments
{...}. Judge it from the current scene. Call approve_action with action_id N if it can
happen, or reject_action with action_id N and a concise reason, then end your turn with a
narrate tool." The sentences after the arguments (about 210 characters, 55 tokens) were
the same every time and stay in the GM's history for the whole run. They are now said once
in the GM's role ("Judge each presented action from the scene: approve it if it can happen,
or reject it with a concise reason") and the request is the fact alone. Intervention
scenario: 0 of 20 runs failed (original text 0 of 32, shortened tool text 7 of 20); device
suite passed. The instruction to end the turn with a narrate tool is no longer repeated in
every request; the role and every decision's result still say it.

## Sizes after player replies (characters, `context_size_by_agent_and_part`)

| Request | Tools | System (role + scene) | History | Total |
|---|---|---|---|---|
| player agent | 1,985 | 1,731 | 118 | 3,834 |
| GM, first action | 5,025 | 3,074 | 208 | 8,307 |
| GM, after four actions | 5,025 | 3,074 | 1,794 | 9,893 |
| NPC | 2,378 | 2,136 | 503 to 1,314 | 5,017 to 5,828 |

Tool definitions are 60% of the GM request and 52% of the player agent's, and are constant;
the GM's history adds about 500 characters per action before compaction. The player agent
and NPC contexts have the same shape as the GM's, with no overhead of their own that the
GM's does not also have.

## How the chat template shows the tools

The model does not read our tool JSON. The Qwen3.5 template renders each tool as
`{"type":"function","function":{"name":...,"parameters":{"required":[...],"additionalProperties":false,
"type":"object","properties":{...}}}}` inside `<tools>`. Rendered offline with the real template and
tokenizer (the GM's tools, 10 of them; the player's 4; the NPC's 4):

| Rendering | GM tokens | Player tokens | NPC tokens |
|---|---|---|---|
| Original template (JSON) | 1,315 | 663 | 783 |
| Lean JSON (experimental, `templates/experimental/`): no wrapper, no `type: object`, no `additionalProperties` | 1,146 | 597 | 717 |
| Text signatures with a line per parameter | 912 | 524 | 608 |
| Text signature with the description on the same line | 907 | 527 | 623 |

Behaviour on the real model, player agent, 4B (`CAIRNWORLD_REPEATS`-style repeats):

| Rendering | "I don't mind. Let him take it." answers do-nothing | Speaks the player's words to Mara |
|---|---|---|
| Original | 20 of 20 | 12 of 12 lines |
| Text signatures, line per parameter | 10 of 20 | not measured |
| Text signature, description on the line | 8 of 8 | 0 of 3 runs clean (the agent speaks as Mara) |
| Lean JSON | 8 of 8 | 12 of 12 lines |

GM side, intervention scenario (the GM asks the player it was told to ask, 30 runs per rendering):
original 28 of 30 (2 not asked), text signatures 30 of 30. The whole real-model suite also passed
once with the text signatures (189 s), after the compaction scenario's trigger was lowered from 700
to 500 input tokens because the shorter tool block no longer reached 700. The player-only runs
(question put straight to the player, GM skipped) are the ones that separated the renderings.

The text forms save another 15 to 20% but changed what the 4B player agent does in two
scenarios, so they are not used. One guess is that the plain-text `words` note, whose example is
`Mara says in a low voice: ...`, is imitated more literally than the same text inside JSON; it is
untested. The whole real-model suite passed with the lean template and with the first text form. The lean
template is kept as an optional `--chat-template`, not the default, because it needs a full
copy of Qwen's template (153 lines, 12 changed). The compaction scenario's trigger (700 input
tokens) is not reached under it, so that scenario needs 500 with the lean template.
`approve_speech`, `reject_action`, `split_group`, `merge_groups`, `party_presence` and
`do_nothing` are not called by any passing real-model scenario, so their rendering is checked
only by reading it.

