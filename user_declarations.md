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
  - How does the classic two-guards/two-doors logic puzzle work?
  - What is the River Crossing Puzzle (Wolf, Goat, and Cabbage)?
  - What is the solution to The Water Jug Measurement Puzzle with 5 and 3 litre jugs?
  - What are the key components that make a good, interesting and satisfying logic puzzle and how do people go about designing them?
  - Write a procedure to design something similar; would following it produce a similar result.

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
summary is one output. It must also create locations like cities, roads,
dungeons, connections between locations, NPCs and items at locations etc.. The
Storyteller will have tool calls exposed so it can populate and edit locations.
Then it must populate NPCs one at a time. Locations and NPCs require custom seed
text to fully describe what they are and their purpose in the setting/dynamic
story. This must be done in stages, especially NPCs, one at a time to allow the
model to give focused descriptions to each. Rust should validate as much as it
can, where explicit relations/IDs are given.

For each NPCs location, the Questioner concept is again executed to add
detail to each. The Questioner is given the world summary and the seed prompt
provided by the Storyteller.

Introducing randomness in LLMs is difficult. One thing I'm tempted to try is
injecting a pile of random nouns, adjectives (and maybe a few numbers in
different scales) into the prompt that may trigger more variance without relying
on LLM "temperature" to do all the work. They would simply be declared as random
items to draw from. Maybe it'd just confuse the LLM but worth checking.

## Character creation

When player characters first enter the world, character creation begins
immediately. The player's agent must guide players through creating a character
with a rich background but must communicate with the Storyteller agent, which
has an agenda. This would be a system prompt so the player is immediately met
with an intro, questions to answer and decisions to make. The Storyteller's goal
is to make the player relevant to the world setting, connecting them to events,
items, locations and NPCs. It must also avoid the same connections as other
players. The Storyteller again works with the Questioner to promote richer ideas
for setting and story integration.

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

Once character creation is complete and the player says they're ready to begin,
the storyteller should give the exact location where the new character enters
the world, and their group. The GM then narrates their entrance as usual.

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

The GM manages dynamic state of characters and their location and arbitrates
character interactions as per the game rules. Mostly approving/modifying action
requests, narrating results and keeping the world consistent.

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
a location. So we could technically have multiple GMs. It'd be nice to have the
GM's context continue with the players (assuming they stay as one party). It'd
also be nice to have multiple GMs to handle unrelated NPC interactions at other
locations. Having a GM move with players when they travel (i.e. it keeps its
chat history) would add consistency, but we run into the problem of possible
multiple GMs at a single location which should not be possible. At this point we
could consider GM location hand-offs or merging GM histories. A better idea is
to have GM inferences use a dynamic context/chat for all characters in a local
group. This is like a shared experience history for all those involved and the
GM can focus on just those interactions. As a bonus, GM narrations would only be
sent to characters within a group and can be shared. Any characters joining
become part of the group, share chat history and compaction. If a character or
characters leave they form a new group, which takes a copy of the history of the
previous group so far. Groups are fungible so there is no difference between a
split being one new group or two new groups with the old one being deleted. This
design allows separation between groups of characters at the same location that
may not be interacting. When they need to interact their groups must be merged.
This means both chat histories get given to the GM. When compacting merged
histories we should prefer to compact individual group histories first, before
making a compaction summary for the merged group's histories - we should have
customizable threshold to tweak. I.e. if all pre-merged histories are small,
there's no point compacting just them. The text in this document will refer to
one GM, but it's implicit that it only has context for the group of players
interacting.

The GM must be aware of other groups at the same location and can issue calls to
merge or split them as their players make actions that would cause direct
interaction.

The GM should provide the Storyteller with encounter summaries when major events
happen. This could be a battle, when quest/story-relevant items are acquired or
lost etc. There should be an explicit way for the storyteller to mark items as
such and tag them with triggers for when the GM must send the storyteller
summaries of encounters/events.

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
matches, compute the dice roll and the result, which is broadcast to the
character group's chats. The approve-action call may include an optional
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

