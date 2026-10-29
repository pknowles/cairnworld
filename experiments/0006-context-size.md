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
| offer no `split_group`, `merge_groups`, `intervention_check` | -845 | | | none |
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
