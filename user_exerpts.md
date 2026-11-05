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

## 2026-10-17: Runtime boundaries and build output

> an app specific feature in mistral.rs?

Applied interpretation: application features and persistence belong in
Cairnworld; mistral.rs changes must provide generic runtime capabilities.

> can we not just have ONE build directory please without LLM sprawl??

Use Cargo's normal `target` output root rather than adding custom build roots.

> you needed to have told me you made changes there

Report dependency-repository changes and any required push explicitly so the
application is not left depending on an undisclosed unpublished commit.

> can we not have random installs littering this project and have the bare
> essentials installed by me globally?

> please clear your broken dependencies. I ONLY want a regular rust build dir

Development tools belong in the user's normal machine/user installations.
The user performs global installations. Ask before downloading or installing
additional tools; do not provision a private toolchain under `target`.

## 2026-10-18: Cargo tools and dependency changes

> you may use cargo to install normal cargo stuff. it is not a free pass to
> hack everything through cargo if it's not meant to be

Normal Cargo CLI installations are authorized. This does not authorize using
Cargo as a substitute installer for unrelated system tools.

> proceed. keep mistral.rs changes separate. you might want to fetch and see if
> upstream has what we need too

Keep inference-library changes and commits in the mistral.rs repository;
Cairnworld retains its dependency pin, application tests, and documentation.

## 2026-10-18: Development secrets

> new secret - save in  some ./secrets env file matching where it should be read
> from in the previous dev env

Keep development OAuth values in Git-excluded `secrets/dev.env`, exported as
`CAIRNWORLD__` environment overrides through the existing configuration library.

## 2026-10-18: Allocation work order

> Finish runtime restoration, then fix allocation separately

The measured growth of temporary CUDA workspaces after startup remains a gap
against the startup-only allocation declaration. Complete Windows runtime
restoration first, then address the complete working-set reservation in the next
plan. Fixed paged KV capacity remains required throughout.

Updated 2026-10-19: the user revised Memory Usage to require bounded allocation
and a launch-time capacity check: "I.e. we compute a maximum we will use and
make sure it's free." Full upfront allocation is preferred where supported,
rather than mandatory. This supersedes the startup-only requirement above.

## 2026-10-20: NPC speech, interruption and game-turn allowance

> Lets go with everyone hears for now by default, but the GM may override it.
> The GM chooses who may intervene. I may change this later after play testing.

> I guess the player needs a "oh nevermind" response to an accidental interrupt
> too. Can this simply be a button rather than a tool call?

The assistant proposed a direct Never mind button releasing the player's pause
without inference, undoing actions or restoring allowance. The user accepted:

> Good call. Once a player has finished their turn (e.g. using their action/move
> or with a call to Wait), we should mark their flags as consumed so they cannot
> delay arbitrarily.

This applies to the character's RPG game-turn, independently of agent chat-turns.
Closing a game-turn consumes its remaining move/action allowance. Interrupt and
Never mind do not reopen it; allowances reset only for the next game-turn.

> Can you update the todo to implement these features now? I think getting NPC
> interaction is the right next step. Maybe this needs moving items forward or
> maybe it can be inserted. Whatever fits most cleanly

NPC interaction precedes the coding-agent interface and the remaining rules
expansion. Existing browser and configured-model paths can verify it directly.

## 2026-10-20 Shutdown and crashes

The earlier request to continue interrupted operations was refined after
comparing arbitrary crash continuation with finishing turns before shutdown:

> perhaps server shutdown should just be made to happen outside of turns at
> pausable points so we don't have to save/restore or keep lots of state in the
> database? in this case a crash mid turn would be considered a failure and
> should be rare rather than common/expected.

The assistant recommended finishing accepted turns during planned shutdown,
preserving completed effects after exceptional crashes, and cleaning up
unanswered tool history without automatic replay. The user accepted:

> sounds good. please update the plan

