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
