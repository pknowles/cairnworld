# Agent tuning: known failures of agent behaviour

Status: in progress. These are measured failures of the current agents that
need prompt/context tuning once the features they depend on exist. Follow the
[AGENTS.md checklist](../AGENTS.md#post-feature-and-pre-commit-checklist) and
[prompt standards](../prompt_standards.md): read the full model input of a failing
run, find the cause, change context, replay on the real model.

## HIGH SEVERITY: the GM cannot see other groups, so it cannot merge correctly

Decided with the user (2026-11-05; user_exerpts.md): the GM's `GmScene`
([src/store.rs](../src/store.rs)) gains the other active groups at its location, each as a
group id and its characters (id and name) only. No group description or position: groups have
none. Merge when characters of different groups could perceive each other; split when
characters move away. `split_group` already acts on the GM's own group (it takes no group id).

Done: the packet lists the other groups; the tools are `split_group(character_ids)`,
`move_to_group(character_ids, group_id)` and `merge_group(group_id)`; groups are named for their
members; the tools act on the acting character's current group; mistral.rs's Qwen parser needs
an optional integer's schema type to be plain (`"integer"`, not `["integer","null"]`), which the
schema normaliser now does. Do not add string-to-integer coercion in the game for it.

Still open (user, 2026-11-05: the GM must regroup characters; the ignored test is
`the_gm_regroups_characters_as_they_move`, `CAIRNWORLD_REPEATS=N` for seeds). Measured over 2
seeds with realistic wording (experiments/0009-group-tools.md): a character joining another
group is merged in with the whole group instead of moved (`merge_group` 2 of 2, no
`move_to_group`); walking off alone makes no group call (0 of 2, 1 of 3 in an earlier wording);
walking back to the others works (2 of 2, as a merge). The GM does call `move_to_group` with
correct arguments when its description is the only one that fits, but often after a wrong
`split_group` of both players. The batched, delayed split belongs to plans/unified-game-time.md.

## Whole-suite test time

Trims made: character creation ends at the GM opening (the GM-take coverage is in the GM and
intervention scenarios), committed-take cases share the player-routing scenario, the compaction
conversation has one plain turn, and tools that other scenarios call are left out of the
tool-interface scenario unless `CAIRNWORLD_ALL_TOOLS` is set. Not yet timed on an idle GPU:
the untrimmed suite was about 270 seconds of device scenarios plus 25 seconds of scripted tests, and a
run on a busy GPU took 467 seconds, so judge it from `nvidia-smi` showing the GPU idle.

## The GM names the wrong character in an intervention check

Players can be asked (plans/archive/player-replies.md), but the GM must name them. The GM's
input in the test is valid: the scene lists every character with an id (Mara 1, Toma 2, the
acting player 3, the player to ask 4) and the note names the player to ask. Over 60 GM-only
samples (30 per case; the game in that test used greedy sampling, not production sampling, until
2026-11-05, and varied only by random character names) the GM named the right id 54 times, Toma 3 and
the acting character 3 (the tool refuses that, so nobody is asked). The first note said the
other player was "beside the sack" while the scene text puts Toma beside the sack; that
contradiction caused part of it (51 right, 8 Toma, 1 actor), and the note now says the player
stands next to the acting one. The remaining wrong picks have no known cause. One difference
worth testing: a player's name carries a random handle ("Adventurer35") unrelated to its id
(4). Runnable: `CAIRNWORLD_REPEATS=20 cargo test a_player_answers_the_gms_intervention_question
-- --ignored --nocapture`.

## The GM approves an ask that is not a question

Direction (user, 2026-11-03): fix the characters misusing `ask` first; the GM correcting
it is a bonus, and no page of rules is worth adding to the GM. Requirement: `ask` is an
explicit question to the GM, and the GM rejects, with a reason, an ask that is not a
question or that it cannot or may not answer.

Now: an ask is judged in a request of its own that offers only `answer` and `reject_action`
(`gm.ask_role`). Measured on the real model at production sampling, 20 samples each: a real
question answered 20/20, an unanswerable question (the story's end) rejected 20/20, an action
rejected 20/20, but speech sent as an ask answered 17/20 and a request to another character
20/20. Before this request existed the GM approved nearly everything sent as an ask.

Tried without gain (experiment 0008): GM role and tool wording, a separate tool, "is it a
question" framing, a neutral instead of an asserting request line (an action then answered
13/20), and the criterion on the tool descriptions (the unanswerable question then answered
20/20). Not tried: `answer` requiring the question it answers to be quoted.
Runnable check: `cargo test the_gm_answers_a_question_and_rejects_any_other_ask -- --ignored`.

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

The check now reads the delivered answer instead of looking only for
`narrate_*` calls: questions use the private `answer` tool. Its split/merge setup
also removes obsolete player follow-up responses (actions end the player's turn)
and shuts down each scripted game before opening the next. Re-measure before
treating the older failures below as current behaviour.

Measured 2026-10-28, 6 runs, none passed. Causes seen:
- Toma asked what Mara said outside and the GM narrated "heron" (3 of 6 runs).
- The GM called `narrate_group` with a `character_ids` field, which it does not have
  (that belongs to `narrate_characters`), so the call was rejected as invalid
  (3 of 6 runs). It looks as if the GM wanted to address the narration to one character.
- One empty narration for the player's control question.

The high-severity group-visibility limitation above should be fixed first: this check's
setup (the GM merging by guessed ids) depends on it.
