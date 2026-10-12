# Multiplayer presence implementation

## Outcome and trace

Actual connections drive arrival/departure narration and party-presence answers.
Authority: [World game page](../user_declarations.md#world-game-page) and
[GM interaction](../user_declarations.md#gm-interaction). Implement the transition
rules/tables in [Membership and presence](../design.md#membership-and-presence).
Developer Mode A–C and D1 supply history, access revocation and group rows.
This plan is executed inside Developer Mode D2. MCP later calls the same
connection methods; no MCP placeholder is needed here.
The narration recipient policy (NPCs and connected PCs in the interacting
group; offline PCs do not receive it) was accepted 2026-09-18 - see
user_exerpts.md and design.md's Membership and presence section.

## Ordered changes

1. Add character_presence and presence_narration with the design's timestamps,
   generation and announced-set fields. Extend WorldEvent with PresenceChanged
   and PresenceDeadline. Game::connect/disconnect update WorldEvents connection
   ID sets and durable zero/one transitions under a short lock before posting
   events, independently of in-flight model execution.
2. Register web connections after active-character authorization, and unregister
   on every exit including error/ban/logout. A connection's owner releases it
   exactly once; multiple tabs
   cannot generate duplicate arrivals or premature departures.
3. Implement the design's immediate occupied-group arrival and 60-second
   departure/empty-group transitions. Use sleep_until posting generation-tagged
   events, not polling. Reconnection recomputes the set difference; a stale
   deadline is ignored by its generation check. Store the new announced set
   with the recorded narration's successful delivery.
   Implementation note (2026-09-19): the 60 seconds is `limits.presence_departure_seconds`
   (default 60), not a hardcoded constant - virtual-time tests
   (`tokio::time::pause`) deadlock against the SQLite connection pool's own
   internal acquire timeout when real blocking I/O and a paused clock compete,
   so tests instead set a real, near-zero deadline and wait real (short)
   wall-clock time. Uses `tokio::time::sleep(Duration)` from the scheduling
   moment, not `sleep_until` an absolute instant; the difference is immaterial
   at this granularity and this codebase has no prior `sleep_until` usage to
   match.
4. At recovery, clear live sets, mark previously connected characters absent,
   restore pending deadlines and apply the same empty-group return rule. Expose
   the partition/union of announced sets to the group transaction implemented
   in Developer Mode D4; timers recheck current group/generation before acting.
   All narration uses the real group GM invocation.
   Status (2026-09-19): not yet implemented. Steps 1-3 and 5 are done and
   verified; this step needs a startup routine analogous to
   `interrupt_open_inferences` (src/inference.rs) - clear in-memory connection
   sets (already true on a fresh process, nothing to do there), mark every
   character whose `character_presence` shows connected as absent, and
   restore/re-fire any `presence_narration` row with a still-future `due_at`.
5. Add party_presence() to the player tool list and return caller-group IDs,
   names and online state. Apply approved group/online recipient rules to every
   narration producer, not just presence narration. Prompt context records
   whether narration was actually delivered to that character.
   Status (2026-09-19): party_presence() and the recipient-filtering change to
   `append_location_narration` are done and verified. "Prompt context records
   whether narration was actually delivered" is not yet implemented - no
   narration-delivery outcome is currently threaded into a character's own
   prompt context.

## Verification and completion

Two tabs for one character produce one arrival and departure only after the last
closes. Departures within one minute consolidate; return before deadline cancels
that departure. An empty group produces no narration until return plus one
minute. Restart and group split/merge during the delay preserve the correct
final transition. Online queries cannot cross group/world boundaries. Exercise
these through production sockets plus a real GM/player chat; inspect the exact
presence facts and narration provenance. Use virtual time for timer cases and a
real browser case for connection ownership.

Update implementation_reference.md. Re-read coding_standards.md and
prompt_standards.md, obtain the required LLM-test review, and complete the
[AGENTS.md checklist](../AGENTS.md#post-feature-and-pre-commit-checklist).
