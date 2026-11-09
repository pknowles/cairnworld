# Todo

Implement these features in order. Each linked plan specifies concrete changes,
declaration links and verification. Follow AGENTS.md's implementation/commit
checklist for each complete slice. In particular, checks like "Check the order
of implementation works and that the plan doesn't require features that aren't
there yet". Remove and archive only verified completions.

- [Code quality checks](code-quality-checks.md): measure and enforce module
  boundaries, function complexity and duplication; correct the reported problems. DO NOT SKIP THIS - secodnd attempt after the first was "verified complete" spuriously.
- [Unified game time](unified-game-time.md): resolve the explicit timing design
  questions, then implement character activities, short-resource ready-times,
  EndTurn, individual initiative, player deadlines and timed effects. Supplies
  timing/admission for the movement and world-authoring features below.
- [GM editing and lookup](gm-editing.md): whole-field and exact-text editing,
  persistent within-location placement/local notes, shared subnotes and object links
  (also in scenario import/export), hierarchical reads/search
  and bounded tool/context sizes. Incidental detail stays in text; significant
  item creation retains its Storyteller responsibility.
- [One GM agent per group](gm-per-group.md): fixes a design mistake (one GM
  agent holding a chat per group); each group owns its GM agent/chat, with
  inherited split/merge context, a group graph and history navigation.
- [Storyteller world initialization](storyteller-worlds.md): core objects
  (create/edit/delete tools, Questioner review that ends with CoreObjectsCreated),
  then the extended summary and detail objects ending with DetailObjectsCreated.
- [Coding-agent playtesting](mcp-development-interface.md): RMCP transport to
  the running game, shared play/query operations and explicit edited replay.
- [Member bans](member-bans.md): invitation rejection, owner Ban list and
  reactivation preserving existing characters/history.
- [Returning-player recap](world-recap.md): durable absence job, character-agent
  inference reference and private WorldDetail display.
- [Model evaluation](model-selection.md): recorded repeated Bread Thief cases,
  measured comparison and default selection for the implemented capabilities.
- [Rules and travel](playable-rules.md): complete creation, player-call replies
  and interventions, transfers and SplitItem/CombineItems, saves/persuasion,
  Rust-owned attack damage, MoraleCheck and timed Travel/Follow using unified
  timing, shared NPC interaction and the existing movement/action paths. Bread
  Thief is the local acceptance scenario, including its prepared half-sack type.
- [Storyteller gameplay](storyteller-gameplay.md): public/private backgrounds,
  creation approvals, entry placement, encounter guidance, event updates and
  replacements, reusing initialized world objects and the completed rules.
- [Agent tuning](agent-tuning.md): measured failures of agent behaviour (GM privacy after a
  merge, the GM's blindness to other groups when splitting and merging, NPC chatter with
  three or more NPCs and after the entrance narration) to tune once the features they
  depend on exist.
- [Campaign completion](campaign-completion.md): event-driven ending,
  talk-only epilogue choices, final account and closed play.
- [Model context size](llm-context-size.md): parked by the user until the rest is built. Tool
  definitions are about 59% of a GM request; two optional chat templates are measured, and the
  text form needs its regression understood before use.

Pending user choices are concrete proposals in
[design.md / Product decisions](../design.md#product-decisions). They block only
the specified dependent changes; they are not instructions to invent a design.