Can we hide who is a PC and NPC from the GM? This would be the ultimate in
fairness. Would it make for a better game or not? Not sure. A big concern would
be not wasting time having the GM make special narrations to NPCs. Maybe the GM
won't notice if it's only asked to make narrations for some characters. Maybe
narrations can be the player agents' job?

Narration should be seen by all characters in the vicinity. I'm not sure on the
details yet, so we should begin with all NPCs at the location and all PCs logged
in. It may be the whole location is too big, but we need a way to have rust
automatically route messages. It may also be that PCs should not see messages
from other locations. This needs play testing.

## NPC Interaction

There will be frequent times when NPCs may be present for some interaction and
occasionally they may need to respond. A response to something that already happened is straightforward. E.g. after a character says something, all those who heard it may make an action to respond. This could be a Say, but it could also start combat and attack.

For simplicity I'm thinking this should be handled with an inference for each
NPC present asking whether they want to respond/act. They will have an explicit
tool call to do nothing and be encouraged to use it appropriately to prevent
explosions of response and high latency from all the LLM inference for every NPC
present. An idea for the future is to have the GM route these requests -
actually it already does with InterventionCheck. Responses may need to happen
before an action is completed.

Some interactions may be preventable by other characters. For example, a
character may try to pick up some treasure and the GM could ask specific other
characters if they wish to intervene, which must happen before the action can be
approved. At first I was thinking this could implicitly reject the action and
the rejecting character's action (or reaction) could then be processed
separately in a new turn to avoid agent recursion/re-entry. However there are
some complexities this doesn't handle.

Some interruptions wouldn't make sense and we need tests to catch it. For
example: a player goes to pick up some treasure in a prison. A prisoner
interrupts the action, calling out "don't pick it up; it's cursed". The player
chooses to pick it up anyway, calling the Take action again. This time the GM
should not allow the prisoner to keep blocking the character's attempt as all
they can do is call out. A simple solution might be to add a committed flag to
actions that player agents would typically keep off.

A further example: a player tries to pick up a trinket on display in the shop.
The shopkeeper is standing next to them and says "hands off; that's not for
sale". The player tries again, this time committed. The shopkeeper should be
able to interrupt this with a physical response, perhaps requiring a STR or DEX
roll or even initiating combat. For simplicity the shopkeeper's roll could be
modelled as a melee attack action at the start of combat, with the option for
ending combat immediately after if both characters accept the outcome. Needs
play testing.

These examples suggest the GM can always make an InterventionCheck, asking
players if they wish to intervene and which action they will intervene with.
Their action then comes back as a possible intervention that the GM may approve.
This is a bit awkward because it stacks up two in-flight actions - one being a
reaction. A ConditionalApprove tool call could be interesting here, but it may
not be necessary. The GM can approve the reaction, wait to see its result and
approve or reject the initial action depending on the result.  A player who is
fast enough might succeed in picking up an item even if it is the start of
combat. The distinction is that a character intervening does not necessarily
stop the action, but tells the GM of the intent. A simple "please don't" when an
action is not "committed" could be enough. ConditionalApprove would make this more
rigid and avoid a possible causal mistake by the GM, but this needs play
testing.

## NPC Inference

NPCs can make regular actions, responding to heard conversations or world events
in a new turn or as a reaction to an InterventionCheck. For now inference is
chosen to conservatively include those hearing something said or seeing some
event. It can be limited - characters who did not hear anything cannot react.
Perhaps this could be modelled with a dirty flag in their chat history (or a
perceived-event flag if a general dirty flag is too broad), which allows them to
respond to any update at all. This may be a performance issue, and needs play
testing.

Example actions and reactions:

- DoNothing
- Say, with a 1-3 eagerness value (indicating they will do/say it unless another
  NPC has a higher value)

Ties are broken at random, unless the action is simply saying something in which
case all things said with highest eagerness can be said at once into the chat. I
was thinking that batch approval could happen but it gets complex with
non-overlapping listeners. Better to have the GM narrate that characters all
talk at once and then approve each Say one by one in any order. The key part is
characters will not have heard each others responses before theirs and that
might be useful to know in chat histories. Simultaneous response may even be
appropriate in combat as per Cairn rules but I haven't thought it through.

