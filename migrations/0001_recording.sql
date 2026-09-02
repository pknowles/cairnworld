-- Pre-1.0: this schema is edited in place rather than migrated. There are no
-- deployed databases, and supporting migrations this early costs more than it
-- saves. Changing a column here invalidates existing database files - delete
-- them and start again. Add real migrations when a database exists that
-- someone would miss.

CREATE TABLE world (
    id INTEGER PRIMARY KEY NOT NULL,
    name TEXT NOT NULL
);

CREATE TABLE agent (
    id INTEGER PRIMARY KEY NOT NULL,
    world_id INTEGER NOT NULL REFERENCES world(id),
    kind TEXT NOT NULL,
    name TEXT NOT NULL
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
