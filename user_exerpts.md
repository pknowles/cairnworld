# User Excerpts

Direct quotes that clarify design intent, too fine-grained for
user_declarations.md. Newest last.

## Chat history is the primary data, not a recording feature

> We just need LLM chat histories to exist somewhere. Not in some "recording
> backend" - in the regular flow of the game/database. Then we need an
> implicit way to reconstruct it.

> We will implicitly need to store chat histories so that the game works.
> Likely as separate messages/inference results.

Recording is a property of storing messages and assembling context, not a
layer added on top of inference.

## Failed inferences are recorded and visible

> Failed inference will be very interesting to capture. It should appear in
> the chat history as a failed output - definitely in the developer chat
> history - and be clickable and we need to be able to properly reconstruct
> the prompt that produced it.

## Design must follow the declarations

> I didn't write design.md. I wrote the user definitions. Design must be
> based on the user definitions and if there's a conflict it's your job to
> raise it (in case I'm the one that's wrong) and then fix it so that
> everything matches user declarations.

On the abstraction this rule was introduced to prevent:

> The problem was we were implementing some abstract recording concept that
> was totally unnecessary just to satisfy the plan. There was a disconnect
> between what was being implemented and what was implicitly required by
> user_declarations.md.

## Remove wrong-direction code outright

> I would prefer the bad code be removed entirely.

The biggest risk of leaving it is that a later agent reads it and makes bad
assumptions.

## Hide roll numbers; distinguish a failed call from a failed save

Observed 2026-07-27: given one hazard, Llama 3.1 8B called `save` three times
with identical arguments, re-rolling after each failure, then emitted a fourth
call as raw text with a difficulty it chose itself.

> to solve this I think we need to hide the number from the model and just
> report the textual result. One issue is we need to distinguish a failed tool
> call from a failed save. I suspect the model is confused. Are there better
> words we can use or can we just be more explicit about the tool call
> succeeding with the result that the save failed?

> I think we should test these ideas without just picking one

The roll value is theatre for the player, not information the model needs. A
tool call that completes is a success even when the save it resolved did not
pass, and the wording must not let those two read as the same thing.

## "Without storing it redundantly" means no whole-input blobs, not dedup

On why `text` should not be keyed by a content hash:

> the point of this was not to store the entire input text in its own field
> for EVERY inference. this would be absurd

> the chance that a model outputs identical text in different chats is so low
> that storing with the hash being the PK just doesn't make sense and will
> lead to more confusion. it's just not going to be worth it.

The recipe of references already satisfies the declaration. Deduplicating
identical strings was never asked for, saves almost nothing (only role prompts
and tool definitions are content-addressed), and buys a shared mutable row that
one edit could use to rewrite history for every inference referencing it. Use
an ordinary surrogate key.

## Safety bounds on the agent loop

> add two project wide configurable safety bounds: one max for individual
> chats and one max for the total llm inference calls that include recursive
> inference. when either is hit the result implicitly returns and propagates
> an error saying which was hit. this will land in the user's terminal since
> it will propagate and agents will not get a chance to respond to it

The bounds exist to stop a runaway, so hitting one is a hard error that
reaches the user - never something an agent can observe and react to.

# Decision Log

## 2026-09-06: Qwen3.5 is a configured evaluation candidate

> please try qwen 3.5

> can you just update the code to use 3.5?

Qwen3.5 4B is available as an explicitly selected development model. It does
not replace the configured default; model selection remains evidence-based on
a playable scenario.

## 2026-09-01: GM narration is already delivered context

> the player agent must see that it was narrated and know that it was sent to
> the player verbatim already. it can then ask me what I want to do. Could we
> even tag GM's narration as "GM"?

GM narration is a distinct, durable player-visible entry. The player agent
receives the exact narration only after it has been sent through that channel;
it must guide the player's next choice rather than repeat or invent the scene.

## 2026-09-05: Character ownership is not membership ownership

> today one membership owns exactly one player agent

> that's a bug and needs fixing. hard coded 1:1 is an intermediate
> implementation state that is wasted effort. I never called for it and it
> should not exist

Membership is the account's access relationship. Each player character owns
its own agent/history, allowing a member to have multiple characters without
crossing their state or chat histories.

## 2026-08-08: Exact compaction accounting

> we should not lie in values stored. be super explicit and use the exact right
> units where there is no trivial conversion to something common.

