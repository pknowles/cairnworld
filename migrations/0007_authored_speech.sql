-- New utterances keep the character agent's complete authored text, with the
-- speaker, addressee and hearers as metadata. Older utterances keep the fixed
-- "Name says" rendering so their recorded requests reconstruct exactly.
ALTER TABLE speech ADD COLUMN text_format TEXT NOT NULL DEFAULT 'legacy'
    CHECK (text_format IN ('legacy', 'authored'));
