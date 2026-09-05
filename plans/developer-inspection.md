# Developer inspection

## Goal

Implement the declared one-way per-world Developer Mode so a playtest's
player-facing chat, game objects, agent histories, sequences, and exact model
inferences can be inspected without a second debugging path.

## Scope

- Let only a world owner enable Developer Mode after the declared warning; make
  the durable enabled state visible to every world member and never disable it.
- Add the two-column game view and navigation among player entries, sequences,
  actions/dice rolls, agent histories including summaries, game objects, and
  inference input/output.
- Render directly from the recording and game-state rows. The feature must not
  duplicate chats, reconstruct approximations in the browser, or expose this
  material outside a Developer-Mode world.

## Steps

1. Confirm the design's debug-spine data is sufficient for every declared view;
   extend `design.md` first if a required relationship is absent, and obtain
   user sign-off before changing design to match code.
2. Persist the owner-authorized irreversible world setting and enforce it at
   every inspection route.
3. Build the server-rendered inspection views and their links from the existing
   sequences, actions, message history, summaries, and inference recipes.
4. Add the world-detail enablement control and update
   `implementation_reference.md`.

## Verification

- A non-owner cannot enable Developer Mode; after an owner enables it, every
  member sees it after reload and it cannot be disabled.
- A player entry, action, and summary each navigate to the correct recorded
  sequence and inference; the displayed model input reassembles from the
  recorded recipe.
- A world that has not enabled Developer Mode cannot retrieve another agent's
  chat, game objects, sequence, or inference through page URLs or requests.

Follow the implementation loop and pre-commit checklist in `AGENTS.md`, plus
`coding_standards.md` and `prompt_standards.md`.