> how will this work when we don't know how many tokens a given number of tail
> messages will consume? ... we could linearly scan through, tokenizing messages
> but that would be rediculous

`compact_before_next_input_tokens` compares the exact model-reported input plus
output token counts of a completed inference. Their sum is the next inference's
starting context, including its static context, history, tools and generation
prompt. It is not a character count, a JSON serialization, or a conversion
estimate. Normal compaction reuses those recorded values; it never invokes a
second tokenizer pass merely to decide whether ordinary compaction is due.

`keep_tail_messages` retains an exact number of persisted raw message rows. It
does not promise a token budget. This avoids both a false conversion between
characters and tokens and tokenizing candidate messages one-by-one. A later
turn retries compaction when its recorded input reaches the trigger again.

Each recorded inference stores the model-reported `input_tokens` and
`output_tokens` for the debug view. Those are factual usage measurements and
together decide whether compaction is queued after a turn. The threshold is
kept below the fixed context capacity with room for the configured output, so a
turn that has not queued compaction can safely run again.

Compaction metadata belongs to the summary itself. When only the retained tail
remains, the pending job retires without a history event; capacity inability
remains an error.

## 2026-10-08: One compaction event

> creating a separate chat message to store metadata for an event sounds like
> a really bad idea. I would prefer consolidation in the storage, not just the
> UI, since it really is the same operation, no?

The summary activation is the one durable compaction event. Its reason and
retained-tail count are fields on `summary`; there is no second notice record.

## 2026-09-01: Capacity fallback tokenization

> The happy path tokenization will NOT run tokenization at all? This was the
> entire reason why I designed the compaction threhsold to happen immediately
> after inference - because we know exactly then

> if we tokenize once on the infrequent over-sized compaction fallback, we can
> guarantee being under input memory requirements whereas estimation can fail?

The ordinary trigger and summary path therefore use only completed-inference
usage. If the normal summary input cannot fit fixed KV capacity, its fallback
uses the loaded model's actual chat-template tokenizer to choose a fitting
linear prefix summary and verify progress; this is the sole tokenization path.

This is an agreed design decision that conflicts with the character-tail rule
currently recorded in `user_declarations.md`; that declaration needs a separate
reconciliation before the design can be committed as fully traceable.

## 2026-08-22: Build only durable vertical slices

> as long as there is minimal effort to make something work inbetween steps -
> just consolidate steps if this is the case. and the final result of course
> must be traced back to the top level user declarations to verify no spruious
> features were added

An intermediate increment may expose an incomplete feature only when it uses
the same durable data and component boundaries as the final feature. If it
would require a temporary relationship or a replacement implementation, merge
it with its dependencies into one complete vertical slice. Every proposed data
relationship and component must cite a user declaration before implementation.

## 2026-08-22: Character tool identifiers

> lets keep them numbers only and two digits unless we run out

Character names are not identifiers. A character receives an immutable `charN`
tool ID with a globally unique numeric suffix. Allocation chooses an unused
two-digit value from 10 through 99; after that pool is exhausted it widens to
three digits, and so on. The value is independent of character name and
creation order.

This is reflected in the Game state section of user_declarations.md.

## 2026-08-22: Character creation opens proactively

> The player's agent would already be given a system prompt telling it to guide
> the player through character creation. It would speak first.

An imported or newly joined Adventurer is blank. The player agent initiates the
ordinary chat-based creation conversation and, when appropriate, calls the
declared creation roll tools. Rust performs and persists each roll; the player
does not issue a special character-creation command.

## 2026-09-07: Developer Mode priority and exact inference inspection

> invitation links are for only invitation. once a player is in they're in and would need to be explicitly removed, not by removing the invitation link.

Invitation deletion and membership removal are independent operations. This
clarification did not specify post-removal link reuse or slot consumption.
The ban/reactivation decision below replaces that open question.

> the thing I really wanted to get to was developer mode. I need this asap so I can actually debug and play test

> would you be able to restructure the plans so they match what I wanted to build, completed items are archived and the order is easily followable without fabricated intermediate stages?

> Then begin work on developer mode - start by fleshing out the design. I'd like to be able to see and navigate the entire turn stack, click on inference items and see the entire prompt. We need testing to make sure what I end up being shown is identical to what was given to the agent (even if when I actuall view it it's reconstructed from database messages for efficiency)