When an NPC does choose to respond, e.g. Say something, the process repeats to
allow a conversation to happen as part of a chat-turn. Many things could happen:
the discussion ends, one character may start combat, leave mid-discussion etc.

If multiple NPCs choose to respond, either replying to conversation or
intervening in some action, one of them must go first. What they say may then
change what others would do, so their initial responses would be cancelled and
they may instead respond to the NPC that did respond.

This needs careful planning as the chat-turn's call stack of tools may grow and
I'm concerned about limiting the depth. Simply saying something should be
considered an instantaneous event that completes and inserts a message into
present characters' chat history, not something that awaits a reply. This will
need some play testing.

Players probably need an explicit interrupt button to pause inference and
discussions while they type. This allows them to react quickly to things said
and even break an infinite loop of discussion. It could be seen as an equivalent
to asking NPCs if they want to respond - an implied "no" from the player unless
they press interrupt and make some action. This must not be exploitable
mid-combat. E.g. any attempt to take multiple actions should be explicitly
rejected by code and not even reach the GM. I had hoped to keep chat-turns a
stack that cannot be recursive, but interrupting means a player may say
something while their original tool call is still awaiting a response. To
prevent this I'm thinkin tool calls should be greedily resolved. Perhaps even
scheduling responses once the tool call returns all the way to the top. E.g. the
player tries to pick up an item (without the committed flag), the GM does an
InterventionCheck and an npc stops them by saying something, wanting to start a
dialog about it. The Say reaction is should be approved and the initial Take
call should be rejected right away. Then the discussion happens as a regular
follow-up outside it. The discussion does not happen as part of the Take call. A
player can press interrupt whenever but it should only take effect between tool
calls so the player cannot accidentally interrupt a tool call.

## Dynamic Storyteller

These are currently ideas for the future. Not the initial version.

New NPCs can enter the story, perhaps as some others exit or die (purely for
performance reasons to balance agent chat processing time). Locations can be
added, new connections can be found or removed (e.g. a fallen bridge).

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

Where the number of raw chat messages after the summary (k) must not exceed some
threshold (message/token/character count). I.e. k is the number of messages to
keep. We may need to reduce it, even to zero in extreme cases, to handle very
large messages that would block future inference. Ideally k would be based off
token count, but I don't think we can know that without wastefully tokenizing
the messages. For now I think halving on failure is a simple solution, plus a
log so we know when a misconfiguration has made failures too frequent.

The ideas being:

- Compaction is expensive, so do it infrequently
- Keep some raw chat history after the summary as this is likely more important to keep accurate
- Compact as much as we have to to stay under our memory constraints.

This must be done with care so that the summary only summarises the portion of
chats that are compacted (summary_0, chat_0 through chat_n). I.e. for the
compaction operation, the LLM should not see newer chats than those being
compacted.

The Before/After examples above is what the model sees, not what is stored or
replaced. Nothing is deleted or moved: the database keeps every message forever
and a summary only changes which of them are selected for future context. For
the debug viewer it would be convenient to see the summaries inline in the full
history of all chats, but of course the model would never see this.

Compaction builds a one-off request from the range being compacted plus a
summarise prompt, and its output is stored as the new summary. The prompt
directs/describes what to summarise. It needs to implicitly know what
information will always be static and up to date, what will be lost and most
importantly which information is important to keep. Many interactions in the
chat are temporary and would not need recording, but some are not. This will
likely need gameplay testing to optimize.

The compaction threshold is likely triggered right after an agent emits some
output rather than before, as this is when the token count is first known.
However, it should happen semi-asynchronously so that the LLM's result can
immediately be used. E.g. if a player's agent replies to the player and must
immediately compact, the player will see the agent's response and can start
typing straight away. Obviously the agent's chat would block if another call to
it comes in before compaction completes. Moreover, we expect to run on a single
GPU so this would not be async with respect to other LLM calls. Ideally we
should process all agents chats, queueing compactions and only block on
compactions on next calls to that agent. Doing this means recursive agent calls
that all happen to compact after returning won't have to finish compaction
before the player(s) see the final response.

