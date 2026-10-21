-- Stale-work detection: committed fiction/state changes in a group advance its
-- revision; inference and proposal bookkeeping never do.
ALTER TABLE "group" ADD COLUMN scene_revision INTEGER NOT NULL DEFAULT 0;

-- A proposal can request at most one actual action.
CREATE UNIQUE INDEX pending_action_one_per_request ON pending_action(requesting_tool_execution_id);
CREATE UNIQUE INDEX action_one_per_request ON action(requesting_tool_execution_id)
    WHERE requesting_tool_execution_id IS NOT NULL;

-- The causal conversation started by one human turn. Its budget is shared by
-- every follow-up; spent_total is saved whenever a complete step finishes.
CREATE TABLE npc_discussion (
    root_sequence_id INTEGER PRIMARY KEY NOT NULL REFERENCES sequence(id),
    spent_total INTEGER NOT NULL CHECK (spent_total >= 0),
    state TEXT NOT NULL CHECK (state IN ('ready', 'settled', 'failed')),
    failure_sequence_id INTEGER REFERENCES sequence(id),
    CHECK ((state = 'failed') = (failure_sequence_id IS NOT NULL))
);

-- One opportunity for a group's NPCs to answer newly heard speech, collected
-- against one scene revision.
CREATE TABLE npc_round (
    id INTEGER PRIMARY KEY NOT NULL,
    root_sequence_id INTEGER NOT NULL REFERENCES sequence(id),
    group_id INTEGER NOT NULL REFERENCES "group"(id),
    scene_revision INTEGER NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('collecting', 'selected', 'executing', 'settled', 'superseded', 'failed'))
);
CREATE UNIQUE INDEX npc_round_one_open_per_group ON npc_round(group_id)
    WHERE state IN ('collecting', 'selected');

-- The committed speech a round answers (the requesting tool execution of each
-- Say/Whisper action).
CREATE TABLE npc_round_source (
    round_id INTEGER NOT NULL REFERENCES npc_round(id),
    source_tool_execution_id INTEGER NOT NULL REFERENCES tool_execution(id),
    PRIMARY KEY (round_id, source_tool_execution_id)
);

-- An NPC invited to answer. Progress is derived from its invocation and
-- tool_execution records, not stored here.
CREATE TABLE npc_candidate (
    round_id INTEGER NOT NULL REFERENCES npc_round(id),
    character_id INTEGER NOT NULL REFERENCES character(id),
    decision_invocation_id INTEGER UNIQUE REFERENCES invocation(id),
    selected INTEGER NOT NULL DEFAULT 0 CHECK (selected IN (0, 1)),
    execution_sequence_id INTEGER REFERENCES sequence(id),
    PRIMARY KEY (round_id, character_id),
    CHECK (execution_sequence_id IS NULL OR selected = 1)
);

-- A separate chat-turn caused by an earlier event (as opposed to an invocation
-- nested in a tool call).
CREATE TABLE sequence_cause (
    child_sequence_id INTEGER NOT NULL REFERENCES sequence(id),
    source_tool_execution_id INTEGER NOT NULL REFERENCES tool_execution(id),
    PRIMARY KEY (child_sequence_id, source_tool_execution_id)
);