Design interpretation: keep immutable inference recipes, prove reconstruction
against input captured on the actual inference path, and verify that the browser
exposes that same content. Include backend formatting evidence so application
messages are not mistaken for the final template-expanded model prompt.
Developer Mode is the next complete implementation feature; unrelated remaining
World detail declarations must not delay it.


## 2026-09-07: Bans, recording terminology and commit scope

> Lets call "removing" more just banning/disabling membership. We can hide the player from the list. They can't join until they're un-banned. We can have a discrete Ban list where world owners can reactivate banned/disabled players. This avoids the issue of a removed player immediately using an existing join link to re-join and the possibility of getting a brand new character generated.

This supersedes the earlier re-entry question and the declaration's return with
another link. The owner reactivates the existing membership and its characters;
an invitation cannot override a ban. The user must update user_declarations.md.

> sometimes bigger chunks make for more efficient/faster development because you skip having to make intermediate state work. Your call as you're the one that's going to be doing it either way.

> It needs better naming so I can understand it just by seeing the name

Actual model prompt recording names observation of the formatted input consumed
by the model. Input checksums are verification details, not separate declared
features; reconstruction itself uses the recorded messages and other inputs.


## 2026-09-07: Shared historical reconstruction

> can you confirm that this schema/recording extension does not have to capture redundant/additional/duplicate data? this is why I keep discussing reconstruction. I'm hoping we can 100% accurately rebuild ALL OF the chat history at any point in time, compaction points, prompt/inference input and output (that would then be appended to chats) etc

> Can you make sure the design makes my reconstruction clear and directly tied to both viewing the chat history and dev mode to view everything

Design decision: all history and inspection views resolve one authoritative
copy of original content. Inference outputs become chat entries, summaries or
narration through references, not copied payloads. Compaction preserves original
messages, earlier summaries, covered ranges and activation points. Inference
recipes retain exact historical inputs and formatting references; checksums
verify reconstruction but cannot replace the independent equality tests.


## 2026-09-09: Developer mode is an owner toggle, not one-way

> I removed the "cannot be disabled" part as I think players may need this in
> order to fix issues in their games. safer to allow

> it's fine if this is a toggle too

user_declarations.md was updated: developer mode is enabled and disabled by the
world owner, in either direction. The earlier "one-way operation" language is
removed (see the note the user added to user_declarations.md). Recording is
unaffected - it runs for every game regardless of the toggle; the toggle only
changes who can see the data. `world.developer_mode` is a plain boolean column.

## 2026-09-14: Plans specify implementation, not future design work

> The plan should concretely describe an implementation and ordering thereof
> for the overal features I need and each item must be traceable to user
> declarations.

> if you disagree with anything or have better alternative ideas, please
> suggest them/discuss with me about whether I should change or add them to
> the user declarations

Design owns the chosen data relationships, algorithms and behavior. Plans name
concrete code/data changes in dependency order, with declaration links and
verification. A step such as “specify the schema” is unfinished design, not an
implementation plan. Product alternatives remain explicit proposals until the
user decides; agents never edit user_declarations.md.

## 2026-09-18: Narration recipient policy accepted

design.md's Product decisions proposal - NPCs and connected PCs in the
interacting group receive narration; offline PCs do not - was accepted to
unblock Developer Mode plan D2 (presence) and D3/D4 (group narration/recipient
routing). This resolves the World detail page's open TODO ("Should GM
narrations be added to all PC chats even when not online? For now lets not,
but this needs testing.") as: no. The user should update
user_declarations.md's World detail page section to remove that TODO and state
the accepted policy directly, since design proposals do not themselves amend
declarations.

## 2026-09-25: Merged GM history order and checked SQL

> Treat it like a git history with merges.

Asked how to order a merged group's GM history. design.md's Context order now
reads every reachable branch once, in creation order (ancestors first), each in
position order - replacing per-parent ordinal order, which is ambiguous when a
shared ancestor is reachable through several parents.

> [compile-time checked SQL] sounds like a good decision

The code moves to sqlx's checked query macros to match design.md
(plans/code-quality.md).

## 2026-09-25: Inspect pane close/reopen

> the current desktop view also has a broken quirk where there is a "close"
> button that doesn't actually close the panel

