# Todo

Implement these features in order. Each linked plan specifies concrete changes,
declaration links and verification. Follow AGENTS.md's implementation/commit
checklist for each complete slice. In particular, checks like "Check the order
of implementation works and that the plan doesn't require features that aren't
there yet". Remove and archive only verified completions.

- [Focused code quality](code-quality.md): consolidate the repeated starting-group
  lookup, then implement checked SQL in feature batches. Broader redesigns and
  additional metrics gates require evidence; they are not prerequisites.
- [Coding-agent playtesting](mcp-development-interface.md): RMCP transport to
  the running game, shared play/query operations and explicit edited replay.
- [Member bans](member-bans.md): invitation rejection, owner Ban list and
  reactivation preserving existing characters/history.
- [Returning-player recap](world-recap.md): durable absence job, character-agent
  inference reference and private WorldDetail display.
- [Model evaluation](model-selection.md): recorded repeated Bread Thief cases,
  measured comparison and default selection for the implemented capabilities.
- [Rules and travel](playable-rules.md): complete creation/items, suspended
  consent, NPC conversations, combat/time and coordinated group travel.
- [Storyteller worlds](storyteller-worlds.md): persisted phased initialization,
  validated objects, public/private backgrounds, guidance and replacements.
- [Campaign completion](campaign-completion.md): event-driven ending,
  talk-only epilogue choices, final account and closed play.

Pending user choices are concrete proposals in
[design.md / Product decisions](../design.md#product-decisions). They block only
the specified dependent changes; they are not instructions to invent a design.
