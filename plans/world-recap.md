# Returning-character recap implementation

## Outcome and trace

A private recap appears on WorldDetail after absence, without becoming an agent
chat message. Authority: [World detail page](../user_declarations.md#world-detail-page).
Design: [Recap storage and execution](../design.md#recap-storage-and-execution).
This plan uses the concrete per-character ownership proposal, pending the user's
answer; it does not silently redefine the declaration's per-player wording.
Presence supplies the durable absence timestamp/generation and timer boundary.

## Ordered changes

1. Add pending_recap and character_recap with the exact design fields. The
   last-disconnect transaction captures the character's history head/cut, static
   recipe and generation with due_at=absent_since+60 seconds. Return before the
   deadline deletes only that generation's pending job.
2. Extend existing deferred admission with the Compaction/Recap enum. A due
   presence event starts its recap at deferred priority after validating absence
   generation. Resume loads both typed job tables at startup. Keep one in-flight
   job per character/generation; retain errors durably and surface them.
3. Add src/recap.rs assembling the captured history/public character context plus
   a closing recap request through complete_recipe. Store its inference reference
   as character_recap and remove that exact obligation in one transaction. An
   older result cannot delete a newer job. Never append recap output to message.
4. WorldDetail resolves the viewer's character recaps under their character rows,
   displaying pending/failed/current status and the last successful text. Normal
   members see only their own; enabled Developer Mode uses ordinary authorized
   inference inspection. No separate debug transcript or stored recap text copy.

## Verification and completion

Return at 59 seconds cancels the job; absence through 60 seconds creates one
runnable obligation. Restart before/after deadline or inference completion does
not lose/duplicate it. A second absence cannot be cleared by the first result.
A real character-agent recap excludes hidden NPC/Storyteller facts, appears in
WorldDetail and leaves the agent's message history unchanged. It summarizes
known history, not unseen events from a missed session.

Update implementation_reference.md. Re-read coding_standards.md and
prompt_standards.md, obtain required LLM-test review and complete the
[AGENTS.md checklist](../AGENTS.md#post-feature-and-pre-commit-checklist).
