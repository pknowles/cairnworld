# Presence correctness and chat delivery

Completed and verified 2026-10-19; implementation and standards self-review passed.

## Goal and authority

Preserve correct presence and complete chat histories across server restarts,
empty periods, group changes and delivery failures. An open passive viewer must
receive committed narration without reloading. Authority:
[World game page](../../user_declarations.md#world-game-page),
[Game Master interaction](../../user_declarations.md#gm-interaction), the accepted
[recipient policy](../../user_exerpts.md#2026-09-18-narration-recipient-policy-accepted),
and [design / membership and presence](../../design.md#membership-and-presence).

This is repair of existing behavior. It does not add a recap system, polling,
automatic model retries, an outbox, or another worker framework.

## Verified defects

| Issue | Evidence and actual consequence |
| --- | --- |
| Lost connections remain online after restart | Store::open and World::load do not reconcile character_presence. A prior restart probe reopened the database, reconstructed Game and still found the disconnected character online. The server itself starts successfully. |
| Pending deadlines stop after restart | process_world creates an empty in-memory deadlines map; handle_presence_changed returns early for an existing due_at. A prior probe reconnected after restart, advanced beyond the departure interval, and observed no narration. |
| Empty groups forget the last narrated membership | handle_presence_deadline calls announce_presence with an empty online set. The prior probe expected the two last-narrated characters and read an empty list instead. Their deferred departures are lost. |
| Split/merge discard presence continuity | A new probe ran real player/action/Game Master tool rounds with two connected players and a pending transition. After both split and merge, the resulting group had an empty announced set and no due_at. The split and merge transactions never transfer presence_narration rows. |
| Open viewers remain subscribed to retired groups | play_connected calls Game::subscribe once. In the same probe, both split and merge narration appeared in the passive player's durable chat, while their existing subscription received no wake-up. Their own later action/reload can conceal the defect by querying history again. |
| A delivery error leaves partial player histories | append_group_narration commits each recipient separately. A probe injected an INSERT failure for the second recipient through a SQLite trigger; the real game path returned that exact error, but the first player's narration remained committed and the second player's did not. Presence narration also updates its announced set in a separate transaction. |

The new probes used production game paths, scenario imports, storage APIs and
the existing scripted backend. The trigger was deliberate fault injection.
Temporary probe code was removed; logs are under target/debug/plan-review-probes.log.
Earlier restart/empty-period evidence is under target/debug/branch-review-tests.log.
Those logs are local investigation artifacts, not required build inputs.

Topology/routing reproduction: create two ready, connected players; complete an
initial presence narration, then disconnect/reconnect one player with a queue
barrier between transitions so a departure deadline genuinely exists. Subscribe
the passive player before the acting player's normal look/split/merge tool rounds.
Keep both players together in the split and one NPC apart. Compare resulting
presence rows, the passive player's stored narration, and both pre-change receivers.

Delivery-failure reproduction: install a test-only BEFORE INSERT trigger on the
second recipient's narration messages, scoped by that recipient's history branch.
Run a normal approved look action and verify the returned error contains the
injected marker. Then read both players' actual chat histories. This separates
the intended storage fault from unrelated model/setup failures.

## Solution choices and tradeoffs

### Durable deadlines: one source of truth

| Approach | Pros | Cons / risks |
| --- | --- | --- |
| Restore the timer map at startup and hand off every split/merge update | Few reads while idle | Startup and every topology/error path must keep durable and live scheduling synchronized; this is the current failure mechanism. |
| Read the earliest durable world deadline when its worker wakes; recommended | Uses existing SQLite state; removes the duplicate timer map; startup and topology changes use the same selection path | One scheduling read per wake/action; must use UTC deadlines correctly and exclude retired groups. |
| Spawn an independent task per deadline | Small local timer callback | Reintroduces detached lifetimes and coordination that the World ownership work removed. |

The recommended worker queries once, then waits on its existing action queue,
presence Notify or sleep_until. It does not poll. An expired deadline is consumed
with a generation check before inference; otherwise a failed inference could
leave an overdue row causing immediate repeated execution. Keep the last narrated
set until a successful delivery commits. Surface the original error, record it
through the existing inference/invocation path, and do not automatically rerun
the model. A subsequent real presence change can schedule a fresh transition.

### Empty-period state

Clearing announced IDs is incorrect: those IDs describe what players last heard.
Keeping them alone is insufficient too: the current handler uses a nonempty
announced set to infer that a group is occupied, which could skip the required
return delay.

Recommendation: represent waiting-for-return separately from the last narrated
set in presence_narration. This is real protocol state. First return starts the
configured departure interval even if all returning IDs equal the last announced
IDs; an unchanged set produces no redundant inference at expiry.

Deriving this phase from per-character timestamps would avoid a field but requires
reconstructing group history across split/merge. An explicit small phase is
simpler to validate. Use a normal migration and retain existing histories; do not
reset the working database.

### Chat notification ownership

| Approach | Pros | Cons / risks |
| --- | --- | --- |
| Resubscribe on every group change | Retains group fan-out | Needs a topology notification and catch-up ordering to close subscribe/commit races. |
| Stable per-character channels | Targeted; follows the transcript identity | Needs character-channel construction/lifetime handling and recipient fan-out. |
| One world chat-change wake-up channel; recommended | Construct once in World; no subscription retargeting or group-channel map; readers use the existing authorized chat cursor | Each wake causes reads for other active viewers in that world. Count reads with several groups/viewers before accepting the implementation. |

The wake-up has no narration or private payload. Every socket reads only its own
authorized durable transcript. Publish after committed chat changes; startup,
reconnect and lag catch up from the same stored cursor. A failure notice carries
only a sequence ID: the server resolves its recorded error with active membership
and current-group checks before sending the existing ServerEvent::Error. Raw
errors must not be broadcast across groups. Keep Developer Mode's per-private-frame
authorization checks and token-loss handling unchanged.

### Atomic narration delivery

Recommendation: one short transaction selects currently eligible recipients,
writes all references to the existing recorded narration, and, for presence
narration, commits the actually narrated membership snapshot/phase/generation.
Publish live wake-ups only after commit.

Per-recipient transactions permit the reproduced partial delivery. A new durable
delivery-job system would add substantially more state and recovery machinery;
it is not justified here. The generated inference and previously completed game
actions remain recorded even if delivery fails. Roll back all delivery references
and presence advancement together, retaining the original error context.

The narrated snapshot and current delivery recipients are distinct: connections
can change during inference. Commit what was actually narrated; queued presence
changes then reconcile later differences.

## One coherent runtime increment

Scheduling, delivery and notification ownership share state and failure paths.
Implement them together rather than building temporary plumbing between commits.

- Update design.md's scheduling/state details before implementation while retaining
  the declared timing, recipient and history behavior.
- Add the explicit empty-period phase and narrowly scoped storage operations,
  including transactional delivery/snapshot advancement and generation-checked
  deadline consumption. Select delivery recipients inside the write transaction.
- Construct the world chat-change channel with World. Replace group-keyed
  subscription plumbing; subscribe once for the connection's lifetime.
- Reconcile formerly connected characters once during execution-host startup,
  before play is advertised ready. Retain last-narrated membership. Do not run
  recovery in general Store::open: inspection/CLI clients may open a live database.
- Use the canonical durable-deadline selection for startup and ordinary worker
  wake-ups. Verify past and future deadlines, consumption on error, and no polling
  or retry spin. Workers remain owned by Game and weak while idle.
- In the same split transaction, partition announced IDs by resulting membership
  and copy a pending deadline to the appropriate child groups. In merge, union
  announced IDs and retain the earliest source deadline. Invalidate retired
  sources. For empty resulting groups, preserve the difference and await return.
  If a nonempty merged group has an unannounced difference but no source deadline,
  schedule the configured window rather than leaving that difference stranded.
- Recompute derived timing/phase from the new groups' actual membership and
  connectivity. Preserve creation-ready gating and the configured interval.
- Use the same transactional delivery operation for ordinary and presence
  narration; genuine later presence changes remain queued/coalesced.
- On a narration failure, consume the attempted deadline, preserve unannounced
  differences, and expose the recorded error through the authorized failure notice.
  Do not leave a silently active/stuck deadline or introduce automatic re-inference.
- Route ordinary player replies and Game Master narration through post-commit
  wake-ups. Keep one authorized stored-history/cursor path for live delivery,
  initial snapshots, reconnect and lag recovery.
- Verify no successful-delivery notification is published for a
  failed transaction and no previously completed action is rerun.
- Measure the same ordinary look action with one and four open viewers across two
  groups, using the scripted backend to exclude model time. Record actual SQL and
  transcript-fetch counts and concurrent inspection latency. Compare fan-out cost;
  do not treat the broken previous subscription path as a performance baseline.
  Review an observed regression before adding targeted channel state; do not
  invent a hard latency threshold or add a new pool merely to mask the cost.

## Verification and completion

Use existing scripted-backend game fixtures for deterministic state, routing and
fault-injection cases. Verify:

- Startup with nobody returning; one/all players returning; past/future deadlines.
- Two tabs, connection coalescing, full action queues and creation-ready gating.
- All players absent at deadline; the same or different players returning;
  multiple returns inside the window; cancelled departure without new narration.
- Split/merge during a pending transition, empty children, multiple source deadlines,
  stale source events, and rejection of foreign-world topology proposals.
- An already-open passive browser after both split and merge, a second tab,
  reconnect, lag, and concurrent chat in another group/world. No private-row leak.
- A second-recipient write failure: no partial delivered references, no presence
  advancement, no live success signal; original error and completed action retained.
- Failure/crash boundaries around inference completion and delivery commit;
  no lost pending transition, duplicate delivered entry, or automatic retry loop.

Run appropriate real browser and configured-model chats and inspect the captured
model input when context changes. Present any new LLM behavior test design for
the user review required by coding_standards.md before implementing that test.
Run the Rust suite and quality script at commit checkpoints. Complete the
[AGENTS.md checklist](../../AGENTS.md#post-feature-and-pre-commit-checklist),
[coding standards](../../coding_standards.md) and [prompt standards](../../prompt_standards.md)
self-review. Update implementation_reference.md only after implementation passes.

Verification: 119 Rust tests pass; server and WebAssembly lint gates pass. Real
Chrome checks cover stable passive subscriptions through split/merge, two tabs,
group isolation, notification lag and reload without duplicate entries. Existing
configured-Qwen split/merge context and browser evidence checks pass. The rebuilt
server starts against the preserved development database and returns ready (204).

The scripted browser comparison measured one/four viewers at 72/309 ms to visible
narration, with concurrent inspection at 6.7/5.0 ms. SQL logging counted 238/306
statements, including the action and inspection, and 2/8 transcript fetches. This
is one browser measurement, not a throughput or percentile claim. Extra reads are
the explicit tradeoff for removing subscription retargeting; inspection did not
show queueing growth in this check. No additional pool or channel state was added.
