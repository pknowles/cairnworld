# Model selection

## Goal

Choose the configured default model from evidence gathered on a real playable
scenario, rather than from the earlier one-tool harness.

## Scope

- Define representative Bread Thief conversations that exercise player-agent
  guidance, character creation, GM action calls, narration, and context growth.
- Run the already working configured models through the same recorded paths.
- Compare success at the intended behavior, tool-call correctness, visible
  latency, token use, and context capacity; record the evidence and select the
  default configuration.

## Steps

1. Write the use cases, adverse results, and meaningful validation methods as
   required by `coding_standards.md`; obtain user review before testing model
   behavior.
2. Capture comparable real chats and inspect their actual assembled inputs and
   tool results.
3. Record the decision and its evidence in `user_exerpts.md`'s decision log,
   then update the configured default and `implementation_reference.md`.

## Verification

- Every tested model has evidence from the same scenario and visible tool
  surface, rather than a synthetic substitute.
- The selected configuration completes the chosen representative chats through
  the normal browser/agent path and its recorded input is inspectable.

Follow the implementation loop and pre-commit checklist in `AGENTS.md`, plus
`coding_standards.md` and `prompt_standards.md`.
