-- A speech batch grouped utterances committed together, but nothing needs the
-- grouping: when an utterance was committed follows from its action, and which
-- NPCs heard it from the messages in their histories.
CREATE TABLE speech_without_batch (
    action_id INTEGER PRIMARY KEY NOT NULL REFERENCES action(id),
    speaker_character_id INTEGER NOT NULL REFERENCES character(id),
    speaker_name TEXT NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN ('say', 'whisper')),
    addressed_character_id INTEGER REFERENCES character(id),
    addressed_name TEXT,
    -- The GM's short phrase shown to each hearer before the words (how or why
    -- only some of them heard it).
    prefix TEXT,
    text_format TEXT NOT NULL DEFAULT 'legacy' CHECK (text_format IN ('legacy', 'authored')),
    CHECK ((addressed_character_id IS NULL) = (addressed_name IS NULL))
);
INSERT INTO speech_without_batch (action_id, speaker_character_id, speaker_name, kind, addressed_character_id, addressed_name, prefix, text_format)
SELECT action_id, speaker_character_id, speaker_name, kind, addressed_character_id, addressed_name, prefix, text_format FROM speech;
DROP TABLE speech;
ALTER TABLE speech_without_batch RENAME TO speech;
DROP TABLE speech_batch;
