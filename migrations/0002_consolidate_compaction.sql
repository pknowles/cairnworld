-- A compaction is one summary event.  Its reason and retained-tail count
-- belong with the summary, rather than in a second visible history entry.
ALTER TABLE summary ADD COLUMN reason TEXT NOT NULL DEFAULT 'threshold'
    CHECK (reason IN ('threshold', 'capacity_recovery'));
ALTER TABLE summary ADD COLUMN retained_messages INTEGER NOT NULL DEFAULT 0
    CHECK (retained_messages >= 0);

-- Preserve the metadata of summaries recorded before consolidation.  Legacy
-- notices were created only for completed summaries, so every existing row
-- receives the matching reason and retained count before the table is removed.
UPDATE summary
SET reason = CASE
        WHEN chat_notice.content LIKE 'Compacted history through a capacity recovery%'
            THEN 'capacity_recovery'
        ELSE 'threshold'
    END,
    retained_messages = CAST(
        substr(
            chat_notice.content,
            instr(chat_notice.content, 'retained the newest ')
                + length('retained the newest ')
        ) AS INTEGER
    )
FROM chat_notice
WHERE chat_notice.source_inference_id = summary.inference_id;

DELETE FROM chat_notice;
DELETE FROM history_entry WHERE kind = 'notice';
DROP TABLE chat_notice;

DROP VIEW summary_content;
CREATE VIEW summary_content AS
SELECT summary.id, summary.history_entry_id, summary.covered_ranges,
       summary.inference_id, summary.reason, summary.retained_messages,
       inference.agent_id, recorded_content.body
FROM summary
JOIN inference ON inference.id = summary.inference_id
JOIN recorded_content ON recorded_content.id = inference.output;
