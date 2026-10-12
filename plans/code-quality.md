# Code quality checks and refactoring

## Outcome and authority

A reproducible command checks Cairnworld-owned Rust/browser source and fails
on warnings, forbidden dependencies and unresolved quality violations. Authority:
coding_standards.md and the user's existing todo request for Clippy, archtest,
jscpd-rs and bca. These are development requirements, not invented game features.

Tool identities: [ArchTest](https://github.com/tdymel/arch_test) checks module
access rules; [jscpd-rs](https://github.com/vv-bogdanov/jscpd-rs) provides the jscpd
clone detector; [big-code-analysis](https://github.com/dekobon/big-code-analysis)
provides bca function metrics/checks. Use these tools, not similarly named crates.

## Ordered changes

1. Add scripts/check-quality.sh running cargo fmt --check, server Clippy with
   --all-targets -- -D warnings, and Clippy for the no-default-features hydrate
   WASM library with -D warnings. Set deny(warnings) and forbid(unsafe_code) on
   Cairnworld crate roots. Keep dependency build-script diagnostics distinct;
   add no allow attributes or relaxed existing checks.
2. Put request_builder and original-version prompt rendering in
   src/model_prompt.rs, taking immutable Request/artifact values. Backend and
   reconstruction call that same module; store/prompt.rs persists/loads evidence
   and no longer imports mistralrs_backend. This removes the observed persistence
   dependency on the live backend while retaining one formatter implementation.
3. Add architecture.json with MayNotAccess rules: UI/shared wire types may not
   import Store/Game/runtime; llm/content/wire data may not import web/MCP;
   Store may not import web/MCP/Game; game operations may not import web/MCP.
   Run cargo archtest from the shared library root, where server modules are
   feature-gated. Do not impose a generic layered graph or prohibit ordinary
   feature-local parent access. Prove rules reject a forbidden import in a
   temporary test fixture and leave valid server/hydrate imports allowed.
4. Add .jscpd.json scanning src and scripts, excluding generated/dependency trees,
   using 5-line/50-token minimum fragments and a 5% maximum duplicate-line ratio.
   Run jscpd with console/JSON reports under target/quality. Consolidate identical
   operation implementations into their feature's existing function, not a new
   generic helper just to erase repeated syntax. All owned source remains visible.
5. Add bca.toml with function cyclomatic and cognitive complexity limits of 15,
   scanning the same owned trees; output detailed reports under target/quality.
   Split complex functions by the real responsibilities shown in the report:
   history traversal/content resolution, socket lifecycle, inspection view and
   game action transitions. Keep each extracted operation cohesive. No source
   suppressions or baseline that marks current violations accepted. If a metric
   demands a worse design, report the concrete case to the user before changing
   its threshold; the initial thresholds are explicit proposals, not claims of
   measured project fitness.
6. Pin tool versions in the documented installation commands after the first
   successful compatible run, record reports before/after, and add all checks
   to the one script. Update README with commands and scopes. Make the script
   exit nonzero on missing tools and violations; do not silently skip checks.

## Verification and completion

All checks pass on the completed Developer Mode source, with no weakened tests.
Run focused tests for moved/refactored operations, the full Rust suite and real
browser verification. Prompt/model-facing changes additionally require real
chat verification and prompt review. Update implementation_reference.md,
re-read coding_standards.md and prompt_standards.md and complete the
[AGENTS.md checklist](../AGENTS.md#post-feature-and-pre-commit-checklist).
