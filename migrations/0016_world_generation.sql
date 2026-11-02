-- The Storyteller and Questioner: one agent each per world, like world_gm.
-- Each run of a role starts on a new empty branch of its agent.
CREATE TABLE world_role_agent (
    world_id INTEGER NOT NULL REFERENCES world(id),
    role TEXT NOT NULL CHECK (role IN ('storyteller', 'questioner')),
    agent_id INTEGER NOT NULL UNIQUE REFERENCES agent(id),
    PRIMARY KEY (world_id, role)
);

-- A generated world's initialization progress; scenario-installed worlds have
-- no row. `stage` selects the next finite step: the Questioner answers the
-- meta-questions (meta), asks questions or declares the world complete (ask),
-- the Storyteller answers question `next_question` of `questions` (answer),
-- replies with the summary (summarize) and, once complete, the name (name);
-- concept_done waits for the authoring tools. A failed step's error stays on its
-- sequence. Each step's outputs commit with the next stage, so a restart resumes
-- at the stored one.
CREATE TABLE world_generation (
    world_id INTEGER PRIMARY KEY NOT NULL REFERENCES world(id),
    stage TEXT NOT NULL CHECK (stage IN ('meta', 'ask', 'answer', 'summarize', 'name', 'concept_done', 'failed')),
    questions TEXT NOT NULL DEFAULT '[]',
    next_question INTEGER NOT NULL DEFAULT 0,
    questioner_branch_id INTEGER NOT NULL REFERENCES history_branch(id),
    storyteller_branch_id INTEGER NOT NULL REFERENCES history_branch(id)
);
