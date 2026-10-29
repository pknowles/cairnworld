# The Stolen Flour

A short encounter for one player: two people with legitimate claims to one sack
of food.

## Situation

Mara, the village storekeeper, promised Toma a sack of flour for repairing the
mill. He finished the work. A flood then spoiled the other village stores, so
Mara withheld his payment to ration the sack among several households. Toma
took it anyway. His household also needs food.

Mara asks the player to recover the flour. Both acknowledge the work and the
promise if questioned. There is no secret villain or hidden supply that makes
either claim disappear.

## At the hut

The hut is Toma's home. He has brought the flour home for his household, not
stopped there on his way somewhere else. The player and Mara start outside its
barred front door. Toma is in the front room with his wood axe within reach.
The tied sack is stored in a separate rear room, out of his line of sight.

Walking around the house and examining the rear reveals a window behind bushes,
with an unlatched shutter. Finding it needs no roll when the player searches
there. The window admits a person and the sack, passed through separately.

The hut and its surroundings form one game location and group. Sharing a group
does not remove the walls or put everyone within arm's reach.

The sack is bulky and carrying it occupies both arms. The axe is Toma's work
tool, not something he is already swinging.

### Sneaking into the rear room

The partition blocks sight, not sound. Climbing in and moving the sack can make
noise on the wooden floor. The GM should explain that being heard risks a
confrontation and require a DEX save for the attempted stealth. Searching for
the window is not the risky part. Use the actual recorded roll, not an invented
result. On success the player can reach and remove the sack unnoticed, provided
their described plan remains feasible; do not demand another roll for each
small movement in that same attempt.

Cairn explicitly lists sneaking under Dexterity: roll a d20 equal to or below
DEX, with 1 always succeeding and 20 always failing
([Cairn rules](https://cairnrpg.com/first-edition/cairn-srd/#saves)).

On failure, Toma hears the intrusion and comes to investigate. The player is
caught entering his home or taking his food. This gives him a concrete reason
to distrust them and makes later persuasion harder. Confrontation may become
combat depending on their actions; a failed save does not itself force an
attack. Mara can occupy him in conversation if persuaded to help, but the
stealth attempt still risks noise.

## Mara

- **Knows:** she promised the flour, Toma completed the repairs, and the flood
  ruined the other stores.
- **Wants:** the flour back for the households depending on her, without anyone
  being hurt.
- **Character:** conscientious about her responsibility for the village,
  defensive about withholding payment, worried about the families she must face.
- **Limits:** she cannot produce replacement food or make commitments on behalf
  of people who have not agreed to them.

## Toma

- **Knows:** the flour was his agreed payment, the repairs are finished, and Mara
  withheld it after the flood.
- **Wants:** to keep his earned food for his household without violence.
- **Character:** proud of his work, angry at being treated as a thief, distrustful
  of another promise from Mara.
- **Capabilities:** healthy and able to fight or carry the sack away, but not
  wield the axe while carrying it.

**6 HP, 12 STR, 12 DEX, 10 WIL, no armor; wood axe d6.**

The NPCs speak and decide for themselves from those facts. Neither has a scripted
offer, concession, line of dialogue or required response to persuasion.

## What the player can change

The player may question either person, argue for either claim, propose sharing,
offer their own resources, attempt deception or theft, use force, or walk away.

These are possibilities, not a menu presented to the player or a sequence to
enforce. Sharing asks Toma to give up part of his earned food and leaves less
for his household. Personal supplies can genuinely help if the player actually has
them; a promise of food is not food already delivered.

Consent, access and possession matter. Saying "I take it" from outside does not
move the sack past the wall and its unwilling defender. Deception can be
discovered; an exposed entrant or someone carrying the sack is vulnerable.
Violence can injure either side or lose the flour. A workable agreement does
not require an obligatory roll; sneaking into the rear room while Toma can hear
does require the stealth save described above.

Let choices and events change the situation. Do not force a fight, invent
objections to a sensible solution, or keep NPCs arguing to prolong the encounter.

## Ending

End when the player settles what they will do about the flour and the immediate
consequences have played out. Returning it serves Mara's request, not an objective
definition of justice. Letting Toma keep his wages can be a considered resolution.
Sharing or contributing supplies has its own cost. A failed plan can leave the
player empty-handed, harm someone, or lose the food.

## Required game operations

These are implementation requirements, not additional instructions deciding
what either NPC must agree to. Use the existing shared action, GM approval,
perception and history paths.

| Approach | Operations needed |
| --- | --- |
| Question, negotiate, distract or deceive | Speak/private speech, Lie and the NPC's BePersuaded; NPCs choose their own responses. |
| Find the window, enter or reposition | Investigate/Open/Ask through Attempt, Move and persistent placement/GM notes. |
| Steal the sack quietly | GM Save(DEX) for noise before approval, then Take; narrate discovery to Toma on failure. |
| Return, exchange or share flour | Take/Give/Drop/Place and SplitItem/CombineItems. |
| Fight or flee | Attack, Retreat, EndTurn and the shared timing/initiative, damage and recovery rules; GM MoraleCheck when Cairn calls for it. |

The JSON prepares full `flour sack` and `half flour sack` ItemTypes. Only the
original full sack exists at the start. If the characters agree to divide it,
SplitItem creates two half-sack items using the prepared type; Give transfers
the agreed portion. CombineItems can turn the two halves back into one full
sack. Rust consumes inputs and creates outputs atomically, retaining their
records/history. Narrating a division alone must not duplicate food or change
its holder. No general fractional inventory system is needed for this encounter.

For attacks, the GM supplies applicable Cairn weakness/advantage flags and
modifiers on approval. Rust rolls and applies all attack damage and its
mechanical consequences; the GM narrates the returned result. Environmental
TakeDamage is separate and is not a way to choose or reapply attack damage.
MoraleCheck rolls NPC WIL in Rust; a failure adds the declared flee instruction
before that NPC's next turn. PCs are unaffected by morale. A character may
choose Retreat without first failing morale.
Retreat still requires its own DEX save before the GM permits a safe Move/run;
a failed retreat does not automatically move the character away from danger.

The human-readable encounter is here; `bread_thief.json` contains the playable
world and each NPC's own background and motives. Changes to either file must
preserve the same situation.

Runtime limitation: this table maps the encounter to declared tools; it is not
a claim that they are all implemented. In particular gameplay saves, item
transformations and combat/morale still require the planned rules work.
Narration must not substitute for actual dice or inventory changes.
