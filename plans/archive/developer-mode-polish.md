# Developer Mode polish

**Completed and verified 2026-10-08.**

## Goal and authority

Finish the Developer Mode interaction issues reported on 2026-10-04 without
changing its authoritative recording model. Authority: [Developer
Mode](../user_declarations.md#developer-mode), [Chat compaction](../user_declarations.md#chat-compaction), and the user's 2026-10-04 feedback recorded in
`user_exerpts.md`.

## Implementation

1. Reconcile a submitted player-chat entry with its durable WebSocket echo so
   one player message gains its ID and inspection link rather than rendering
   twice. Keep a durable server entry distinct from an unsent local entry.
2. Make an implicit model instruction an explicit recorded message origin.
   It remains a `user` role at the model boundary, but inspection calls it an
   **Instruction** so it is never misrepresented as a human player's message.
3. Present every compaction summary with one reusable, yellow, collapsed card
   in raw history and Turn views. The card links to the real summary inference
   and renders the summary's own recorded compaction metadata. Suppress the
   empty successful-inference marker because its output already links to that
   exact inference; keep failed inference markers visible.
4. Refresh an open agent-history view on durable inspection changes. Preserve
   the inspection pane's scroll offset in browser history state before an
   inspect selection changes, and restore it after Back/Forward rehydrates the
   prior selection. This is local navigation state, not a shareable URL
   parameter.
5. Treat a peer closing a live-inspection WebSocket as normal connection
   termination rather than a server error. Ensure strict tool schemas retain
   their referenced definitions when embedded in mistral.rs's combined
   tool-call grammar.
6. Add focused Rust and browser coverage for the durable origin, compaction
   card, echo reconciliation, live history refresh, and Back/Forward scroll.
   Update the implementation reference, run the real browser path and quality
   checks, then self-review against `AGENTS.md`, `coding_standards.md`, and
   `prompt_standards.md` before committing.
7. Treat an empty normal player-agent completion as a silent turn finish. Keep
   it durable and explicit in Developer Mode, omit it from player chat, and
   warn when it is the first reply with no preceding tool call. State this
   option concisely in player-agent context; tool results report only their
   durable game outcome, not speculative delivery state.
8. Consolidate compaction metadata into the summary record. Retire an ordinary
   no-op compaction without a history event; preserve capacity inability as an
   error. Do not expose internal history branch IDs in player-agent compaction
   cards, and omit empty completed tool-result rows from Developer Mode.

## Verification

Run focused Rust tests for inspection/history/schema behavior, the hydration
and Developer Mode browser suites, and the project quality gate. The browser
checks must fail for a duplicate user echo, a successful empty inference link,
an expanded/non-yellow compaction summary, a stale open history, or a lost
history scroll position after Back. A scripted tool turn ending in an empty
normal completion must retain the empty inference for inspection while exposing
only the already-delivered game narration to the player; an empty first reply
must warn without failing the player's connection.
