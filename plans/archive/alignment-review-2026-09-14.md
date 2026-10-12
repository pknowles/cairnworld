Historical review evidence; superseded as planning guidance by the active plans.

# Alignment review — 2026-09-14

## Scope and conclusion

Reviewed clean repository HEAD `61ed3c3`, recent Developer Mode commits,
AGENTS.md, coding_standards.md, prompt_standards.md, user_declarations.md,
user_exerpts.md, design.md, active plans, implementation_reference.md and the
relevant persistence, context, game, web, browser and runtime-test code.

The foundation is appropriate: one Rust application, durable original content,
reconstructable inference recipes, ordinary tools executing Rust-owned rules,
and real rendering/query tests. Preserve it. Developer Mode completion claims
outstrip the implementation, and group-based GM context was missing from the
build order. The changes from this review are design/planning/reference changes;
the defects below remain implementation work in the first plan. No runtime,
prompt, test, dependency or user_declarations.md changes were made.

## Findings and their owners

| Finding | Evidence and impact | Required correction |
| --- | --- | --- |
| Live inspection retains revoked access and hides data loss | `web.rs::dev_events_connection` receives no viewer/store identity, checks only its cached mode, and ignores `RecvError::Lagged`. `ui.rs::connect_dev_events` silently ignores errors and has no close handler. A lost disable notification can leave the cached mode stale. | Developer Mode: current authorization and explicit stream loss/snapshot recovery before further interface work. Verify the actual server socket. |
| Raw history is not causally ordered | `store.rs::agent_raw_history` filters inference IDs using message-ID bounds and anchors summaries at `covers_to_seq`. It returns no events when the message page is empty. Independent IDs cannot order events across tables; coverage is not summary activation. | Developer Mode: persist actual event positions, paginate all event kinds, retain activation independently of coverage. |
| Shared storage still substitutes equal-text interning for provenance | `store/content.rs` interns every text fragment and content body. `append_location_narration` accepts text and appends ordinary messages without their producing inference/tool. Equal narration text can share storage but cannot identify who produced it. | Developer Mode: direct source/content references, simple original-content ownership, no global transcript-fragment interning needed to make sharing work. |
| Turn and inference views omit required evidence | `SequenceNodeView` is an inference list with action summaries. `ToolCallView` drops call IDs; `RequestView` retains messages only. No durable sequence completion or tool-invocation parent ties every child to its exact sibling call. Inference completion refresh handles only the turn selection. | Developer Mode: real ordered causal tree, unabridged request/result fields, settings, source highlighting and correct async selection handling. |
| Location GM history conflicts with declared groups | `location_gm`, `character_location`, `gm_scene` and narration queries are location-based. Declarations require separate groups at one location and histories that split/merge and follow travel. The prior todo deferred DAG history without a preceding group deliverable. | Design corrected from declarations; group/history implementation and its authentic viewer belong inside the first feature before presence/travel build on it. |
| Verification scope was overstated | `check_developer_mode.cjs` forwards real JSON queries but replaces both websocket servers in JavaScript. Its live inference ID is synthetic and never stored; completion merely triggers a refetch. It copies formatted tool text, not all raw input fields. | Preserve useful browser tests, add actual socket/game-path coverage. Do not call simulated events evidence of production broadcasting or complete exact-input fidelity. |
| Plans contained stale constraints and unowned omissions | One-way enablement remained in design/navigation and test expectations. Raw tools/settings were called lower priority; history groups were deferred without a dependency. Code-quality work had tool names but no plan. | Rewritten Developer Mode plan and concise todo; bounded quality plan; explicit downstream dependencies. No required Developer Mode item is silently deferred. |

These source findings are not all runtime reproductions. The raw-history SQL
failure below is reproduced; live revocation, loss and race cases need focused
production-path reproductions before their implementation fixes.

## Verification performed

- `cargo test --lib --bins`: 73 passed, 9 intentionally ignored device/browser
  tests. These cover existing implemented behavior, not the whole declaration.
- `npm run test:hydration`: four browser scripts passed, plus all three injected
  browser-failure checks rejected the injected failure. The initial sandbox
  attempt could not bind a socket; rerunning with permitted localhost access
  passed. This confirms rendering/hydration and tested HTTP paths, with the
  websocket-fixture limitation above.
- `cargo test generation_prompt_observation_matches_request_reconstruction --
  --ignored --nocapture`: passed with the configured Qwen3.5 4B on the host GPU.
  Independently observed prompt and token IDs matched reconstruction. This is a
  runtime-boundary test; it does not cover complete browser play, compaction,
  historic tokenizer changes or every configured model.
- Executed the exact failure-selection SQL extracted from `agent_raw_history`
  against the production migration in an in-memory SQLite database. An existing
  message with ID 100 and a later failed inference with ID 1 returned no failure
  row. Both are valid independent primary keys. This reproduces an omitted
  failure without modifying application code or inventing a substitute query.

No new LLM tests were designed or prompts changed. Full model/browser play
coverage remains a Developer Mode completion gate, not a claim of this audit.

## Simplicity and dependency decisions

- Keep shared records and immutable bounded reconstruction; do not replace them
  with a logging framework or per-inference copies of conversations.
- Extend history references for the declared GM branches; do not introduce a
  graph database, copied transcripts, or location-based substitute groups.
- Treat live messages as updates to durable state; do not add a durable token
  stream just to animate generation. Explicitly report unrecoverable partial
  output and refresh completed records.
- Share plain serialization types in the existing library where possible;
  several manually synchronized browser mirrors are not forced by WASM.
- Keep MCP attached to the existing execution/authorization boundaries. A second
  live writer is not equivalent to another transport.
- Keep model evaluation in its existing slot, but limit its conclusion to the
  implemented Bread Thief tool surface. Verify the selected model against later
  full rules and Storyteller acceptance cases as those capabilities land.
- Design player-confirmation suspension before item consent/travel; a suspended
  action cannot occupy the only path by which another player can answer it.

## Declaration questions requiring user decisions

- The specific group-history/narration rule and the older location-wide
  recipient suggestion coexist. Group context is explicit; presence must
  resolve online/offline delivery and NPC recipients rather than equating
  group membership with active browser connections.
- Implementation Directives / Chat requests concise propagated failure context
  for agents; design.md excludes failed outputs from their context. A failed
  record and a later interruption marker can be separate facts. Decide that
  contract before changing prompts; safety-bound errors still terminate the
  current invocation immediately.
- A world recap is per player while memberships can own multiple character
  agents. Its author/source needs a decision. With offline narrations excluded,
  it cannot automatically recount a missed session.
- user_exerpts.md's older capacity-tokenization decision disagrees with the
  current retry-based design/code; its alleged character-tail declaration
  conflict is stale (current declarations allow an appropriate count). Do not
  restore either algorithm from the log without comparing current capacity
  recovery evidence and reconciling the intended decision with the user.

The user declarations were left untouched. These questions do not prevent the
concrete live access, history-ordering and interface corrections from proceeding.

## Review completion

Re-read both standards documents and checked the AGENTS.md post-feature and
pre-commit gates for this documentation-only review. Plans trace to declarations,
retain required behavior, order real dependencies before consumers and specify
verification rather than asserting completion. No feature was marked complete,
no runtime fix was inferred from an untested hypothesis, and no existing test
was weakened. This review is evidence for the current plan, not a permanently
maintained implementation reference.
