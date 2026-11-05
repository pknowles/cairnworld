# Handoff: Storyteller world generation (delete this file when done)

Worktree `c:\src\cairnworld-storyteller`, branch `pknowles/storyteller-questioner`. Read AGENTS.md,
coding_standards.md, prompt_standards.md first. The user is a busy solo developer: short plain answers,
ask decisions as plain questions, no internal jargon, never claim "done" until a real-model run shows it.

## Goal and finish line

A seed prompt becomes a ready, reviewed "small world" (phases 1 and 2 of
[storyteller-worlds.md](storyteller-worlds.md)): Questioner and Storyteller develop a summary, the
Storyteller builds objects with tools, the Questioner reviews them, `core_objects_created` makes the
world ready. Phases 3 and 4 (extended summary, detail objects) come later, as does a "smaller world"
checkbox. The user plans to merge `pknowles/npcs` (another agent in `c:\src\cairnworld`, with
context-size work) and re-branch once small worlds work.

**Finish line:** `a_world_is_generated_from_a_seed_by_the_real_model` ends with `ready=true` **at the
small-card context (4096 tokens, 3070 GPU, ~5 GB)** and the website path (create game, watch in Developer
Mode) works with the real server. Not reached yet.

## User decisions (2026-11-05, also in user_exerpts.md)

- Tools identify objects by **ID** (not names). Create returns the assigned ID.
- **Must fit a small card**: 4096-token context. "Same tools, make it fit." Do not hide or drop tools.
- Use **a generic tool with an `object` enum** instead of 17 per-kind tools (the GM tools did similar).
- The summary request probably needs no tools, certainly not full descriptions; idea to try: list tool
  names without descriptions so the summarizer knows what the agent controls.
- The user's own edits to user_declarations.md / prompt_standards.md / AGENTS.md get committed with the
  feature they relate to (new AGENTS.md rule).

## What is committed and verified

`git log`: 840fcb2, 1a18e7b, dae0484, 5f5dc33, 46ee47d, 39f81c8. At 16384 context (product default) the
whole flow worked on the real model once (542 s, 22 reviewed objects). Verified pieces: Questioner decides
the summary is complete from what it covers (replayed on recorded requests); duplicate connections are
refused (they could never be marked reviewed); the summary request keeps the role text but **not the
tools** (measured: with the Questioner's tools the model answers by calling one); the review caught gaps
in a partial world once. 256 unit tests, `scripts/check-quality.sh`, and all 9 browser checks
(`npm run test:hydration`) passed at 39f81c8.

## The unfinished work: IDs + generic tools (uncommitted before the temporary commit)

The working tree is mid-refactor and **does not compile**. A temporary commit "TEMPORARY WIP (does not
build)" holds it. Done in that commit:

- `src/store/authoring.rs`: new `Fields` (all object fields as Options), `ObjectKind::fields()` (field
  list and required flag per kind), `Fields::check`, `DeleteOptions`, `Changed { id, counts }`,
  ID-based `create_object(world, kind, &Fields)`, `ensure_exists` (replaces `named_id`). Inserts now
  return the new id.
- `src/store/editing.rs`: `Change` enum, ID-based `update_object` and `delete_object`, `ensure_name_free`.
- `src/store/review.rs`: `ReviewEntry.id`, `read_object` / `set_reviewed` take ids.
- `src/store.rs`: re-exports changed.
- `src/game/authoring.rs`: three Storyteller tools `create_object` / `update_object` / `delete_object`
  (flattened `Fields`, unknown arguments refused), Questioner `read_object` / `mark_object_reviewed`
  by `object` + `id`.

**Still to do to get it compiling and tested** (in this order):

