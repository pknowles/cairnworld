# Project Overview

This project is a tiny multiplayer web game.
Players join and interact just by chatting.
It is an RPG (role playing game) where the GM (game/dungeon master) and NPCs are LLM agents, and each player chats to an agent to perform actions.
I want the ruleset to be super simple and promote emergent gameplay.
The plot will tend to be generated around chatting and detective-like investigation, uncovering plots played out by NPC agents.
NPC and PC agents may only interact with the world using a strict set of LLM facing tools (like MCP, but without a separate "server").
This acts as a boundary for LLM hallucinations corrupting game state, means each agent may focus on honestly helping the NPC or player.
The GM agent then has less responsibility and can focus on its own job without conflating context.

Philosophical tangent: most of this could happen in a single chat of a very
large model (well, ignoring multiplayer), but in practice the context quickly
becomes overrun, hallucinations destroy the experience and the chat is all too
easily guided by the user's text simply allowing any action or plan to happen
without good consequences. The hope is the structure introduced here allow
better separation between characters, context management, fixed structure and
real dice rolls.

# Game setting and story narrative

The setting must make sense for the rules of the game. It doesn't always have to
be a "dark and mysterious Wood". The world, setting and plot are built by the
Storyteller agent (there is only one) with a Questioner agent (a concept used
frequently in this project). In practice these are just roles in chats that may
not see the entire history from each other. However they should be aware of each
other's role and the process so that they understand what they should be doing.
World building is done through iterations of meta-questions and questions.
answers and summaries.

The Questioner is first given the input world prompt, some example questions and
some hard coded meta-questions which it must initially ask itself and expand
into concrete questions, ultimately making a list of questions for the
Storyteller.

The Storyteller must answer each question in turn with a high LLM temperature
(if the model supports it). This expands the seed and story variance. Once done
it is asked to make a world summary. Only the summary is returned to the
Questioner, which makes follow-up questions. The Storyteller runs again, but
this time starting only with the world prompt and its most recent summary (the
answers from the previous session should now be in the summary). Again it
answers each in turn and makes a new summary. It is told that it may drop ideas
from the previous summary that no longer make sense or work for a cohesive story
after reviewing the previous answers. This process loops until the Questioner is
satisfied the world feels complete or some limit is reached. The final "world
summary" and the world/campaign name is the product.

The looping above is split into the following phases. The questioner is simply
prompted with these phases, which will flow through to the Storyteller.

- Overall setting, factions, NPCs of high importance, motives
- Expanding to important locations, where the NPCs start, how many other NPCs in their party, supporting NPCs at each location
- Expanding to intermediate locations, less important NPCs and world details to make the setting feel alive

Possible future idea: *freeze* summary sections after each phase to avoid the
model having to regurgitate the same text that likely didn't change.

My thinking is the Questioner is like an explicit reasoning model where we have
fine grain control over how much thinking is done before the final output is
accepted. Maybe there's already technologies that could do this more robustly.
I'd be interested to know.

Some examples of meta-questions and questions are below. Testing the
initialization loop with a few of these and evaluating would be important. Maybe
trying to further consolidate too.

- **Meta-questions and seed questions**
  - What are the steps of Joseph Campbell's the hero's journey?
  - What are the steps of Dan Harmon’s story circle?
  - What makes a good RPG setting?
  - What makes a good detective story?
  - What types of characters make dramatic and interesting mysteries to uncover?
  - What kinds of supporting characters make the world feel more real and immersive?
  - What questions need to be answered to give characters real personality, beliefs, values and ambitions?
  - What settings fit well with a world that has these game rules?
  - What are some proven storytelling structures?
  - For some of the ideas above, what are some ways to incorporate the rule of three writing principle?
  - What other questions should we answer that would help make this setting and story interesting?
  - What questions should a designer ask to determine whether a scenario contains meaningful choices?
  - What questions reveal whether one option dominates all the apparent alternatives?
  - What questions expose hidden assumptions about the player’s resources, abilities, knowledge, or incentives?
  - What questions test whether the central conflict arises from genuinely incompatible goals?
  - What questions help determine whether NPCs are coherent agents rather than scripted obstacles?
  - What questions should be asked about the information players need to make informed decisions?
  - What questions reveal whether clues and discoveries create opportunities rather than gate progress?
  - What questions determine whether the environment meaningfully affects decisions and outcomes?
  - What questions test whether character abilities and equipment matter without trivializing the whole scenario?
  - What questions distinguish a meaningful cost from one that is merely nominal or irrelevant?
  - What questions determine whether different approaches produce genuinely different trade-offs and consequences?
  - What questions should be asked before resolving an action with a roll or other mechanic?
  - What questions reveal whether failure changes the situation rather than merely delaying success?
  - What questions test whether players can improve their position through preparation, knowledge, equipment, or creativity?
  - What questions help identify decorative, redundant, or disconnected scenario elements?
  - What questions test whether the scenario remains coherent when players behave unexpectedly?
  - What questions should be asked from the perspectives of cautious, aggressive, compassionate, and exploitative players?
  - What questions reveal whether the scenario rewards judgment rather than guessing the designer’s intended solution?
  - What questions determine whether the scenario is as small and simple as it can be without losing depth?
  - What questions provide the strongest adversarial review of an open-ended RPG scenario before play?
  - What questions should a designer ask to uncover categorical flaws in a small, open-ended RPG scenario, especially flaws involving dominant solutions, hidden assumptions, incoherent motivations, irrelevant costs, gated information, weak consequences, and illusory choice?

