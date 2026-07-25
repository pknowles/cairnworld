# 0001 - Infrastructure to Bread Thief

Status: draft (2026-07-25)

## Goal

Build the framework layers of design.md, in small independently-verifiable
milestones, up to a human playing the Bread Thief scenario end to end with
full debug inspection.

## Scope

design.md sections: Inference layer, Persistence and recording, Agent loop,
Sequences, Web server and UI, Chat compaction, Dev CLI, MCP - plus the
minimal slice of the Game layer that Bread Thief needs (per the "initial
proof of concept" simplifications in user_declarations.md: no Storyteller,
hard-coded notes, stat-roll-only character creation).

## Milestones

Ordered so debugging tools exist before the systems that will need them.
Each may get its own detailed plan when it begins.

1. **Inference REPL.** `llm` layer with the mistral.rs backend;
   `cairnworld chat` streams a conversation in the terminal. Verifies: model
   loads, generation quality/speed, sampling params work.
2. **Recording + replay.** `store` layer; the recording wrapper; `text` +
   `inference` tables; `cairnworld replay`; reconstruction unit test over
   real recorded REPL chats. Verifies: any inference's input can be rebuilt
   verbatim (hash-equal) and re-run.
3. **Tool calls + model bake-off.** Tool registry (schemars-derived schemas)
   + agent loop with one toy tool (a dice roll); exercised from the REPL
   against Qwen3 8B, Hermes (Llama 3.1 8B) and Llama 3.1 8B Instruct; add
   the OpenAI-compatible HTTP backend for the comparison. Verifies: the
   chosen model reliably emits valid tool calls - **decision gate for the
   model** (design.md, Model selection).
4. **Chat history + compaction + fork.** `agent`/`message`/`summary` tables;
   context assembly; compaction with artificially low thresholds;
   `chat --fork` and `--prompts` replay substitution. Verifies: summaries
   cover the right ranges, reconstruction still holds across compaction, and
   the prompt-iteration loop works end to end.
5. **Webserver + UI.** Axum + Leptos, Google OAuth2 login, one world, one
   player agent, websocket chat page. Verifies: a friend can log in from
   another machine and chat against the real model.
6. **Multi-agent + actions.** Player agent → GM call tree, `CallContext`,
   sequences, action IDs with `pending_action`/approve-action, narration
   broadcast. Verifies: the attack-approval flow from user_declarations.md
   end to end with two real agents.
7. **Bread Thief.** Game state schema, JSON import/export, the simplified
   scenario (scenarios/bread_thief.md) with the minimal action set: Move,
   Say, Attack, Save, TakeDamage, BeginCombat/EndCombat, Give/Take, Look.
   Verifies: a human can play the scenario to any of its endings.
8. **Dev mode.** The split view: chat histories, sequence view, inference
   view, game object browser. Verifies: every Bread Thief playtest
   interaction is fully inspectable down to verbatim model input.
9. **MCP.** RMCP server over the existing tool surface and debug queries.
   Verifies: a coding agent can drive a playtest and pull sequences without
   custom scripts.

Milestones 7 and 8 may swap or interleave in practice - playtesting without
the inference view will get painful fast, and that pressure is fine to
follow.
