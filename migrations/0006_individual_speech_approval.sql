-- Tied NPC replies are approved one at a time, so a group may have a selected
-- set being executed and, alongside it, one collecting follow-up round that
-- waits for the set to finish.
DROP INDEX npc_round_one_open_per_group;
CREATE UNIQUE INDEX npc_round_one_collecting_per_group ON npc_round(group_id)
    WHERE state = 'collecting';
CREATE UNIQUE INDEX npc_round_one_selected_per_group ON npc_round(group_id)
    WHERE state IN ('selected', 'executing');
