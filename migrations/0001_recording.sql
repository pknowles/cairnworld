-- Pre-1.0: this is the complete, authoritative schema. Existing databases are
-- deliberately incompatible while the project has no real users or data that
-- must be retained: delete the database and recreate it after changing this
-- file. Stop editing schema history in place and add forward migrations once
-- real user databases exist.

CREATE TABLE world (
    id INTEGER PRIMARY KEY NOT NULL,
    name TEXT NOT NULL,
    time INTEGER NOT NULL DEFAULT 0,
    initial_prompt TEXT NOT NULL DEFAULT '',
    storyteller_summary TEXT NOT NULL DEFAULT '',
    next_action_id INTEGER NOT NULL DEFAULT 0,
    gm_notes TEXT NOT NULL DEFAULT '{}',
    storyteller_notes TEXT NOT NULL DEFAULT '{}',
    -- Owner-controlled toggle. Recording runs regardless; this only decides
    -- whether active members can open the developer inspection views.
    developer_mode INTEGER NOT NULL DEFAULT 0
);

-- OAuth identity is email only. A player-visible name is ordinary profile text:
-- several accounts may deliberately choose the same one.
CREATE TABLE user (
    id INTEGER PRIMARY KEY NOT NULL,
    email TEXT NOT NULL UNIQUE,
    display_name TEXT NOT NULL
);

-- A sandbox chat world has no owner. Every player world has exactly one row
-- here, which keeps developer chat infrastructure from becoming a partial user
-- account model.
CREATE TABLE world_owner (
    world_id INTEGER PRIMARY KEY NOT NULL REFERENCES world(id),
    user_id INTEGER NOT NULL REFERENCES user(id)
);

-- Removal changes access without deleting the relationship or its future
-- characters. The membership, not a client-supplied agent id, is the boundary
-- browser routes resolve before a player event.
CREATE TABLE world_member (
    id INTEGER PRIMARY KEY NOT NULL,
    world_id INTEGER NOT NULL REFERENCES world(id),
    user_id INTEGER NOT NULL REFERENCES user(id),
    access TEXT NOT NULL CHECK (access IN ('active', 'removed')),
    UNIQUE (world_id, user_id)
);

-- An invitation is a revocable capability to activate a membership. It does
-- not become an enduring relationship after acceptance: membership is the
-- relationship, while deleting this row immediately makes its link unusable.
CREATE TABLE world_invitation (
    token TEXT PRIMARY KEY NOT NULL,
    world_id INTEGER NOT NULL REFERENCES world(id),
    max_uses INTEGER CHECK (max_uses IS NULL OR max_uses > 0),
    uses INTEGER NOT NULL DEFAULT 0 CHECK (uses >= 0),
    CHECK (max_uses IS NULL OR uses <= max_uses)
);

CREATE TABLE agent (
    id INTEGER PRIMARY KEY NOT NULL,
    world_id INTEGER NOT NULL REFERENCES world(id)
);

-- A character does not have a location until it is created and placed. This
-- avoids incomplete placeholder rows for the Adventurer created on joining.
CREATE TABLE character (
    id INTEGER PRIMARY KEY NOT NULL,
    world_id INTEGER NOT NULL REFERENCES world(id),
    tool_id INTEGER NOT NULL UNIQUE,
    name TEXT NOT NULL,
    role TEXT NOT NULL CHECK (role IN ('pc', 'npc')),
    in_combat INTEGER NOT NULL DEFAULT 0 CHECK (in_combat IN (0, 1)),
    moved INTEGER NOT NULL DEFAULT 0 CHECK (moved IN (0, 1)),
    acted INTEGER NOT NULL DEFAULT 0 CHECK (acted IN (0, 1)),
    time INTEGER NOT NULL DEFAULT 0,
    description TEXT NOT NULL DEFAULT '',
    background TEXT NOT NULL DEFAULT '',
    motive TEXT NOT NULL DEFAULT '',
    ambition TEXT NOT NULL DEFAULT '',
    sheet TEXT NOT NULL DEFAULT '{}',
    gm_notes TEXT NOT NULL DEFAULT '{}',
    storyteller_notes TEXT NOT NULL DEFAULT '{}'
);

CREATE TABLE player_character (
    member_id INTEGER NOT NULL REFERENCES world_member(id),
    character_id INTEGER PRIMARY KEY NOT NULL REFERENCES character(id),
    agent_id INTEGER NOT NULL UNIQUE REFERENCES agent(id)
);