This decision replaces automatic continuation of a crashed agent call stack.
Durable work already between safe steps may still resume after restart.

### 2026-10-20 Failure results in history

While reviewing crash cleanup the user stated how failures are recorded:

> all failures get inserted authentically in the chat. better to have an agent
> see an error than be confused. this follows our coding stanards

> yes, players should see the error as this is not a game state failure but a
> system failure

On how a failed action should reach its caller (the assistant first misread this
as the approval call itself failing; the user corrected it: the approval worked,
and the GM is shown the result of the thing approved, whatever it was):

> gm approved; the approval worked fine. the GM should always be shown results
> of things they approve whether success/fail/error; then the caller should
> receive the error instead of a result and can continue.

> I said the GM sees the *result*, regardless of whether it's an error or a
> regular result.

For system failures:

> yes, this should fail the whole turn and the player(s) should see the error.
> no contiuing. agents must also see this in their chats too, otherwise we run
> into the same issue of missing tool call results etc.

On where errors live:

> Errors should relate to items in the history so an error is always linked to
> the thing that was happening at the exact time it happened. this should also
> make displaying it straightforward. I.e. if a message is added to the chat
> history, it has already happened and thus no error. If a tool call happens
> that needs a result, we should always have a result or an error recorded in
> the chat history that is agent visible anyway.

Decision (2026-10-20): the assistant proposed storing player-visible system errors
either by deriving them from `sequence.error` and the trigger message
(`message.trigger_sequence_id`) or in a new error table. The user had first said
"I think we do need an error table for these cases... the actual error would be
tied to the system 'instruction' prompt. However without developer mode we should
still display system errors to players", then, shown that every sequence already
has such a trigger and error:

> 1 sounds good. lets record this discussion and decision in the plan too

Selected: derive from existing records, no table. See plans/archive/server-shutdown.md.

## 2026-10-21 NPC speech selection and GM approval

After considering sequential delivery without special overlap mechanics, the
user retained GM batch approval and simultaneous responses:

> I see - so we already have some batching for the GM's approval planned. I like
> this. You know what, lets keep the simltaneous response then. I think we need
> GM approval for speaking as who hears it and whether characters can speak is
> very fundamental. This is the point of the GM; there is no getting around it.

> I'd like eagerness to be small, to encourage occadional race conditions :).
> Maybe 1-3 only. Not 100.

The user also clarified that replacing GM-voiced NPC dialogue with NPC-agent
responses is implementation work, not a policy question:

> 4 isn't really a question.

The revised declaration permits the GM to describe overlap with a prefix.
Selected tied replies share their pre-response history and may be delivered in
any order. Keep mandatory GM approval; no automatic overlap marker is required.

### 2026-10-21 Individual approvals and expressive speech

This supersedes the batch-approval choice above, retaining tied speech composed
before hearing the other selected replies:

> Re. batching and whispers I think we should remove batching afterall. Cool
> idea but that we are finding awkward edge cases is the design trying to tell
> us we did it wrong.

On character-authored phrasing and delivery style:

> Giving player agents the ability to add style and phrasing will greatly
> improve an otherwise dry game. Moreover the GM and NPCs get to see how
> something is said which can add subtlety to the communication. Hard coding
> "Name says" was a mistake and not something I asked for.

The user accepted committed as the intent distinction:

> committed is great. updated

The declaration now allows individual GM approvals in any order for tied speech.
Tentative means willing to pause for an objection; committed means proceeding
despite objections, while actual prevention and ordinary action rules still
apply. ConditionalApprove remains speculative, not an implementation requirement.

## 2026-10-22 Code quality checks

The user requested a new plan, retaining the old archived plan:

> the first 3 sounds important. can you make a new code quality plan - leave the
> old one - that adds modularity/arch checks, complexity and duplication?

The scope clarification was:

> no idea where tooling enforcement came from. perf measurements are useful but
> I don't remember asking for it; same for many of the others

