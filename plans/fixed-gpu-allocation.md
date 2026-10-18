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
- Trace allocations through model loading, recurrent state, prefill, decode,
  graph capture and library workspaces at configured context and concurrency.
  Establish which allocations use the CUDA pool and which bypass it.
- Compare existing backend reservation controls, startup execution of the full
  supported workload with retained buffers, and explicit reusable workspace
  ownership. Measure coverage and overhead before choosing; avoid an additional
  allocator abstraction unless existing controls cannot meet the requirement.
- Reserve the measured complete working set before accepting requests. Fail
  startup with the required and available bytes if it cannot fit. Preserve
  configured context/output limits and typed capacity errors for compaction.

## Verification and review

Exercise the real chat path at maximum context/concurrency, repeated long and
short turns, tool continuations, cancellation, overflow rejection and recovery.
Assert no new device reservation after startup, including allocations outside
the pool. Check a deliberately insufficient startup budget fails before listen.
Compare output correctness and throughput with the restoration baseline.

Follow coding_standards.md, prompt_standards.md and AGENTS.md's complete
post-feature/pre-commit checklist. Update design and implementation references
from verified findings; obtain user approval before changing a design contract.
