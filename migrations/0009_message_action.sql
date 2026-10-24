-- A history message that shows an action (a spoken line, for now) says which
-- action on the message itself, instead of through a table of its own.
ALTER TABLE message ADD COLUMN action_id INTEGER REFERENCES action(id);
UPDATE message
SET action_id = (
    SELECT speech_message.speech_action_id FROM speech_message
    WHERE speech_message.message_id = message.id
)
WHERE id IN (SELECT message_id FROM speech_message);
CREATE INDEX message_by_action ON message(action_id);
DROP TABLE speech_message;
