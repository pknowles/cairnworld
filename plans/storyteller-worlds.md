# Storyteller worlds and characters implementation

## Deliverable and trace

World creation produces a seeded, named playable setting; characters receive
negotiated backgrounds and approved replacements. Authority:
[Game setting](../user_declarations.md#game-setting-and-story-narrative),
[initialization tools](../user_declarations.md#storyteller-initialization-output-and-tools),
[Character creation](../user_declarations.md#character-creation),
[GM interaction](../user_declarations.md#gm-interaction), and
[World detail](../user_declarations.md#world-detail-page).
Implement [Storyteller world creation](../design.md#storyteller-world-creation)
after the rules/creation/group constructors, not a second initialization model.

## Ordered changes

1. Add world.phase and world_build with the exact phase/round/step/cursor fields
   in design.md. Create seed/owner/Storyteller/initialization job transactionally.
   Add optional seed input to the existing create-world form and a collapsed
   seed display plus initialization progress/error on WorldDetail. Keep scenario
   import as its own development operation; only validated worlds enter playing.
2. Add src/storyteller.rs with the three-phase question/answer/summary loop.
   Record Questioner as an invocation role with schema {questions,satisfied};
   pass only seed/phase/meta-questions/latest summary. Run Storyteller answers
   separately, one per question, followed by a summary; next round starts from
   seed plus that summary. Persist each completed step and inference reference.
   Set max_rounds_per_phase=3; honor ordinary inference budgets and make failure
   visible rather than looping indefinitely. Resume an interrupted step from
   its last committed output, not by regenerating completed answers.
3. Expose typed create/update tools for locations, Connections, groups, NPCs,
   item types/items and notes, backed by the existing constructors. Execute in
   that dependency order. Give each location/NPC its seed and a focused enrichment
   loop using the final world summary. Validate endpoint/world references, group
   membership, item ownership and reachability from the starting location before
   switching to playing. All draft inspection remains available in Developer Mode.
4. Add background_offer with private/public content references. Implement three
   initial offers, player counterproposal and Storyteller approval tools using
   explicit offer/character IDs. Player agents receive only public descriptions;
   acceptance writes the approved background and private object notes. Hook
   creation rolls into Storyteller approval/modifiers, and choose_name into
   approve-or-suggest-alternatives. Reuse the existing creation-state validator.
   Once creation is complete and the player confirms readiness, obtain the
   Storyteller's exact entry location and group: an existing group or a new group
   for a lone entrant. Validate world/location/group references and apply the
   placement before invoking that group's GM entrance narration. Replace the
   development setup's arbitrary existing-group placement at this point; browser
   reconnection preserves an existing character's group.
5. Add encounter-guidance, SpawnItem and SpawnNPC approval calls; use the same
   validated constructors, recording and current-observation notes. Add death
   icons and request_replacement through the player agent: check actual death,
   obtain Storyteller approval, create a fresh Adventurer/agent under the same
   member, then use ordinary group entry/creation. Refusal leaves state intact.

## Verification and completion

Generate two worlds from different seeds, plus repeat one seed, through the
browser/real model; inspect the three phases and focused object creation. Verify
that references are valid, the setting offers meaningful alternative approaches,
and the Questioner sees summaries rather than the complete prior answer history.
Restart after an answer and after a summary without duplicating completed steps.
Two players negotiate distinct backgrounds, including a counterproposal; private
plot details never enter their public offer context. Test failed creation,
refused spawn/replacement and successful replacement with independent histories.
Exercise entry into an existing group and entry alone at another location;
verify the destination GM narrates only after placement and reconnecting does
not repeat Storyteller placement; ordinary presence narration still applies.

Use imported scenarios for deterministic constructor tests and actual generated
worlds for model quality. Update implementation_reference.md. Re-read
coding_standards.md and prompt_standards.md, obtain the required LLM-test review
and complete the [AGENTS.md checklist](../AGENTS.md#post-feature-and-pre-commit-checklist).
