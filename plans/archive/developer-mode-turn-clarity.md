# Developer Mode turn clarity

**Completed and verified 2026-10-01:** ready-gated presence narration,
narration-to-Turn links, nested tool execution, readable scene context, and
top-right technical evidence all pass focused and browser verification.

## Goal and authority

Make Developer Mode explain the actual game execution without duplicated
transport evidence, and prevent GM presence narration before character creation
is complete. Authority: [Developer Mode](../../user_declarations.md#developer-mode),
[Character creation](../../user_declarations.md#character-creation), and
[Developer Mode inspection clarity](../../user_exerpts.md#2026-09-30-developer-mode-inspection-clarity).

## Implementation

1. Keep unready characters out of narrated presence transitions; the creation
   ready barrier, not a connection deadline, is the only route to initial GM
   narration.
2. Deliver GM narration with the producing inference relationship so its player
   chat entry opens the Turn directly.
3. Render one turn execution tree: model output tool calls contain their
   execution result/action, with nested child inference links. Do not repeat
   the same call as a separate action or message summary.
4. Keep the exact reconstructed model prompt and measurements in a top-right
   technical popover. Render request context as readable records; opaque call
   IDs remain persistence validation only.

## Verification

Add focused store/game tests for unready presence and narrated-chat Turn
links. Rebuild the frontend and run the browser inspection path, then the
quality gate.