The Inspect pane's "Close" button only clears the current selection
(`selection.set(None)`); the `<aside>` panel itself is never conditionally
hidden, so it stays visible and empty. Asked to brainstorm; presented 10
options (fix-in-place with a reopen tab, floating/draggable panel, slim
collapsible sidebar, bottom drawer, separate browser tab, modal overlay,
resizable split, localStorage-persisted state, keyboard shortcut, and a
URL-selection-backed reopen button) with pros/cons for each.

> 1+3 sounds fine

Chosen: options 1 (fix-in-place with a reopen tab) and 3 (slim collapsible
sidebar) combined, not option 1 alone - over a floating/draggable panel, on
the grounds of fixing the actual bug with the least new surface area. Filed
as plans/developer-inspection.md's section F, item 3.

> [2026-09-28 correction] An earlier write-up of this entry named only
> option 1 (a separate chat-header reopen button) and was implemented that
> way. The user clarified "1+3" meant both combined: Close collapses the
> panel to a thin, always-visible strip (option 3) rather than removing it
> and relying on a separate button to bring it back (option 1 alone). Fixed
> in plans/developer-inspection.md and rebuilt.

## 2026-09-30: Developer Mode inspection clarity

> It's confusing knowing which view I'm currently looking at. "Agent 4" does
> not imply my player agent ... Similarly Agent 1 does not imply the GM.

> I see no "Turn" view ... I would expect "input" to be some part of the
> inference view, not muddled in the turn view. I would also expect input to
> come before output.

> Humans don't compute hashes ... Lets make "copy" just an tiny icon in the
> corner that doesn't take up any horizontal space.

Developer Mode must foreground game concepts over database identifiers. Agent
links and history headings name the player/NPC/Game Master role and character or
group context when applicable. A chat entry opens a titled Turn: an ordered
execution summary whose inference links lead to the detailed Inference view.
Only that detailed view shows the reconstructed input, before its output. Model,
timing, token, formatter and integrity evidence belong in a compact technical
details disclosure. Record serialization is an HTTP transport format, not an
inspection view, and is not rendered as raw JSON. Tool calls lead with their
tool name; opaque call IDs remain available only where needed to follow a real
relationship. Copying exact text remains available through an accessible icon.

## 2026-10-01: Developer Mode turn clarity

> The GM's inference has a "Current scene packet" in json which is not human
> readable ... Either way there is unnecessary duplication here.

> The GM appears to have narrated the user entrance before their character was
> ready or called begin game. What triggered this inference??

> Clicking on the GM's narration didn't work.

> Can we make technical details section hidden in a "..." triggered popover
> from the top right of the card?

> How can this tool call be better displayed where we see what the model
> generates and then tool call results. If there is nesting, then I would like
> the UI to be able to show that too visually nested.

GM scene state is model input rendered as labelled game facts, while the exact
backend-formatted prompt is technical evidence and must not duplicate it in the
main inference view. Only creation-ready characters participate in presence
narration. A delivered GM narration keeps its producing inference relationship
so it opens its Turn. A Turn shows each model-generated tool call once, with its
Rust result and linked GM decision nested beneath it; opaque call IDs remain
durable persistence validation, not user-facing content.

## 2026-10-04: Developer Mode finishing polish

> My messages in green are always printed twice ... Reloading the page shows
> just one message.

> The agent chat history does not update automatically when I have it open.

> [The GM opening] is not actually me, the user ... What about calling it an
> "instruction"?

> When I use the back button ... can we remember the scroll position or would
> that need to be recorded in the GET args and get messy?

Developer Mode reconciles the immediate local chat echo with its durable server
event, refreshes an open raw history after committed inspection changes, and
labels game-authored model input as **Instruction**. Scroll position belongs in
browser history state, so URLs continue to name only shareable inspection
records. Compaction has one yellow, collapsed presentation in turns and raw
history; the durable underlying timeline remains unchanged.

## 2026-10-06: Silent player-agent completions

> In developer mode I would like to see an empty response as something
> explicit. In chat, we just drop the empty response entirely and the player
> can type in their next message.

> An empty response as the first reply to a turn would be an error. Not fatal,
> because the player can re-prompt, but this would be highly unexpected from
> the agent and it should invite scrutiny to investigate why it happened.

An empty normal model completion ends a player-agent turn. It remains a durable
inference and assistant-history event for Developer Mode, which names it as no
player-facing response; player chat omits it. A completion with no preceding
tool call is anomalous rather than fatal and logs durable turn/inference context
for investigation. The player prompt explains the silent completion option
when another game message, such as a GM narration, already communicates the
result.
