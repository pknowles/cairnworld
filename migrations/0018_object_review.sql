-- The Questioner's review of the objects the Storyteller creates: a row means
-- the object was reviewed and not changed since. Changing an object (or
-- anything it contains) deletes its row.
CREATE TABLE object_reviewed (
    kind TEXT NOT NULL CHECK (kind IN ('location', 'connection', 'group', 'npc', 'item_type', 'item')),
    object_id INTEGER NOT NULL,
    PRIMARY KEY (kind, object_id)
);

-- Deleting an object only flags it. Nothing is removed, so its links, agents
-- and history stay inspectable; everything that enumerates objects skips it.
ALTER TABLE location ADD COLUMN deleted INTEGER NOT NULL DEFAULT 0 CHECK (deleted IN (0, 1));
ALTER TABLE connection ADD COLUMN deleted INTEGER NOT NULL DEFAULT 0 CHECK (deleted IN (0, 1));
ALTER TABLE "group" ADD COLUMN deleted INTEGER NOT NULL DEFAULT 0 CHECK (deleted IN (0, 1));
ALTER TABLE character ADD COLUMN deleted INTEGER NOT NULL DEFAULT 0 CHECK (deleted IN (0, 1));
ALTER TABLE item_type ADD COLUMN deleted INTEGER NOT NULL DEFAULT 0 CHECK (deleted IN (0, 1));
ALTER TABLE item ADD COLUMN deleted INTEGER NOT NULL DEFAULT 0 CHECK (deleted IN (0, 1));
