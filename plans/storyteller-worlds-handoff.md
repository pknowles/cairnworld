# Handoff: Storyteller world generation (delete this file when done)

Worktree `c:\src\cairnworld-storyteller`, branch `pknowles/storyteller-questioner`. Read AGENTS.md,
coding_standards.md and prompt_standards.md first. The user wants short plain answers, decisions asked as
plain questions, no invented jargon, and no claim of "done" until a real-model run shows it.

## State

Phases 1 and 2 are built: seed -> summary -> Storyteller builds objects with three tools (`create_object`,
`update_object`, `delete_object`, by ID) -> Questioner reviews (`read_object`, `mark_object_reviewed`,
`ask_questions`, `core_objects_created`) -> world ready. See implementation_reference.md ("World generation").
Phases 3 and 4 and the "smaller world" checkbox are deferred by the user's decision.

All unit tests (255), `scripts/check-quality.sh` and `npm run test:hydration` pass.

## The finish line and where it stands

`a_world_is_generated_from_a_seed_by_the_real_model` (ignored, GPU) at the 4096-token small-card setting
(`DeviceModel::load()`): the user's target is a 3070 with about 5 GB. Results so far after the last fixes
(compaction halving restored per the declarations): 1 of 3 runs reached a ready world; 2 of 3 failed in the
Storyteller's build turn at `max_inferences_per_chat = 8` after creating 12 and 15 objects (the prompt asks
for about 8). Earlier runs failed on things since fixed (stale chat summary, oversized summaries, the
declared halving not applied on a repeated failure, duplicate connections, unknown IDs).

**Open decision for the user (ask plainly):** a world needs more model calls in one Storyteller turn than
the per-turn limit of 8 allows, mostly because an object that refers to another needs that object's ID
first. Options: raise the per-turn limit for world building; or let a build turn that hits the limit simply
continue into the review (the Questioner asks for what is missing); or tell the Storyteller a smaller world.

## Other open problems (evidence in git history and plans/llm-context-size.md)

- A review that sends the Storyteller back can get stuck when the Storyteller's prose claims an object
  exists that it never created and its chat summary records the claim. Idea: end the build turn with a
  tool call instead of a prose note. Test by replaying recorded requests before changing prompts.
- The Questioner's meta-question answer runs to the 1024-token cap despite "briefly".
- `prompts.toml` is checked out with CRLF line endings, so the model receives `
`.
- NPC `sheet` contents are not validated.
- Completing the core phase puts a waiting Adventurer in the world's first group (interim, until entry
  placement exists).
- Not yet run through the website with the real server.

## How to run things

- `cargo test` (about 30 s); `bash scripts/check-quality.sh`; `npm run test:hydration` (about 3 min, CPU).
- Real model: `set -a; . /c/src/cairnworld/secrets/dev.env; set +a; export HF_HOME="$PWD/models/huggingface";
  cargo test <name> -- --ignored --nocapture > <scratchpad file> 2>&1` (never print secrets). The user is
  often on battery: keep GPU runs few and say why.
- SQL metadata after changing a query: `export DATABASE_URL=sqlite://target/sqlx-schema.sqlite
  SQLX_OFFLINE=false; rm -f target/sqlx-schema.sqlite; cargo sqlx database setup --source migrations;
  cargo sqlx prepare -- --all-targets`.
- Replay approach that works: record the requests from a run, then replay one with edited text, one
  inference each, before editing a prompt. Remove such experiment code afterwards.
- Do not touch other agents' processes; check by executable path before killing. Never `git add -A` or
  `git checkout` files. Nothing in /tmp. Shell: heredocs mangle backslashes, so write scripts with the
  Write tool and confirm an edit applied before a GPU run.
