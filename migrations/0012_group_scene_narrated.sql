-- Whether the GM has narrated the group's initial scene to the characters in
-- it. A group installed from a scenario starts without; groups made by a split
-- or merge continue a scene that was already narrated, and so does every group
-- that existed before this column.
ALTER TABLE "group" ADD COLUMN scene_narrated INTEGER NOT NULL DEFAULT 1 CHECK (scene_narrated IN (0, 1));
