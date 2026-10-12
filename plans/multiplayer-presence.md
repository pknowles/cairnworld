# Multiplayer presence

## Goal

Make connected-player presence a durable world-event input: the location GM
narrates arrivals and delayed departures, and player agents can answer who is
present in the party.

## Scope

- Derive presence from live game-page connections, not client claims.
- Queue the declared one-minute transitions through the world's existing
  serialized event path: consolidate nearby departures; while nobody is
  present, defer narration until someone returns, then wait a further minute.
- Add the player-agent presence operation with only the caller's relevant
  world/location information and return its result through the ordinary agent
  loop.

## Steps

1. Expand `design.md`'s event sequence with the exact connection-state and
   timer transitions before implementation; do not add a polling loop or a
   separate world-event executor.
2. Store or derive only the presence state needed to make reconnect, several
   simultaneous browser closures, and server restart unambiguous.
3. Route the resulting arrival/departure events and presence operation through
   the existing world queue, GM call path, recording, and browser broadcast.
4. Update `implementation_reference.md`.

## Verification

- Two members connecting and disconnecting in close succession produce the
  declared consolidated narration in world-event order.
- A reconnect before the delay cancels the departure transition; an empty world
  remains quiet until a player returns and the additional delay elapses.
- The presence operation cannot report a member from another world or location,
  and every narration and operation is visible in its recorded sequence.

Follow the implementation loop and pre-commit checklist in `AGENTS.md`, plus
`coding_standards.md` and `prompt_standards.md`.
