-- A connection has a name, like "North Gate" or "Basement Door". Existing
-- connections are named after the locations they join.
ALTER TABLE connection ADD COLUMN name TEXT NOT NULL DEFAULT '';
UPDATE connection SET name = (SELECT origin.name FROM location AS origin WHERE origin.id = connection.from_location_id)
    || ' to ' || (SELECT target.name FROM location AS target WHERE target.id = connection.to_location_id);