- Ideally any chat increase implicitly checks the threshold and schedules
  compaction, but this would require tokenizing and it might be worth avoiding
  that and checking the threshold after each inference. A consequence is
  overshooting input threshold before inference, where we would take the
  corrective steps below and re-run inference.
- Compaction runs lazily at the next free moment (e.g. as soon as control is
  returned to the user)
- If compaction has not completed before a chat needs another inference,
  compaction happens inline immediately.

If the size of the messages are too large and we cannot inference anything with them, compaction should take care of it. However, compaction must reduce the total input such that we can inference successfully. If the number of raw messages kept is too high or the message size is too large, we must reduce the number of messages kept.

If the threshold is overshot when compacting and we cannot even fit the input
anymore, we must reduce the amount we compact and summarise. For example, lets
say we would like to compact [summary_1, chat_0, ..., chat_n-1] but it doesn't
fit. We can instead keep chat_n-1 as raw history, or chat_n-2 or however many we
need. As long as we can compact [summary_1, chat_0] we have shortened the input
and can iterate until back under the compaction threshold. Slow, but at least
doable in a fixed GPU allocation and it saves us from the occasional giant
messages pushing us past both the threshold and input limit in one go.

Ideally compaction fallbacks are only needed in rare cases of very large messages, but a misconfiguration may also trigger it so we need logging for feedback and clear entries in developer mode for when it happens, directly linked to what caused it.

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
- Character
  - Name and ID - the ID is charN where N is a small unique number
    calls to uniquely identify the character
  - Character sheet info
  - InCombat, Moved and Acted booleans for combat actions
  - Time
  - Group - both a reference to the group object and a string description within
    it only if specified, e.g. the player has climbed a tree, rather than just
    generally with the rest of the group's characters. Any number of players can
    be part of a group and individual players can have their own group. Players
    must belong to exactly one group at any one time. This will be used by the
    GM to share GM-facing character context/chat history. E.g. when the party
    enter an ambush, all PCs and enemies merge into one group/chat history for
    the GM to operate in.
- Player characters
  - Character
  - Player-specific character sheet info
- Non-player characters
  - Character
- Group
  - Location ID and simple description of where it is in the location.
- Location
  - Type - an unstructured string, e.g. "city", "river", "dark wood", "clearing".
  - Description - details everything characters can interact with
- Connection
  - Connecting location references {A, B}
  - Description of the transition/connection. It could be the start of a path or
    road where A is the north entrance to a city and B is the south end of a
    road to another city. It could be the door to a tavern within a city, or its
    stairs to a mysteriously large basement.
- World state
  - Time
  - Initial prompt (just for record; unused except for initialization input)
  - Storyteller summary
  - Next action ID
- ItemType
  - Description
  - Relevant stats, if weapons/armor etc. (type safe/structured)
- Item
  - ItemType
  - Location ID and description - could be in a character's inventory or at a
    location in a locked/trapped chest
  - Unique Description - optional.
  - Storyteller trigger?
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

The GM may see the Storyteller's notes on objects at the location, but cannot
edit them. It is up to the GM to maintain consistency with whatever the
Storyteller decrees. The Storyteller should not see the GM's notes or it would
be overloaded with unnecessary detail.

## Game turns and time

(*There are RPG game-turns and LLM agent chat-turns which are separate)

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
NPCs to suddenly enter a location, for example arrive at an encounter along a
path while players are there.

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

All tool calls should actually use snake_case. I only used camel in this doc as
a mild convention make references to special names distinct.

## Character creation actions

See [character
creation](cairn/second-edition/players-guide/character-creation.md) in Cairn
second edition.

- ChooseBackground - returns the list of available names
- RollBackground - if not choosing a background; the agent offers the player a
  choice
- ChooseName - may require storyteller approval; if rejected, the storyteller
  must suggest alternatives that fit the story
