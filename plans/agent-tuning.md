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

Done: the packet lists the other groups; the tools are `split_group` (a new group),
`move_to_group` (into an existing group; every character of the group moving is a merge) with the GM
naming the groups, made unique when they clash; the tools act on the acting character's current
group; mistral.rs's Qwen parser needs an optional integer's schema type to be plain (`"integer"`,
not `["integer","null"]`), which the schema normaliser now does. Do not add string-to-integer
coercion in the game for it.

Still open (user, 2026-11-05: the GM must regroup characters; the ignored test is
`the_gm_regroups_characters_as_they_move`, `CAIRNWORLD_REPEATS=N` for seeds). Results are in
experiments/0009-group-tools.md. Joining another group now works with `merge_group` removed; the GM
often makes no group call at all when a character walks off, or back to the others.
The batched, delayed split belongs to plans/unified-game-time.md.

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

Cause found (experiments/0010-ask-precedent.md): the GM repeats whatever it decided on the
previous submission, whatever the new one says. Its earlier asks stay in its history, so in a
run of submissions the answer to the one before is the strongest cue. Asks put in the order
non-questions first were all rejected correctly and then the real questions after them were
rejected too (8 of 8 wrong); in the original order 2 of 24 were answered wrongly. Moving the
rule next to each submission did not help. A fix that does not depend on the model resisting
its own precedent keeps earlier ask decisions out of the history an ask is judged against
(for instance judging it on a branch forked from the group's head and dropping it afterwards).
That changes where the GM's history is kept, so it is the user's call.
Runnable check: `cargo test the_gm_answers_a_question_and_rejects_any_other_ask -- --ignored`
(`CAIRNWORLD_REPEATS=N` for seeds).

## NPC chatter with three or more NPCs, and NPC reactions to the entrance narration

Measured only for two NPCs in one scene (experiments/0005-npc-chatter.md): with the
"operate a character" role and `npc.before_deciding` on every decision, a discussion
ends after about one line per NPC. Not yet measured: three or more NPCs in one group,
and what the NPCs do when the GM narrates a player's entrance (the declared flow offers
them a response opportunity with nobody to talk to). Add no filter for it; run the real
model several times, read the recorded contexts, and record the counts in the experiment
file.

## The GM leaks one side of a split to the other after a merge (fixed)

Declaration: a character may only learn what they perceived
([Observed Events](../user_declarations.md#observed-events)); after a merge the GM
holds both groups' histories but must not reveal one side's secrets to a character who was on
the other. Measured 2026-10-28 and again 2026-11-05: 2 of 4 questions leaked (a character was
told what had been said where they were not), with the group labels naming groups but not who
was in them, and the role saying only "answer it from the scene".

Fixed by context, not by asking the model to keep a secret: each group's label in a merged
history says who was in it (kept when the group retires, migration 0018), and a GM answering
an ask is shown only the groups the asker was in (`live_segments_for`, the `perceiver` of a
turn). The real-model check (`the_gm_keeps_each_side_private_after_a_merge`, in the suite as
"merge privacy") passes, and a scripted test checks the cut. Narration to several characters
still relies on the GM's recipients; only the answers to asks are cut this way.