- **More specific questions**
  - What will the conflict be between the players, their goal and other NPCs?
  - What structure(s) should this story take? E.g. beginning/setup,
    middle/confrontation, end/climax/resolution Chiastic? Aristotle's ("A
    beginning is that which is not a necessary consequent of anything else but
    after which something else exists or happens as a natural result. An end on
    the contrary is that which is inevitably or, as a rule, the natural result
    of something else but from which nothing else follows; a middle follows
    something else and something follows from it. Well constructed plots must
    not therefore begin and end at random, but must embody the formulae we have
    stated."), Complication and dénouement? Horace's 5-act ("It should favour
    the good, and give friendly advice, Guide those who are angered, encourage
    those fearful Of sinning: praise the humble table's food, sound laws And
    justice, and peace with her wide-open gates: It should hide secrets, and
    pray and entreat the gods That the proud lose their luck, and the wretched
    regain it."), One-act play like Cyclops, Freytag's pyramid
  - What promise does the opening make to the players, hooking their interest?
  - What central question will keep the players wanting to investigate more?
  - What is the story really about beyond seemingly simple material acts?
  - What idea, tension, or contradiction exists in the setting to explore?
  - What elements should be expanded to make this a good detective story setting?
  - Which characters are directly involved with the plot(s)?
  - Which characters indirectly know about the plot(s) and do they care enough to act on the knowledge?
  - What makes this world fundamentally different from ours?
  - How does that difference affect ordinary daily life?
  - Who holds power, of what kind, and how do they maintain it?
  - What does this society value, fear, and condemn?
  - What major conflicts and tensions shape the setting? Describe two to three.
  - What historical events created the present situation?
  - How do geography, climate, and resources shape the culture?
  - What institutions govern law, knowledge, religion, and magic?
  - What inequalities or contradictions does the society depend on?
  - Which parts of the setting directly create problems for the characters?
  - Which parts of the setting align and aid the player characters?
  - How does each character interact with the world? I.e. in terms of their wants, actions, impact. In a good setting, _everything_ is connected.
  - For some of the ideas above, what are some ways to incorporate the rule of three writing principle?
  - What does each involved party want, fear, and refuse to sacrifice?
  - Why can’t everyone’s goals be satisfied immediately?
  - Would the situation remain interesting if no dice were rolled?
  - Is any solution clearly safer, cheaper, easier, and more rewarding than all others?
  - Are supposed costs actually meaningful within the scenario’s timescale?
  - Does every viable solution require some combination of sacrifice, risk, leverage, discovery, or ingenuity?
  - Does character creation meaningfully change which approaches are available or attractive?
  - Are we accidentally assuming every character owns an item that trivializes the problem?
  - Does a useful ability or item solve one obstacle, or collapse the entire scenario?
  - Do NPCs respond according to coherent motives, or behave like fixed-price puzzle mechanisms?
  - Can NPC behavior be predicted reasonably when the player attempts something unexpected?
  - What information must the player know to make an informed first decision?
  - Are important opportunities discoverable through ordinary observation and interaction rather than gated behind arbitrary checks?
  - Does each environmental feature create a decision, opportunity, danger, or source of leverage?
  - Could any location, object, clue, or NPC be removed without materially changing play?
  - Before calling for a roll, what concrete negative consequence is the character risking?
  - Does failure change the situation enough to prevent consequence-free repetition?
  - Can preparation, positioning, equipment, patience, or negotiation remove the need for a roll?
  - Do different methods—compassion, deception, force, stealth, compromise—produce meaningfully different consequences?
  - Can the scenario survive the player ignoring the apparent objective or intended solution?
  - What would the most cautious, exploitative, compassionate, and violent players each do first?
  - Is the interesting part deciding what to do, rather than discovering what the designer expects?

## Storyteller initialization output and tools

At some point the storyteller must be able to initialize game state. The world
summary is one output. It must also create locations, paths between locations
and NPCs at each location. The Storyteller now has tool calls exposed to it to
populate locations and paths. Then it must populate NPCs one at a time.
Locations, paths and NPCs require custom seed text to fully describe what they
are and their purpose in the setting/dynamic story. The NPCs must be populated
one at a time to allow the model to give longer input to each.

For each NPCs location and path, the Questioner concept is again executed to add
detail to each. The Questioner is given the world summary and the seed prompt
provided by the Storyteller.

## Character creation

When player characters first enter the world, character creation begins
immediately. The player's agent must guide players through creating a character
with a rich background but must communicate with the Storyteller agent, which
has an agenda. The Storyteller's goal is to make the player relevant to the
world setting, connecting them to events, items, locations and NPCs. It must
also avoid the same connections as other players. The Storyteller again works
with the Questioner to promote richer ideas for setting and story integration.

I'm not sure exactly how this interaction will be implemented. We'll have to
experiment with a few ideas and see which gives the best results. My first idea
is to have the player's agent ask the Storyteller for three background ideas.
The storyteller must make three background and "scrub" the story spoiler
components from them. The player will not see the tool call result directly. The
player agent then describes the options but gives the player the additional
option of suggesting changes or even their own idea. The player's agent then
gives the player's changes/counter idea to the Storyteller, which attempts to
adapt it to the story, then return a single obfuscated/spoiler-scrubbed result.
This process continues until the player is happy with the background, at which
point the player's agent can assign the background and move on.

IIUC most of the Cairn character creation is otherwise well defined.

One future idea is to allow the player to negotiate more powerful abilities or a
wealthier start, if they accept a penalty or character flaw. The Storyteller
must approve such a request. Another idea is to allow the Storyteller to suggest
a blind penalty where the player's request is accepted and the Storyteller
applies the penalty to the character sheet directly (likely a story/setting
related background penalty that makes the campaign harder for the character).

## End Conditions

The campaign completes when the storyteller says the players won or when all
players are dead.

The storyteller must run periodically so it can check for the win condition.
This should happen when any relevant information may change. I tentatively think
this should be a tool call from the GM. E.g. when combat ends because the
players defeated the villain, or when the players return an item to someone or
finish a conversation. The GM should "update" the storyteller about these bigger
events, which will immediately let the Storyteller make a decision to end the
game. This may require the GM to see a Storyteller note on an NPC or Location
about whatever it is that's the win condition. Even items or NPC interactions of
significance, once interaction is concluded the GM should send a short summary
to the Storyteller.

## Epilogue sequence

This is a game mode that's set, after which no actions can be made except for
talking.

I'm not really sure how this will work yet. I would like players to have some
input to decide what their characters will do. Players may like to read what
NPCs end up doing too, and may even appreciate being able to chat with NPCs to
decide what next. It can be assumed players can directly chat to any NPC they
reference, i.e. no need to travel first. Some ideas are below.

