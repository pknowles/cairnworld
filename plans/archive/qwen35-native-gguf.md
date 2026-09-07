# Qwen3.5 native GGUF support

Completed and verified: `dev-qwen35` runs the full game path - character
creation, opening narration, arbitration, compaction, multiplayer - with the
shared sampling and no chat-template errors; all ignored live tests pass
against it.

## Goal

Run the local Qwen3.5 4B Q4 GGUF as an explicitly selected development model
through Cairnworld's normal in-process, fixed-paged GPU backend.

## Scope

- Rebase the vendored mistral.rs fork onto its native GGUF loader, preserving
  Cairnworld's still-needed fork fixes.
- Let a configured model name provide the original model identifier whose
  configuration, tokenizer, and chat template native Qwen3.5 loading requires.
- Add the local Qwen3.5 GGUF as a named development candidate without changing
  the configured default, prompts, tools, or GPU-memory policy.
- Carry the compaction summary in the leading system message rather than a
  second system turn, which Qwen3.5's chat template rejects anywhere but first.
- Pass a stored tool call's `arguments` to the chat template as a decoded
  object, which Qwen3.5's template iterates as key/value pairs.
- Give every GM turn a user-role event message, and carry GM narration in
  history under a `Narration` role delivered to the model as a user-role
  message; Qwen3.5's template rejects a system-only turn and a mid-conversation
  system message.
- Close the compaction input with the instruction as a user-role message, and
  reject any assembled request that is not a well-formed conversation (one
  leading system message, a real user turn) at assembly.
- Give sampling per-model configuration with a shared default. Untruncated
  sampling of a 4B model produces incoherent text and spurious tool calls;
  Qwen3.5 needs `top_p`/`top_k` truncation to follow its prompt.

## Steps

1. Rebase the mistral.rs fork onto the upstream native-GGUF support and verify
   each carried fork change against the replacement upstream code.
2. Add optional `source_model` configuration to the existing model data and
   pass it directly to `GgufModelBuilder` before it is built.
3. Configure `dev-qwen35` with its local GGUF and original Qwen model ID.
4. In `request_for_segments`, fold a `Segment::Summary` into the preceding
   system message as a marked block, or make it the leading system message when
   the recipe has no role prompt.
5. In the fork's `add_message_with_tool_call`, decode a tool call's `arguments`
   JSON string to a `Value` for the template; render it with `| tojson` in
   `hermes-tools.jinja`.
6. Split each GM prompt into a `System` role prompt and a `User` request; add a
   `Role::Narration` variant stored for narration and mapped to a user-role
   message by the backend; update the browser display and history filter.
7. Close the compaction input with the instruction as a user-role message;
   have `request_for_segments` reject an assembled request that is not a
   well-formed conversation.
8. Add a `SamplingConfig` with a common `[sampling]` block and per-model
   overrides, resolved into `llm::Sampling`; apply `top_p`/`top_k`/`min_p`/
   `presence_penalty` in the backend. Set the shared truncation in
   `default.toml`.
9. Start the application with `--model dev-qwen35` and exercise a normal
   browser chat that requires tool use; inspect the recorded request and result.
   Drive a chat long enough to compact and confirm the post-compaction request
   reaches the model; repeat once with `--model dev-qwen3` and once with a
   Hermes model for no regression. Confirm the opening waits for the player
   instead of calling a creation tool.
10. Update the implementation reference, run applicable tests and formatting,
   then self-review under `AGENTS.md`, `coding_standards.md`, and
   `prompt_standards.md` before committing the complete slice.

## Verification

- `dev-qwen35` loads without an unknown-GGUF-architecture error.
- Its ordinary game chat reaches the model and records the resulting inference.
- A post-compaction request reaches `dev-qwen35` without a chat-template
  "System message must be at the beginning" error, and `dev-qwen3` is
  unaffected.
- A turn with a prior tool call reaches `dev-qwen35` without a chat-template
  "cannot convert value into pairs" error; `dev-qwen3` and Hermes are
  unaffected.
- The full character-creation to opening-narration flow completes on
  `dev-qwen35` with no "No user query found" or "System message must be at the
  beginning" error; `dev-qwen3` is unaffected.
- With the shared sampling, `dev-qwen35`'s opening turn produces text that
  waits for the player rather than calling a creation tool.
- Existing configured GGUFs still load without a `source_model`.

