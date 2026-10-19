ALTER TABLE presence_narration
ADD COLUMN waiting_for_return INTEGER NOT NULL DEFAULT 1
CHECK (waiting_for_return IN (0, 1));

UPDATE presence_narration
SET waiting_for_return = NOT EXISTS (
    SELECT 1 FROM character_group
    JOIN character_presence USING (character_id)
    WHERE character_group.group_id = presence_narration.group_id
      AND character_presence.absent_since IS NULL
);