This authorizes the three checks and the changes needed to satisfy them. The
other removed architectural/performance proposals are not reinstated by this
request. The new plan retains the recorded numerical limits; any proposed change
to scope or limits must be presented to the user rather than made to obtain a pass.

## 2026-10-23 Interrupt is a raised hand

The user simplified the pause design and said what Interrupt means:

> it should be like putting your hand up in a teams meeting because you want to
> say something at the very next available opportunity. the interrupt button is
> toggled on
>
> it is tied to the player who interrupted. if another player says something
> before the interrupting player, inference moves on and the interrupting player
> may need to re-interrupt

and asked for no "paused group" concept, group lock or world lock: Interrupt only
stops NPC responses for now, and Never mind lets them continue. This replaces the
earlier per-character hold protocol with live pause acknowledgement.

Updated 2026-10-29: asked whether another player's message should clear a raised
hand or leave it up, the user chose to keep it raised ("Keep it raised") so the player
can finish typing. This replaces the "may need to re-interrupt" expectation above.

## 2026-10-24 Perceptions and narration audiences

The user clarified that delivery and response must be shared across action kinds:

> yes, start thinking holistically about how everything has to fit together when
> implemented, not just the current slice

The prefix remains content-specific:

> prefix is speech only. doesn't make sense for ALL actions. maybe for others
> but not all. like - I make an attack. how would yo think prefix makes sense

The user's correction to the self-response exclusion was:

> character’s own speech -> own action

For narration, the user identified the missing audience control:

> the GM currently has no tool to distinguish this I believe. I wonder if we
> need some narration that is given to the player making an action, a reaction
> or all players capable of perceiving. More generally, the GM occasionally
> needs to provide different narration to different characters.

After noting that narration currently comes from the GM's final text, the user
expressed a preference for distinct model-facing tools:

> Something for a description given exclusively to the character whos turn it
> is. Another that takes a second parameter for characters watching. It is my
> hope separate tool calls encourage the GM LLM to route appropriately as I
> expect it would otherwise over-use optional parameters.

Updated 2026-10-24: after discussing the agent loop and avoiding a useless
closing inference, the user superseded the separate-tool design:

> ok, I think the GM must use a structured final response then, probably much
> like its other tool calls I assume. Narration would always come last and it
> must do all narration in one batch, picking from one of a few structures,
> like I described

The standard form was clarified:

> The standard example would be one narration simply given to all characters
> in the group

Selected design: the GM's structured final answer contains one narration batch
with distinct group, actor-only and explicitly addressed forms. It is not a
tool call and requires no separate end call or closing inference. The earlier
separate narration tools are superseded; the distinction between audience forms
is retained. The shared-detail setting guides ordinary narration but never
widens an explicitly private delivery.

Updated 2026-10-24: the plan had written the final answer as an open-ended
array of narration entries. The user corrected it:

> I specifically said pick ONE narration configuration. It is not an unlimited
> batch. Some narrations take two outputs for different observers.

and, on the `kind` discriminator it used:

> should the structured output not be a tool such as "narrate_group": ...?
> unless "kind" is specifically what qwen was trained on.

Design interpretation: four terminal narration tools (group, actor and others,
addressed characters, nothing), exactly one per answer and at most two texts. The
model is trained on tool-call syntax, not on a custom JSON discriminator, and the
existing argument validation applies. The same exchange settled that "action"
keeps its meaning of any character action; Cairn's combat sense gets another name
(for example action allowance) when combat is implemented, without a glossary.

## 2026-10-24 Distinct names for general GM requests

The user clarified:

> Oh - one thing I've been noticing is agents thinking "look" is an appropriate
> generic action. it is not. it's a terrible name to use for general actions or
> questions.

After the assistant proposed separate Look, Investigate, Open and Ask tools
sharing implementation, the user accepted:

> that sounds good. please add that to the plan

Design interpretation: preserve the actual requested intent in tool names and
recorded GM requests, with shared schema/submission/arbitration. Ask is directed
to the GM; questions spoken to another character use Say/Whisper. Separate names
do not require separate action execution or perception systems.

