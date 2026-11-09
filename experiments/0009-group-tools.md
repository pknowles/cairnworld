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

## Where each tool applies, and a group of eleven

The fixture groups were named "outside" and "inside", which the GM copied into its own names, so
they are now what the GM would write ("Toma in the hut"). A fourth case has eleven characters outside
(Rowan, Iris, Mara, eight villagers) and the action leads all of them in to join Toma; right is
everyone in one group. Four cases, 3 seeds, same fixture:

| Tool descriptions | Joins Toma | Eleven join Toma | Walks off | Walks back |
|---|---|---|---|---|
| Committed: split "when they separate from the rest", move "when they join its characters" | 1/3 | 0/3 | 1/3 | 0/3 |
| Split "when they go off where no other group is", move "when they go to where its characters are" | 3/3 | 3/3 | 3/3 | 2/3 |

With the first wording the GM called `split_group` to join an existing group in nearly every run, naming
the new group after the one it was joining. Saying where each applies fixed it.

### Whole-group shortcut: tried, not used, removed

`move_to_group` with no characters listed moves the GM's whole group (the user asked for a shortcut for
large groups). Eleven characters outside, 3 to 4 seeds each:

| What changed | Result |
|---|---|
| Action names everyone ("I lead Iris, Mara and all eight villagers in") | All eleven ids listed, 3/3. Confounded: the action itself lists everyone. |
| Same, role "when characters join", argument "Leave character_ids empty to move this whole group" | All eleven ids listed, 4/4. Same confound. |
| Action "Everyone outside follows me" | Only Rowan moved, by `split_group`, 4/4: it reads as the others acting for themselves. |
| Action "We all climb in" (three characters, earlier wording) | Only the two players listed. |

A group is the characters who can interact, and the GM acts on one character's action at a time, so a
whole group moves because one character's action brings another group into range, not because it is
told to. The fair test is that: eleven characters close together, Toma a separate group just out of
earshot of the crowd, Rowan walks a few steps until he can talk to Toma, and everyone ends in one group.
With positions given to the GM it made no merge call in any run:

| Role text | Result (4 seeds each) |
|---|---|
| Committed | `split_group` of Rowan alone or no group call, 0/7 seen |
| Plus "the characters in this group: those who can interact with each other, directly or through others" | no group call, 4/4, and again 4/4 with every crowd member given a position |
| Plus "all of them joining merges the two" | `split_group` of Rowan alone 3/4, no call 1/4 |

A separate `merge_group` tool (this group and another group id, no character list; the role named it for
"the whole group and another come into range") was tried instead of the empty list. In the fair test it
was never called: no group call in 3/3 runs, the other cases unchanged (a single character joining
Toma 3/3, so it did not pull single joins into merges). Where everyone climbs into the hut together it
was never called either: `move_to_group` with all eleven ids, 2/2 (the process then died on the third
seed). The empty list and the separate tool were both unused, so the shortcut was removed.

The GM never reached the choice between listing the crowd and listing none, so the shortcut has no
measured use. What the input showed: the scene packet lists every character with an id, and the only
way to bring Toma into the crowd is to move the crowd into Toma's group, a backwards phrasing for
"Toma is now in range". Not tested: whether the GM sees range changing at all (thinking is off, so
its reasoning is not visible), and a result text that says who is now in range of whom.
