CREATE TABLE text (
    hash TEXT PRIMARY KEY NOT NULL,
    content TEXT NOT NULL
);

CREATE TABLE inference (
    id INTEGER PRIMARY KEY NOT NULL,
    segments TEXT NOT NULL,
    sampling TEXT NOT NULL,
    output TEXT NOT NULL,
    input_hash TEXT NOT NULL,
    input_tokens INTEGER NOT NULL,
    output_tokens INTEGER NOT NULL,
    duration_ms INTEGER NOT NULL,
    model TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);
