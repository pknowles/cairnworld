# MCP development interface

## Goal

Let coding agents run real playtests and inspect the recorded debug spine
through the project's existing game and recording boundaries.

## Scope

- Expose scenario import/export, chat/replay, and the required recorded
  sequence, inference, reconstruction, message, and game-object queries.
- Reuse the same operations and authorization boundaries as the CLI, web
  Developer Mode, store, and game services; do not create a parallel mutable
  administration model.
- Include the declared historical-inference regeneration operation if the
  existing replay command does not satisfy it after Developer inspection makes
  its inputs selectable.

## Steps

1. Trace each MCP operation to a declared developer need and existing project
   operation; extend `design.md` first for any missing boundary.
2. Implement the RMCP surface as a thin invocation of those operations,
   including complete error context and recorded effects.
3. Exercise it with a coding-agent playtest and update
   `implementation_reference.md`.

## Verification

- A coding agent can initialize Bread Thief, execute a real playtest, retrieve
  its sequence and exact inference reconstruction, and replay a selected
  inference without a temporary script.
- MCP access cannot bypass the project's world, membership, or Developer Mode
  visibility boundaries.

Follow the implementation loop and pre-commit checklist in `AGENTS.md`, plus
`coding_standards.md` and `prompt_standards.md`.
