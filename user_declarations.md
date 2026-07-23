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
finish a conversation. This may require the GM to see a Storyteller note on an
NPC or Location about whatever it is that's the win condition.

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
question the agent should forward to the Storyteller. In turn this may divulge
some enriching arbitrary history or provide a useful clue. Whichever it is, the
Storyteller must record it.

Again, I like the idea that the agent is a friendly entity to the player, both
helping tell the story but also on the player's side and trying to help. Much
like the GM in a real RPG, but in this case with separate context to improve LLM
performance, balance and fairness.

When a player dies they stop being able to interact with the world. They must
either request a new character (the player agent can make a tool call to create
one, that will require confirmation) or the must wait for other PCs to find a
way to revive them, if that's even possible in the Storyteller's world.

## GM interaction

The GM agent handles tool requests from characters - both NPC agents and player
agents.

Single GM vs multiple? If we had one, or rather a single chat history for it,
this might cause contextual confusion if many players travel to separate
locations. So far the GM seems to resolve local interaction, which by name is at
a location. So we will technically have multiple GMs. The text here may refer to
one, but it's always implied to be the one for the character's location. The GM
manages sub-location dynamic state and arbitrates character interaction as per
the game rules. Easy to change later too.

In the simplest form, a player might wants to attack an NPC. The basic attack
tool call is very rigid and has a deterministic outcome. The GM arbitrates
whether this is possible given their current situation. If granted as-is, the GM
simply approves the request and rust can trivially compute the result. The GM
result for a requested attack should include an optional difficulty or
situational modifier to allow granting but with modification. If not granted,
the GM returns a text explanation for why it's not possible, not so simple and
may describe extenuating circumstances that change how this action should
happen. The GM should suggest alternatives the player has at their disposal. The
player's agent then relays this, but may also suggest further ideas as above.

The GM has the option of applying further environmental or situational
consequences. For example the attack may be loud and draw the attention of other
NPCs in the location. In this case in addition to approving the attack, the GM
has its own tool calls that it can make.

Initially, there will be no absolute positional tracking of characters. The
Location, its NPCs and Storyteller prompt should imply relative layout that the
GM can expand on.

A more complicated example might be the player wanting to make an improvised
action. In this case the GM must decide what can happen, make an appropriate
dice roll for the result and narrate it. Doing so likely consumes the player's
action for a turn and could progress time.

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
far). This may need some playtesting.

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
chats that are compacted (chat_0 through chat_n).

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
  - Time
  - Location - both a reference to the location object and a string description within it
- Non-player characters
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
- Items
  - Description
  - Relevant stats, if weapons/armor etc. (type safe/structured)

All game objects, including the World, will have a GM notes string for short
term dynamic state. The GM will be encouraged to edit these frequently to track
where characters are, whether there are applied conditions or rules it should
follow. This becomes state fed into the GM prompt separately from its chat
history.

Similarly, all game objects, including the World, will have Storyteller notes.
Most importantly, this allows the world to evolve. E.g. a player asks a question
which is yet to have an answer. The Storyteller creates an answer and records it
so that it stays consistent as the game progresses.

The GM may see the Storyteller's notes on directly relevant objects, but cannot
edit them. It is up to the GM to maintain consistency with whatever the
Storyteller decrees. The Storyteller should not see the GM's notes or it would
be overloaded with unnecessary detail.

## Turns and time

With many PC actions, the GM returns how much time has progressed. The GM may
decide since the players were only chatting, not much time has passed. Rust
keeps track of this in the world state, but this is not shown in the UI.

For a strict time system, every character (NPC and PC) would have a current
time. Any character action would advance their current time. The world time
advances to the maximum of all character times. Then all characters take turns
to catch up in order of their current time (resolving ties with the combat DEX
save if there is combat). However, it may be inconvenient to process time for
all NPCs at the same granularity. I'd lean towards first saying any combat at a
location must finish first. Next that NPCs in other locations may only act when
whole hours pass.

An easy multiplayer solution: players would have a greyed out send button in
their chat until they can take a turn. Making actions out of order when out of
combat is probably fine as long as they don't get too far ahead (e.g. when doing
something for an hour they should not be allowed more actions until other
players have decided what they will do). They may pre-type what they want to do
though. A more complex one would allow asking GM and Storyteller questions. This
may trigger note updates (see the quatum resolution) out of order mid-combat,
but that's probably OK. Not sure how to handle the UI there though because the
chat may or may not be a question. Something to decide on later.

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

## Spell ideas

In addition to the Cairn rules, the following spells would be uniquely useful
given the technology of this game. However, given their utility and power they
should be later game, volatile or difficult to cast.

- **Read mind** is a tool call that gives the NPC's internal chat history as context to an LLM that produces a stylized summary. NPCs could even cast this on PCs!
- **Command** can inject a thought directly into the chat of an NPC or PC. Players get to type this directly. If cast on a player it replaces their turn with the injected text.
- **Erase mind** can delete one or more chat entries from an NPC, depending on dice roll success magnitude.

## Chat scheduling and priority for GM and NPC agents

# Tech stack

- Rust
- mistral.rs GPU Inference, maybe with with Sao10K/L3-8B-Stheno-v3.3-32K Q4_K_M
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

There is a button to enable developer mode for a world. This mode cannot be
disabled and becomes immediately visible to all players once enabled. A big
warning modal must be accepted before changing to developer mode, noting that it
enables complete visibility into internal game state and is effectively
cheating. Developer mode enables complete inspection of chat logs from all
agents - the Storyteller, GM and NPCs.

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

**Polish ideas**

Ideas for the future, after the basics are implemented.

- For actions/saves the player rolls for, we could have a dice modal popup with
  a rolling animation and result. If players want to roll their own dice, the
  world owner could allow all players to enter their results (depending on their
  judgement of their friends' honesty).

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

## Developer Mode

Developer mode should split the main view into two columns. The regular chat on
the left and all world objects, descriptions, agents, chat histories navigatable
and displayed on the right. This should be available to all players. Again,
developer mode is a one-way operation. For real games, players should not see
this data as it is meta-gaming and would ruin the experience.

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
