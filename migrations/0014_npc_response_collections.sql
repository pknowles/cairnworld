-- An "NPC round" suggested the combat-style turns this game does not have. What
-- the table holds is a collection of NPC responses to some messages: each
-- invited NPC's decision, then the chosen ones, executed.
ALTER TABLE npc_round RENAME TO npc_response_collection;
ALTER TABLE npc_round_source RENAME TO npc_response_source;
ALTER TABLE npc_response_source RENAME COLUMN round_id TO collection_id;
ALTER TABLE npc_candidate RENAME COLUMN round_id TO collection_id;
DROP INDEX npc_round_one_collecting_per_group;
DROP INDEX npc_round_one_selected_per_group;
CREATE UNIQUE INDEX npc_collection_one_collecting_per_group ON npc_response_collection(group_id)
    WHERE state = 'collecting';
CREATE UNIQUE INDEX npc_collection_one_selected_per_group ON npc_response_collection(group_id)
    WHERE state IN ('selected', 'executing');
