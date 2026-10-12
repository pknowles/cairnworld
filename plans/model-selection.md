# Recorded model evaluation

## Outcome and trace

Select the default for the implemented Bread Thief tool surface using recorded
play, as required by [Tech stack](../user_declarations.md#tech-stack),
[proof of concept](../user_declarations.md#an-initial-proof-of-concept) and
[Debugging and Telemetry](../user_declarations.md#debugging-and-telemetry).
This is an evaluation recipe, not an instruction to design another harness.
Use the completed MCP/browser/inspection paths and configured models.

## Ordered procedure

1. Run each explicitly configured candidate with its checked-in sampling and
   the same fixed context/output capacities on the same GPU. Create a fresh
   Bread Thief world for each run. Store model/artifact/settings IDs, scenario
   digest, sequence IDs, results and notes in a Markdown evaluation table.
2. Exercise these conversations through normal play: proactive creation and
   readiness; a scene question followed by a clarification; asking Mara about
   the flour; a take request requiring GM approval; repeating a refused request
   with acknowledgment; two-player narration; and long context followed by
   recall of an early relevant fact. Use the same player intentions, not a
   predetermined exact model transcript. Repeat each case three times.
3. Review the actual backend inputs, tools, state and player-visible outputs.
   A passing case has lawful effects, coherent factual context, appropriate
   refusal/continuation and no broken tool exchange. Record failures even when
   later replies recover. Compare latency, input/output tokens and capacity
   failures from recorded measurements; do not score models on style alone.
4. Choose a model that passes every required case; among those, use measured
   latency/resource use and conversational clarity to decide. If none passes,
   record the capability/context failure and resolve it before selecting a
   default. Commit the evidence and configured default in default.toml; leave
   local model paths/secrets in local.toml. Re-run the selected model through
   one full browser playtest after the configuration change.

## Review and completion

The cases/results above and two validation methods—persisted state/tool evidence
and independent review of exact context/visible replies—are the LLM test design
for user review under coding_standards.md before evaluation starts. Examples of
failure include invented inventory, silent tool failure, revealing unrelated
NPC context, repeated invalid saves or forgetting a still-relevant fact. Both
methods must detect those, without requiring exact prose or tool-call counts.

Record evidence in user_exerpts.md's decision log and implementation_reference.md.
The decision covers existing capabilities; full combat/travel/Storyteller real
chat acceptance tests exercise it as those features land. Re-read both standards
and complete the [AGENTS.md checklist](../AGENTS.md#post-feature-and-pre-commit-checklist).
