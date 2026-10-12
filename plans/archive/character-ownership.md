# Character ownership

Completed and verified 2026-09-07 after the AGENTS.md self-review against
`coding_standards.md` and `prompt_standards.md`.

## Goal

Replace the accidental one-membership/one-character/one-history restriction
with the declared ownership graph: an active world member owns a list of
independently playable characters, each with its own player-agent history. Let
an active member create a further blank `Adventurer` so one account can playtest
that graph.

## Declaration trace

`user_declarations.md`, **World detail page**, calls for a tree whose player
nodes contain their list of characters, with an entry shortcut beside each
character. It creates an `Adventurer` when a player joins. Its death rule also
allows the player to request a new character, which cannot reuse the former
character's chat history. The user clarified on 2026-09-05 that a
membership-owned singular player agent is an intermediate implementation bug.

## Design boundary

- `world_member` remains the durable account-to-world access relationship.
  It owns zero or more `player_character` rows and removal retains all of
  them.
- `player_character(member_id, character_id, agent_id)` owns one PC and its
  one player-agent history. `character_id` and `agent_id` are unique; `member_id`
  is deliberately not. This removes `member_player_agent` entirely.
- An active account reaches a character through
  `/world/{world_id}/characters/{character_id}/play`. The server resolves the
  session account, active membership, and exact owned character in one store
  query. The URL identifies a requested resource; it is never authority.
  The websocket uses the same resolved character relationship.
- Creating an additional character creates a new agent, blank `Adventurer`,
  player-character relationship, and starting-location placement in one
  transaction. A world records its scenario starting location so later
  characters do not depend on where an earlier character has travelled.
- This increment provides the direct active-member **Create character**
  control requested for playtesting. The later death/replacement request and
  Storyteller confirmation will call the same store construction boundary;
  they are not invented here.

## Implementation

1. Replace the initial schema's singular ownership constraints in the
   authoritative pre-release schema. Existing local databases are intentionally
   incompatible and must be recreated until real user data exists. Add the
   durable world starting-location relationship.
2. Replace `MemberAgent` with a character-scoped relationship containing
   membership, account, world, character, and agent identifiers. Make all
   store game queries, agent operations, and broadcasts use that relationship.
   Rename methods that say `member` when they actually act on a character.
3. Make scenario installation and invitation acceptance create one initial
   character through the same `create_player_character` transaction used by
   the new control. It must create no partial agent or character on failure.
4. Change member loading to return each member with its ordered characters;
   update the world-detail tree to render them. Add the active-member create
   form and character-specific entry links.
5. Change game page and websocket routes to include `character_id`, resolving
   the active owned character on every HTTP and websocket entry. Do not trust a
   hidden field, query string, browser-supplied agent id, or prior page load.
6. Update `implementation_reference.md` after verification.

## Verification

- A newly installed world and an invited member each have one blank character
  with a separate agent history; an active member can create another and its
  history starts empty.
- A member's world-detail page lists all of their characters and provides one
  entry link per owned character. A route for another member's character, a
  removed member's character, or a character from another world is forbidden.
- Two characters under one account can open different histories and act
  without crossing character state, location, or broadcasts.
- A fresh database initializes the complete schema. `cargo fmt --check`,
  focused store/web/game tests, the complete Rust suite, and the browser
  hydration suite pass. Complete the `AGENTS.md` self-review before committing.

## Verification record

- `cargo fmt --check`, `cargo check`, and `cargo test` passed; opt-in device
  tests remain opt-in.
- The compiled loading and player-chat browser hydration checks passed.
- The scripted arbitration test covers one GM narration reaching every
  co-located character's history under one account.
- Store and game tests cover independent character histories, authorization,
  retained removed memberships, creation state, and action arbitration; the
  world-detail rendering test covers character-specific entry links.
- Review corrected the remaining membership-named character queries and the
  implementation reference's opening-operation scope. No prompt or design
  changes were needed.
