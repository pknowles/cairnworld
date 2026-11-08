-- The Storyteller and Questioner each have exactly one chat (their agent_history),
-- so generation progress no longer names a branch for them.
CREATE TABLE world_generation_one_chat (
    world_id INTEGER PRIMARY KEY NOT NULL REFERENCES world(id),
    spent INTEGER NOT NULL DEFAULT 0
);
INSERT INTO world_generation_one_chat (world_id, spent)
    SELECT world_id, spent FROM world_generation;
DROP TABLE world_generation;
ALTER TABLE world_generation_one_chat RENAME TO world_generation;
