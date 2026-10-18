# Coding-agent playtesting implementation

## Outcome and trace

A coding agent imports a scenario, chats through the real game and inspects or
replays a recorded inference without temporary scripts. Authority:
[Live Coding Agent Interaction with MCP](../user_declarations.md#live-coding-agent-interaction-with-mcp),
[Debugging and Telemetry](../user_declarations.md#debugging-and-telemetry), and
[proof of concept](../user_declarations.md#an-initial-proof-of-concept).
Implementation follows [MCP design](../design.md#mcp-for-coding-agents).
Developer Mode's shared query types and group history are prerequisites.

## Ordered changes

Before implementation, reconcile the MCP design and tool table with the user's
direct action/tool testing requirement. The current design only reaches GM/NPC
tools through character-agent inference, which does not meet that requirement.
Direct calls must reuse production validation, recording and world serialization;
define their development access scope before implementing the transport.

1. Add the server-only rmcp dependency and `src/mcp.rs`. Add serve's opt-in
   mcp_socket/mcp_user arguments and the thin `mcp --socket` proxy subcommand in
   main.rs. Bind a mode-0600 Unix socket only after resolving the configured
   existing user and obtaining the running Game instance. Each accepted stream
   hosts an RMCP session; stdout of the proxy is reserved for protocol bytes.
2. Expose the exact tool table from design.md. Bind user identity to the session;
   derive PlayerAgent through the existing membership/character lookup. Queue
   enter/chat through Game and call inspection's existing store queries. Keep
   tool errors intact. Track entered characters with session-owned presence
   handles backed by Game::connect/disconnect from Developer Mode
   D2; leave/close releases them through that same implementation.
3. Move import/export/replay command bodies from main.rs into server library
   functions callable by CLI and MCP. import creates a new owned world; export
   uses the same scenario serializer. replay accepts explicit replacement segment
   IDs/text, creates a new sandbox recording, and never executes returned tools.
   Return source and new inference IDs so both can be opened in Developer Mode.
4. Document the coding-agent stdio command, server prerequisites, authorization,
   and an enter/chat/inspect/replay example in README. Reuse the actual typed
   tool schemas; no duplicate handwritten argument definitions.

## Verification

Connect an actual RMCP client through the proxy to the running server. Import
Bread Thief, enable Developer Mode through the owner warning modal, enter a
character, perform a real model conversation and inspect its
same sequence in the browser. Replay with one explicit prompt replacement;
confirm the original request hash/content is unchanged. Two sessions target one
world and still observe serialized state. A foreign world, another member's
character, disabled Developer Mode and a banned session fail at the shared
boundary. Closing a session leaves no live transport resources.

Update implementation_reference.md; run focused transport/game tests and real
client/browser/model paths. Re-read coding_standards.md and prompt_standards.md,
obtain the required review before new LLM tests, and complete the
[AGENTS.md commit checklist](../AGENTS.md#post-feature-and-pre-commit-checklist).
