# World detail

## Goal

Render the declared membership graph as a player/character tree and put each
viewer-owned character's entry control beside that character. The page must
retain the existing invitation and removal behavior. Character selection and
authorization are supplied by the preceding Character ownership increment.

## Declaration trace

`user_declarations.md`, **World detail page**, requires every player to see
the joined players with their characters below them, and a shortcut to enter the
world with a character. It also requires owner-managed invitation links and
removal while retaining the removed association and character.

The preceding Character ownership increment establishes the declared
one-membership-to-many-characters relationship. This plan consumes that
relationship rather than recreating any ownership or access logic.

## Current boundary

- `world_member` is one `(world_id, user_id)` association with `active` or
  `removed` access. Its row remains after removal.
- `player_character` gives each membership zero or more PCs, each with a
  separate player-agent history. `Store::world_members(world_id)` returns one
  `WorldMember` per membership with its characters. It includes removed
  memberships so their retained association is visible.
- `world_detail` in `src/web.rs` first verifies the signed-in account has an
  active membership; an unauthenticated, removed, or unrelated user cannot
  render the page. It then loads the world, member rows, and invitations (for
  the owner only) and passes them to `WorldDetail`.
- Character play routes resolve the signed-in account, active membership, and
  the character named in their path again. A route parameter is a requested
  resource, never authority; the resolved character relationship selects its
  sole history safely.

## Scope

1. Replace `WorldDetail`'s flat `"name — character (access)"` row with nested
   player and character markup. Each membership is one player node; its list
   of current characters are child nodes. Keep `access` visible on the
   player node so a removed association is not misrepresented as an active
   participant.
2. In every character child node owned by the active viewer, render **Enter
   world**. Link it to that character's membership-resolved play route.
3. Keep the owner-only **Remove** form on another active member's player node;
   preserve the existing condition that prevents the owner removing themself.
   Do not add a client-side authorization check in place of
   `Store::remove_member`.
4. Leave the invitation form and list unchanged: owner-only creation, optional
   positive slot limit, remaining-use display, revocation, and invitation
   acceptance all already have their store and route boundaries.

## Explicit non-goals

- Do not alter character ownership, migrations, or the character-scoped play
  access boundary established by Character ownership.
- Do not add a form value, query parameter, or websocket field that identifies
  a character, membership, user, or agent. The character path component is the
  established requested-resource identifier and must be re-authorized server
  side.
- Do not implement dead-character icons, replacement Adventurers, Storyteller
  approval, world status, recap, epilogue, Developer Mode, or Storyteller world
  initialization. Each needs a later state/design increment.
- Do not add a second game or websocket boundary. Character ownership's
  character-scoped routes remain the sole entry boundary this page links to.

## Implementation steps

1. In `src/web.rs`, consume Character ownership's `WorldMember` value with its
   ordered `characters`. Its existing `viewer_id` is sufficient to decide
   whether a displayed membership is the viewer; no extra store query or view
   model is needed.
2. Rewrite the Players section in `WorldDetail`:

   - Keep the outer `ul` over `members`.
   - Render the member's display name and access as the parent row.
   - Render a nested `ul`/`li` over the member's characters.
   - Build an entry link from `world.id` and that character's id only when the
     membership belongs to the active viewer.
   - Keep the current removal form and its owner/other-active-member condition
     on the parent row.

   The resulting HTML expresses the durable relationship already returned by
   `WorldMember`; it does not make selection or access decisions itself.
3. Keep the active-member **Create character** form supplied by Character
   ownership in the player node. It creates a new blank character through the
   store transaction; this view never constructs a character itself.
4. Update the Web play paragraph in `implementation_reference.md` after the
   feature is verified: state that the world detail page renders the current
   membership/character tree and that character entry remains server-resolved.

## Test design

The meaningful outcomes are visible membership relationships and preservation
of the server-side access boundary. Do not add tests that merely assert Leptos
markup or restate SQL joins.

- Extend the real `Store` coverage to assert that `world_members` returns an
  active member's two distinct characters and a removed member's retained
  characters. This validates the data the page needs across meaningful access
  transitions.
- Add a focused `src/web.rs` rendering test if the existing server-rendered
  component test style can inspect the rendered page without duplicating the
  component. It should distinguish: an active viewer gets one entry link per
  owned character; another member is displayed but its characters have no
  entry links; a removed member is displayed as removed and has no links.
  Avoid asserting CSS class names or a particular DOM nesting implementation.
- Exercise the real browser path with one signed-in account: create a second
  character, confirm both appear with distinct entry links, and open each.
  Existing store and route tests cover invitation and removal as distinct
  account relationships without requiring an unavailable second account.

## Definition of done

- The World detail page visibly represents each retained membership as a player
  with every character beneath it.
- Each entry link rendered for a signed-in member enters that owned character
  through the character-specific, server-resolved route.
- Invitation and removal behavior remains covered and works in the real server
  path.
- `implementation_reference.md` describes the delivered view, tests pass, the
  implementation is self-reviewed against `AGENTS.md`, `coding_standards.md`,
  and `prompt_standards.md`, and the plan is marked completed-and-verified only
  after those checks pass.