All NPCs are sent a canned message saying the campaign has concluded and this is
now epilogue discussion (this is just added to their chat and they do not get a
chance to act unless also spoken to by players).

All characters receive the Storyteller end game narration. Player agents then
receive a prompt to ask the player what they would like to do next, noting they
may discuss with others.

Once all players call a ReadyToEnd tool call (only available during the
epilogue). This call takes a complete summary of what the players has decided.
The Storyteller is given the summaries for each player and narrates a final
epilogue. This is shown in the world detail page. Player chat boxes are disabled
and once they close the game page they won't be able to return.

# Agents

An agent is just chat history. We will make an effort to compact it, which is
important because there will be many. I.e. every NPC. We likely want to page out
inactive NPC agent chats to disk to avoid memory pressure.

## Player agents

All players interact with the world purely by chatting with their agent and its
singular chat history. The agent's job is to help the player succeed in the game
and above all have fun. The agent can prompt them to explore when they're stuck,
remind them that they can ask the GM or Storyteller questions to uncover more of
their perceived environment before acting. Doing so may require active skill
checks. For example, the player might ask "can I see any signs of a secret
compartment?" which is more of a local environment question that the agent
should forward to the GM. The GM would then answer it and make sure it is
recorded appropriately in the location notes. Perhaps their character knows
something of a building's history but without asking the player would never
know. For example: "has my player heard of this place before" would be a
question only the Storyteller can answer. The GM must recognise this and defer
to the Storyteller. In turn this may divulge some enriching arbitrary history or
provide a useful clue. Whichever it is, the Storyteller must record it.

Again, I like the idea that the agent is a friendly entity to the player, both
helping tell the story but also on the player's side and trying to help. Much
like the GM in a real RPG, but in this case with separate context to improve LLM
performance, balance and fairness.

When a player dies they stop being able to interact with the world. They must
either request a new character (the player agent can make a tool call to create
one, that will require confirmation) or the must wait for other PCs to find a
way to revive them, if that's even possible in the Storyteller's world.

Each player chats with their own agents, but some messages, such as GM narration
at a location would be queued and visible to all players at that location. This
way, a player may attack an enemy. The GM may queue the narration of it while
returning the approval. All other players get to see the result of the action
made by the player.

## GM interaction

When players begin play they should have an initialized location reference and
description of where they are within the location. The GM immediately greets
them by narrating the scene, their surroundings and any plot-relevant details
related to things they can see or are arriving at. E.g. "you enter/arrive
at/pass the gates of ..." and "somewhere in this city is probably ...".

The GM agent handles tool requests from characters - both NPC agents and player
agents.

Single GM vs multiple? If we had one, or rather a single chat history for it,
this might cause contextual confusion if many players travel to separate
locations. So far the GM seems to resolve local interaction, which by name is at
a location. So we will technically have multiple GMs. The text here may refer to
one, but it's always implied to be the one for the character's location. The GM
manages sub-location dynamic state and arbitrates character interaction as per
the game rules. One possible pitfall is when characters move from one location
to another, the GM in the new location would not have their recent context.
Given the GM should provide the Storyteller with frequent encounter summaries,
this may not be a problem - i.e. the new location's GM still has enough context
to do its job. Easy to change later too.

The following are a few examples that should serve to define the interface. In
the simplest form, a player might wants to attack an NPC. They say that to their
agent. The agent issues the tool call. The basic attack tool call is very rigid
and has a deterministic outcome, but first it must be granted. The GM receives
the tool call with a new global action ID (just nextId++ set on the request by
rust) and arbitrates whether this is possible given their current situation. If
granted as-is, the GM simply makes an approve-action call with the ID. Using an
ID allows forwarding the tool call data and reduces LLM responsibility to copy
it correctly. The arguments should be visible though, e.g. maybe the character
has an injury preventing them using a two handed weapon and the GM needs to see
all arguments to be able to reject the request. Rust can trivially verify the ID
matches, compute the dice roll and the result, which is broadcast to players'
chats in the current location. The approve-action call may include an optional
difficulty or situational modifier that the GM deems appropriate, even adding,
subtracting or limiting possible damage. The GM will see the result of the tool
call and should issue a narration of the result, which is also broadcast to
local player chats. If the GM does not grant the request it does not narrate
anything and instead returns a text explanation for why it's not possible, not
so simple, or may describe extenuating circumstances that change how this action
should happen. The GM may suggest alternatives the player has at their disposal.
The player's agent would then inform and help the player understand and might
suggest a additional alternatives of its own. If the action was approved, the
agent should already see both the dice roll result, the GM's narration and the
success return value of the tool call and probably doesn't need to say anything
more to the player - its output can be empty.

Since the GM sees the tool result it has option of applying further
environmental or situational consequences with additional tool calls at its
disposal. For example the attack may be loud and draw the attention of other
NPCs in the location. Maybe the attacked character drops their burning torch
onto flammable ground. Maybe other NPCs must make saves to avoid being forced to
flee. All just examples.

Currently I have no play for absolute positional tracking of characters. The
Location, its NPCs and Storyteller prompt should imply relative layout defined
by the GM's game object notes. I expect the game mechanics should simply avoid
needing such state. Area-of-effect abilities are a possible future concern
though.

A more complicated GM interaction example might be the player wanting to make an
improvised action. In this case the GM must decide what can happen and make an
appropriate dice roll based on a difficulty assessment, then apply effects using
tool calls and finally narrate it. Doing so likely consumes the player's action
for a turn and could progress time. I expect gameplay testing to uncover the
required GM tool calls for this over time.

The GM agent's role in this project is a little simpler than a real GM as it is
split between the Storyteller. The GM resolves immediate interactions, whereas
the storyteller decides longer term interaction.

Possible idea: What if the GM agent was not told which characters are NPC and
PCs, to make the world interaction more realistic and appear fair. For example,
ChatGPT generally allows the player to do anything they want and rarely enforces
restrictions. In practice the Storyteller makes the game fun, so the world isn't
necessarily fair, but local interactions (which the GM handles) must remain
consistent for a more immersive experience.

Who decides how difficult an encounter should be and what control do they have
over adjusting it?

