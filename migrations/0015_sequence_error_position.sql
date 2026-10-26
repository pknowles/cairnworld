-- Where a failure belongs in the chat: the highest history entry id that
-- existed when the sequence failed, so it is shown after what happened before
-- it and before what came later. Entry ids share one timeline across every
-- history. Failures recorded before this column are placed by time, which is
-- exact to the second.
ALTER TABLE sequence ADD COLUMN error_position INTEGER;
UPDATE sequence SET error_position = COALESCE(
    (SELECT MAX(history_entry.id) FROM history_entry
       JOIN message ON message.id = history_entry.id
      WHERE message.created_at <= sequence.finished_at), 0)
WHERE error IS NOT NULL;