## 2026-10-24 GM editing and incidental detail

The user required both whole-value writes and text editing from the first
working implementation:

> both - same tools you get for editing files. whole value is great for many
> fields but terrible when the fields are lareg and only a small change is needed

The initial use case was:

> the first working example should support player movement within a location,
> however do think about the design holistically as many more fields will exist
> for the GM to keep track of.

The user corrected the assistant's assumption that incidental detail needed
separate objects:

> I'm heading towards a hybrid of items in the database for stuff that really
> matters and incidental objects just stored as notes.

> In most cases I think the storyteller will be the one creating items. The GM
> would handle local interaction and add richness and detail to the world that
> is mostly inconsequential to the overall plot/setting

The user accepted recording changes before final narration. Existing placement
fields and GM notes hold current state separately from history; tools should
make committed edits available to the next inference. This does not authorize
a post-narration save loop or automatic object creation. The later declaration
also says not to start with a separate review stage. Ordinary read/edit tool
results are part of the agent loop, not an extra review system.

## 2026-10-24 Narration cases for review

On the narration tools:

> narrate_nothing  is fine. we can tweak later if it doesn't work.

and on naming:

> re. action - we have a word for action and the cairn action must be named
> differently - llike you said. don't introduce a glossary just for that lol.

Reviewing the proposed cases, the user corrected what `narrate_nothing` is for:

> sorry? how would an action be unnoticable to the character doing the action???
>
> lol. no; this is for the case where the GM believes the action is already
> narrated. a classic example is during conversation; the gm does not narrate who
> says what

On the opening narration and the meaning of the actor form:

> you mention opening narration. this would simply be a private narration to the
> character entering the scene.
>
> that's a point though - the GM should describe the initial scene to NPCs too.
> In fact, I'd prefer the GM gives a general narration to the group without any
> player present. Then the player joins the group and we can take the path of the
> GM narrating the player's entrance to the game. For the very first turn/entry of
> a new character, the player agent could be instructed to ask what the player
> wants to do after the GM's narration.

On an invalid narration:

> In developer mode there should be a visible failure. Not sure what you mean by
> no repair, but the GM should be given an error response to its final output and
> re-run inference. For development this failure must be addressed. In a live
> game, the GM may be able to recover in some rare cases. We should not treat this
> as normal. This should cause our test cases to fail.

Design interpretation: an invalid narration is an ordinary rejected tool call, or
for plain text an instruction message, answered to the GM, which runs again within
the usual limits. It is an error to investigate, logged, flagged in Developer
Mode and failing every real-model test, never a path to rely on. The scene-setting
and entrance proposal is recorded here as the user's idea; the plan does not yet
change the opening flow.

Decision, 2026-10-24: the user accepted the entrance flow and the actor definition:

> yes to entrance and actor

and deferred the open question on response opportunities after the scene is
narrated with no player present:

> I don't know yet how to handle response opportunities, so lets not build
> anything for this yet and just see what happenes. I need to think on it and
> possibly explore

The plan therefore narrates the scene to the group first, then the entrance with
the entering character as the actor, and adds no handling for NPC responses to
the scene narration.

## 2026-10-25 Narrate as the last call; InterventionCheck built

Decision for the user to confirm. The plan required the GM's narrate call to be
the only call in its response. On the real model that looped: the model sends
`approve_action` and a narrate call together and repeats the identical pair after
every rejection. The code now treats a narrate call as the last call of its
response, valid when every earlier call succeeded, and the GM is told in each
request and action result to end its turn with a narrate tool. The user's own
words on this: "can it trivially work if you just accept all tool calls with
narration as the final response?"