Thoughts: In a real RPG it'd be the GM. In this case it might be more of the
storyteller's job. The players may also need to be told how difficult an
encounter might look. There should be some encounters that are very easy and
some that are impossibly hard - that players must be able to recognise and
avoid. The number of enemies is the simplest control, but this should be set by
the Storyteller. The equipment, weapons and skills of enemies can be set at
their time of first use or when players first see them (quantum dynamics style,
as long as they are constrained to be consistent with player observations so
far). I'm kinda leaning towards a collaboration: the GM knows when an encounter
might begin and should make a Storyteller tool call to get some guidance. The
Storyteller may in turn create NPCs for the GM and then the GM would adjust
their attributes/equipment/behaviour "as needed" (a common RPG phrase) based on
the Storyteller's instructions for the encounter. The GM may then narrate the
look/difficulty. This may need some playtesting.

## Dynamic Storyteller

These are currently ideas for the future. Not the initial version.

New NPCs can enter the story, perhaps as some exit or die. Locations can be
added, new paths can be found or some paths can be removed (e.g. a fallen
bridge). These are initiated by a structured GM tool call, and succeed when a
dice roll meets a GM-provided success threshold.

The storyteller may implant ideas in NPC chats or append to their
background/description. E.g. NPCs may need to react to player interaction or
other NPCs in more complex ways to make the story interesting. This can happen
organically with individual NPCs progressing their own chat history. However,
the Storyteller has opportunity as it is aware of the broader context and can
make decisions to make the setting and plot more interesting and fun for the
players. For example, the Storyteller may notice PCs preferring certain kinds of
play style and can increase the frequency and richness of the world in just that
area for them. NPCs going off the rails may also need Storyteller guidance to be
put back on track.

## Agent syntax and tool calls

Agent loop:
- Send conversation, instructions, and tool definitions to the LLM.
- The LLM returns either ordinary text or one or more structured tool calls.
- Application validates and executes those calls.
- Append the tool results to the conversation.
- Run the LLM again.
- Repeat until it produces a final response.

Agents can call other agents. Each agent has its own separate context. In most
cases agents have their own chat history that follow the same chat compaction
rules as all others.

In some cases action IDs are used to refer to and forward requests. This reduced
LLM responsibility forwarding arguments, allows for validation and also avoids
hidden/implicit state of "the most recent request" for example.

Agent calls can be recursive. E.g. the GM may be asked to approve some action
but in turn it may need to verify with the Storyteller, receive the response and
then return the approval result.

Recursion must be a tree. It is an error for an agent to call another already in
its stack. Errors must propagate all the way to the top. Silent failures and
default values are strictly forbidden. This goes for production too! If
something fails the end user gets the full error stack complete with context and
details of what happened so they can report it. Errors should encapsulate the
context of their stack as they propagate so everything is trivially traceable.

## Chat compaction

LLM context is limited. When the total chat history token count is greater than
some threshold, a portion of the chat history must be summarized/compacted:

- Before: [summary_0, chat_0, ..., chat_n, chat_n+1, ..., chat_n+k]
- After: [summary_1, chat_n+1, ..., chat_n+k]

Where the number of raw chat messages in front of the summary (k) must not
exceed some character count threshold. The ideas being:

- Compaction is expensive, so do it infrequently
- Keep some raw chat history after the summary as this is likely more important to keep accurate

This must be done with care so that the summary only summarises the portion of
chats that are compacted (chat_0 through chat_n). I.e. for the compaction
operation, the LLM should not see newer chats than those being compacted.
Another way of thinking of this is that its history is temporarily truncated
while it produces the summary, then the raw chats since are added back. When
compaction happens, the agent itself is given a prompt directing/describing what
to summarise. It needs to know what information will always be static and up to
date, what will be lost and most importantly which information is important to
keep. Many interactions in the chat are temporary and would not need recording,
but some are not. This will likely need gameplay testing to optimize.

## Notes editing tools

Much like coding agents editing a file, notes may get bigger than LLMs can be
expected to reproduce faithfully.

Test options and pick one or a combination that works best:

- Provide line range replacement
- Provide paragraph index replacement (risky as the index could trivially be wrong)
- Add a review stage, where the model gets to look at the result, or maybe some
  small context around the replaced region for verification and can undo it.
- Store notes as key/value pairs and the whole value can be overwritten.
  Actually this sounds pretty solid.

What happens when notes get too big for context? Providing a list of note names
and a query tool relies on agents knowing to query the name and they might miss
things. Something to explore.

## Consistency Checks

Unproven future idea to test: would it be useful to regularly inject a question
to the Storyteller (and maybe the GM too?) asking if all game objects are
consistent with the story. Asking small models this sounds risky, especially
asking them to make changes.

# Game state

Game objects
- Player characters
  - Name and ID - the ID is generated from the name and used by agents in tool
    calls to uniquely identify the character
  - InCombat, Moved and Acted booleans for combat actions
  - Time
  - Location - both a reference to the location object and a string description within it
  - Character sheet info, including background
- Non-player characters
  - Name and ID - the ID is generated from the name and used by agents in tool
    calls to uniquely identify the character
  - InCombat, Moved and Acted booleans for combat actions
  - Description: background, motive, ambition, how they fit into the world,
    implies how they should act
- Locations
  - Type - an unstructured string, e.g. "city", "river", "dark wood", "clearing".
  - Description - details everything characters can interact with
- Paths
  - Connecting location references {A, B}
  - Normal travel time
  - Description of what is along the path in the order from location A to B, whether it is blocked and what would be needed to pass.
- World state
  - Time
  - Initial prompt (just for record; unused except for initialization input)
  - Storyteller summary
  - Next action ID
- ItemType
  - Description
  - Relevant stats, if weapons/armor etc. (type safe/structured)
- SpellType
  - Description
  - Relevant stats (type safe/structured)

There could be an Item object with a Location|Character reference, and/or a
fixed player inventory. Probably the former, since it'd be stored in the
database, keeping the player inventory as an implicit query and making sure any
modifications are implicitly validated to maintain inventory type/limit
constraints appropriately.

Rust union may be beneficial for handling/processing items.

