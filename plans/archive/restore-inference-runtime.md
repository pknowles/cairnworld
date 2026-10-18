Completed and verified (2026-10-18).

# Restore the inference runtime

## Goal

Build and run the existing Cairnworld application with the configured Qwen3.5
model by reconstructing the unavailable mistral.rs changes from the published
fork. Preserve the existing design's exact-input verification contract.

## Scope and order

- Fetch upstream and retain the published fork's tool-call and paged-capacity
  fixes. Use upstream's maintained CUDA 13.4 bindings and keep the Windows
  compatibility changes and commits in the mistral.rs repository.
- Restore generic request-scoped prompt observation and retained chat-template
  formatting inputs. Observe the runtime's normal tokenization pass and await
  observer completion before generation. Keep application persistence and
  Developer Mode entirely in Cairnworld.
- Use only developer-installed tools in their normal machine/user locations.
  Normal Cargo CLI installations are authorized; additional system tools
  require the user's approval. Keep one normal Cargo output layout, without a
  private tool installation under `target`.
- Save the requested OAuth environment values in Git-excluded `secrets/dev.env`.
  Use the existing configuration library's environment overrides and verify the
  production server starts with those values.
- Establish Windows ARM support through numerical GPU tests, including
  64-bit FFI indices/stream handles, grouped-query attention and CUDA-graph
  replay. Preserve fixed cache allocation and admission limits; do not bypass
  the platform check.
- Query Windows UMA availability through the CUDA driver's combined
  dedicated/shared budget. Do not infer CUDA capacity from either the dedicated
  carveout alone or free CPU RAM.
- Exercise existing prompt-reconstruction, real-model chat, tool round-trip,
  game, and frontend checks. Update the implementation reference and setup
  instructions to match the verified result.
- Review and commit the complete changes under AGENTS.md's post-feature and
  pre-commit checklist, coding_standards.md, and prompt_standards.md. Dependency
  changes must be retained with the application rather than requiring an
  undisclosed unpublished commit on another machine.

## Verification

- A Windows implementation must first pass the existing numerical paged
  attention/cache tests. Changing platform guards is not evidence of support.
- Measure startup and peak GPU memory at configured maximum context and
  concurrency, including recurrent state and temporary buffers outside the
  fixed KV pool. Repeated turns must not produce unbounded retained allocations.
  Strict startup-only allocation is explicitly deferred by the user to the next
  increment, `fixed-gpu-allocation.md`; do not mark that requirement satisfied by
  the cache reservation or change the declaration.

- Existing clock/reconstruction and corruption-detection tests pass.
- Real Qwen3.5 inference supplies the exact prompt/tokens through the generic
  observer, including a tool-call round trip, and existing game tests pass.
- Observer failure prevents generation and preserves the error context.
- The application and hydrated frontend build and the quality gate passes.
- Browser startup is verified with the supplied OAuth environment configuration.

## Verification results

`real_model_compacts_a_long_chat_and_keeps_an_early_fact` passes with Qwen3.5.
At the user's direction its fixture is an enduring character goal: finding a
missing sister named Maribel. The assertion still requires the early detail to
survive compaction. Failures include the actual observed model prompts; the
production summarization prompt is unchanged.

The normal executable and production server run with 16,384-token capacity.
Library tests (1,582), regular application tests (108, sequentially), numerical
CUDA tests, capacity/overflow recovery, streaming, tool history, character
creation, split/merge context, browser checks, exact real-model browser evidence,
formatting and server/WASM Clippy checks pass. Production landing/package routes,
model readiness and Google OAuth redirection are verified; interactive Google
login has not been performed. mistral.rs is committed locally at `75dde672` and
has not been pushed. Startup-only GPU allocation remains the separately ordered
next increment in `plans/fixed-gpu-allocation.md`.
