# GM field editing and hierarchical lookup

Pending implementation. First deliverable: a character moves within its existing
location, the GM edits persistent placement and relevant notes, and the next
inference/turn reads that state before producing consistent narration. Both
whole-value replacement and exact-text editing are required from the outset.
The movement example uses the preceding unified timing feature: CombatMove
updates placement immediately, while Move starts an activity whose completion
or interruption resolves actual progress. Read/edit tools do not own another clock.

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

Each object's GM notes and Storyteller notes are separate fields, each with a
top-level header/description and named subnotes. A short notes field can consist
of just the header. Shared subnotes appear in the same list as ordinary subnotes;
they do not replace or share the object's whole notes field. The header and
bounded subnote names/descriptions orient the GM before it reads larger bodies.
Show a shared marker and stable subnote ID in the listing before opening it.
There is no separate shared-notes browser or second lookup/edit system.

Compare mappings onto existing JSON fields and normalized subnote records using
the same short/large discovery and edit cases. Select storage based on measured
context sizes, code and maintenance cost, preserving this model-facing hierarchy.
Check that the GM can discover relevant detail without already knowing its key.
Both editing operations apply to headers and subnote text; identity and link
changes use explicit operations rather than replacing an object's entire notes
field and accidentally overwriting shared bodies.

## Dependencies and initial bounds

