# Agent tuning: known failures of agent behaviour

Status: nothing started. These are measured failures of the current agents that
need prompt/context tuning once the features they depend on exist. Follow the
[AGENTS.md checklist](../AGENTS.md#post-feature-and-pre-commit-checklist) and
[prompt standards](../prompt_standards.md): read the full model input of a failing
run, find the cause, change context, replay on the real model.

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

To establish first: the GM's scene packet has no list of other groups at its location
and no group id, so a real GM cannot name the ids `merge_groups` and `split_group`
require. A real-model check of valid split and merge calls (partitions complete, ids
from the packet, resulting groups read from the stored state) belongs with this.
