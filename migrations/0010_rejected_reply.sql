-- A reply the agent loop refused: plain text from an agent that must end its
-- turn with a terminal tool call. Developer Mode flags it like a rejected tool
-- call, since either is the model producing an invalid answer.
ALTER TABLE message ADD COLUMN reply_rejected INTEGER NOT NULL DEFAULT 0 CHECK (reply_rejected IN (0, 1));