Shared subnotes are part of this feature, not a separate plot/scenario subsystem.
The [shared-subnote design](../design.md#shared-subnotes) follows the declaration that
one subnote may belong to multiple objects, with agent-managed links and linked
objects visible when reading it. Implement storage, scenario integration and
read/edit/link tools together in the first checkpoint; the second extends their
navigation/search. Hard-coded scenarios can then use them without waiting for
the Storyteller executor. This does not reorder todo.md or require a temporary
import-only implementation.

NPC interaction supplies the terminal narration tools and shared action path;
code-quality checks and [Unified game time](unified-game-time.md) precede this
plan in todo.md. Use its agreed action admission, ready-times, EndTurn and activity
start/completion/interruption operations. Do not increment a world-time column,
introduce combat flags or add a lead-window policy. Timing questions that block
movement must be resolved in that design/plan before this example is implemented.

Before enabling writes in the first checkpoint, configure finite field/note,
key/index and tool-response limits. Choose and record their numerical values
from measured assembled inputs for the configured model, including schemas,
old/new edit arguments, returned content and next-inference/output headroom.
Use small configurable limits in deterministic checks. There is no unbounded
first version; the second checkpoint extends this measurement to retrieval.

## First checkpoint: read/edit and movement

- Add world-owned shared subnotes with stable IDs, bounded titles/text bodies and
  GM/Storyteller categories. Keep existing single-object note fields; a shared
  subnote is an explicit choice, not a mandatory migration of every subnote. Store
  each shared body once. Link it to existing typed object references (World,
  Character, Group, Location, Connection, ItemType, Item and supported SpellType
  references), reusing their world checks. Enforce endpoint existence and unique
  subnote/object pairs in storage; do not accept dangling arbitrary table/ID pairs.
- Extend the same authorized read/write/exact-edit operations to shared-subnote
  IDs. Add create, link and unlink operations with explicit subnote/object IDs;
  creation includes its initial links in one transaction. Return the assigned
  ID and committed links. Repeated linking produces one association. Unlinking
  retains the body and other links, including when the final link is removed;
  retain evidence for Developer Mode. Do not implement automatic archival or
  physical note deletion. Keep note edits and link changes in ordinary recorded
  tool executions rather than adding another audit system.
- Apply existing role permissions to shared bodies and link operations. A GM
  accesses notes through authorized local objects or its world reference; a
  remote link alone does not expand object authority. Editing an authorized
  shared GM note changes its body for every linked object. Linking/unlinking
  requires authority over the endpoint and authority to change the note; GM
  cannot relink read-only Storyteller notes. Character links identify relevance,
  not permission for that character's agent to read private notes. Full reads
  return linked object IDs/types/names as well as text; large link lists use the
  same bounded paging as other reads, with partial results clearly indicated.
- Add an optional shared-subnotes collection to the existing scenario format,
  defaulting to empty for existing files. Each entry defines a scenario-local
  key, category, title/body and typed object references. Resolve references
  through the scenario importer's existing object mapping after objects exist;
  use stable scenario-local keys where names cannot uniquely identify an object.
  Reject duplicate keys and unresolved/foreign links without a partially
  installed world. Reuse shared-subnote validation/storage instead of writing JSON
  copies into linked objects. Export one body per note with its links and
  remapped local references, preserving unlinked notes too. Import/export is a
  development capability, not a prerequisite Storyteller inference.
- Adapt Bread Thief as the concrete example: one Storyteller subnote linked to Mara
  and Toma records the completed mill work, owed flour, flood damage and competing
  household needs. Each character's private background still supplies its own
  memories, beliefs and aspirations: shared GM/Storyteller notes do not replace
  that character's knowledge. Remove redundant GM-facing copies only after captured
  GM contexts show the situation remains available. Verify this through the real
  game path; do not count matching tool names as proof of preserved knowledge.
- Define a small typed set of writable fields on existing object references:
  character placement within its current group, group placement, relevant public
  descriptions and named GM note values. Reuse existing object IDs/types where
  appropriate, but not unrestricted Developer Mode query access. Preserve
  arbitrary note keys; only text values accept exact-text edits. Never accept
  SQL, filesystem paths or model-selected table/column names as executable input.
  A whole-value write may create a named GM note on an existing authorized
  object. Include explicit removal of a named note through the same field access,
  so obsolete short-term notes do not accumulate as empty keys; retain the old
  content in recorded evidence. Neither operation
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
  Fields for health, inventories, membership, activity lifecycle, resource
  ready-times and timed effects stay behind their action operations. Descriptive edits do
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
  Include shared-subnote changes and link changes: deduplicate by subnote ID when
  several current objects link the same note. Use the measured note discovery
  representation chosen above; do not automatically expand all linked bodies or
  silently omit facts needed for the current decision. Editing a shared body
  invalidates affected current scenes just as edits to local note values do.
- Add the declared Move/CombatMove through shared character submission and GM
  arbitration. Both use the canonical relative-placement field. CombatMove checks
  and consumes its ready resource and applies its approved short displacement
  immediately. Move starts a longer activity with GM-assigned duration; approval
  records observable departure, not arrival. GM updates placement/local facts at
  actual completion or interruption using the existing timing resolution.
- Demonstrate movement behind a pillar using CombatMove and longer movement
  using Move. GM reads current placement/location notes, applies authorized field
  edits consistent with actual progress, sees the results and ends its invocation
  with the appropriate narration tool. Later requests use persisted placement.
  Neither bookkeeping edits nor repeated field writes authorize another move or
  complete an activity early. Reuse this movement code in Rules and travel;
  cross-location Travel and damage/attack rules remain there.

## Second checkpoint: hierarchical lookup, search and bounded notes

- List existing child objects from actual relationships: world locations and
  ItemTypes, location groups/items/connections, group characters and an object's available
  fields/note keys. Return IDs, names and bounded orientation data; do not dump
  every body or fabricate a directory tree. Returned references are usable in
  subsequent read/edit calls. Keep World notes in this same object/field model.
  Include authorized prepared ItemTypes that have no item instance yet, so the
  GM can read their IDs/descriptions/stats for later SplitItem/CombineItems
  approval. Mechanical stats are read-only here; their application stays in
  the action operations, and type authoring stays with the Storyteller.
- Search literal text in explicitly selected authorized object kinds, fields or
  named note values. Return matching object/field references and bounded context,
  with paging/continuation for more results. Share authorization with reads and
  edits; neither search snippets nor counts may expose unauthorized notes.
  No regex language, raw SQL or associative-memory engine is required initially.
- Include shared-subnote references in each object's subnote listing and shared
  bodies in authorized searches; deduplicate matches by subnote ID rather than returning one
  copy per link. Return link context so the caller can understand why a note
  concerns its current objects. A full read must allow navigation of all linked
  references through bounded pages. No recursive transitive loading of note or
  object bodies is implied. Listing a shared subnote identifies it as shared
  without loading its body; unlinking it leaves the object's header and other
  subnotes unchanged.
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
| Movement and next-turn consistency | Exercise CombatMove's immediate placement/resource use and Move's activity start plus completion or interruption. Inspect state, histories and captured GM inputs before/after arrival; the destination is not applied prematurely and later narration uses persisted progress after reconnect/restart. Group/location IDs do not change. Repeated edits cannot bypass ready-times or grant completion. |
| Short and long edits | Scripted calls replace a whole short value and one passage in a large value, then create and remove an incidental named note. Unrelated passages/keys remain intact; a chair remains location text/notes without a new Item. Missing/ambiguous text fails visibly without mutation. No game object is created/deleted by note operations. |
| Newly invented incidental detail | GM reads existing relevant detail before adding it to descriptions/GM notes, then narrates only after its write result. A subsequent read finds the detail. No forced object creation and no extra after-narration inference. |
| Permissions and mechanical state | Foreign-world/local-scope violations, GM writes to Storyteller notes and edits to mechanical fields fail without mutation or search leakage. Valid private notes stay out of player/NPC perception histories. |
| Hierarchical discovery | Starting from world/location references, find a known field/key, read/search its body and edit it through returned IDs. Captured context distinguishes partial reads from complete content and does not include all remote object bodies. |
| Shared subnote and unlink | Import a subnote linked to two characters and a location. Each object's notes listing identifies it as shared before reading its body; full reads expose linked references. Edit through one reference and read the same committed body through another. Unlink one object: its header and ordinary subnotes stay unchanged, the others keep the shared subnote and no body is deleted, including after removing the last link. Reload and export/reimport preserve relationships/content without multiplying bodies. |
| Shared-subnote authority and context | Foreign links and unauthorized reads/edits fail without mutation or snippets leaking private content. Linking a character grants it no private context or perceived event. Multiple local links contribute one subnote body; a shared edit reaches the next GM input. Bread Thief retains both NPCs' own knowledge and the GM's dispute facts while remote unrelated notes stay outside the request. |
| Bounds and failures | Oversized writes fail unchanged with measured size/limit; search/read outputs are bounded and indicate remaining results. SQL/tool errors retain full context and Developer Mode evidence. No correction loop, truncation or synthetic success. |
| Shared lifecycle | Edits invalidate relevant stale NPC proposals, but note receipts do not start NPC responses. A terminal narration tool ends the GM turn without a closing inference. A later failure retains committed edits and reports failure under existing crash/turn rules. |

Run appropriate Rust/SQL/lint checks and the representative real chat for changed
tools/context. Follow the five-minute testing budget; reserve broad checks for
complete checkpoints. Update implementation_reference.md only for implemented
behaviour, self-review and commit each complete checkpoint. Do not mark this plan
complete until both editing modes, shared-subnote links/import/export, movement,
retrieval/limits and verification are working. Item creation and Cairn rule lookup
are not hidden prerequisites.