- RollHitProtection
- RollAttributes
- RollTraits
- RollBonds
- RollAge
- ReadyToBegin - called when the player has finished character creation; rust validates the character sheet is complete

ChooseBackground and RollBackground are only available when the Storyteller is
not being used, e.g. during initial development (see An initial proof of
concept).

If the storyteller is active, each roll must be approved by the storyteller,
which may add background-specific modifiers if the story calls for it. The
storyteller may add some background narrative to the result that directly or
indirectly explains why this happened to connect the character with the story.

## Character actions

- DoNothing - a common reply to a request to intervene or respond to a question.
  Kinda similar to Wait, except wait has an implied duration.
- Move - the character describes going to a new position in the current
  location. This can be during or out of combat. If out of combat, the GM
  narrates how far the character gets if not all the way, anything they see
  along the way if applicable and the scene once they arrive. If in combat,
  follow the move rules of combat. The GM augments the move action with the time
  the move takes, ideally being consistent with the described layout of places
  within the location in the location notes.
- Travel - like move, but takes the argument of a Connection. First, other
  characters in the group are informed of the action and asked if they will
  stay, join or attempt to prevent travel (verbally or with force). If movement
  is possible at the time and no other characters object, the GM approves the
  action. The group may be split if some characters stay. The GM narrates the
  character(s) moving to and through the connection to the destination. If
  rejected, the GM narrates what happens instead, e.g. how far the character
  actually get from their current position to the connection in the current
  location before something else happens (e.g. some other event). If accepted,
  rust then moves the group's location to the Connection's other side and makes
  the GM re-populate their description within the location to note their
  position near the connection they entered from, among any other required
  consistency updates.
