# One GM agent per group

Pending implementation and verification.

## Goal and authority

The declarations ([GM interaction](../user_declarations.md#gm-interaction)) ask
for multiple GMs, each with context for one group of characters: a group's GM has
one chat. Membership changes produce successor groups through splits or merges;
the resulting GMs inherit the appropriate earlier context. A group's own chat
contains only messages produced for that group. Developer Mode displays the
relationships between groups in a git-style graph and opens the selected group's
GM chat. Authority also includes the graph and history navigation requirements
in [Developer Mode](../user_declarations.md#developer-mode).

The current implementation uses one world GM agent (`world_gm`) with several
group-specific history branches. Its inspector stacks those branches as chat
sections; it does not draw the declared graph. Correct the ownership and replace
that stacked interface. Keep history inheritance and compaction where they serve
the declared split/merge behaviour; the graph is a view of those relationships,
not another model-context or message-storage system.

Follow [AGENTS.md's checklist](../AGENTS.md#post-feature-and-pre-commit-checklist),
[coding standards](../coding_standards.md) and [prompt standards](../prompt_standards.md).

## Target model

- `group` owns its GM agent (`group.gm_agent_id`), created with the group.
  `world_gm` goes. A GM is an ordinary agent with an `agent_history` chat; nothing
  but a group's GM is called a GM.
- Split: each resulting group gets a new GM agent/chat with a reference to the
  source chat through its settled split point. Merge: the new group's GM chat
  references all source chats through their settled merge points. Source groups
  are deleted; their GM agents/chats retain the participant IDs, labels and
  source relationships needed for inspection. Moving a character out of an
  established group is a split. Only GMs attached to existing groups take
  gameplay turns; recorded inferences and compaction belong to durable chats.
- Preserve each chat's own messages separately from its inherited context.
  Inference assembly follows bounded source references, includes shared ancestry
  once and uses the existing compaction policy. Selecting a group in the
  inspector initially loads only its own GM chat entries. Navigating to another
  group changes which chat is being viewed, rather than flattening the whole DAG
  into one agent's history. Prompts and extra inference context stay in the
  inference view; chat summaries remain expandable inline where made.
- Compaction, budgets, invocation roles and tool surfaces work per chat as for
  every other agent; the merge-compaction preference in the declarations is
  unchanged.
- Developer Mode provides a group/GM graph with split and merge edges. Every
  graph node identifies a group and its GM agent/chat, one-to-one. Clicking it
  opens that group's history. A name such as "Game Master - <group>" can label
  its agent in the ordinary agent list as well.
- At a chat's beginning/end, scrolling can continue to a predecessor/successor
  group's chat. Choose the adjacent group with the most player characters, then
  the most characters in total; a remaining tie may take either path. Use the
  participant IDs recorded on the source chats when the groups were replaced,
  not a separately maintained preference or current-membership approximation. Split/merge boundary
  entries name and link the other related groups so every alternative path is
  accessible. Preserve paging and message/inference deep links during navigation.

## History representation

| Option | Benefit | Cost / decision |
| --- | --- | --- |
| Bounded references to source chats | Reuses stored messages, provenance and ancestry-aware compaction | Change the current same-agent parent constraint to authorized same-world GM ancestry; selected. |
| Materialize inherited message references into each new chat | Linear reads | Requires copying/reference expansion and shared-ancestor/summary deduplication at each transition; no demonstrated simplification. |
| Summarize every source into a fresh chat | Small initial context | Forces lossy compaction even when histories are small, contrary to the declaration; rejected. |

The existing `history_parent` schema enforces one agent for both endpoints.
Replace that assumption with validated source-chat relationships between the
groups' GM agents in the same world, preserving immutable cuts and acyclic
ancestry. Do not simply remove foreign-history checks: arbitrary agents and
foreign-world chats must remain inaccessible. Reuse the existing traversal,
shared-ancestor deduplication and captured input ranges with corrected ownership.
Derive graph edges and history boundaries from these same transition records;
do not maintain another graph or duplicate the transcripts.

Gameplay groups and recorded chats have different lifetimes. Keeping retired
gameplay groups would require active/retired filtering and name-reuse handling
throughout gameplay. Keeping transcripts alone would lose the participants and
relationships required for context privacy and navigation. Retain that metadata
with the chats and delete replaced gameplay groups; no separate historical-group
system is needed.

Existing development saves are not a compatibility requirement for this change
(user clarification, recorded in user_exerpts.md). Target fresh databases, retain
SQLx migrations, and leave existing database files untouched. Do not reconstruct
missing historical membership or rewrite recorded inference authorship.

## Build order

The NPC gameplay and Storyteller branches are merged. Coordinate changes to
groups, split/move and inspection with the other plans in todo.md; the
Storyteller plan's later steps depend on this ownership correction.

1. Trace the group/GM selectors, split/merge, history validation, compaction and
   inspector against the aligned design. The declarations and this task authorize
   the ownership/graph correction; do not implement the old world-GM model again.
2. Schema and store: `group.gm_agent_id`; one chat
   per agent everywhere (add the constraint the schema now lacks), and same-world
   source-chat references. Store source participant IDs and labels with their
   chats before deleting replaced groups. Historical NPC response records must
   refer to durable chat identity; live presence state must end with its group.
   Remove `world_gm` and the special selectors for choosing among several
   chats of one GM. Keep recorded inference provenance and exact reconstruction;
   do not rewrite old inputs to pretend another agent produced them.
3. Game: split and merge create GM agents with parent references; GM turns select
   the group's GM agent. Successor work uses its actual agent's invocation under
   the same trigger/budget, not a source invocation reassigned to another agent.
   Shutdown, replay and admission retain their existing behaviour.
4. Inspector: replace stacked multi-chat GM sections with the declared group/GM
   graph, one-group history selection, scroll navigation and named clickable
   boundaries. Share ordinary chat rendering/paging with other agents. The graph
   is graphical, not merely a list of headings saying "continues from".
5. Tests: split and merge produce the right agents and chat references; replaced
   group rows are absent and their GMs take no new gameplay turns; existing GM scenario tests pass unchanged in
   behaviour. Include shared ancestry, cross-world rejection and the navigation
   cases below. Remove obsolete multi-chat selectors/UI/tests as their replacement
   lands, rather than shipping both systems or compatibility fallbacks.
6. Documentation: design.md, implementation_reference.md, remove this plan once
   archived. Update only implemented behaviour in implementation_reference.md.

## Verification

| Case | Required outcome |
| --- | --- |
| Group deletion and durable history | Each resulting group has one GM agent/chat; replaced group rows and live presence are gone, while their chats, participant IDs, recorded responses and inference evidence remain inspectable. |
| Split then merge | New own messages appear only in their group's chat; captured GM input contains the required source context, with the shared ancestor once. Expandable compaction and old inference reconstruction remain valid. |
| Graph and selection | Browser displays groups connected by split/merge edges. Selecting either group opens that group's own entries, not all ancestors' messages. |
| Navigation | Scrolling across both kinds of boundary chooses by PC count, then total count, allowing either final tie. Named boundary links reach every other source/result. No messages are lost or duplicated across paged navigation. |
| Scope | Foreign-world or unrelated-agent ancestry and inspection requests fail without exposing private content. |

Reuse existing split/merge/presence tests and the Developer Mode browser check.
Inspect actual requests from a real GM scene across a split and merge. Keep
unchanged game outcomes, complete the prompt-standards review for affected model
context and AGENTS.md's self-review, and respect the five-minute testing budget.