InterventionCheck and the `committed` flag on Take follow the plan's section on
the intervention lifecycle; the plan's verification cases for them were not
reviewed with the user first, so the scripted cases written are the ones in
`src/game.rs` (`a_warning_from_an_npc_can_stop_a_tentative_take_before_it_happens`,
`a_committed_take_goes_ahead_and_the_gm_is_told_it_is_committed`,
`an_intervention_check_only_asks_npcs_other_than_the_actor_about_the_presented_action`,
`a_proposed_reaction_must_be_decided_before_the_gm_narrates`,
`an_intervention_check_where_nobody_objects_changes_nothing`).

## 2026-10-26: Item transfers use intervention, not an acceptance step

Asked whether Give/Drop/Place was planned, the assistant said a give to a person
"must be accepted", which is what user_declarations.md says (Give/Drop/Place, and
the Accept action). The user replied: "not must be accepted. both take and give
can be intervened. The exact action could be for the reciever to say or gesture -
a clear no. Then the GM should reject the transfer." On the location description:
"this must be provided by the GM with the accept call. I already said this" (the
declaration says the GM provides it "with the approval"). On capacity: "this is
something for way later; I'll be happy if it just works now."

design.md and plans/playable-rules.md follow this: no Accept tool or transfer
continuation, only Travel keeps the suspended-consent protocol, and
`approve_action` carries the location description for a drop or place. The
declaration lines that still describe acceptance (Give/Drop/Place "If to a person
it must be accepted", Take/Request/Pickup's player-approval timeout, and Accept)
are now stale and need the user's update. Also earlier in the same conversation:
"Many have duplicate names for similar actions... the implementation should merge
them and have an enum to distinguish", which the `transfer` tool follows.

## 2026-10-26: InterventionCheck asks players; waiting is not pausing

The assistant said InterventionCheck only asked NPCs. The user: "no - this should
ask players too. can that be done?" (user_declarations.md: "asking players if they
wish to intervene and which action they will intervene with"). Of the two ways
offered, the user chose "Ask the human and wait for the answer", with: "Lets have
a required player response timeout config value. Make it 5 minutes to start with."

On wording: "I don't like the sound of 'pausing'. please make a note of this.
pausing implies the game is always running in the background, but it isn't. either
the server stays waiting, or we schedule a wake-up timeout job that chooses a
default response. the latter may be better in case the server crashes while
waiting for the timeout because on re-launch it'll recover the turn."

design.md ("Concurrency", "Player replies and coordinated travel") and
plans/playable-rules.md follow this: a stored deadline, a persisted wake-up that
records do_nothing, overdue replies answered first on restart, `[game]
player_response_timeout` required with no code default. The declaration's
one-minute window for travel and take-from-player is replaced by this setting.

The user then added to user_declarations.md (\"Players are expected to respond in a
timely manner\"): a configurable 5 minute timeout starts for a player only when the
game cannot progress because of them; at the end they automatically DoNothing/wait/
whatever idle response applies; after 3 in a row without any response they are
navigated back to the world detail page with a message that they were
disconnected for being AFK. design.md and plans/playable-rules.md follow this,
with the setting kept as `[game] player_response_timeout`.

On idle responses for travel: "I think an AFK character would travel with the
majority of the party, but otherwise do nothing and skip turns." This replaces the
design's earlier proposal that silent PCs join after 60 seconds.

Ties: "agreed. ties result in players staying".

## 2026-10-27: The GM is not told which characters are player- or NPC-controlled

user_declarations.md asks "Can we hide who is a PC and NPC from the GM?" and
frames it as a possible idea. The user then stated it as the rule: "the GM should
not know the difference. this is in my declarations. please fix this", and on
wording: "say 'player' where they mean character. I have made this mistake myself
a few times and this needs fixing". On controllers: "the player agent can act.
characters cant change" (a character keeps its controller; an away player is
covered by the player agent acting for them).

The GM scene packet is one `characters` list with the same fields for every
character; the GM prompts say character where they meant a controller; the
`[game]` setting is `characters_share_narration`. The player's and NPC's own scene
packets still differ in shape (`npcs` for a player, `present` for an NPC), and the
declaration could be updated to state the rule rather than ask it.

## 2026-10-28: NPC chatter: stop when there is nothing new, uniformly for characters

Asked to reduce NPCs endlessly answering each other: "I would ideally like NPCs to
say at most 2 things before calling do_nothing" (a limit, not a target: saying nothing
or one line is as good). On what should end a conversation: "once an NPC has said
what it has to say, there is no point rehashing the same old crap. this is what
should cause the conversation to wait for a player to respond. this is how
conversations work in every day life."

