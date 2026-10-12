# World recap

## Goal

Show each returning player a durable, current recap of their world after they
have been absent for 60 seconds, without adding the recap to their agent chat
history.

## Scope

- Maintain per-member recap data separate from messages and summaries.
- Queue recap generation durably after the declared logout/absence condition;
  it must survive restart and run at deferred priority.
- Display the current player's recap on the world-detail page.
- World completion and epilogues are excluded: their behavior remains an
  undesigned End conditions and epilogue feature in `design.md`.

## Steps

1. Design the durable recap obligation in `design.md`, comparing direct reuse
   of pending compaction work, a shared representation for the two real durable
   inference obligations, and a separate recap representation. Choose the
   smallest design that owns real data and obtain user sign-off before code.
2. Persist the absence-triggered obligation and its completion atomically with
   the recap result; restart resumes unfinished work.
3. Assemble the player's recap inference through the normal recorded context
   path while keeping its result outside the player-agent transcript.
4. Render the member's stored recap on the world-detail page and update
   `implementation_reference.md`.

## Verification

- Logging out and remaining absent for 60 seconds creates exactly one durable
  recap obligation; returning first prevents it.
- Restarting between queueing and completion preserves the obligation and does
  not create duplicate recaps.
- The recap is visible only to its member, is absent from that agent's chat
  history, and its inference remains inspectable through the normal recording
  path.

Follow the implementation loop and pre-commit checklist in `AGENTS.md`, plus
`coding_standards.md` and `prompt_standards.md`.