All game objects, including the World, will have a GM notes string for short
term dynamic state. The GM will be encouraged to edit these frequently to track
where characters are, whether there are applied conditions or rules it should
follow. This becomes state fed into the GM prompt separately from its chat
history.

Similarly, all game objects, including the World, will have Storyteller notes.
Most importantly, this allows the world to evolve. E.g. a player asks a question
which is yet to have an answer. The Storyteller creates an answer and records it
so that it stays consistent as the game progresses.

The GM may see the Storyteller's notes on objects at the location/path, but cannot
edit them. It is up to the GM to maintain consistency with whatever the
Storyteller decrees. The Storyteller should not see the GM's notes or it would
be overloaded with unnecessary detail.

TODO: a path is just another location. Could consolidate or union them. Using a
separate name would probably help LLMs understand the difference. This will be
better because then neighboring locations could simply connect without requiring
some intermediate path that must be traveled.

## Turns and time

With many PC actions, the GM returns how much time has progressed. The GM may
decide since the players were only chatting, not much time has passed. Rust
keeps track of this in the world state, but this is not shown in the UI.

For a strict time system, every character (NPC and PC) would have a current
time. Any character action would advance their current time. The world time
advances to the maximum of all character times. Then all characters take turns
to catch up in order of their current time (resolving ties with the combat DEX
save if there is combat). To facilitate the per-turn single move and single
action mechanic of Cairn, characters have a boolean to mark whether they have
moved or acted.

However, sticking to the above strict time tracking for the whole world may be
inconvenient. We'd have to process time for all NPCs even though consequences
would be unrelated. Drawing on quantum mechanics, there's no point processing
until the outcome could affect anything. I'd lean towards first saying any
combat at a location must finish first. Next that NPCs in other locations may
only act when whole hours pass. I.e. the game and implementation may be better
if we do not follow strict time ordering, but we can be close to it with rigid
rules. To clarify, rust code will define when players are allowed to act, not
the GM.

An easy multiplayer solution: players would have a greyed out send button in
their chat until they can take a turn. Making actions out of order when out of
combat is probably fine as long as they don't get too far ahead (e.g. when doing
something for an hour they should not be allowed more actions until other
players have decided what they will do). They may pre-type what they want to do
though. A more complex one would allow asking GM and Storyteller questions. This
may trigger note updates, quantum resolution style out of order mid-combat.
That's probably OK because the duality collapse just happens earlier than later.
Not sure how to handle the UI there though because the chat may or may not be a
question. Something to decide on later.

When time advances, main NPCs in other locations may get a chance to take
actions, chat, organise and plot with other NPCs. While rare, this could allow
NPCs to suddenly enter a location or an encounter along a path between locations
while players are there.

If the time of day changes such as sunrise/sunset/darkness, the GM should
narrate this to the player in the next chat response. Narration is an optional
tool call the GM can make, which queues up the message, is played back
immediately visibly to the player and the player agent can see this (in the same
chat) before the tool call result is displayed to it. Time of day may in turn
affect the difficulty the GM decides some actions such as perception checks
should be.

We need to design the game rules so the GM can have all PCs complete their turns
up to the advanced time, with allowed room for error. During combat this would
be strictly turn by turn and time advancement could probably be ignored
entirely.

When one player does something for a few hours, another player may get many
turns to do something while they wait, e.g. have a conversation with some NPC in
the same city/town. They shouldn't be forced to do actions until the time adds
up perfectly. I guess the GM could effectively just ask what they want to do in
the meantime and waiting could be a perfectly valid response. This probably
means every PC has their own time variable as state, which then advances up to
the global world time through actions or waiting.

I think being in and out of combat needs to be a distinct state. It would be
nice if not - time could simply advance by a set turn time. However, per the
Cairn rules, there is an explicit limit of one move and one action per turn that
only applies while in combat. It would be good to enforce this with rust flags
so characters can't accidentally cheat and so we activate chats in turn order.
I.e. out of combat, players and NPCs could often act simultaneously as long as
their current time does not get too far ahead of the others (in which case they
wait for the others to catch up, i.e. their chat is blocked). However, when
in-combat only one player should have an active chat box at once and their turn
should be implicitly over after making one move and one action, or explicitly
skipping the rest of their turn. There is also a DEX roll to determine turn
order, which is made only at the start of combat. Combat is somewhat ambiguous -
starting when violence or active hostilities begin. Sticking with matching real
RPGs my conclusion is to have the GM make the call to begin and end combat. This
probably needs a prompt reminder and description for events to look out for that
indicate the start and end. A worry is that this state may be tricky for the GM
LLM to track. One idea is to add some safeties such as reminding the GM or
forcing combat when an attack roll is made. Then reminding the GM if a character
attempts an action that takes longer than a turn. A further worry is if some
characters in a location become out of sync with their InCombat state. This may
need some reworking and play testing. I.e. I'm not sure what ideas will work
best so we should try a few that includes complicated situations where there may
even be NPC or player bystanders, maybe also in the same Location but not in the
same area within it, and pick the one that works best.

# Tool calls

Many tool calls will have a short description that the player agent sees. When
used, the GM is provided with an extended version from the rulebook. This
prevents the GM's context from being overloaded with the entire rulebook text
for all actions. For example, when a player attacks the GM will receive the
weapon description and relevant rules from the rulebook to resolve the
interaction. The same idea can apply to tool results, where additional context
and rules are provided on a need-to-know basis.

Many tool calls imply a roll will be made. Rolls are made by rust code, not
LLMs! When implemented PC rolls will be made in the UI by players clicking a
"roll" button - this is purely theatrical.

## Character creation actions

See [character
creation](cairn/second-edition/players-guide/character-creation.md) in Cairn
second edition.

- ChooseBackground
- RollBackground - if not choosing a background; the agent offers the player a choice
- RollHitProtection
- RollAttributes
- RollTraits
- RollBonds
- RollAge
- ReadyToBegin - called when the player has finished character creation

ChooseBackground and RollBackground are only available when the Storyteller is
not being used, e.g. during initial development (see An initial proof of
concept).

## Character actions

