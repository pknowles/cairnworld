-- Consecutive response deadlines a player let pass without responding between
-- them (design.md, Player replies). Per membership, reset by any response.
ALTER TABLE world_member ADD COLUMN missed_replies INTEGER NOT NULL DEFAULT 0;

-- The action a reaction was proposed in answer to (an intervention check), so
-- the GM is shown the reactions of a question together with the action.
ALTER TABLE pending_action ADD COLUMN reacts_to INTEGER;

-- One character's answer to a question about a pending action, with the
-- deadline by which a player must give it. An open reply is settled by the
-- character's next input (answered) or by its deadline (timed_out, which
-- records do-nothing). response holds the answer once settled. Not a foreign key to
-- pending_action: that row is retired when the action is resolved.
CREATE TABLE pending_reply (
    world_id INTEGER NOT NULL,
    action_id INTEGER NOT NULL,
    character_id INTEGER NOT NULL REFERENCES character(id),
    deadline TEXT NOT NULL,
    status TEXT NOT NULL CHECK (status IN ('open', 'answered', 'timed_out')),
    response TEXT,
    PRIMARY KEY (world_id, action_id, character_id)
);