- Say/Whisper - text to be said that may include descriptions of how it is said,
  with an optional target for who the character is addressing. The character
  agent may need to translate a general desire into a specific sentence. E.g. "I
  want to ask if the bartender has heard of any unusual events recently, but
  discretely" -> "PC-name says in a low voice: Hey NPC-name, have you noticed
  anything unusual recently?". This must pass GM approval (i.e. unless they were
  incapable of speaking). The GM may provide an optional list of characters who
  overhear what was said (or who can't overhear), otherwise everyone in the
  group hears. Whisper is a Say but takes specific characters to whisper to.
- Lie - the same as Say, except the speaker and all characters who hear it make
  a WILL roll. Those that pass receive a canned note saying they think
  Speaker-Name may be lying. This opposed check diverts from Cairn rules, but
  fits the game mechanics here better. Player agents may confirm with the player
  if they intend to be lying before making this tool call if there is possible
  ambiguity. NPC agents must use it appropriately/honestly.
- Attack - using a particular weapon
- Retreat - implies a DEX save
- Give/Drop/Place - initiates an item transfer.
- Take/Request/Pickup - sends a request for an item; if from another character,
  the GM may forward that request to the character's agent for approval. If it's
  from a PC, the player's agent must ask for player approval, which may time out
  after a minute. A timeout is an error that propagates. The triggering
  character agent receives both the timeout error and the narration that the
  other character just stands there motionless.
- Accept - a character's agent must approve an item being given to them or taken
  from them. If it's a player character, the agent would ask the player if they
  accept and make the returning call appropriately.
- Look/Investigate/Open/Ask - more of a catch-all chat-with-the-gm. The
  character agent may want more information about their surroundings from the
  GM, to clarification something previously said or to actually spend time
  searching for something. The GM may additionally require a save or advance
  time. The GM may reject the request saying that that this is the middle of
  combat and would cost an action and may leave them more vulnerable to attack
  if the choose to proceed. The player should be able to acknowledge this and
  make a second request to proceed regardless. Note that this example is a
  rejection with a suggestion followed by a retry with an acknowledgement. The
  GM LLM must be capable of performing this little dance as it will be common
  during play and custom situation resolution. There is no structured "retry";
  my hope is that the GM agent will recognise the retry, particularly if its
  prompt implies this proceedure and the player agent's prompt suggests to
  include the text "risk aside/nevertheless, spending the action to...". An
  alternative would be a separate confirmation dialog.
- Wait - skips the remainder of their turn if in combat, waits a given amount of
  time. This could default to waiting to catch up to world time (e.g. waiting
  for another PC to finish doing something). If the wait is significant (i.e.
  not in combat), the GM should quickly narrate the wait, e.g. what they see
  while waiting. A future idea is to add a combat Delay call, but not for now.

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

- Approve and Reject - single response to accept or reject a characters's
  action, given as an argument to the tool call. The result applies immediately.
  The GM gets to see the result of whatever they approve, including failures or
  errors, as it may affect future decisions. If rejecting, the GM must give a reason so that the caller may try something else.
- InterventionCheck - called with an action ID and character(s) as arguments,
  who are then each requested if they wish to intervene before the character
  makes this action. Regardless of whether the action is committed (not
  tentative) or not. E.g. if the situation implies the character may stop if
  requested their agent should not set "committed" and the GM may reject the
  action if another character intervenes. If characters are in no position to
  prevent an action and the player has made it clear they're going through with
  it then the GM would Approve the action and possibly Reject the reaction (e.g.
  if it were an invalid physical attempt). The GM may call this before approving
  actions. Depending on how reliable the GM is, we may tie it explicitly to
  Approve and auto-deny if any character intervenes. Needs play testing.
- Save - makes a character roll a given stat to pass a check, defend against
  something etc. For example when the player wants to look for something, the GM
  may make them save to find it. The save roll should take a very short string
  saying what the save is for. The GM may apply difficulty modifiers. Not to be
  used for persuasion (see BePersuaded)
- RollInitiative - the same as Save, except this takes a list of characters and
  writes the results to their objects so that they make actions in the correct
  order as time updates. XXX does BeginCombat superseed this?
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
- EndCombat - unsets the characters' InCombat flag. InterventionCheck could be
  used first to see if all characters actually wish to end combat.
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

# User Interface

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
not return for 60 seconds. This is generated with deferred priority, much like a
chat summary, and if the server goes down the queued job to make a recap
remains. It does not persist in the player agent's chat history; it's only for
the world recap.

TODO: what if a player misses a session? Should GM narrations be added to all PC
chats even when not online? For now lets not, but this needs testing.

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
world creator can deactivate/ban joined users. Their association and characters remain
but until they get reactivated by the world owner they will be unable to access that
world.

Next to each player is a shortcut Join button to enter the world with that
character.

When a player joins a world they are automatically given a new character, with a
placeholder name "AdventurerN", where N is to disambiguate multiple characters.
The stats are undefined until they enter the game with that character, at which
point the player's agent will guide them through character creation, with the
help of the Storyteller, asking them to name it at the end.

There is no character detail page. Instead, characters would ask their agent to
describe it.

Dead players will be marked with an icon. They can even still join the world,
after which they may either wait for their companions to revive them or ask
their agent for a new blank Adventurer character, which must be approved by the
Storyteller.

There is a button to enable developer mode for a world that only the owner can
click. This mode becomes immediately visible to all players once enabled. A big
warning modal must be accepted before changing to developer mode, noting that it
enables complete visibility into internal game state and is effectively
cheating. Developer mode enables complete inspection of chat logs from all
agents - the Storyteller, GM and NPCs.

## World game page

This is the main page where the game is actually played. It is simple: one big
chat history, a box at the bottom to enter text and a send button. Also an
interrupt button that may be used to pause NPC discussions or say something
mid-combat - specifically, prevent future NPC replies until the player(s) acts.

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

Dice rolls should be displayed inline in the chat in the order they happen in.
Modifiers the GM adds should also be visible.

The main chat may include additional inline messages such as warnings, or roll
results in developer mode. E.g. if compaction is ever triggered twice in a row,
this indicates the thresholds are badly set or there is a once-off very large
message. Rather than emit messages separately to a log, keeping everything
inline in the one chat gives one unified user interface and preserves ordering
to make development much easier.

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
- Allow the GM to dynamically create rules that can be executed and evaluated by
  the code. This keeps things more fair and random by asking the LLM to make
  structure that then gets enforced. E.g. an NPC tries to stop the player
  picking up an item by force - the GM then constructs an opposed strength test
  whos result leads to the player taking the item or not.


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
efficient. Perhaps this could just be a database dump of everything related to a
game/world. Something for much further down the line when we actually start to
have too much data in the database.

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
the left and a developer/debug view on the right. This should be available to
all players. For real games, players should not see this data as it is
meta-gaming and would ruin the experience.

The developer view will need to be able to display the following.

- Browseable world objects, descriptions, tools, agents and links to their chat
  histories. A search/filter is probably important for database objects as there
  will likely be many. The aim is to just present data cleanly and intuitively
  including relations/links without any effort to project or transform relations
  into simplified displays. This is for developers.
- A chat-turn view. User messages may trigger or be involved in full agent
  turns, where a turn is the entire stack of messages and tool calls,
  appropriately displaying tool calls and their results. In most cases turns are
  initiated by players, but some player messages may be responses (e.g. when a
  travel action asks players if they will to join).
- An inference view. As mentioned earlier we need a way to reconstruct all that
  went into an LLM's response. Messages in the debug view should link to an
  inference view that shows the entire verbatim input prompt, system message,
  all that was given to the LLM and its returned output. This will be most
  important to debug why agents misbehave as 99% of the time, the cause will be
  a misleading prompt or incomplete information.
- A chat history view for a particular agent. This view is a much longer
  infinite scrolling view of all messages for a chat history. This is the most
  complex as it is a higher level concept than the data. I.e. inference view is
  real and tangible but the chat history is more of a timeline view of messages.
  For example, the linear messages for an individual player or NPC agents. The
  GM agent chat history is more complex as it will form a DAG where histories
  split and merge. To support this, the GM's chat history view needs to
  authentically display the DAG, including multiple heads - one per Group of
  characters and their history of messages. This view should include expandable
  compaction/chat-summaries inline into the history at the point they were made.
  It does not include additional agent prompt/context. When opening a message
  from the current player's chat, it would show the player agent's chat history,
  including its agentic loop and tool calls etc that would normally be hidden
  from the player.

There needs to be a button on chat entries on the left to inspect them, which
will open and highlight their entry in the turn view on the right. This will be
the full tool call stack through all agents involved in responding to the
initial message. Similarly, clicking on a dice roll should show the tool call in
the turn view that the dice roll was involved in.

Links between views are important for navigation. E.g. when looking at a turn
view (which only shows the direct request/responses from agents), developers may
want to see the agent's raw chat history before that and would like to switch to
view that message in its history view. E.g. to see the overall flow of the chat
that a GM agent sees, not just the short request/response.

In general the aim of developer mode is to include everything, all the raw data
verbatim. This can be a bit overwhelming so some parts may be hidden. We should
include an expandable section in raw chat histories to view summaries at the
points compaction has occurred.

Chat compaction is the result of LLM inference, so summaries will have an
inference view too, again, showing the exact input and output.

Scheduled inference such as chat compaction or an NPC response should retain a
link to the event that triggered it. For example it would be important to know
which NPC chose to reply to a Say or intervene in a player's action
(InterventionCheck) and the point in the turn at which they did, but the
response to it (discussion, begin-combat, whatever) is probably going to be a
separate turn.

Implementation wise, it is important that we do not redundantly store duplicate
prompts. The database would contain chat messages that can be stitched together
to reconstruct the full chat history, summaries AND prompt/inference input
identically as it appeared. This would need testing where we do store the prompt
input and verify it matches live reconstruction identically.

I was thinking developer mode should be a one-way operation, but giving the host
a way to briefly enable players to inspect bugs is too valuable.

## Live Coding Agent Interaction with MCP

The game data must be trivially accessible to coding agents working on the
project. Moreover, agents may want to test features and repro bugs quickly
without writing temporary scripts. MCP that claude+codex could talk to directly
may speed up development and I imagine could reuse the exact same LLM tool
interfaces that game agents use. As an example, agentd may want to directly
trigger in-game actions and tool calls to test rather than having to indirectly
set it up to happen.

# An initial proof of concept

Before jumping into the deep end, we should implement a minimal version of this
game, text and explore. This will hopefully reveal directions that work and
don't work early on.

The smallest setup I can think of skips the Storyteller initialization and hard
codes the setting, NPC(s), location(s) etc. Then we can jump straight to testing
basic game mechanics, interaction with agents and the GM.

New PCs simply enter an existing group of characters at their location; no
storyteller interaction and it doesn't matter which.

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

# Implementation Directives

## Scheduling

See the note on delaying compaction until the chat turn is complete and then
compacting while waiting for user(s) to improve responsiveness/latency.

This is designed to run on one machine with one GPU. While mistral.rs does
support concurrent requests, we probably don't want to overload it. Limiting to
some fixed concurrency may be a good idea to improve latency and maybe even
memory access patterns. If we can use mistral.rs directly to do this that'd be
even better - less code for us to maintain and we get a feature for free.

## Memory Usage

LLMs are big and VRAM intensive. Much like any video game, we should not do any
possibly unbounded dynamic allocation so that once launched successfully we can
be confident the server won't crash due to OOM. I.e. we compute a maximum we
will use and make sure it's free. Ideally it would all be allocated once at
launch, but backends probably don't support that. It would be nice if we could
have our conversation compaction limit guide the expected usage limit as I
expect our chosen input and output size for the model will be the main memory
requirement drivers. I'd like to see the amounts allocated. This should be
printed during initial allocation. It would be interesting to see the
conversation's memory usage breakdown in the developer's inference view too.
I.e. to guide balancing allocations, context, system/tool prompts, conversation
history size, output size limit etc.

## System prompts

These will make or break the game and are particularly important. Keeping them
in an easy to find place is useful, however stick to the "group what changes
together principle". I'll want to hand edit these and easily trace (i.e. just
find) inference operations in the developer view to them.