CREATE TABLE location (
    id INTEGER PRIMARY KEY NOT NULL,
    world_id INTEGER NOT NULL REFERENCES world(id),
    name TEXT NOT NULL,
    kind TEXT NOT NULL,
    description TEXT NOT NULL,
    gm_notes TEXT NOT NULL DEFAULT '{}',
    storyteller_notes TEXT NOT NULL DEFAULT '{}',
    UNIQUE (world_id, name)
);

CREATE TABLE world_starting_location (
    world_id INTEGER PRIMARY KEY NOT NULL REFERENCES world(id),
    location_id INTEGER NOT NULL REFERENCES location(id)
);

CREATE TABLE character_location (
    character_id INTEGER PRIMARY KEY NOT NULL REFERENCES character(id),
    location_id INTEGER NOT NULL REFERENCES location(id),
    description TEXT NOT NULL
);

CREATE TABLE location_gm (
    location_id INTEGER PRIMARY KEY NOT NULL REFERENCES location(id),
    agent_id INTEGER NOT NULL UNIQUE REFERENCES agent(id)
);

CREATE TABLE npc_agent (
    character_id INTEGER PRIMARY KEY NOT NULL REFERENCES character(id),
    agent_id INTEGER NOT NULL UNIQUE REFERENCES agent(id)
);

CREATE TABLE path (
    id INTEGER PRIMARY KEY NOT NULL,
    world_id INTEGER NOT NULL REFERENCES world(id),
    from_location_id INTEGER NOT NULL REFERENCES location(id),
    to_location_id INTEGER NOT NULL REFERENCES location(id),
    travel_time INTEGER NOT NULL,
    description TEXT NOT NULL,
    gm_notes TEXT NOT NULL DEFAULT '{}',
    storyteller_notes TEXT NOT NULL DEFAULT '{}',
    CHECK (from_location_id <> to_location_id)
);

CREATE TABLE item_type (
    id INTEGER PRIMARY KEY NOT NULL,
    world_id INTEGER NOT NULL REFERENCES world(id),
    name TEXT NOT NULL,
    description TEXT NOT NULL,
    stats TEXT NOT NULL DEFAULT '{}',
    gm_notes TEXT NOT NULL DEFAULT '{}',
    storyteller_notes TEXT NOT NULL DEFAULT '{}',
    UNIQUE (world_id, name)
);

CREATE TABLE spell_type (
    id INTEGER PRIMARY KEY NOT NULL,
    world_id INTEGER NOT NULL REFERENCES world(id),
    name TEXT NOT NULL,
    description TEXT NOT NULL,
    stats TEXT NOT NULL DEFAULT '{}',
    gm_notes TEXT NOT NULL DEFAULT '{}',
    storyteller_notes TEXT NOT NULL DEFAULT '{}',
    UNIQUE (world_id, name)
);

CREATE TABLE item (
    id INTEGER PRIMARY KEY NOT NULL,
    world_id INTEGER NOT NULL REFERENCES world(id),
    item_type_id INTEGER NOT NULL REFERENCES item_type(id),
    name TEXT NOT NULL,
    description TEXT NOT NULL,
    gm_notes TEXT NOT NULL DEFAULT '{}',
    storyteller_notes TEXT NOT NULL DEFAULT '{}'
);

-- The two holder relations make item ownership queryable with real foreign
-- keys. Scenario installation creates exactly one holder; the later item
-- transfer operation will change the holder transactionally.
CREATE TABLE item_location (
    item_id INTEGER PRIMARY KEY NOT NULL REFERENCES item(id),
    location_id INTEGER NOT NULL REFERENCES location(id)
);

CREATE TABLE item_character (
    item_id INTEGER PRIMARY KEY NOT NULL REFERENCES item(id),
    character_id INTEGER NOT NULL REFERENCES character(id)
);

