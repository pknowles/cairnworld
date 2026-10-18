# Enforce startup-only GPU allocation

## Goal and scope

Meet the GPU-memory declaration: reserve the complete inference working set at
startup and keep subsequent requests within that reservation. Fixed paged KV
capacity alone does not satisfy this requirement.

On 2026-10-18 the user explicitly ordered: "Finish runtime restoration, then fix
allocation separately". This is the next increment after runtime restoration;
the declaration remains unchanged.

## Evidence and implementation order

- The Windows Qwen3.5 16,384-token test completed three near-capacity requests.
  CUDA pool high-water usage reached 7,186,696,608 bytes and reservation reached
  8,489,271,296 bytes. Before the first request the pool used 5,538,226,264 bytes
  and reserved 5,570,035,712 bytes. Steady used memory returned to roughly
  5.56 GB between requests. These pool measurements are distinct from host-RAM
  estimates and the CUDA driver's combined dedicated/shared budget.
- Use the measured allocation attribution below to bound attention workspace.
  Check remaining allocations outside the CUDA pool and all supported request
  paths before declaring a complete startup reservation sufficient.
- Compare existing backend reservation controls, startup execution of the full
  supported workload with retained buffers, and explicit reusable workspace
  ownership. Measure coverage and overhead before choosing; avoid an additional
  allocator abstraction unless existing controls cannot meet the requirement.
- Reserve the measured complete working set before accepting requests. Fail
  startup with the required and available bytes if it cannot fit. Preserve
  configured context/output limits and typed capacity errors for compaction.

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

Compare a fused paged-prefill path, smaller workspace-budgeted query chunks,
and reusable buffers for the current gather path. Fused attention could avoid
materializing the large score/probability matrices; smaller chunks reduce those
matrices but retain the full-context KV copies. Measure correctness, memory and
throughput before choosing. Startup warm-up only changes allocation timing and
does not reduce these buffer requirements.

## Verification and review

Exercise the real chat path at maximum context/concurrency, repeated long and
short turns, tool continuations, cancellation, overflow rejection and recovery.
Assert no new device reservation after startup, including allocations outside
the pool. Check a deliberately insufficient startup budget fails before listen.
Compare output correctness and throughput with the restoration baseline.

Follow coding_standards.md, prompt_standards.md and AGENTS.md's complete
post-feature/pre-commit checklist. Update design and implementation references
from verified findings; obtain user approval before changing a design contract.
