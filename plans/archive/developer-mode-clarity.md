# Developer Mode inspection clarity

**Completed and verified 2026-09-30:** role-aware inspection labels, Turn and
Inference hierarchy, compact technical details, readable tool relationships,
icon copy controls, and real-browser coverage all pass.

## Goal and authority

Make Developer Mode readable in the order a developer investigates play:
recognize the actor and current view, inspect a turn's execution story, then
open the exact inference input and output. Authority: [Developer
Mode](../user_declarations.md#developer-mode), [Developer Mode inspection
clarity](../user_exerpts.md#2026-09-30-developer-mode-inspection-clarity), and
[design.md's Dev mode](../design.md#dev-mode).

## Implementation

1. Carry each inference's existing agent relationship through the inspection
   views so links and headings use role plus character/group context, rather
   than an opaque agent ID. Keep IDs in route state and relationship data, but
   do not use them as the primary human label. The single world Game Master
   remains one agent with one history branch per current group; branch headings
   name those groups.
2. Return and render a named Turn envelope rather than an unlabelled inference
   array. A turn begins with its trigger and presents ordered, compact execution
   summaries. Each summary identifies its actor, outcome/tool calls/actions and
   an explicit link to its detailed Inference; it does not inline reconstructed
   input.
3. Render the detailed Inference view in debugging order: title/actor, input,
   output or error, then reasoning. Put model, timing and token measurements in
   a collapsed technical-details section. Replace hash/formatter prose with the
   meaningful verification state; remove the raw serialized-record panel.
4. Label tool calls by tool name and retain opaque call IDs only in a compact
   relationship detail when needed. Change exact-text copy controls to an icon
   with an accessible name and visible copied confirmation, without consuming
   horizontal text space.
5. Update the browser checks to prove player and Game Master labels, a Turn
   title, input's absence from the Turn summary and its ordered presence in the
   Inference view, no raw JSON/hash display, readable tool labels, and icon copy
   behavior. Update implementation reference after the production browser path
   passes.

## Verification

Run focused Rust tests for inspection reconstruction, `npm run test:hydration`,
and `scripts/check-quality.sh`. The browser test must use the existing real
Store/Game/routes fixture: it must fail if an ID is restored as the heading, a
turn inlines input, the inference ordering reverses, or raw JSON returns.