- Move - the character describes going to a new position in the current
  location. This can be during or out of combat. If out of combat, the GM
  narrates how far the character gets if not all the way, anything they see
  along the way if applicable and the scene once they arrive. If in combat,
  follow the move rules of combat. The GM augments the move action with the time
  the move takes, ideally being consistent with the described layout of places
  within the location in the location notes.
- Travel - like move, but takes the argument of a Path (if the character is at a
  location) or Location (if the character is on a path). The GM either narrates
  how far the character gets from their current position to the connection in
  the current location/path if not all the way (this is a rejected action), or
  verifies the character can move to the beginning of the path and narrates them
  beginning travel (action accepted). Rust then moves the character's location
  to the Path and re-initializes their description within the location to note
  the location they entered from. The character then implicitly performs the
  Travel tool call, which goes to their new Path's GM. Following the same flow,
  the GM there narrate them moving to the location at the other end of the path,
  or how far they get if they are interrupted (e.g. by event/ambush). Once Path
  travel has completed, rust again moves the character location to the
  destination, sets the location-description and they are greeted by the new
  location's GM with a description of their surroundings (see above).
- Say - just raw text that the GM is told the character wishes to say, with an
  optional target for who the character is addressing. The character agent may
  need to translate a general desire into a specific sentence. E.g. "I want to
  ask if the bartender has heard of any unusual events recently" -> "Hey
  NPC-name, have you noticed anything unusual recently?". The GM would typically
  just accept/forward this to the NPC provided they can hear it. The GM add an
  additional list of characters who overhear what was said.
- Lie - the same as Say, except the speaker and all characters who hear it make
  a WILL roll. Those that pass receive a canned note saying they think
  Speaker-Name may be lying. This opposed check diverts from Cairn rules, but
  fits the game mechanics here better. Player agents may confirm with the player
  if they intend to be lying before making this tool call if there is possible
  ambiguity. NPC agents must use it appropriately/honestly.
- Attack - using a particular weapon
- Retreat - implies a DEX save
- Give/Drop/Place - initiates an item transfer
- Take/Request/Pickup - sends a request for an item; if from another character,
  the GM may forward that request to the character's agent for approval. If it's
  from a PC, the player's agent must ask for player approval, which may time out
  after a minute. A timeout is an error that propagates. The triggering
  character agent receives both the timeout error and the narration that the
  other character just stands there motionless.
- Look/Investigate/Open/Ask - more of a catch-all generic action. The character
  agent may want more information about their surroundings from the GM, to
  clarification something previously said or to actually spend time searching
  for something. The GM may additionally require a save or advance time. The GM
  may reject the request saying that that this is the middle of combat and would
  cost an action and may leave them more vulnerable to attack if the choose to
  proceed. The player should be able to acknowledge this and make a second
  request to proceed regardless. Note that this example is a rejection with a
  suggestion followed by a retry with an acknowledgement. The GM LLM must be
  capable of performing this little dance as it will be common during play and
  custom situation resolution. There is no structured "retry"; my hope is that
  the GM agent will recognise the retry, particularly if its prompt implies this
  proceedure and the player agent's prompt suggests to include the text "risk
  aside/nevertheless, spending the action to...". An alternative would be a
  separate confirmation dialog.
- Wait - skips the remainder of their turn if in combat, waits a given amount of
  time. This could default to waiting to catch up to world time (e.g. waiting
  for another PC to finish doing something). If the wait is significant (i.e.
  not in combat), the GM should quickly narrate the wait, e.g. what they see
  while waiting.

The Give/Take actions are a formal way to let rust transfer items in the world.
The idea is to avoid the risk that the GM fails/hallucinates and an item is
duplicated or lost.

On multiplayer party movement. Having individual players Travel would suck.
Ideally the first PC to Travel from a location results in other players being
asked if they want to stay behind with a one minute timeout (making the default
keeping the party together). Then travel can be performed as a group with the
GM's narrations broadcast to all those traveling. To make this happen, the
travel request could include a list of characters to travel with that the player
agent populates. Other players in the list get the chat request where they can
say they'll stay. NPCs could be included (they would need to accept the implied
invitation to join the party). Icing on the top: if any character declines to
travel, the original player should have the option of aborting travel. I need to
decide if we have a confirmation dialog for this. The simple solution is any
player rejection propagates and the player must ask again, this time
specifically excluding those that rejected. Then we have an awkward duplicate
ask for staying behind for the players who are included in travel the second
time too. Maybe auto-dismiss if they haven't added anything to the prompt since
the previous question was asked?

### NPC character actions

- BePersuaded - during normal conversation between characters, the NPC character
  agent may call this, with their own chosen difficulty modifier. It takes the
  persuading character as an argument which defines the WILL roll made. This
  makes character stats meaningful and adds a little randomness to the
  persuasion.

Saves to persuade/convince an NPC to do something needs careful handling as NPC
agents are their own entity. The first idea that comes to mind is to skip the GM
entirely and have the NPC agent initiate the roll. We would include the in NPC
agent prompts that they are an NPC and when someone is asking something of them
they should make a tool call to decide if they should be convinced. I added
BePersuaded. They are in charge of adding the difficulty modifier, which could
be implicitly affected by previous player interactions. The reasoning is that
agent's chat history has the ideal context for whether they really would be
convinced. They see the roll result and a "you have been persuaded" or not, as
the case may be.

## GM tools

- Save - makes a character roll a given stat to pass a check, defend against
  something etc. For example when the player wants to look for something, the GM
  may make them save to find it. The save roll should take a very short string
  saying what the save is for. The GM may apply difficulty modifiers. Not to be
  used for persuasion (see BePersuaded)
- RollInitiative - the same as Save, except this takes a list of characters and
  writes the results to their objects so that they make actions in the correct
  order as time updates.
- SpawnItem - the GM may create items and loot on-the-fly. As an example, the
  act of looking for loot may remind the GM that some loot should exist, but
  only if the players succeed a check. Creating an item after-the-fact is fine
  as long as it makes sense for it to have existed all along. This tool call
  must be approved by the Storyteller or it is rejected with the Storyteller's
  reason and the GM must decide what to do differently.
