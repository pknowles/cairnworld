# Focused code quality

Scope reviewed 2026-10-19. Remaining implementation is pending.

## Goal and authority

Make statically shaped SQL compile-time checked. Keep the existing warning and
unsafe-code gates. Authority: [coding standards](../coding_standards.md),
[user declarations](../user_declarations.md), and the explicit acceptance of
checked SQL in [user excerpts](../user_exerpts.md#2026-09-25-merged-gm-history-order-and-checked-sql).

The completed lint, history ownership, presence, world-isolation and shared
starting-group lookup work is
indexed in [implementation reference](../implementation_reference.md).
This plan does not require replacing every Store method, Option, or runtime check.
In content.rs, text_in interns prompt text while content_in records per-event
content and reasoning. They are different operations, not duplicate query twins.

## Evidence and priority

| Item | Verified evidence | Decision |
| --- | --- | --- |
| Unchecked SQL | Cargo uses sqlx, but statically shaped queries in Store are runtime query/query_as/query_scalar calls. design.md explicitly requires checked macros. | Fulfil the accepted requirement incrementally. This is an unmet build-time guarantee, not a claim that a particular current query is broken. |

## Ordered increments

### Incremental checked SQL

- Use sqlx query!/query_as! and the matching locked sqlx-cli version for fixed-shape
  queries. The project currently locks sqlx 0.8.6. Normal Cargo installation is
  sufficient when this increment starts; no system installations are part of it.
- Prepare an isolated schema database under target using the real migrations.
  Commit standard .sqlx offline metadata and verify an offline build.
- Continue the remaining feature batches in order: group/presence operations,
  recording/history, then inspection. Membership/scenario coverage is indexed in
  implementation_reference.md. No separate whole-tree file move is needed.
- Keep genuinely dynamic QueryBuilder operations parameterized and test their
  supported shapes. Do not claim those have become compile-time checked.
- Add cargo sqlx prepare --check to the quality workflow once checked queries
  exist. Prepare/check against an isolated schema, never the running user database.
- Prove the guarantee with a temporary invalid column/type in a checked query:
  compilation must fail. Restore it before committing. A fresh offline build and
  schema-metadata check must pass.
- Do not drop reconstruction, authorization, ordering, or range checks just
  because a query compiles: checked SQL does not establish those invariants.

| Solution | Benefit | Cost / risk |
| --- | --- | --- |
| Runtime queries plus existing tests | No metadata maintenance | Does not meet the accepted compile-time guarantee; untouched paths can contain schema/type mistakes. |
| Checked macros in feature batches; recommended | Build-time schema/type validation while retaining visible SQL | Offline metadata must be refreshed; SQLite nullability overrides need justification. |
| New ORM or query framework | Could change query construction | Broad dependency/API rewrite with no demonstrated need. |

## Proposals removed from unconditional implementation

These are assessment results, not deferred mandatory tasks. Reconsideration needs
specific new evidence and an explicit decision.

| Proposal | Real benefit | Why it is not required work now / risk |
| --- | --- | --- |
| Move all persistence into free functions and new feature files | Could improve cohesion | Moving code alone fixes no defect. Extract an operation when it removes actual duplication or establishes ownership; Store already owns real pool data. |
| Replace every optional value with handles or enums | Can eliminate invalid combinations | Missing cursors and unfinished timestamps are real data. Existing AgentBranch and TopologyChanges already protect identified immutable ownership boundaries. No blanket migration. |
| Cache inspection authorization | Could save reads | Access can be revoked while a socket is open. web::dev_events_connection deliberately checks SQLite before each private frame. A request-local handle must not become connection-long authority. Retain this behavior. |
| Separate read/write pools | Could reduce contention | No measured queueing/latency evidence here. design.md specifies one pool. Two pools do not prevent contention with other processes or session writers. Pool size does not solve uncommitted-data visibility or replace passing a transaction's connection. |
| Construct permanent agent recipes | Could avoid repeated text/tool SQL and JSON work | The extra work is real: context::segments interns pieces and request_for_segments reads them back. Its latency impact is unmeasured. player_prompt includes changing scene data; selected tools also change with readiness. If investigated, compare invocation-local prepared pieces with the current path; a permanent cache is not justified. |
| Normalize every JSON reference into new tables | Foreign keys could enforce existence and ownership | Schema/reader/writer rewrite; foreign keys alone cannot prove range order, coverage or DAG semantics. Existing request ownership checks reject foreign references. No supported-writer corruption reproduced here. |
| Move prompt formatting to another file | Removes a dependency direction | Generation and reconstruction already share request_builder. No duplicated formatter or incorrect output demonstrated; file movement is not a prerequisite. |
| Replace inference admission | Might reduce maintained code if a backend offers identical priority semantics | Existing tests cover capacity, priority and cancellation. No verified simpler equivalent or scheduling failure identified in this review. |
| Mandatory ArchTest, jscpd-rs and bca gates | May identify specific structural problems | No useful baseline, measured false-positive rate or justified 5%/15 thresholds. Runtime modules are in main.rs while lib.rs contains UI/shared wire types: actual analysis coverage must be proved before relying on a gate. Existing Rust gates stay mandatory. |

## Verification and completion

Every increment follows the [AGENTS.md checklist](../AGENTS.md#post-feature-and-pre-commit-checklist),
[coding standards](../coding_standards.md) and [prompt standards](../prompt_standards.md).
Review before implementation and before committing.

Use production operations for fixtures and appropriate end-to-end checks; preserve
all existing coverage. Run the full Rust suite and quality script at commit
checkpoints. For changed prompt/context assembly, inspect captured model input
and exercise the real configured-model chat path, including related use cases.
New LLM behavior tests require the user review specified in the coding standards.

Update implementation_reference.md only for implemented behavior. Do not mark
this plan complete until the required increments pass verification and self-review.