-- The original content and reasoning of one event, as a single serialized
-- body. Deliberately no uniqueness constraint: two independently produced
-- events with equal text are still distinct events (design.md's Chat schema).
CREATE TABLE recorded_content (
    id INTEGER PRIMARY KEY NOT NULL,
    body TEXT NOT NULL
);

-- The single writable branch of one agent's linear history. Positions are
-- allocated by UPDATE ... RETURNING next_position in the same transaction
-- that inserts the history_entry using that position.
CREATE TABLE history_branch (
    id INTEGER PRIMARY KEY NOT NULL,
    agent_id INTEGER NOT NULL REFERENCES agent(id),
    next_position INTEGER NOT NULL DEFAULT 0
);

-- Immutable bounded references from a branch to an earlier branch it
-- continues from, ordered by ordinal. Empty in this slice: no branch has
-- parents until split/merge (groups) exist. Parents belong to the same agent
-- and predate their child.
CREATE TABLE history_parent (
    branch_id INTEGER NOT NULL REFERENCES history_branch(id),
    ordinal INTEGER NOT NULL,
    parent_branch_id INTEGER NOT NULL REFERENCES history_branch(id),
    through_position INTEGER NOT NULL,
    PRIMARY KEY (branch_id, ordinal)
);

-- The single writable branch of a linear player, NPC, Storyteller or sandbox
-- history. Every agent kind gets one row here at construction; GM heads move
-- to groups once groups exist.
CREATE TABLE agent_history (
    agent_id INTEGER PRIMARY KEY NOT NULL REFERENCES agent(id),
    branch_id INTEGER NOT NULL UNIQUE REFERENCES history_branch(id)
);

-- The raw history index: one row per message, inference start or summary
-- activation, in branch/position order. Not a duplicate transcript - message,
-- inference and summary rows carry the actual payload.
CREATE TABLE history_entry (
    id INTEGER PRIMARY KEY NOT NULL,
    branch_id INTEGER NOT NULL REFERENCES history_branch(id),
    position INTEGER NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN ('message', 'inference', 'summary', 'notice')),
    UNIQUE (branch_id, position)
);

-- One row for every external game trigger and its lifecycle. Inferences and
-- actions reference this tree root so an entire player event can be
-- reconstructed without duplicating its inputs or outputs. finished_at/error
-- are set once the foreground (root, player-triggered) invocation returns;
-- a deferred compaction this sequence's reply caused gets its own invocation
-- and does not extend this wall-clock span.
CREATE TABLE sequence (
    id INTEGER PRIMARY KEY NOT NULL,
    world_id INTEGER NOT NULL REFERENCES world(id),
    trigger TEXT NOT NULL,
    started_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    finished_at TEXT,
    error TEXT,
    CHECK (finished_at IS NOT NULL OR error IS NULL)
);