- SpawnNPC - if combat is too easy or there's a skeleton hiding in a chest, for
  example, the GM may use this to request a new NPC be spawned at the location.
  Like SpawnItem this must be approved by the Storyteller.
- TakeDamage - characters may take environmental damage. For example from traps,
  falling, burning etc. This is strictly separate from damage from attacks and
  allows the GM to create more interesting obstacles and situations. The damage
  roll and possible save are programmable and decided by the GM. The GM should
  be aware of how much damage would be lethal and know that they need to have
  presented sufficient warning to players about consequences before applying
  this.
- BeginCombat - takes a list of characters, sets their InCombat flag and they
  all roll DEX for turn order.
- EndCombat - unsets the characters' InCombat flag.
- UpdateStoryteller - send the Storyteller a text summary of some
  event/interaction/resolution that just happened or completed, letting the
  Storyteller make actions, update notes in turn.

Saves could be rolled immediately by rust code. Character agents do not roll or
resolve saves. If a player's character is making a save, we may want to add an
interactive roll feature to the UI.

Trapped chest example. The storyteller leaves a note on an important item saying
it is in a hidden and trapped chest in a hut at a location. When the player uses
Move to enter the hut, the GM uses Save to have the player to notice a chest
poorly concealed by a cloth. When the player tries to open the chest, the GM
makes them Save again to detect the trap and rejects the open request if they
succeed, with the message that they notice a trap and stop before proceeding. If
they fail to notice the trap, the GM uses TakeDamage as the penalty, unless they
can roll a DEX save to jump out of the way in time. When the player finally
opens the chest, the GM describes the item they found.

## Storyteller tools

- EndWorld - concludes the game for all players with a text narration of the
  immediate ending and begins the epilogue sequence.

## Spell ideas

In addition to the Cairn rules, the following spells would be uniquely useful
given the technology of this game. However, given their utility and power they
should be later game, volatile or difficult to cast.

- **Read mind** is a tool call that gives the NPC's internal chat history as context to an LLM that produces a stylized summary. NPCs could even cast this on PCs!
- **Command** can inject a thought directly into the chat of an NPC or PC. Players get to type this directly. If cast on a player it replaces their turn with the injected text.
- **Erase mind** can delete one or more chat entries from an NPC, depending on dice roll success magnitude.

Future idea: In rare cases the storyteller may create plot-relevant spells that
can be discovered or learned.

# Tech stack

- Rust
- mistral.rs GPU Inference - we should try a few models to see which fit best out of the box
- Axum
- Tokio?
- sqlite
- Cairn SRD second edition ruleset
- RMCP

# UI

## Landing page

The main/landing page has a banner and description of the game.

Without loggin in there is only a log in button.

After logging in in with OAuth2, e.g. a google account they see more. We only allow oauth; there is no user/password storage and accounts are simply keyed by email address (this is not visible on any page). The main/landing page allows players to change their name, has a button to create a world, lists worlds they have created. Each world takes them to the world detail page.

## World detail page

Each world is a game instance. When users create a world it's a new playable
game setting. When they do they can optionally provide a seed prompt that is fed
to the Storyteller agent for world initialization. This prompt is hidden by
default but can be expanded.

There is no enforced limit on the number of worlds that can be created.

The world/campaign is given a name after initialization. Below it is a world
status of in-progress or complete. Below that is a recap for the current player.
The recap is written by the player's agent after the player logs out and does
not return for 60 seconds. It does not persist in the player agent's chat
history; it's only for the world recap.

If the world is complete it will have a short epilogue, describing what each
player ends up doing. This will be written by the Storyteller. TODO: detail how
this works. It should involve player agents briefly asking players what they'd
like to do next. Future idea: have the Storyteller write and maintain a complete
high level story of what plays out so the whole campaign is recorded and
presented at the end. This would be the perfect input prompt for an epilogue.

Clicking on a world from the main page goes to a world detail page. The player
that owns the world can create special invitation links so their friends can
also join the world. There is no email invite; only a unique link to the world.
Friends must log in and click a Join button to accept the invitation, after
which they are redirected to the world detail page. Players may optionally
specify a limit for how many friends may use that link to join the world. For
the owner, the world's detail page lists the links with remaining slots left.
Links can be deleted at any time, revoking their use if accidentally posted
publicly. Links just allow players to join; there is no relation once joined.
All players can see the names of all other players who have joined the world.
This is a tree view and under each player are their list of characters. The
world creator can remove joined users. Their association and characters remain
but until they get another link to join they will be unable to access that
world.

Next to each player is a shortcut Join button to enter the world with that
character.

When a player joins a world they are automatically given a new character, with a
placeholder name "Adventurer". The stats are undefined until they enter the game
with that character, at which point the player's agent will guide them through
character creation, with the help of the Storyteller, asking them to name it at
the end.

There is no character detail page. Instead, characters would ask their agent to
describe it.

Dead players will be marked with an icon. They can even still join the world,
after which they may either wait for their companions to revive them or ask
their agent for a new blank Adventurer character, which must be approved by the
Storyteller.

There is a button to enable developer mode for a world that only the owner can
click. This mode cannot be disabled and becomes immediately visible to all
players once enabled. A big warning modal must be accepted before changing to
developer mode, noting that it enables complete visibility into internal game
state and is effectively cheating. Developer mode enables complete inspection of
chat logs from all agents - the Storyteller, GM and NPCs.

## World game page

This is the main page where the game is actually played. It is simple: one big
chat history, a box at the bottom to enter text and a send button.

When players join, the GM agent narrates their entry, e.g. entering the
camp/walking up to the party from along the path/through the tavern
door/whatever fits. When players close their browser the GM narrates them
leaving. This happens after a 1 minute timer to allow for the case where all
players close their browser at once or they come and go in groups. The GM can
consolidate the narration. If no players remain, no narration happens until at
least one player joins and then narration happens for the transition to the new
state after one more minute. At any time, players can ask in chat "who else in
my party is here?" and their agent can respond with the appropriate tool call to
check.

LLMs generate tokens over time. Streaming this to the browser would create a
good user and developer experience, even if the end result requires parsing
intermediate tool calls.

