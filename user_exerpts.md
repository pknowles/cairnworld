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

## 2026-09-01: GM narration is already delivered context

> the player agent must see that it was narrated and know that it was sent to
> the player verbatim already. it can then ask me what I want to do. Could we
> even tag GM's narration as "GM"?

GM narration is a distinct, durable player-visible entry. The player agent
receives the exact narration only after it has been sent through that channel;
it must guide the player's next choice rather than repeat or invent the scene.

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

An inline, non-model-facing chat notice records every compaction and the case
where only the retained tail remains. Repeated notices make a bad threshold or
one-off huge message visible without interrupting play.

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
