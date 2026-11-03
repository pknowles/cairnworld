# One GM agent per group

Pending implementation and verification.

## Goal and authority

The declarations ([GM interaction](../user_declarations.md#gm-interaction)) ask
for multiple GMs, each with context for one group of characters: a group's GM has
one chat; a split gives the new group a GM that starts from a copy of the old
chat; a merge gives the merged group a GM that starts from both chats. Every agent
has exactly one chat.

**This corrects a design mistake.** design.md's "Groups and shared GM history"
instead has one world GM agent (`world_gm`) holding several chats (one history
branch per group, with the branch graph in Developer Mode). That is "multiple
chats inside one agent" and is not what the declarations say. Agents designing or
implementing against that section must treat it as a bug to remove, not a
precedent. The same shape must not be reused for any other agent (the Storyteller
is one agent with one chat).

Follow [AGENTS.md's checklist](../AGENTS.md#post-feature-and-pre-commit-checklist),
[coding standards](../coding_standards.md) and [prompt standards](../prompt_standards.md).

## Target model

- `group` owns its GM agent (`group.gm_agent_id`), created with the group.
  `world_gm` goes. A GM is an ordinary agent with an `agent_history` chat; nothing
  but a group's GM is called a GM.
- Split: each resulting group gets a new GM agent whose chat references the old
  chat up to the split point (the existing parent reference), then continues on its
  own. Merge: the merged group's GM references both chats. Retired groups keep
  their GM agent and chat for inspection; only the active group's GM takes turns.
- Compaction, budgets, invocation roles and tool surfaces work per chat as for
  every other agent; the merge-compaction preference in the declarations is
  unchanged.
- Developer Mode lists each GM as an agent named for its group ("Game Master -
  <group>"), with an ordinary chat history. The GM branch graph and its headings go
  away; a split or merge appears as the new chat's reference to its sources.

## Build order

This changes code the item before it in todo.md may also touch (groups, split and
merge, inspector), so it starts only after that work is merged, and the
Storyteller plan's later steps wait for it.

1. Re-read design.md's group/GM sections, the split and merge tools, compaction
   and inspector code; list every place that selects a GM history by group; update
   design.md first (with sign-off) and the user's declarations where unclear.
2. Schema and store: `group.gm_agent_id`, migrating existing GM chats; one chat
   per agent everywhere (add the constraint the schema now lacks); remove
   `world_gm`, `GroupHistory`-style selectors and the group-selects-branch paths.
3. Game: split and merge create GM agents with parent references; GM turns select
   the group's GM agent; shutdown, replay and admission behave as before.
4. Inspector: GM agents in the agent list, ordinary chat view, delete the branch
   graph UI; update browser checks.
5. Tests: split and merge produce the right agents and chat references; a retired
   group's GM never takes a turn; existing GM scenario tests pass unchanged in
   behaviour.
6. Documentation: design.md, implementation_reference.md, remove this plan once
   archived.

## Verification

Existing split/merge/presence tests, the Developer Mode browser check and a real
model GM scene run across a split and a merge, comparing GM requests to the
previous behaviour. Complete AGENTS.md's self-review.