-- One agent loop (agent::complete_with_call_context). branch_id is the branch
-- it started on; each inference within it records its own actual branch.
-- The root invocation of a sequence has parent_tool_execution_id NULL; a
-- nested invocation records the exact tool_execution that spawned it.
-- Deferred compaction's invocation has no parent tool call, but records its
-- triggering inference as cause_inference_id instead.
CREATE TABLE invocation (
    id INTEGER PRIMARY KEY NOT NULL,
    -- NULL only for deferred compaction recovering fixed-KV capacity outside
    -- any sequence, mirroring inference.sequence_id's existing nullability
    -- for the identical reason.
    sequence_id INTEGER REFERENCES sequence(id),
    agent_id INTEGER NOT NULL REFERENCES agent(id),
    branch_id INTEGER NOT NULL REFERENCES history_branch(id),
    parent_tool_execution_id INTEGER REFERENCES tool_execution(id),
    cause_inference_id INTEGER REFERENCES inference(id),
    -- Next ordinal to allocate for an inference started within this
    -- invocation, via UPDATE...RETURNING (allocate_position's pattern) so
    -- allocation is one point update instead of a COUNT(*) rescan of every
    -- prior inference in the invocation.
    next_inference_ordinal INTEGER NOT NULL DEFAULT 0,
    started_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    finished_at TEXT,
    -- A short outcome label (settled, rejected, error, budget_exhausted,
    -- interrupted); the inference/tool_execution rows it produced carry the
    -- actual content and errors. Set together with finished_at by the RAII
    -- wrapper on every exit path (agent.rs's InvocationGuard).
    outcome TEXT,
    CHECK ((finished_at IS NULL) = (outcome IS NULL))
);

-- Uniquely identifies each returned tool call by (inference_id, ordinal). The
-- model call ID/name/arguments are not duplicated here: they resolve from the
-- original inference output at that ordinal. One row is created per call in
-- a returned batch before any of them executes, so siblings and unexecuted
-- calls are always visible even if an earlier call fails.
CREATE TABLE tool_execution (
    id INTEGER PRIMARY KEY NOT NULL,
    inference_id INTEGER NOT NULL REFERENCES inference(id),
    ordinal INTEGER NOT NULL,
    result_content_id INTEGER REFERENCES recorded_content(id),
    status TEXT NOT NULL CHECK (status IN ('running', 'completed', 'rejected', 'failed', 'unexecuted')),
    error TEXT,
    started_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    finished_at TEXT,
    UNIQUE (inference_id, ordinal)
);

-- One event in a branch's ordered history. `id` is also its history_entry ID:
-- the branch/position it occupies is looked up there, not duplicated here.
CREATE TABLE message (
    id INTEGER PRIMARY KEY NOT NULL REFERENCES history_entry(id),
    role TEXT NOT NULL,
    content INTEGER NOT NULL REFERENCES recorded_content(id),
    -- The inference that produced this entry: an assistant reply or a tool
    -- result. NULL for player input and delivered narration, which no
    -- inference of this agent emitted.
    source_inference_id INTEGER REFERENCES inference(id),
    -- For a tool-result entry, the tool_execution it answers, so a result
    -- links to its exact call within the producing inference's ordered output.
    source_tool_execution_id INTEGER REFERENCES tool_execution(id),
    -- The sequence this entry triggered, for player input that begins a turn
    -- before any inference exists yet. An entry produced by an inference
    -- resolves its sequence through source_inference_id instead; this stays
    -- NULL there rather than duplicating the same fact.
    trigger_sequence_id INTEGER REFERENCES sequence(id),
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);

-- Notices are visible alongside chat history but never become model context.
-- `id` is also its history_entry ID, like `message`.
CREATE TABLE chat_notice (
    id INTEGER PRIMARY KEY NOT NULL REFERENCES history_entry(id),
    content TEXT NOT NULL,
    -- The compaction (or capacity-recovery) inference this notice reports,
    -- so the developer view links a notice to the summarisation that caused it.
    source_inference_id INTEGER REFERENCES inference(id),
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);

-- Static prompt pieces an inference referenced: role prompts and the tool
-- definitions sent with a request. Rows are written once and never updated, so
-- a recipe referring to one always resolves to the text actually sent.
CREATE TABLE text (
    id INTEGER PRIMARY KEY NOT NULL,
    content TEXT NOT NULL UNIQUE
);

-- Compaction never changes message rows. A summary only changes the portion of
-- a branch's permanent history selected for its next live context. Its
-- activation is a separate history entry from the ranges it covers.
CREATE TABLE summary (
    id INTEGER PRIMARY KEY NOT NULL,
    -- The history_entry ID of this summary's activation (when it became the
    -- selected point-in-time replacement), distinct from the ranges below.
    history_entry_id INTEGER NOT NULL UNIQUE REFERENCES history_entry(id),
    -- Ordered branch/position ranges of original messages this summary
    -- replaces, as JSON `[{branch_id, first_position, last_position}, ...]`.
    -- Always length 1 in this slice (no branch has parents yet); the general
    -- shape avoids a second migration once split/merge produce more.
    covered_ranges TEXT NOT NULL,
    inference_id INTEGER NOT NULL REFERENCES inference(id)
);

CREATE TABLE inference (
    id INTEGER PRIMARY KEY NOT NULL,
    -- The history_entry ID of this inference's start. Completion updates this
    -- row in place; it does not create a new history entry.
    history_entry_id INTEGER NOT NULL UNIQUE REFERENCES history_entry(id),
    agent_id INTEGER NOT NULL REFERENCES agent(id),
    sequence_id INTEGER REFERENCES sequence(id),
    invocation_id INTEGER NOT NULL REFERENCES invocation(id),
    -- Position of this inference within its invocation.
    ordinal INTEGER NOT NULL,
    segments TEXT NOT NULL,
    sampling TEXT NOT NULL,
    output INTEGER REFERENCES recorded_content(id),
    error TEXT,
    input_hash TEXT NOT NULL,
    input_tokens INTEGER,
    output_tokens INTEGER,
    duration_ms INTEGER,
    finished_at TEXT,
    model TEXT NOT NULL,
    tool_choice TEXT NOT NULL DEFAULT '"auto"',
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    CHECK (
        (finished_at IS NULL AND output IS NULL AND error IS NULL AND duration_ms IS NULL)
        OR (finished_at IS NOT NULL AND ((output IS NULL) != (error IS NULL)))
    ),
    CHECK (
        (output IS NULL AND input_tokens IS NULL AND output_tokens IS NULL)
        OR
        (output IS NOT NULL AND input_tokens IS NOT NULL AND output_tokens IS NOT NULL)
    ),
    UNIQUE (invocation_id, ordinal)
);

CREATE VIEW summary_content AS
SELECT summary.id, summary.history_entry_id, summary.covered_ranges,
       summary.inference_id, inference.agent_id, recorded_content.body
FROM summary
JOIN inference ON inference.id = summary.inference_id
JOIN recorded_content ON recorded_content.id = inference.output;

-- Character-action arguments are validated once, retained while the location
-- GM arbitrates, then executed from this row after approval. The world-local
-- id is the value the GM sees; it never has to reproduce model-supplied JSON.
CREATE TABLE pending_action (
    world_id INTEGER NOT NULL REFERENCES world(id),
    id INTEGER NOT NULL,
    sequence_id INTEGER NOT NULL REFERENCES sequence(id),
    -- The tool_execution row for the call that submitted this action; resolves
    -- to its model call ID/name/arguments via its inference's output.
    requesting_tool_execution_id INTEGER NOT NULL REFERENCES tool_execution(id),
    character_id INTEGER NOT NULL REFERENCES character(id),
    location_gm_agent_id INTEGER NOT NULL REFERENCES agent(id),
    tool TEXT NOT NULL,
    args TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (world_id, id)
);

CREATE TABLE action (
    id INTEGER PRIMARY KEY NOT NULL,
    sequence_id INTEGER NOT NULL REFERENCES sequence(id),
    -- The tool_execution whose call performed this action. NULL only for the
    -- server-driven test path that has no inference.
    requesting_tool_execution_id INTEGER REFERENCES tool_execution(id),
    -- The tool_execution of the GM's approval call, when one arbitrated it.
    approving_tool_execution_id INTEGER REFERENCES tool_execution(id),
    result_content_id INTEGER NOT NULL REFERENCES recorded_content(id),
    -- Whether this resolution actually ran the action's state mutation
    -- (true) or only recorded a rejection/no-op (false). A repeated
    -- resolve attempt with a different outcome than this must be rejected,
    -- not silently reported as success - design.md: "They cannot be
    -- approved twice", which requires detecting a *conflicting* second
    -- resolve, not only preventing a second state mutation.
    approved INTEGER NOT NULL CHECK (approved IN (0, 1)),
    -- The world-local pending_action.id this action resolved, so a repeated
    -- approval attempt can look up the already-recorded outcome by matching
    -- world_id (via the sequence join) plus this id, even though the source
    -- pending_action row is deleted on resolve. Not a foreign key for that
    -- reason: the referenced row no longer exists once this is set.
    pending_action_id INTEGER,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);

-- A final reply that crossed the compaction threshold creates one durable
-- obligation. It is not derived from current history, so restart cannot lose
-- work that was already required when the reply was delivered. Still 1:1
-- with an agent in this slice (agent_history is one branch per agent), but
-- keyed by branch so it composes with parent branches once they exist.
CREATE TABLE pending_compaction (
    branch_id INTEGER PRIMARY KEY NOT NULL REFERENCES history_branch(id),
    -- The exact cut this obligation covers: the branch position of the reply
    -- (or capacity-rejected history) that made compaction due.
    position INTEGER NOT NULL,
    -- The sequence whose final reply crossed the threshold. Carried onto the
    -- compaction inference so a summary links back to its triggering event.
    -- NULL when a fixed-KV rejection forced recovery outside any sequence.
    sequence_id INTEGER REFERENCES sequence(id),
    -- The inference whose reply queued this job, carried onto the deferred
    -- compaction invocation as its cause_inference_id (design.md: "deferred
    -- compaction has its triggering inference as cause"). NULL for a
    -- fixed-KV rejection, which has no successfully settled inference to
    -- attribute the obligation to.
    cause_inference_id INTEGER REFERENCES inference(id),
    next_input_tokens INTEGER NOT NULL,
    sampling TEXT NOT NULL,
    model TEXT NOT NULL,
    static_segments TEXT NOT NULL DEFAULT '[]'
);

-- A formatting recipe and verification evidence, never an assembled prompt copy.
CREATE TABLE inference_prompt (
    inference_id INTEGER PRIMARY KEY REFERENCES inference(id),
    format_id INTEGER NOT NULL REFERENCES text(id),
    rendered_at TEXT NOT NULL,
    prompt_hash TEXT NOT NULL,
    tokens_hash TEXT NOT NULL,
    max_output_tokens INTEGER NOT NULL
);
