-- What an NPC response round answers is the messages that reached the NPCs'
-- histories, whatever produced them, rather than the Say/Whisper call that
-- requested speech. Each source is one NPC's received message; the speaker's
-- own copy of its words was never a source.
CREATE TABLE npc_round_source_by_message (
    round_id INTEGER NOT NULL REFERENCES npc_round(id),
    message_id INTEGER NOT NULL REFERENCES message(id),
    PRIMARY KEY (round_id, message_id)
);
INSERT INTO npc_round_source_by_message (round_id, message_id)
SELECT DISTINCT npc_round_source.round_id, message.id
FROM npc_round_source
JOIN action ON action.requesting_tool_execution_id = npc_round_source.source_tool_execution_id
JOIN speech ON speech.action_id = action.id
JOIN message ON message.action_id = action.id
JOIN history_entry ON history_entry.id = message.id
JOIN agent_history ON agent_history.branch_id = history_entry.branch_id
JOIN npc_agent ON npc_agent.agent_id = agent_history.agent_id
WHERE npc_agent.character_id <> speech.speaker_character_id;
DROP TABLE npc_round_source;
ALTER TABLE npc_round_source_by_message RENAME TO npc_round_source;

-- A sequence is caused by a tool call (a proposal an NPC made, or a call that
-- requested an action) or by a message that reached an NPC; never both.
CREATE TABLE sequence_cause_by_either (
    child_sequence_id INTEGER NOT NULL REFERENCES sequence(id),
    source_tool_execution_id INTEGER REFERENCES tool_execution(id),
    source_message_id INTEGER REFERENCES message(id),
    CHECK ((source_tool_execution_id IS NULL) <> (source_message_id IS NULL)),
    UNIQUE (child_sequence_id, source_tool_execution_id),
    UNIQUE (child_sequence_id, source_message_id)
);
INSERT INTO sequence_cause_by_either (child_sequence_id, source_tool_execution_id)
SELECT child_sequence_id, source_tool_execution_id FROM sequence_cause;
DROP TABLE sequence_cause;
ALTER TABLE sequence_cause_by_either RENAME TO sequence_cause;