1. In `store/authoring.rs`: add `kind: &'static str` to `Changed` and set it in create/update/delete
   (`kind.as_str()`); add `Fields::names()` and `DeleteOptions::names()` (serialize a default value and
   take the keys); give `DeleteOptions` `Deserialize` + `JsonSchema` derives with `#[serde(default)]`
   on its bool fields (it is flattened into the delete tool's arguments); remove now-unused imports
   (`Location`, `Group`, `Npc`... are still used) and the old `NewObject`/`NewItem` leftovers.
2. **Rewrite the test modules** in `store/authoring.rs`, `store/editing.rs`, `store/review.rs`: they call
   the old by-name API. Fixtures are in `authoring.rs` `fixtures` (`building_world`, `location`,
   `group_at`, `npc_in`, `set_stage`); add small helpers that create by `Fields` and return the id.
   Tests to keep (same intent): build a world object by object; a rejected object changes nothing; only
   while stage is `build`; NPC-held item belongs to the NPC and un-reviews it; duplicate connection
   refused; core objects not whole until connected/complete/reviewed; update replaces only named fields
   and un-reviews; delete only flags and dependents decide; deleted NPC gone from scheduling.
3. `game/generation.rs`: object lists show ids: Storyteller `objects_state` lines and the Questioner
   `review_list` lines become `- {kind} {id} {name}` (and `[x]` flag for the Questioner).
4. `prompts.toml`: replace the 17 `[storyteller.tools.*]` tables with `create_object`, `update_object`,
   `delete_object` (arguments are `object`, `id`, the `Fields` names, and the `DeleteOptions` names; the
   `describe()` helper requires a text for every argument and refuses extras). Change the Questioner's
   `read_object` / `mark_object_reviewed` arguments from `kind`/`name` to `object`/`id`. Put the per-kind
   field list once in `create_object`'s description (draft: "Fields by object: location: name, kind,
   description. connection: from_id, to_id, travel_time, description. group: name, location_id,
   description. npc: name, group_id, description, background, motive, ambition, sheet. item_type: name,
   description, stats. item: name, item_type_id, description, and location_id or held_by_id."). Keep
   every text as short as possible (prompt_standards.md new item: ask what breaks if a sentence is
   removed). `build_request` / `fix_request` mention names ("Give every field a value"): reread them.
5. `game.rs` scripted test `a_world_is_generated_from_a_seed_into_reviewed_objects_and_then_opens_play`
   calls tools by name; switch to ids (ids are sequential per table, so Belfry=1, Wardens=1, Ysolde=1,
   Corvin=2). The real-model tests need no change in code but their prompts do.
6. Regenerate SQL metadata and run everything. Commands (bash): 
   `export DATABASE_URL=sqlite://target/sqlx-schema.sqlite SQLX_OFFLINE=false; rm -f target/sqlx-schema.sqlite;
   cargo sqlx database setup --source migrations; cargo sqlx prepare -- --all-targets` then `cargo fmt`,
   `cargo test`, `bash scripts/check-quality.sh`, `npm run test:hydration` (about 3 minutes, CPU only).
7. **Measure tool size**: serialize the three Storyteller definitions (the 17 old ones were ~8,000
   characters, about 2,000 tokens; the whole fixed prefix was ~2,900 tokens). Target well under 1,000
   tokens for all three. Then run the seed-to-ready real-model test **at the 4096 device-test settings**
   (`DeviceModel::load()`, not `load_with_default_limits()` in `game.rs` tests) and fix what no longer
   fits (compaction of the Storyteller's chat must still work).

## Findings you should know (evidence in the commit messages and plans/llm-context-size.md)

- **Fix loop gets stuck**: the Storyteller ends a build turn with a free-text note; it sometimes claims
  objects exist that it never created (after a rejected call it does not retry). Its compaction summary
  then records the claim as fact ("NPCs: Captain Brannoch") and it answers "already created" forever
  while the Questioner (who only sees objects) asks the same question until the 64-inference limit.
  Facts already supplied: the Storyteller sees the list of existing objects; the Questioner is told the
  summary's `[[marked]]` names that have no object. Not enough in one run. Idea to test: end the build
  turn with a terminal tool call instead of prose so the chat holds actions, not claims. Test by
  replaying recorded requests (see the replay approach below) before changing prompts.
- Moving the Questioner's summary next to its decision did **not** help (experiment); don't redo it.
- `max_inferences_per_chat = 8`: dependent creations made one at a time (NPC before its group) burn it.
- The Questioner's meta-question answer runs to the 1024-token cap despite "briefly".
- `prompts.toml` is checked out with CRLF, so the model receives `\r\n`; unresolved.
- NPC `sheet` is not validated (the model wrote `"armor": "None"`); only `scenarios/bread_thief.json` has
  a numeric `armor`, and no code reads it yet. Cairn rules were not checked.
- Completing the core phase puts any waiting Adventurer in the first group: an interim choice of mine
  (`place_waiting_adventurers`), until entry placement exists in storyteller-gameplay.md.
- Names vs IDs conflicts with nothing in the declarations; design.md still says IDs for tools (now true).
  design.md was not edited by me; update only with the user's sign-off.

## How to run things

- Unit tests: `cargo test` (about 30 s). Real model (GPU, minutes; the user is often on battery, ask or
  keep runs short): load secrets and cache, then run ignored tests:
  `set -a; . /c/src/cairnworld/secrets/dev.env; set +a; export HF_HOME="$PWD/models/huggingface";
  cargo test <name> -- --ignored --nocapture > <scratchpad file> 2>&1` (never print secrets).
- Real-model tests in `src/game.rs`: `a_world_is_generated_from_a_seed_by_the_real_model` (finish line),
  `the_real_model_builds_and_reviews_core_objects_from_a_summary`,
  `the_real_model_questioner_catches_what_the_storyteller_left_out`,
  `a_summary_of_a_stuck_tool_loop_keeps_the_task_in_progress`. They print `WORLD`, `OBJECT`, `CHAT` lines;
  the `Recording` backend keeps every request the model received.
- **Replay approach that worked**: record requests to JSON from a run, then replay one request with edited
  text (one inference each) to compare wordings before touching a prompt. Remove such experiment code
  afterwards (none remains).
- Do not touch other agents' processes (a cairnworld test from `c:\src\cairnworld` may be running). Check
  your own by executable path before killing. Stop your background runs when done.
- Shell gotchas on this machine: bash heredocs with `\\n` or triple quotes get mangled and silently
  skip edits; write Python scripts with the Write tool, run them, and **confirm the edit applied** before a
  GPU run. The Edit/Write tools require a fresh Read after the file changed. PowerShell tool may be absent;
  use `powershell.exe -NoProfile -Command ...` from bash. Never use `git add -A` or `git checkout` on files.
- Never put anything in /tmp; use the session scratchpad.

## Questions to put to the user once it works (ask plainly, one by one)

- Placing a waiting player in the "first group" when the core phase ends: acceptable until entry placement?
- Merge `pknowles/npcs` now and re-branch?
