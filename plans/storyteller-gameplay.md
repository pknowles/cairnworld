# Storyteller support during play

Pending implementation and verification. Gameplay integration follows world
initialization and the applicable character/item/death rules.

## Goal and dependencies

Connect player characters and consequential encounters to the initialized world.
Authority: [Character creation](../user_declarations.md#character-creation),
[GM interaction](../user_declarations.md#gm-interaction),
[GM tools](../user_declarations.md#gm-tools), and
[Storyteller world creation](../design.md#storyteller-world-creation).
Requires [world initialization](storyteller-worlds.md) and the applicable
[creation, death and item rules](playable-rules.md). Reuse their constructors,
authoring/editing tools and world execution; no second initialization model.
Follow [AGENTS.md](../AGENTS.md#post-feature-and-pre-commit-checklist),
[coding standards](../coding_standards.md) and [prompt standards](../prompt_standards.md).

## Ordered work

- Add private/public background offers: three initial proposals, player choice
  or counterproposal, then Storyteller approval using explicit offer/character
  IDs. Only public descriptions reach the player agent. Acceptance records the
  chosen background and relevant private notes. Connect creation rolls and name
  approval/alternatives to the existing creation-state validator.
- After character creation and player readiness, obtain the Storyteller's exact
  entry location and group, either existing or a new group for a lone entrant.
  Validate/apply placement before the destination GM's entrance narration.
  Replace development setup placement here; reconnecting an existing character
  does not repeat it.
- Add encounter guidance and GM SpawnItem/SpawnNPC approval calls through the
  same authoring constructors. Preserve observed facts and independent NPC
  agency. Add UpdateStoryteller with an event summary and referenced object IDs,
  including object significance/notification metadata, so consequential events
  can reach the Storyteller before a campaign ends. Its context uses world
  summary, Storyteller notes and the supplied event, not GM private notes/history.
- Add death display and replacement requests: verify actual death, obtain
  Storyteller approval, create a fresh character/agent under the same member,
  then use ordinary creation and entry. Preserve the dead character/history;
  refusal does not mutate state.

Campaign ending and epilogue remain in [campaign completion](campaign-completion.md),
which reuses UpdateStoryteller rather than implementing it again.

## Verification

Use two players to negotiate distinct backgrounds, including a counterproposal;
inspect captured inputs for private plot leakage. Exercise existing-group and
lone entry, reconnect, approved/refused spawns and approved/refused replacement.
Verify an event update reaches the Storyteller with its real causal source and
without GM private context. Reuse scripted production paths for authorization
and state effects and a representative real conversation for model-facing work.
Review LLM cases with the user first; observe the testing budget, update the
implementation reference and complete AGENTS.md self-review before committing.

## Ideas to explore, not implementation requirements

The declaration's Dynamic Storyteller section remains future work. Its newer
suggestion of using the Questioner during play to notice player preferences,
staleness or a lack of competing problems belongs with that exploration. This
plan does not prescribe a periodic review loop, automatic new-problem generator
or unsolicited NPC thought injection. Agree the timing/scope before implementing
that gameplay expansion.
