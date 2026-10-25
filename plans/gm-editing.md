# GM field editing and hierarchical lookup

Pending implementation. First deliverable: a character moves within its existing
location, the GM edits persistent placement and relevant notes, and the next
inference/turn reads that state before producing consistent narration. Both
whole-value replacement and exact-text editing are required from the outset.
Read/edit work can proceed independently. The Move example includes its declared
elapsed-time update; multiplayer time-window policy is not a prerequisite for
demonstrating persistent placement within a location.

## Authority and scope

Follow [Note Editing and Search Tools](../user_declarations.md#note-editing-and-search-tools),
[Game state](../user_declarations.md#game-state),
[GM tools](../user_declarations.md#gm-tools),
[the editing decisions](../user_exerpts.md#2026-10-24-gm-editing-and-incidental-detail),
and [the design](../design.md#gm-field-editing-and-lookup).
Each checkpoint follows the [AGENTS.md checklist](../AGENTS.md#post-feature-and-pre-commit-checklist),
[coding standards](../coding_standards.md) and
[prompt standards](../prompt_standards.md). No production prompt/tool changes are
verified merely by writing this plan; inspect captured production contexts and
exercise real chat before claiming model behaviour works.

The GM maintains local interaction and incidental detail. A chair can remain in
location text/notes; it does not need its own database object. Significant items
use the existing item model and Storyteller creation/approval responsibilities.
Do not build item creation, a virtual filesystem, RAG or Cairn rule lookup here.

## Existing data and selected approach

`character_group.description` already holds a character's relative placement;
`group.description` describes the group's position in its location. Existing
objects have descriptions and GM/Storyteller note columns, currently JSON-backed.
Reuse those fields and existing object relationships. Do not introduce a second
position store or copy placement into character identity and several notes.

| Choice | Benefit | Cost / selection |
| --- | --- | --- |
| Whole-value replacement only | Small interface; good for short fields | Rewrites long text for small changes. Rejected as insufficient. |
| Whole replacement plus exact find/replace | Familiar text editing; preserves untouched passages | Exact match can be missing/ambiguous; report that explicitly. Selected together. |
| Line/paragraph patches or virtual files | Rich editing surface | Index drift, parser/path machinery and extra representations. Not needed initially. |

Before choosing the note representation, compare named note values with bounded
key listings against the declaration's header/subnote proposal. Use the same
short-note and large-note lookup/edit cases for both: can the GM discover relevant
detail without already knowing its key, what must remain in each prompt, and
what storage/tool code is needed? Record the context sizes, maintenance costs
and limitations, then select the approach against those requirements. Neither
option must first prove the other inadequate. Reuse existing object relationships
for navigation and avoid storing the same authoritative state twice. Both editing
operations remain required whichever note representation is selected.

## Dependencies and initial bounds

NPC interaction supplies the terminal narration tools and shared action path;
code-quality checks precede this plan in todo.md. The current schema has world
and character time columns, but gameplay clock advancement is not implemented.
Do not confuse those columns with an existing Move/time operation. Implement the
within-location Move's declared GM-supplied elapsed-time update using those
authoritative fields. This does not require settling the broader
[time-window proposal](../design.md#product-decisions), adding a lead limit or
implementing multiplayer turn admission here. Those remain with the rules plan;
this example does not claim that scheduling policy is implemented.

Before enabling writes in the first checkpoint, configure finite field/note,
key/index and tool-response limits. Choose and record their numerical values
from measured assembled inputs for the configured model, including schemas,
old/new edit arguments, returned content and next-inference/output headroom.
Use small configurable limits in deterministic checks. There is no unbounded
first version; the second checkpoint extends this measurement to retrieval.

## First checkpoint: read/edit and movement

- Define a small typed set of writable fields on existing object references:
  character placement within its current group, group placement, relevant public
  descriptions and named GM note values. Reuse existing object IDs/types where
  appropriate, but not unrestricted Developer Mode query access. Preserve
  arbitrary note keys; only text values accept exact-text edits. Never accept
  SQL, filesystem paths or model-selected table/column names as executable input.
  A whole-value write may create a named GM note on an existing authorized
  object. Include explicit removal of a named note through the same field access,
  so obsolete short-term notes do not accumulate as empty keys. Neither operation
  creates or deletes the game object; fixed fields such as placement can be
  cleared but not removed. Validate permissions and bounds before any change.
- Provide whole-field write and exact-text edit operations through the same
  authorized field access. Exact edit takes the complete literal old passage and
  replacement, changes exactly one match and leaves all other text untouched.
  Zero or multiple matches are visible rejected tool calls with object/field
  context and no mutation. Do not guess, use fuzzy replacement or add a retry
  coach. Whole replacement is explicit, not a fallback for a failed edit.
- Provide field reads, returning complete content when it fits. Support explicit
  line ranges for larger text, with total extent and returned range stated, so
  partial results are never mistaken for complete descriptions. Reads use current
  persisted values. Editing tools return the committed change and resulting
  size/budget information as ordinary tool results, ready for the next inference;
  no separate review agent, undo stage or mandatory verification inference.
- Resolve authority from the calling GM's world and group branch. It may edit
  current group members' placement and authorized local descriptions/GM notes,
  and read applicable local Storyteller notes. Storyteller notes are read-only
  to the GM. World-level GM notes are accessible through the world reference;
  broader remote-object access must follow declared authority, not arbitrary IDs.
  Fields for health, inventories, membership, combat flags, time and other
  mechanics stay behind their existing action operations. Descriptive edits do
  not transfer an item, change group membership or authorize an extra action.
- Use existing recorded tool executions, shared errors and object inspection to
  retain who changed which field and the real result. Perform validation and the
  write in a short transaction, then advance affected scene revisions so pending
  NPC proposals cannot act on stale state. Note/field bookkeeping does not itself
  become a perceived event. Narration conveys observable consequences through
  the shared delivery path after edits; no special movement response subsystem.
- Refresh the current-state context between GM inferences after edits, not just
  at invocation entry. Keep this state separate from chat history and preserve
  captured immutable inputs for historical reconstruction. Tool results and the
  next GM request must agree with the committed database, including after reload.
- Demonstrate the complete path: the player requests movement behind a pillar;
  the GM reads the relevant current placement/location text and notes, resolves
  the request, edits placement and affected local facts, sees the tool results,
  then finishes with the appropriate terminal narration tool. A later request
  must use the new position. Use the declared Move action through the shared submission/arbitration
  path, not Look or a disguised Ask. Implement its out-of-combat within-location
  path and the GM-supplied elapsed time through shared action/clock operations
  that this movement path must supply; do not edit time as arbitrary text or
  assume time advancement already exists. Reuse this in the later rules plan. Travel
  and combat movement stay in that plan; this checkpoint does not claim those
  mechanics work or add a second time system.

## Second checkpoint: hierarchical lookup, search and bounded notes

- List existing child objects from actual relationships: world locations,
  location groups/items/connections, group characters and an object's available
  fields/note keys. Return IDs, names and bounded orientation data; do not dump
  every body or fabricate a directory tree. Returned references are usable in
  subsequent read/edit calls. Keep World notes in this same object/field model.
- Search literal text in explicitly selected authorized object kinds, fields or
  named note values. Return matching object/field references and bounded context,
  with paging/continuation for more results. Share authorization with reads and
  edits; neither search snippets nor counts may expose unauthorized notes.
  No regex language, raw SQL or associative-memory engine is required initially.
- Enforce configured hard field/note and response bounds, including note keys
  and their listing, before writes/reads can grow context without limit. Reject
  an oversized edit unchanged with actual size and limit; never silently truncate
  stored text. Read/search pagination handles large existing values without
  pretending they were returned in full. Normal tool results report remaining
  allowance without adding an extra model call.
- Extend the first checkpoint's measured bounds to paged search/list workloads
  and representative large existing notes. Record assembled input sizes and any
  justified configuration changes. Do not assume character counts equal model
  token counts. Exercise repeated lookup/edit calls through the existing request
  budgeting and compaction path, including approaching the context limit. Field
  and page limits bound individual values/results; they do not guarantee that
  accumulated conversation fits. Preserve explicit capacity failures when the
  request cannot be reduced sufficiently, rather than promising every legal
  sequence can continue.
- Storyteller integration later reuses these field operations with its own
  authority: it writes Storyteller notes and does not read GM notes. Do not add
  a placeholder Storyteller executor in this plan. Keep this boundary separate
  from GM permission to read local Storyteller instructions.
  [World initialization](storyteller-worlds.md) adds the Storyteller executor,
  object creation and editable world summary using this same field access.

The first checkpoint must itself have bounded inputs/results using the initial
configured limits; the second completes broader search/navigation and measures
the full retrieval workload. These are consecutive deliverables, not permission
to ship unbounded storage and repair it later.

## Verification and completion

Review model-facing use cases/validation with the user before adding LLM tests
as required by coding_standards.md. Combine scenarios and reuse production
Store/Game/tool paths; no new mock world engine or repeated full GPU suite.

| Case | Required outcome and verification |
| --- | --- |
| Movement and next-turn consistency | Real chat uses Move within a location. Inspect the recorded placement edit, elapsed time and next captured GM input; later narration uses persisted placement after reconnect/restart, not just remembered dialogue. Group/location IDs do not change. No arbitrary time-field edit or duplicate clock implementation. |
| Short and long edits | Scripted calls replace a whole short value and one passage in a large value, then create and remove an incidental named note. Unrelated passages/keys remain intact; a chair remains location text/notes without a new Item. Missing/ambiguous text fails visibly without mutation. No game object is created/deleted by note operations. |
| Newly invented incidental detail | GM reads existing relevant detail before adding it to descriptions/GM notes, then narrates only after its write result. A subsequent read finds the detail. No forced object creation and no extra after-narration inference. |
| Permissions and mechanical state | Foreign-world/local-scope violations, GM writes to Storyteller notes and edits to mechanical fields fail without mutation or search leakage. Valid private notes stay out of player/NPC perception histories. |
| Hierarchical discovery | Starting from world/location references, find a known field/key, read/search its body and edit it through returned IDs. Captured context distinguishes partial reads from complete content and does not include all remote object bodies. |
| Bounds and failures | Oversized writes fail unchanged with measured size/limit; search/read outputs are bounded and indicate remaining results. SQL/tool errors retain full context and Developer Mode evidence. No correction loop, truncation or synthetic success. |
| Shared lifecycle | Edits invalidate relevant stale NPC proposals, but note receipts do not start NPC responses. A terminal narration tool ends the GM turn without a closing inference. A later failure retains committed edits and reports failure under existing crash/turn rules. |

Run appropriate Rust/SQL/lint checks and the representative real chat for changed
tools/context. Follow the five-minute testing budget; reserve broad checks for
complete checkpoints. Update implementation_reference.md only for implemented
behaviour, self-review and commit each complete checkpoint. Do not mark this plan
complete until both editing modes, movement, retrieval/limits and verification
are working. Item creation and Cairn rule lookup are not hidden prerequisites.
