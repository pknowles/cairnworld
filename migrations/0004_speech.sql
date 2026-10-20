-- Speech is a completed game event: the approved words are stored once (the
-- action's result content) and every actual hearer's history references them.
-- speech_batch is the unit committed together; simultaneity is derived from a
-- batch holding several utterances.
CREATE TABLE speech_batch (
    id INTEGER PRIMARY KEY NOT NULL,
    world_id INTEGER NOT NULL REFERENCES world(id),
    group_id INTEGER NOT NULL REFERENCES "group"(id),
    sequence_id INTEGER NOT NULL REFERENCES sequence(id)
);

-- One utterance. The names are frozen so old history reconstructs after a
-- rename; the ids keep who spoke and who was addressed. Addressing is not
-- privacy (a whisper's hearers are exactly those holding a speech_message).
CREATE TABLE speech (
    action_id INTEGER PRIMARY KEY NOT NULL REFERENCES action(id),
    batch_id INTEGER NOT NULL REFERENCES speech_batch(id),
    speaker_character_id INTEGER NOT NULL REFERENCES character(id),
    speaker_name TEXT NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN ('say', 'whisper')),
    addressed_character_id INTEGER REFERENCES character(id),
    addressed_name TEXT,
    CHECK ((addressed_character_id IS NULL) = (addressed_name IS NULL))
);

-- Identifies a history message as speech without changing message's origin
-- check; the message still shares the action's result content.
CREATE TABLE speech_message (
    message_id INTEGER PRIMARY KEY NOT NULL REFERENCES message(id),
    speech_action_id INTEGER NOT NULL REFERENCES speech(action_id)
);
CREATE INDEX speech_message_by_speech ON speech_message(speech_action_id);
