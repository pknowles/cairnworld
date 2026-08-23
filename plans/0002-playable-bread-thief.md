# Playable Bread Thief

Status: in progress (2026-08-22). Slice 1 is complete.

This plan follows AGENTS.md's implementation-loop and pre-commit checklist,
coding_standards.md, and prompt_standards.md. Each product interface below is
traced to user_declarations.md; test boundaries target Cairnworld behavior,
not framework behavior.

## Goal

Deliver the first real game vertical slice: a Google-authenticated player can
create or join a Bread Thief world and play it in a browser. Its durable world,
agent, action, recording, and websocket path has no temporary single-user chat
or replacement ownership model.

## Trace to declarations

- **Accounts and access:** User Interface / Landing page and World detail page:
  OAuth-only accounts keyed by email; editable names; a creator-owned world;
  invitation links; joined-player visibility; removal retains the association
  and characters but removes access.
- **Game identity:** World detail page and World game page: joining creates an
  Adventurer; a player's agent guides it; the game page is one chat history and
  input; GM presence narration and the party-presence query are game events.
- **Agent topology:** Agent syntax and tool calls; GM Interaction: agents have
  distinct histories, calls are a tree, and a location can have its own GM for
  PCs or NPCs. The 2026-08-22 declaration explicitly keeps multiple
  location-scoped GMs as a playtestable option.
- **Initial-scope limit:** An initial proof of concept: Bread Thief uses
  hard-coded scenario data and stat-roll-only character creation. It does not
  implement Storyteller world generation or background negotiation.

## Durable model

`user.email` is the unique login identifier. `user.display_name` is editable
and non-unique. A world records its owner. `world_member` records one user's
current access to that world, even after removal, and owns that member's player
agent. A PC belongs to a membership. An NPC owns its agent. A location owns its
GM. These relationships, not an `agent.kind` string, determine an agent's role.

The Storyteller relationship is deliberately not implemented in this increment:
the proof-of-concept declaration excludes Storyteller initialization. The
schema must not gain an unused Storyteller agent merely because the later design
will need one.

## Complete slices

1. **World topology and scenario data.** Implement the user, owner,
   membership/access, player-agent, character/NPC-agent, location/GM, item and
   path data required by Bread Thief; import/export the checked-in scenario;
   create the owner membership and blank Adventurer through the same join
   operation that invitations will use. The player agent later opens the
   character-creation conversation and uses the declared roll tools; the only
   account bootstrap needed for this slice is store-level creation, so it
   remains independent of Google.

   Verify structural invariants through the real store: an email identifies one
   account while display names may repeat; joining through valid access produces
   exactly one player agent and Adventurer; scenario export/import preserves the
   usable game graph. These are our relationship rules, not assertions that SQL
   inserted a field. Owner-driven removal is implemented with the invitation/UI
   flow in slice 2, where it has a real caller rather than a standalone access
   mutation API in slice 1.

2. **Web-playable Bread Thief.** Add Axum + Leptos SSR/hydration, Google OIDC,
   SQLite-backed sessions, landing/world-detail/game pages, invitation creation
   and acceptance, and the websocket together with the player/GM game service
   it invokes. On first entry, the membership's player agent opens the declared
   character-creation conversation. Its declared roll tools persist Rust-made
   rolls; player-to-location-GM calls use action IDs and approved stored
   arguments. NPC and GM histories are the real related agents from slice 1.
   Browser handling supplies the authenticated membership to this one service;
   it neither assembles context nor performs actions itself. The world event
   queue serializes player messages, join/leave timers, and broadcasts.

   Every route and websocket resolves the authenticated user's active world
   membership once, then passes that membership rather than any client-supplied
   user or agent id into the player event. Verify the resulting boundary: an
   unauthenticated, removed, or unrelated account cannot read or mutate a world
   by URL or websocket; two members' browser events select their own
   player-agent histories; an event arriving while its world is busy is visibly
   queued and ultimately processed in order; browser history shows resolved
   player-facing text only. OIDC discovery/exchange, session middleware,
   SSR/hydration, and websocket framing are exercised in their real integration
   path, not duplicated with mocks.

3. **Real deployment check and review.** Run the server with the real GPU model;
   a second person logs in with their own Google account from another machine,
   joins via an invite, and completes a multi-turn Bread Thief play session.
   Confirm reload/reconnect uses the stored history and `replay` reconstructs
   browser-originated inferences. Update implementation_reference.md, run the
   full checks, review prompt/context changes through real chats, and commit
   complete independently working slices only.

## Test-design checkpoint

Before code, review the use cases and adverse outcomes above with the user,
then select the smallest tests that distinguish failures in Cairnworld from
third-party integration failures. No test is added merely to prove persistence,
framework behavior, or implementation details.