## Chat

Agents get very confused when they miss context. If there's any error, we should
emit it to agents' chats as it propagates through the stack. This lets the agent
know their tool call failed and they didn't even get a chance to output
anything. We should keep what the agent sees VERY small as context is expensive
and it may not be able to do anything about it.

# TODO

How will experience and character growth happen?

The GM should be given an initial RollOmens call to make after all players have
entered the game, but before the game starts. This means there needs to be a
sync point where the GM waits until all players have called ReadyToBegin.
Players should be warned by their agent only to call this when all players are
online and have entered the game, otherwise the GM will need to bring them in
later in the campaign.

**Future idea — associative hierarchical memory:** Add a long-term
conversational memory layer that stores retired conversation history as embedded
chunks, searchable automatically via semantic similarity and exact-text
retrieval. Relevant memories could be injected implicitly from the current user
message, recent context, or even the assistant’s draft output, allowing the
model to “stumble upon” past context without explicitly deciding to call a
memory tool. Over time, frequently related memories could be clustered into
higher-level episode/concept summaries, forming a RAPTOR-/A-MEM-like
hierarchical or DAG structure while retaining raw source turns for precise
recall. Useful search terms: *agent memory, long-term conversational memory,
associative retrieval, hierarchical RAG, RAPTOR, MemoRAG, A-MEM, episodic
memory, semantic memory, hybrid vector/BM25 retrieval, recursive summarization,
memory consolidation, retrieval from generated clues*.

It's tempting to have the storyteller be able to pre-generate some backgrounds
and player portraits. A high quality blur filter on a 512x512 image can actually
look OK for a full screen background of the chat window. I'm wondering if we
could run Stable Diffusion 1.5 or something locally. We'd need to unload the LLM
first probably, or accept background CPU generation.

Perhaps even MusicGen (small), Stable Audio 3 Small-SFX, Stable Audio Open 1.0,
that could create some music clips and foley to mix and play as the game state
changes or on entrance to areas. Feature creep that may never happen atm.