On how to frame the NPC: "I also have a hunch we should stop prompting NPCs that
they *are* the character and instead that they operate/control the character in an
RPG and it is their responsibility to play the best NPC they can." The measurements
(experiments/0005-npc-chatter.md) bore this out: the "operate" role is what lets an
addressed character answer while the closing question ends the exchange.

On the design: "one holistic solution. handle characters. not PCs vs NPCs" and "stopping
chatter is important though, and finding the right prompts to use is one of the
current higher priorities, beyond of course writing holistic simple code that does
not have edge cases and bandaids". So the question ends every NPC decision, with no
check on who spoke last. Open ideas the user suggested: answer when addressed or
when "a GM would have the character say or do something", and a "short and
to-the-point" preference.

## 2026-10-29: the shared-detail setting is invisible to the GM

The GM had been told the setting in a sentence of its scene text. The user, on seeing it:

> lol. that's not at all what I intended. how in the world did that happen?? can you
> remove that entirely and make it implicit? GM doesn't need to know about it at all!!!

and, after a first misreading (choosing the GM's tools by the setting), clarified:

> narrate tools operate the way they're described regardless. ignore sharing entirely.
> then, adding sharing is simple. if any player character sees a narration that is not
> private, the same narration is showing to ALL player characters. done.

Implemented in delivery (`Narration::players_share`), not in the GM's tools or text.

## 2026-10-29: Bread Thief item types and attack damage

> I added MoraleCheck and Split/CombineItem. We'll need half flour sack ItemTypes
> prepared. Damage from attacks should all be handled by the code and NOT the GM.

## 2026-11-04: A summary is the agent itself, and could reuse its KV cache

On whether the compaction summary request is a different model or the agent itself:

> can you confirm the summary request is an execution of the agent itself, not some other
> LLM? this may even be able to reuse the kv cache since the operation is basically
> temporarily discarding the last few messages and asking for a summary; then dropping the
> summary request and splicing in the summary output

Status (assistant's note, not a quote): the summary is the same agent, model and sampling,
and now starts with the agent's role text. The KV-cache idea is not implemented: the
backend disables prefix caching (`with_prefix_cache_n(None)`), and the summary request omits
the tool definitions and dynamic context, so it shares only the role text with the agent's
normal requests. Reuse would need the summary request to keep the same leading context.

## 2026-11-05: World generation targets small cards, uses IDs, and keeps every tool

On whether world generation should need the larger context:

> less context. same tools. make it fit. this needs to run on a 3070 GPU with WAY less
> RAM! like 5gb
>
> we cannot sactifice tools. some progress has been made in the main worktree to reduce
> size too. we should merge and re-branch soon. I'm planning to once you have small worlds
> working

On tool identifiers (the assistant had built them by name; the answer chose IDs):

> Switch to IDs

On merging each create/update pair:

> switching to a generic tool and enums might help. we did that for at least one of the GM tools

On the summary request and tools:

> prob doesn't need tools, certainly not the full description. doesn't hurt. opportunity to
> save some tokens maybe. could think about listing tools without descriptions since what an
> agent can control would be great context to infer what info needs to be kept (this might
> be useful in user exerpts)

The last is an idea to try, not a decision: the summary request could list the agent's tool
names without descriptions, so the summarizer knows what the agent can do and so what to keep.
Measured so far: with the Questioner's full tools in the summary request, the model answered
by calling one instead of summarizing.