**Polish ideas**

Ideas for the future, after the basics are implemented.

- For actions/saves the player rolls for, we could have a dice modal popup with
  a rolling animation and result. If players want to roll their own dice, the
  world owner could allow all players to enter their results (depending on their
  judgement of their friends' honesty). This is entirely theatrical, for the
  player experience of feeling part of the process and seeing their character
  stats come into play. A one minute timeout could fall back to rust code just
  making the roll for the player.
- Confirmation dialog? E.g. the GM says "you might fall to your death; are you
  sure?" Better yet, this is a GM message in chat with a text response. The
  problem with this is how to handle non-binary responses. Having the player
  agent interpret would be ideal but we will hit the recursion bug where the
  player's agent is expecting a response for the initial tool call but instead
  sees a confirmation request. Will need to think about it.

# Debugging and Telemetry

All games must be recorded in full. Data from old playthroughs can be useful in
future testing. Real chats can be used as example data for validation. We can
see if similar issues have happened before or even track when certain
patterns/regressions started appearing. We should record the entire LLM chat
history, summaries included, but in a way where we can reconstruct the identical
input and output. This includes tool calls implicitly since they are all json.
Since compaction produces summaries covering a known range, we simply record the
range of summaries and the raw chat index after which the summary was used as
input instead of the previous history. Given an LLM inference result/chat output
we need code to query and provide the input. We then have unit tests to verify
the record has been made correctly. These chats will eventually become large. We
will need a way to extract and archive them by date or age so we don't lose
everything when we reclaim disk space. Archiving with compression should be
efficient.

I have a hunch that sifting through logs for cases where the chat output worked
particularly well will eventually allow us to fine tune LLM models to produce
better output.

I expect the most difficult part of this project will be managing LLM context
size and generation performance. We must track input and output token counts and
processing time for every inference operation. The cumulative values for strings
of operations must also be available. For example, the player tells their agent
they want to attack an NPC. Their agent makes a tool call. The GM must approve
it. The result must also be narrated by the GM and perhaps side effects happen.
Every step adds up, which takes time. If we make a mistake and context size
blows up for one of the agents we need to know.

It will be common for a coding agent to want to test a prompt fix against many
chat histories. A useful debugging feature could be to reference a specific LLM
output message (maybe even save it to disk for later testing), replace the
history to match the prompt changes and then re-generate. Ideally it would be a
complete replay of its chat history, provided it's not too long.

## Developer Mode

Developer mode should split the main view into two columns. The regular chat on
the left and all world objects, descriptions, agents, chat histories etc. are
navigatable/queryable and displayed on the right. This should be available to
all players. Again, developer mode is a one-way operation. For real games,
players should not see this data as it is meta-gaming and would ruin the
experience.

In addition to the raw agent chat histories, we'll need some structured debug
views:

- There needs to be a button on chat entries on the left to inspect them, which
  will open a sequence view on the right. This will be the full tool call stack
  through all agents involved in responding to the player's message. Clicking on
  a dice roll should open the sequence view that the dice roll was involved in.
- As mentioned earlier we need a way to reconstruct all that went into an LLM's
  response. Messages in the debug view should link to an inference view that
  shows the entire verbatim input given to the LLM and its returned output.

Shortcuts between views are important for navigation. E.g. when looking at a
sequence view (which only shows the direct request/responses from agents),
developers may want to see the agent's raw chat history before that. E.g. to see
the overall flow of the chat that a GM agent sees, not just the short
request/response. This is different to the inference view because the chat
history would be infinite scrolling raw chats and would not include additional
agent context and summaries from compaction.

In general the aim of developer mode is to include everything, all the raw data
verbatim. This can be a bit overwhelming so some parts may be hidden. We should
include an expandable section in raw chat histories to view summaries at the
points compaction has occurred.

Chat compaction is the result of LLM inference, so summaries will have an
inference view too, again, showing the exact input and output.

## Live Coding Agent Interaction with MCP

The game data must be trivially accessible to coding agents working on the
project. Moreover, agents may want to test features and repro bugs quickly
without writing temporary scripts. MCP that claude+codex could talk to directly
may speed up development and I imagine could reuse the exact same LLM tool
interfaces that game agents use.

# An initial proof of concept

Before jumping into the deep end, we should implement a minimal version of this
game, text and explore. This will hopefully reveal directions that work and
don't work early on.

The smallest setup I can think of skips the Storyteller initialization and hard
codes the setting, NPC(s), location(s) etc. Then we can jump straight to testing
basic game mechanics, interaction with agents and the GM.

A simple example was generated by GPT: [Bread Thief](scenarios/bread_thief.md).
This can serve as a base for hard coding initial game objects and GM input
prompts. Being able to save and load the world to json would be great for this.
E.g. a CLI utility to manipulate the database - DRY/using project code of
course. Then we could check in a playable scenario to git to initialize a world
with. Maybe even for automated testing, ideally where we can assert tool calls
are made in rust, but maybe also where an LLM evaluates agent output.

Ideas to make the first round of implementation even simpler:

- Skip the epilogue sequence entirely. The game just ends. We use the
  Storyteller's world ending summary as the epilogue summary instead.
- Skip the background part of character creation with the Storyteller (it won't
  work well if the storyteller didn't create the scenario anyway). We just
  populate character stats as per the rules.
- Skip the intro with Mara entirely and jump straight to the encounter. The
  intro could simply be summarised as a location note that the GM can narrate
  for the player's MO. The win condition can simply happen when the player has
  the flour and is free to leave. The loss condition would be when the flour is
  gone or destroyed. Storyteller notes can all be pre-written/hard coded in the
  json file we use to initialize the world.
- The hut and Mara are both in the same Location, so there is no travel between
  them. The location notes will need to specify the hut and Mara are not close
  so the player would need to Move from one place to the other.

# TODO

How will experience and character growth happen?

The GM should be given an initial RollOmens call to make after all players have
entered the game, but before the game starts. This means there needs to be a
sync point where the GM waits until all players have called ReadyToBegin.
Players should be warned by their agent only to call this when all players are
online and have entered the game, otherwise the GM will need to bring them in
later in the campaign.
