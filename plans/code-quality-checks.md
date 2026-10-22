# Modularity, complexity and duplication

Pending implementation and verification.

## Goal and authority

Measure and check application module boundaries, function complexity and code
duplication, then correct the reported structural problems. The outcome is working
checks and simpler code, not merely installing tools or producing a report.

Authority: the user's [2026-10-22 scope clarification](../user_exerpts.md#2026-10-22-code-quality-checks),
[coding standards](../coding_standards.md), and
[design quality boundaries](../design.md#code-quality-boundaries).
Follow the [AGENTS.md checklist](../AGENTS.md#post-feature-and-pre-commit-checklist)
and [prompt standards](../prompt_standards.md) for each complete increment.
The archived code-quality plan remains unchanged and is not this plan's authority.

This scope comprises the three checks and refactoring needed to satisfy them.
Performance benchmarking, database/pool redesign, recipe caching and a separate
tool installation/version-enforcement project are not part of it. Ordinary Cargo
installation of the selected development tools is permitted by the existing user
instructions; do not download private toolchains or system binaries.

## Required checks

| Check | Scope and acceptance |
| --- | --- |
| Modularity / architecture | Analyze both `src/main.rs` (runtime) and `src/lib.rs` (UI/shared types), including their child modules. UI/shared wire data must not depend on Store/Game/runtime; model/content/wire data must not depend on web/MCP; persistence must not depend on web/MCP/Game; game operations must not depend on web/MCP. Check declared module relationships and demonstrate violations are detected. |
| Duplication | Scan maintained code in `src` and `scripts`, including tests. Use minimum fragments of 5 lines and 50 tokens, with a maximum duplicated-line ratio of 5%. Record scanned files, total and duplicated lines, ratio and clone locations. Do not silently substitute token ratio for line ratio. |
| Complexity | Measure functions in maintained `src` and supported source files in `scripts`, including tests. Cyclomatic and cognitive complexity must each be at most 15. Report each violating function with its location and both values. Report parser failures or unsupported files as coverage gaps. |

Exclude dependencies, generated output, model assets and offline SQL metadata;
the source scopes above already avoid those directories. Do not exclude a source
file because its results are inconvenient. Show production and test findings
separately for review, while retaining both in the stated acceptance scope.
Do not raise thresholds, introduce suppression/accepted-violation baselines, or
remove requirements to obtain a passing result. If a result appears incorrect or
fixing it would worsen the design, present the exact case to the user; it remains
unresolved until a decision, not silently complete.

## Approach and trade-offs

Use the named tools from the original requirement:
[ArchTest](https://github.com/tdymel/arch_test),
[jscpd-rs](https://github.com/vv-bogdanov/jscpd-rs), and
[big-code-analysis](https://github.com/dekobon/big-code-analysis) (`bca`).
Verify their actual CLI/configuration and parser support before writing commands;
configuration filenames do not prove a tool read them or covered this code.

| Choice | Benefits | Costs / risks | Decision |
| --- | --- | --- | --- |
| Manual review alone | Understands intent and ownership | Cannot reproduce measurements or reliably detect later regressions | Retain review alongside automated checks. |
| Named static tools with project configuration | Reuses existing analyzers; produces locations and repeatable results without model inference | Parser/module resolution and metric semantics need verification | Selected. Prove coverage and failure detection before trusting results. |
| Custom analyzer or production-wide module relocation first | Could accommodate tool limitations | Adds maintained code or unrelated restructuring before any evidence | Avoid; report a concrete compatibility blocker before proposing an alternative. |

For architecture, prefer ArchTest's library API with explicit module trees for
both roots if its Cargo command cannot cover both. This is an ordinary small
architecture test, not a new analysis framework. It avoids moving runtime modules
into the library solely to accommodate a tool. Check aliases, re-exports and
qualified paths against the tool's actual analysis; record unsupported forms.
Do not add blanket cycle/parent-access rules without tracing them to a required
boundary. Current bidirectional implementation dependencies are not automatically
permission to weaken the boundaries above.

## Implementation order

### Establish measurements and prove the checks

- Read current module ownership and imports, and map the required boundaries to
  actual modules. Record any conflict with design.md before changing code.
- Configure the named tools and capture an initial report under `target/quality`.
  Include the commit, dirty-tree status, tool versions, commands, file coverage
  and elapsed analysis time. This is measurement evidence, not an exemption for
  existing violations. Summarize counts and concrete hotspots in the plan while
  the work is pending so results are not lost with temporary output.
- Prove detection using temporary fixtures under `target/quality`: a forbidden
  dependency in each Rust root, a copied fragment exceeding the configured
  minimums, and functions exceeding each complexity limit. Confirm clean controls
  pass. Use the same analyzer/configuration as the application check; clean up
  fixtures and do not introduce violations into another agent's active files.
- Match the scanned-file inventory against maintained source. Count shared files
  once in duplication reports. An empty or UI-only architecture pass is failure
  to establish coverage, not success. A tool compatibility problem blocks this
  increment; it does not remove the requirement.

### Correct the measured problems in coherent feature slices

- Review each forbidden dependency, clone group and complex function in context.
  Identify the real responsibility, data owner and reason for repetition or
  branching. List the intended behaviour and affected use cases before editing.
- Fix architecture violations at their ownership boundary. Use existing shared
  data types and feature operations; do not create managers, wrapper layers or
  one function per metric point. A module move must establish a useful boundary.
- Consolidate duplicated operations at their existing feature owner. Similar
  syntax serving different rules is not evidence for a generic abstraction.
  Preserve separate fixtures when their differences express distinct use cases;
  share repeated setup through real production operations rather than copying
  production logic into test helpers.
- Simplify complex functions by separating real responsibilities and arranging
  construction/data flow so repeated checks become unnecessary. Do not scatter
  one decision across trivial helpers merely to lower a score.
- Address overlapping findings together when one ownership correction removes
  both duplication and branching. Re-measure each completed slice and preserve
  behavioural coverage. Do not undertake the other archived redesign proposals
  unless a concrete finding needs them and their scope is agreed.

### Make checks routine and verify completion

- Add the three checks to the existing quality workflow, using checked-in
  architecture rules and metric settings. Document the commands and source scope
  in README. Missing analysis or a violation must be visible as an unsuccessful
  check, not silently skipped. No separate tool provisioning framework is needed.
- Capture final reports using the same settings and source scope as the initial
  measurements. Record the before/after totals, remaining findings (must be none
  against the acceptance rules), coverage and elapsed time. A configuration change
  must not be presented as a code improvement.
- Verify relevant behaviour through existing production-path tests. Follow
  AGENTS.md's five-minute testing budget; static measurement does not require GPU
  inference. Prompt/context changes, if any are actually necessary, still require
  the prescribed real-chat verification and prompt review. Avoid unrelated test
  expansion or repeated full-suite runs between refactors.
- Update design.md for agreed structural changes and implementation_reference.md
  for implemented checks and boundaries. Perform self-review against the scope
  and acceptance table above as well as the standards; an edited-away requirement
  is not a completed requirement.

## Completion evidence

Mark complete and archive only when all three checks cover the required source,
negative fixtures demonstrate detection, the configured limits and boundaries
pass, affected behaviour is verified, and self-review passes. Keep the concise
measurement results and reproduction commands with the completion record.
Keep the old archived plan intact.
