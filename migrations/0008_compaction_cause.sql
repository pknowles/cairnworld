-- A compaction forced by a request that did not fit says so itself, and names
-- the rejected inference as its cause (carried onto the compaction invocation),
-- instead of being recognised by having no cause.
ALTER TABLE pending_compaction ADD COLUMN reason TEXT NOT NULL DEFAULT 'threshold'
    CHECK (reason IN ('threshold', 'capacity_recovery'));
UPDATE pending_compaction SET reason = 'capacity_recovery' WHERE cause_inference_id IS NULL;
