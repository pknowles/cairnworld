# Check bounded GPU memory at startup

Completed and verified: 2026-10-19.

## Goal and scope

Meet user_declarations.md's Memory Usage requirement: compute a bounded working
set from current model/context/concurrency settings, check available memory at
launch and print the budget. Fixed paged KV capacity alone is insufficient.
Full upfront reservation is preferred if supported, rather than a requirement.

## Evidence and implementation order

Reuse mistral.rs's prompt workspace estimator with the configured per-request
context and the scheduler's full sequence count and prefill chunk length. Budget
one additional workspace for cached/in-flight allocations, plus the existing
CUDA maintenance safety margin, and subtract reusable idle pool memory. Reject
insufficient capacity before publishing the loaded game model. Log each
component. Verify the estimate covers the measured pool-reservation peak, not
only live tensor bytes, and test exact-fit/insufficient/reusable-pool accounting.

Launch validation verified on 2026-10-19: the 16k/single-inference budget is
9,162 MiB, including allocator and safety headroom. Three near-limit requests
peaked at 7,712 MiB of pool reservation; the budget also covers the earlier
8,096 MiB peak. The native 262k context was rejected by capacity estimation
without allocating that workspace. Fixed-cache overflow rejection and short
request recovery passed, as did the real-model browser flow.

- The Windows Qwen3.5 16,384-token test completed three near-capacity requests.
  CUDA pool high-water usage reached 7,186,696,608 bytes and reservation reached
  8,489,271,296 bytes. Before the first request the pool used 5,538,226,264 bytes
  and reserved 5,570,035,712 bytes. Steady used memory returned to roughly
  5.56 GB between requests. These pool measurements are distinct from host-RAM
  estimates and the CUDA driver's combined dedicated/shared budget.
- Use the measured allocation attribution below to bound attention workspace.
  Include allocator growth and allocations outside the CUDA pool in headroom.
- Fail model startup with required and available bytes if it cannot fit.
  Preserve configured context/output limits and typed capacity errors for
  compaction. The check estimates capacity rather than allocating a test workload.

## Measured allocation attribution

Nsight Systems CUDA memory tracing of the existing configured-capacity test
identified the dominant peak in prefix-prefill attention. The profiled test
passed all three long requests, overflow rejection and recovery (92.14 seconds).
The trace and SQLite export are under `target/debug/windows-allocation-trace`.
No inference code or allocation settings were changed for this measurement.

The current build chooses `PrefixPrefillPlan::GatherSdpa`. The largest peak
processes 512 query tokens against 15,360 cached tokens with 16 attention heads:

| Live attention buffer | Size |
|---|---:|
| Gathered keys and values, four KV heads | 2 x 30 MiB |
| Keys and values expanded to sixteen query heads | 2 x 120 MiB |
| BF16 attention scores | 240 MiB |
| FP32 score conversion | 480 MiB |
| FP32 softmax output | 480 MiB |

These seven allocations total 1,500 MiB of the roughly 1,572 MiB pool-use
increase. The two 480 MiB allocations are 503,316,480 bytes each and correspond
to `cast_bf16_f32` and `softmax_f32` kernels. The trace shows cache gather,
head-expansion copies, cuBLAS matrix multiplication, conversion and softmax in
the same operation sequence. Two additional 64 MiB allocations outside the
async pool occur during initialization/first inference; they do not account for
the context-dependent spike.

Relevant dependency code is `paged_attention/plan.rs` (prefill backend choice),
`paged_attention/layers/paged_attention.rs` (prefix-cache gather), and
`attention/mod.rs` (KV-head expansion, cuBLASLt scores and FP32 softmax).
Continuation prefill uses 512-token chunks. Score storage scales with
query-chunk length times cached-context length times query-head count. It is
bounded by configured token/concurrency limits; the measurement does not show
an unbounded per-conversation leak.

## Verification and review

Exercise the real chat path and configured maximum context/concurrency, repeated
long and short turns, tool continuations, overflow rejection and recovery.
Assert measured peak reservation stays within the startup estimate. Check a
deliberately insufficient budget is rejected without allocating its workspace.
Verify an ordinary real-model chat becomes ready and remains usable.

Follow coding_standards.md, prompt_standards.md and AGENTS.md's complete
post-feature/pre-commit checklist. Update design and implementation references
from verified findings; obtain user approval before changing a design contract.
