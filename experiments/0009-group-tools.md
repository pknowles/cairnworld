# The GM and the group tools

How the real GM (Qwen3.5-4B, production sampling, fixed seeds) uses split, move and merge.
`the_gm_regroups_characters_as_they_move` puts three moves to it, each from its own start, and
reads who shares a group afterwards (`CAIRNWORLD_REPEATS=N` varies the seed). Rowan and Iris are
the players; Mara is an NPC outside, Toma an NPC in the hut.

- joins Toma: Rowan climbs into the hut. Right: Rowan with Toma, Iris with Mara, apart.
- walks off: Rowan walks away from everyone. Right: Rowan alone, the other three together.
- walks back: Rowan, alone, walks back to the others. Right: all four together.

## What reading the GM's input showed

1. With a name for each group as a required argument the GM named the new group "inside", which
   existed, was told to choose another name, and repeated the same failing call, ending with one
   name for both groups. Names are now derived from the members and the arguments are gone.
2. The group tools were bound to the group the turn began with. After one split the GM's group was
   replaced, and every later call failed with "group unavailable". They now act on the acting
   character's group as it is when the call is made.
3. mistral.rs's Qwen call parser reads only one schema `type`, so an optional integer typed
   `["integer", "null"]` arrived as a string and was rejected. The sent schema now gives the plain type.
4. After `approve_action` the result said only to narrate, and the role did not mention groups
   changing: the GM never called a group tool (0 of 8). The role now names the three tools, and the
   approval result says to apply the action's group changes.

## Results (right / runs)

| Tools and wording | joins Toma | walks off | walks back |
|---|---|---|---|
| Single `split_group` with optional target and name arguments, merge | 0/2: `merge_group` of everyone | 0/2: no group call | 2/2 |
| Separate `move_to_group`, names as arguments | 0/3: `split_group` named after Toma's group | 0/3 | 2/3 |
| Names derived, current-group fix | 0/3: a wrong `split_group` of both players, then moves | 1/3 | 3/3 |
| Same, with the action worded as a player would write it | 0/2: `merge_group` of everyone | 0/2 | 2/2 |
| `merge_group` removed (a move of every character is a merge), names derived | 2/2: `move_to_group` of Rowan | 0/2 | 1/2 |
| Final: `merge_group` removed, the GM names the groups (made unique when they clash) | 3/3: `move_to_group` of Rowan, correct group | 2/3 | 1/3 |

With `merge_group` gone and the GM naming the groups, joining another group is right 3 of 3 times, each
a `move_to_group` of the one character to the right group. Walking off is right 2 of 3 (once no call), and
walking back 1 of 3 (once no call, once a `split_group` instead of a move). What remains is the GM making no
group call, or choosing the wrong one, when a character leaves or returns; the joins are solved. The names it
writes are descriptive but not always accurate ("outside: Rowan and the Adventurer").

Why `merge_group` went: with it beside `move_to_group` the GM took "join" to mean merging, which pulls the
other characters along; it names who moves when only a move is offered.
