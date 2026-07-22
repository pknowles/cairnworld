# Project Overview

This project is a tiny multiplayer web game.
Players join and interact just by chatting.
It is an RPG (role playing game) where the GM (game/dungeon master) and NPCs are LLM agents, and each player chats to an agent to perform actions.
I want the ruleset to be super simple and promote emergent gameplay.
The plot will tend to be generated around chatting and detective-like investigation, uncovering plots played out by NPC agents.
NPC and PC agents may only interact with the world using a strict set of LLM facing tools (like MCP, but without a separate "server").
This acts as a boundary for LLM hallucinations corrupting game state, means each agent may focus on honestly helping the NPC or player.
The GM agent then has less responsibility and can focus on its own job without conflating context.

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
summary" is the product.

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

Some examples of meta-questions and questions are below.

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

## Storyteller output and tools

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

## Dynamic Storyteller

This is an idea for the future. Not the initial version.

New NPCs can enter the story, perhaps as some exit or die. Locations can be
added, new paths can be found or some paths can be removed (e.g. a fallen
bridge). These are initiated by a structured GM tool call, and succeed when a
dice roll meets a GM-provided success threshold.

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
  - Description of what is along the path in the order from location A to B
- World state
  - Time

## Turns and time

With many PC actions, the GM is asked how much time has progressed. The GM may
decide since the players were only chatting, not much time has passed. Rust
keeps track of this in the world state, but this is not shown in the UI.

When time advances, main NPCs in other locations may get a chance to take
actions, chat, organise and plot with other NPCs. While rare, this could allow
NPCs to suddenly enter a location or an encounter along a path between locations
while players are there.

If the time of day changes such as sunrise/sunset/darkness, the GM should
narrate this to the player in the next chat response. This in turn may affect
the difficulty the GM decides perception checks should be.

We need to design the game rules so the GM can have all PCs complete their turns
up to the advanced time, with allowed room for error. During combat this would
be strictly turn by turn and time advancement could probably be ignored
entirely. When one player does something for a few hours, another player may get
many turns to do something while they wait, e.g. have a conversation with some
NPC in the same city/town. They shouldn't be forced to do actions until the time
adds up perfectly. I guess the GM could effectively just ask what they want to
do in the meantime and waiting could be a perfectly valid response. This
probably means every PC has their own time variable as state, which then
advances up to the global world time through actions or waiting.

## GM interaction

Possible idea: What if the GM agent was not told which characters are NPC and PCs, to make the world perfectly fair.

## Spell ideas
- **Read mind** is a tool call that gives the NPC's internal chat history as context to an LLM that produces a stylized summary. NPCs could even cast this on PCs!
- **Command** can inject a thought directly into the chat of an NPC or PC. Players get to type this directly. If cast on a player it replaces their turn with the injected text.
- **Erase mind** can delete one or more chat entries from an NPC, depending on dice roll success magnitude.

## Chat scheduling and priority for GM and NPC agents

# Agents

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

# Tech stack

- Rust
- mistral.rs GPU Inference, maybe with with Sao10K/L3-8B-Stheno-v3.3-32K Q4_K_M
- Axum
- Cairn SRD second edition ruleset

# UI

## Landing page

The main/landing page has a banner and description of the game.

Without loggin in there is only a log in button.

After logging in in with OAuth2, e.g. a google account they see more. We only allow oauth; there is no user/password storage and accounts are simply keyed by email address (this is not visible on any page). The main/landing page allows players to change their name, has a button to create a world, lists worlds they have created. Each world takes them to the world detail page.

## World detail page

Each world is a game instance. When users create a world it's a new playable game setting. When they do they can optionally provide a seed prompt that is fed to the Storyteller agent for world initialization.

Clicking on a world from the main page goes to a world detail page. Players can change their character name for that world, click a Join button to enter the world. The player that owns the world can create special invitation links so their friends can also join the world. There is no email invite; only a unique link to the world. Friends must log in and click a Join button to accept the invitation, after which they are redirected to the world detail page. Players may optionally specify a limit for how many friends may use that link to join the world. For the owner, the world's detail page lists the links with remaining slots left. All players can see the names of all other players who have joined the world and their character names.

There is a button to enable developer mode for a world. This mode cannot be disabled and becomes immediately visible to all players once enabled. A big warning modal must be accepted before changing to developer mode, noting that it enables complete visibility into internal game state and is effectively cheating. Developer mode enables complete inspection of chat logs from all agents - the Storyteller, GM and NPCs.

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
