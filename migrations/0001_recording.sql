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
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    UNIQUE (agent_id, seq)
);

CREATE TABLE text (
    hash TEXT PRIMARY KEY NOT NULL,
    content TEXT NOT NULL
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
