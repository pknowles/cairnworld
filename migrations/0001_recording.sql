-- Pre-1.0: this schema is edited in place rather than migrated. There are no
-- deployed databases, and supporting migrations this early costs more than it
-- saves. Changing a column here invalidates existing database files - delete
-- them and start again. Add real migrations when a database exists that
-- someone would miss.

CREATE TABLE world (
    id INTEGER PRIMARY KEY NOT NULL,
    name TEXT NOT NULL,
    time INTEGER NOT NULL DEFAULT 0,
    initial_prompt TEXT NOT NULL DEFAULT '',
    storyteller_summary TEXT NOT NULL DEFAULT '',
    next_action_id INTEGER NOT NULL DEFAULT 0,
    gm_notes TEXT NOT NULL DEFAULT '{}',
    storyteller_notes TEXT NOT NULL DEFAULT '{}'
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

CREATE TABLE agent (
    id INTEGER PRIMARY KEY NOT NULL,
    world_id INTEGER NOT NULL REFERENCES world(id)
);

-- An agent is just a history. This relationship is what makes it the one
-- player-facing history for a membership; other agent roles get their own
-- owning game relationship as those game objects are introduced.
CREATE TABLE member_player_agent (
    member_id INTEGER PRIMARY KEY NOT NULL REFERENCES world_member(id),
    agent_id INTEGER NOT NULL UNIQUE REFERENCES agent(id)
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
    member_id INTEGER PRIMARY KEY NOT NULL REFERENCES world_member(id),
    character_id INTEGER NOT NULL UNIQUE REFERENCES character(id)
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

CREATE TABLE message (
    id INTEGER PRIMARY KEY NOT NULL,
    agent_id INTEGER NOT NULL REFERENCES agent(id),
    seq INTEGER NOT NULL,
    role TEXT NOT NULL,
    content TEXT NOT NULL,
    -- Verbatim model reasoning, empty when none was emitted. Recorded because
    -- it is model output, but never fed back into an agent's context.
    reasoning TEXT NOT NULL DEFAULT '',
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    UNIQUE (agent_id, seq)
);

-- Notices are visible alongside chat history but never become model context.
CREATE TABLE chat_notice (
    id INTEGER PRIMARY KEY NOT NULL,
    agent_id INTEGER NOT NULL REFERENCES agent(id),
    after_message_id INTEGER NOT NULL REFERENCES message(id),
    content TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);

-- Static prompt pieces an inference referenced: role prompts and the tool
-- definitions sent with a request. Rows are written once and never updated, so
-- a recipe referring to one always resolves to the text actually sent.
CREATE TABLE text (
    id INTEGER PRIMARY KEY NOT NULL,
    content TEXT NOT NULL
);

-- Compaction never changes message rows. A summary only changes the portion of
-- an agent's permanent history selected for its next live context.
CREATE TABLE summary (
    id INTEGER PRIMARY KEY NOT NULL,
    agent_id INTEGER NOT NULL REFERENCES agent(id),
    covers_to_seq INTEGER NOT NULL,
    content TEXT NOT NULL,
    inference_id INTEGER NOT NULL REFERENCES inference(id),
    UNIQUE (agent_id, covers_to_seq)
);

CREATE TABLE inference (
    id INTEGER PRIMARY KEY NOT NULL,
    agent_id INTEGER NOT NULL REFERENCES agent(id),
    segments TEXT NOT NULL,
    sampling TEXT NOT NULL,
    output TEXT,
    error TEXT,
    input_hash TEXT NOT NULL,
    input_tokens INTEGER,
    output_tokens INTEGER,
    duration_ms INTEGER NOT NULL,
    model TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    CHECK ((output IS NULL) != (error IS NULL)),
    CHECK (
        (output IS NULL AND input_tokens IS NULL AND output_tokens IS NULL)
        OR
        (output IS NOT NULL AND input_tokens IS NOT NULL AND output_tokens IS NOT NULL)
    )
);

-- A final reply that crossed the compaction threshold creates one durable
-- obligation. It is not derived from current history, so restart cannot lose
-- work that was already required when the reply was delivered.
CREATE TABLE pending_compaction (
    agent_id INTEGER PRIMARY KEY NOT NULL REFERENCES agent(id),
    after_message_id INTEGER NOT NULL REFERENCES message(id),
    input_tokens INTEGER NOT NULL,
    sampling TEXT NOT NULL,
    model TEXT NOT NULL
);
