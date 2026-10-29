# Agent tuning: known failures of agent behaviour

Status: nothing started. These are measured failures of the current agents that
need prompt/context tuning once the features they depend on exist. Follow the
[AGENTS.md checklist](../AGENTS.md#post-feature-and-pre-commit-checklist) and
[prompt standards](../prompt_standards.md): read the full model input of a failing
run, find the cause, change context, replay on the real model.

## HIGH SEVERITY: the GM cannot see other groups, so it cannot split or merge correctly

`GmScene` ([src/store.rs](../src/store.rs)) holds the location, the interacting group's
own description and notes, items, and that group's characters. Four things the GM needs
are missing, so the `split_group` and `merge_groups` tools exist but a real GM cannot use
them reliably:

- **Its own group id.** `split_group` needs `group_id`; nothing in the packet gives it.
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
