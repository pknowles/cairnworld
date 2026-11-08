# Agent tuning: known failures of agent behaviour

Status: nothing started. These are measured failures of the current agents that
need prompt/context tuning once the features they depend on exist. Follow the
[AGENTS.md checklist](../AGENTS.md#post-feature-and-pre-commit-checklist) and
[prompt standards](../prompt_standards.md): read the full model input of a failing
run, find the cause, change context, replay on the real model.

## HIGH SEVERITY: the GM cannot see other groups, so it cannot split or merge correctly

`GmScene` ([src/store.rs](../src/store.rs)) holds the location, the interacting group's
own notes, items, and that group's characters. Things the GM needs
are missing, so the `split_group` and `merge_groups` tools exist but a real GM cannot use
them reliably:

- **Its own group id** is not needed by the GM at all: it operates on one group, so
  `split_group` should drop its `group_id` argument and act on the GM's own group
  (today the tool asks the model for it, which is a design flaw, not a context gap).
- **The other groups at its location.** `merge_groups` needs `group_ids`; the GM cannot
  see that another group, its members, or its ids exist.
- **When to merge.** A merge is due when any two members of different groups could cross
  paths. The GM sees neither the other groups nor where their members are headed, so it
  has no basis to notice this.
- **When to split.** A split is due when members of a group part ways. The GM is never
  shown the characters' intentions or movement as a signal to partition, so it splits
  or fails to split by chance.

Fix needs context design first (what the packet shows about sibling groups: ids, members,
descriptions, what each can perceive of the other; a prompt telling the GM when to
merge and split), then real-model checks of valid calls: partitions complete, ids taken
from the packet, resulting groups read from stored membership, for both a split when
characters go separate ways and a merge when they cross paths. Measure repeat rate;
keep in `device_suite` only if reliable.

## The player agent narrates and rewrites what the player says

Played game: after its `speak` call the agent wrote Toma's and Mara's reactions, spoke
words the player never typed, and kept doing it when told to stop. Runnable check (not in
`device_suite`): `cargo test the_player_agent_says_what_the_player_says_and_stops -- --ignored`;
all four lines fail every run. Measurements and the rewrites that did not help are in
[experiment 0008](../experiments/0008-attempt-misuse.md). Candidate: end the agent's turn
after an action tool, since the game shows the player what happens.

## The GM approves an ask that is not a question

`ask` is a question for the GM. The GM approves everything sent as an ask, including
speech and actions, and then narrates it. Runnable check:
`cargo test the_gm_answers_a_question_and_rejects_any_other_ask -- --ignored`. Wording,
presentation and a separate tool did not change the GM's decision (experiment 0008).
Characters now rarely misuse `ask` because `speak` is offered first.

## NPC chatter with three or more NPCs, and NPC reactions to the entrance narration

Measured only for two NPCs in one scene (experiments/0005-npc-chatter.md): with the
"operate a character" role and `npc.before_deciding` on every decision, a discussion
ends after about one line per NPC. Not yet measured: three or more NPCs in one group,
and what the NPCs do when the GM narrates a player's entrance (the declared flow offers
them a response opportunity with nobody to talk to). Add no filter for it; run the real
model several times, read the recorded contexts, and record the counts in the experiment
file.

## The GM leaks one side of a split to the other after a merge

Declaration: a character may only learn what they perceived
([Observed Events](../user_declarations.md#observed-events)); after a merge the GM
holds both groups' histories ([user_declarations.md](../user_declarations.md), merged
histories) but must not reveal one side's secrets to a character who was on the other.

Runnable check (not in `device_suite`): `cargo test the_gm_keeps_each_side_private_after_a_merge -- --ignored`
(`real_model_gm_does_not_leak_one_side_to_the_other_after_a_merge`). The player was
outside with Mara and heard "the password is heron"; Toma, inside, muttered "I hid the
lantern". Four questions to the real GM on the merged history: the player asks about
Mara's whisper (must mention heron) and about Toma's words (must not mention lantern);
Toma asks about Mara's words (must not mention heron) and about his own (must mention
lantern).

Measured 2026-10-28, 6 runs, none passed. Causes seen:
- Toma asked what Mara said outside and the GM narrated "heron" (3 of 6 runs).
- The GM called `narrate_group` with a `character_ids` field, which it does not have
  (that belongs to `narrate_characters`), so the call was rejected as invalid
  (3 of 6 runs). It looks as if the GM wanted to address the narration to one character.
- One empty narration for the player's control question.

The high-severity group-visibility limitation above should be fixed first: this check's
setup (the GM merging by guessed ids) depends on it.
