# Todo

Implement these features in order. Each linked plan specifies concrete changes,
declaration links and verification. Follow AGENTS.md's implementation/commit
checklist for each complete slice. In particular, checks like "Check the order
of implementation works and that the plan doesn't require features that aren't
there yet". Remove and archive only verified completions.

- [NPC interaction](npc-interaction.md): consolidate speech and narration into
  shared perception delivery and message-driven NPC responses, including Take,
  privacy and causal inspection; then tentative/committed intervention.
  Interrupt/Never mind exist; restraint and natural stopping are user-owned tuning.
- [Model context size](llm-context-size.md): measure what each agent's context
  is made of (tool definitions are about 59% of a GM request) and find what does
  not earn its place. Problems to investigate only; no remedy is decided.
- [Code quality checks](code-quality-checks.md): measure and enforce module
  boundaries, function complexity and duplication; correct the reported problems. DO NOT SKIP THIS - secodnd attempt after the first was "verified complete" spuriously.
- [GM editing and lookup](gm-editing.md): whole-field and exact-text editing,
  persistent within-location placement/local notes, hierarchical reads/search
  and bounded tool/context sizes. Incidental detail stays in text; significant
  item creation retains its Storyteller responsibility.
- [Coding-agent playtesting](mcp-development-interface.md): RMCP transport to
  the running game, shared play/query operations and explicit edited replay.
- [Member bans](member-bans.md): invitation rejection, owner Ban list and
  reactivation preserving existing characters/history.
- [Returning-player recap](world-recap.md): durable absence job, character-agent
  inference reference and private WorldDetail display.
- [Model evaluation](model-selection.md): recorded repeated Bread Thief cases,
  measured comparison and default selection for the implemented capabilities.
- [Rules and travel](playable-rules.md): complete creation/items, suspended
  consent, persuasion, combat/time and coordinated group travel using the shared
  NPC interaction and action paths.
- [Storyteller worlds](storyteller-worlds.md): persisted phased initialization,
  validated objects, public/private backgrounds, guidance and replacements.
- [Campaign completion](campaign-completion.md): event-driven ending,
  talk-only epilogue choices, final account and closed play.

Pending user choices are concrete proposals in
[design.md / Product decisions](../design.md#product-decisions). They block only
the specified dependent changes; they are not instructions to invent a design.
