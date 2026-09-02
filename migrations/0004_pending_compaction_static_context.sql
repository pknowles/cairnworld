-- A pending compaction reuses the static portion of the exact normal request
-- that created it. The deferred worker needs this durable recipe only if its
-- ordinary summary cannot fit and it must measure a fallback candidate.
ALTER TABLE pending_compaction ADD COLUMN static_segments TEXT NOT NULL DEFAULT '[]';
