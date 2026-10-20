# Continuing interrupted turns

Implementation pending. Shared dependency before NPC interaction.

## Outcome and evidence

After a process restart, continue each accepted unfinished turn from its first
unresolved operation. Keep completed actions and chat history, finish missing
results/delivery, then continue child and parent agents in order. No rollback,
repeated mutation, replacement of completed model output or fresh safety budget.

Authority: the persistent chat/world requirements in user_declarations.md,
[shared execution design](../design.md), and the user's clarification:
“we need to instead continue from the current unresolved item, not fail.”

The existing Store::interrupt_open_inferences marks open inferences, tool
executions, invocations and sequences interrupted at startup. The ordinary loop
in src/agent.rs starts a new invocation and keeps batch position, nested awaits
and per-chat spending in process memory. Action transactions already preserve
their committed result, but that alone does not continue an unfinished turn.
Queued WorldEvent inputs are also process-local. Audit acceptance persistence
before promising that an acknowledged input survives restart.

Follow [AGENTS.md](../AGENTS.md#post-feature-and-pre-commit-checklist),
[coding standards](../coding_standards.md) and
[prompt standards](../prompt_standards.md). This is shared execution work;
NPC tables, combat, human-consent suspension and new scheduling frameworks are
not prerequisites.

## Approach and boundaries

| Option | Benefit | Cost/risk and decision |
| --- | --- | --- |
| Fail the interrupted turn | Little additional state | Abandons unresolved calls and parent replies; does not satisfy turn continuity. Reject. |
| Restart the original input | Reuses today's entry point | Can repeat actions, rolls and delivered messages or generate different tool calls. Reject. |
| Continue recorded operations in order | Retains model choices, completed effects and chat structure | Requires durable invocation inputs, call progress and atomic effect/result boundaries. Select; extend the existing recording and execution paths. |

Use existing sequence, invocation, inference and tool-execution identities.
Add concrete missing continuation data only after auditing existing records and
call sites. Reconstruct typed operation state from those records; do not persist
closures/futures or invent a general workflow language. The normal and restarted
paths must run the same operations rather than duplicate the agent loop.

## Implementation order and required decisions

- Inventory every production tool and enclosing Game operation: model inputs,
  child calls, effect commit, result recording, delivery, deferred topology
  application and sequence completion. For each await boundary, identify the
  durable evidence telling whether that operation happened. Resolve gaps before
  enabling automatic continuation; a tool labelled running does not prove its
  effect was uncommitted.
- Specify a migration and typed continuation states for that inventory before
  implementation. Retain invocation input references, branch/model/sampling/tool
  contract and parent relationship, outstanding tool ordinal, post-result work,
  and enclosing Game delivery. Reference recorded content rather than copy a
  transcript. Persist accepted input before acknowledging acceptance. Recheck
  access on restart without erasing already committed effects.
- Unify inference admission with durable budget charging. Preserve causal total
  and per-invocation limits/counts; nested calls and later NPC sequences share
  their original budget. An interrupted inference attempt stays recorded and
  charged. If no completed output exists, run a new recorded attempt for that
  unresolved inference under the remaining allowance. Do not rerun completed
  output just because its tool rows or decision receipt are still missing.
- Close each effect/result gap through its actual Store transaction. Retain
  original rolled values and action results keyed to their source executions.
  Repeated admission retrieves that result. Make recipient message insertion
  and delivery completion atomic, using the original source identity to avoid
  duplicate messages. Rebuild live notifications from committed history.
- Resume the deepest unfinished child first. Finish its result and pending
  delivery, return to the original parent tool, then run remaining sibling calls
  in recorded ordinal order before asking that parent model for its next output.
  Preserve deferred topology order and original branch cuts. Continue enclosing
  Game operations even if the agent already settled but public delivery did not.
- Startup identifies interrupted attempts separately from actual errors and
  loads unfinished world work before later world events. Keep one world executor;
  never resume the same turn concurrently. A second restart during continuation
  follows exactly the same rules. True errors and exhausted budgets remain
  visible failures; interruption alone is not a terminal turn failure.

This plan intentionally makes the tool/transaction inventory the first design
step: the NPC plan cannot safely assume those missing boundaries already exist.
Do not mark the prerequisite complete after only changing startup status labels.

## Verification and completion

Before writing model-behavior tests, present the required use cases, edge cases
and two or three validation approaches for user review under coding_standards.md.
Exercise the real Store/Game path with controlled process stops:

- During inference; after output recording but before tool-row creation.
- Between sibling tools and while a nested GM call is unfinished.
- After a roll/action commits but before its receipt; after its receipt but before
  delivery; after delivery but before parent/sequence completion.
- Around topology application, accepted queued input and budget admission.
- Restart twice at the same boundary, with disconnected browsers and later input.

Inspect original IDs, effects, message order, prompt captures and resumed parent
responses. Verify one action/roll/delivery, retained completed output, all tool
calls properly answered and no renewed allowance. Include actual tool/model
failure cases to distinguish failure from interruption. Use repeated real-model
chat for the final end-to-end check; database flags alone do not prove a coherent
continued conversation.

Update implementation_reference.md for implemented behavior, run appropriate
Rust/SQL/lint/browser checks, self-review and commit complete slices. Archive and
remove from todo only once ordinary player/GM turns continue correctly; NPC work
then extends the same path for its new operation types.
