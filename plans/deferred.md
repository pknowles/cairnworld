# Deferred features

Live index of things user_declarations.md (or design.md, elaborating a
declaration) asks for, that a milestone did not build because a dependency
it needs did not exist yet at that point in the build order. This is not a
record of what happened - git and the per-milestone plan sections are that -
it is a backlog of declared behavior that must not get lost just because of
implementation ordering. Keep it current: add an entry whenever a milestone
knowingly cuts something declared, remove an entry once its feature ships,
and update the trigger if a milestone's scope shifts and changes when a
dependency actually lands.

Do not add anything here that is not traceable to user_declarations.md or a
design.md elaboration of one. A cut made up on the spot with no source is a
bug to fix at the point it was introduced, not a deferred feature.

## Invitation links, player roster tree, shortcut Join buttons

Declared: user_declarations.md, World detail page. Unique invite links with
optional slot limits, deletable at any time; a tree view of joined players
and their characters; a Join button per player to enter with their
character; the owner can remove joined users.

Blocked on: the `character` table (there is nothing to join *as* without
it) and a multiplayer concept beyond one owner per world.

Trigger: milestone 7 (Bread Thief, game state schema) introduces
`character`. Build the join/invite/roster UI once that milestone's schema
exists, rather than building it against milestone 5's single-owner world
and rebuilding it later.

## World status and the logged-out recap

Declared: user_declarations.md, World detail page. An in-progress/complete
world status, and a per-player recap written by that player's agent,
generated with deferred priority once 60 seconds have passed since the
player last logged out. This is a durable queued job, not a live
in-process timer - it must survive a server restart.

Blocked on: a deferred-job queue that survives restart. Milestone 4
introduces the closest precedent (durable deferred compaction), but
milestone 5 (webserver) predates it in the build order.

Trigger: once milestone 4's durable deferred-job infrastructure exists,
add the recap as a job of that kind. Do not build a one-off timer for it
in the webserver milestone.

## GM-narrated join/leave, and "who else is here?"

Declared: user_declarations.md, World game page. The GM narrates a
player's arrival on join. On disconnect, a 1-minute timer before the GM
narrates them leaving (consolidated if several leave close together); if
no players remain, no narration happens until someone rejoins, then a
further 1-minute delay before narrating the transition. Players can ask
"who else in my party is here?" and their agent makes a tool call to
check.

Blocked on: the GM agent, which does not exist until multi-agent play
does.

Trigger: milestone 6 (Multi-agent + actions), once the GM exists.

## Adventurer auto-created character on join

Declared: user_declarations.md, World detail page. Joining a world
automatically creates a new character for that player, placeholder-named
"Adventurer", with undefined stats until the player enters the game with
it, at which point the player's agent guides them through character
creation with the Storyteller's help and names it at the end.

Blocked on: the `character` table and character creation, both milestone
7 (Bread Thief, game state schema - the initial proof of concept
simplifies character creation to stat-rolls only, no Storyteller).

Trigger: milestone 7. Note the proof-of-concept simplification already
adopted (no Storyteller-guided background) still applies until the
Storyteller itself is built - see user_declarations.md, An initial proof
of concept.

## Re-running a stored inference after prompts/code change

Declared: user_declarations.md, Debugging and Telemetry - "reference a
specific LLM output message..., replace the history to match the prompt
changes and then re-generate." Milestone 4 considered a `--prompts <dir>`
CLI flag for this and dropped it: prompts are assembled from stored text
and message rows, not files, so reassembly against current code already
serves this need without a flag.

Blocked on: nothing structural - the mechanism (reassembly from stored
rows) already exists. What is missing is the CLI/MCP surface to trigger a
re-run against a chosen historical message and inspect the result.

Trigger: milestone 9 (MCP) or dev mode (milestone 8), whichever exposes
message-level replay controls first - this is squarely "agents want to
test features and repro bugs quickly without writing temporary scripts"
(user_declarations.md, Live Coding Agent Interaction with MCP).
