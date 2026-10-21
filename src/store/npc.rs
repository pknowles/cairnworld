//! NPC response rounds (design.md's NPC speech and response rounds). Speech an
//! NPC hears opens a round in which each hearing NPC may answer; progress is
//! derived from the invocation, tool-execution and sequence records the steps
//! produce, never copied into a parallel state machine.

use anyhow::{Context, Result, bail, ensure};
use sqlx::{Sqlite, Transaction};

use super::{
    AgentBranch, CharacterActor, PendingAction, SceneItem, Store, interruption::CallClosure,
};
use crate::llm::{Content, Message, MessageContent, ToolCall};

/// A speech action an NPC would take, with how strongly it wants to.
/// Proposals never act; the GM decides whether the words are said and who
/// hears them.
pub struct SpeechProposal {
    /// The action tool the NPC called: `say` or `whisper`.
    pub tool: &'static str,
    /// The action's own arguments, exactly as that tool takes them.
    pub arguments: String,
    pub eagerness: u8,
}

/// The one terminal decision an NPC returns.
pub enum Decision {
    Nothing,
    Propose(SpeechProposal),
}

/// The action's own tool schema plus the NPC-only `eagerness` argument.
pub fn with_eagerness(mut schema: serde_json::Value) -> serde_json::Value {
    schema["properties"]["eagerness"] = serde_json::json!({
        "description": "How strongly you want to act now: 1 mild, 2 moderate, 3 strong.",
        "type": "integer",
        "minimum": 1,
        "maximum": 3,
    });
    match schema["required"].as_array_mut() {
        Some(required) => required.push("eagerness".into()),
        None => schema["required"] = serde_json::json!(["eagerness"]),
    }
    schema
}

impl Decision {
    /// The decision a model's whole response makes, or why it breaks the
    /// protocol: exactly one `do_nothing`, `say` or `whisper` call.
    pub fn parse(content: &Content) -> Result<(ToolCall, Self)> {
        let Content::ToolCalls(calls) = content else {
            bail!("the NPC replied with text instead of calling do_nothing, say or whisper");
        };
        let [call] = calls.as_slice() else {
            bail!(
                "the NPC made {} tool calls; exactly one of do_nothing, say or whisper is required",
                calls.len()
            );
        };
        let decision = match call.name.as_str() {
            "do_nothing" => {
                let arguments: serde_json::Value = serde_json::from_str(&call.arguments)
                    .context("parsing do_nothing arguments")?;
                ensure!(
                    arguments.as_object().is_some_and(serde_json::Map::is_empty),
                    "do_nothing takes no arguments"
                );
                Self::Nothing
            }
            name @ ("say" | "whisper") => {
                let mut arguments: serde_json::Map<String, serde_json::Value> =
                    serde_json::from_str(&call.arguments)
                        .with_context(|| format!("parsing {name} arguments"))?;
                let eagerness = arguments
                    .remove("eagerness")
                    .context("an eagerness from 1 to 3 is required")?;
                let eagerness = eagerness
                    .as_u64()
                    .filter(|value| (1..=3).contains(value))
                    .with_context(|| {
                        format!("eagerness {eagerness} is not an integer from 1 to 3")
                    })?;
                Self::Propose(SpeechProposal {
                    tool: if name == "say" { "say" } else { "whisper" },
                    arguments: serde_json::Value::Object(arguments).to_string(),
                    eagerness: u8::try_from(eagerness).context("eagerness out of range")?,
                })
            }
            name => bail!("the NPC called `{name}`, not do_nothing, say or whisper"),
        };
        Ok((call.clone(), decision))
    }
}

/// What an NPC knows of itself and its surroundings: nothing the GM or
/// Storyteller keeps private.
#[derive(serde::Serialize)]
pub struct NpcScene {
    pub you: Person,
    pub background: String,
    pub motive: String,
    pub ambition: String,
    pub location: String,
    pub location_description: String,
    pub items: Vec<SceneItem>,
    pub present: Vec<Person>,
}

#[derive(serde::Serialize)]
pub struct Person {
    pub name: String,
    pub description: String,
}

#[derive(Clone, Copy)]
pub struct NpcRound {
    pub id: i64,
    pub root_sequence_id: i64,
    pub group_id: i64,
    pub world_id: i64,
}

/// The next finite piece of NPC work in a world.
pub enum NpcStep {
    /// The scene changed since the round was collected; answer afresh.
    Refresh(NpcRound),
    /// Collect one NPC's decision.
    Decide { round: NpcRound, character_id: i64 },
    /// Every NPC has decided: pick the winners.
    Select(NpcRound),
    /// Take the next selected utterance to the GM.
    Execute(NpcRound),
}

/// A started decision: its own sequence and invocation, claimed before inference.
pub struct DecisionClaim {
    pub root_sequence_id: i64,
    pub sequence_id: i64,
    pub invocation_id: i64,
    pub branch: AgentBranch,
}

/// One selected utterance, now a pending action for the GM.
pub struct ExecutionClaim {
    pub round_id: i64,
    pub root_sequence_id: i64,
    pub sequence_id: i64,
    pub pending: PendingAction,
    /// Other replies were chosen alongside this one, composed without hearing it.
    pub tied: bool,
}

struct Decided {
    character_id: i64,
    decision: Decision,
}

impl Store {
    /// Called in the transaction that commits an utterance. Advances the
    /// group's scene revision and opens a round at the new revision for every
    /// NPC that actually heard it; see `reopen_round`.
    pub(super) async fn open_response_round(
        transaction: &mut Transaction<'_, Sqlite>,
        group_id: i64,
        action_id: i64,
        sequence_id: i64,
    ) -> Result<()> {
        let revision = Self::advance_scene_revision(transaction, group_id).await?;
        let Some(source) = sqlx::query_scalar!(
            "SELECT requesting_tool_execution_id FROM action WHERE id = ?",
            action_id,
        )
        .fetch_one(&mut **transaction)
        .await
        .context("finding the call that requested the speech")?
        else {
            return Ok(());
        };
        // A follow-up speech belongs to the discussion whose step produced it;
        // anything else is a human turn starting its own.
        let own = sqlx::query!(
            "SELECT npc_round.id, npc_round.root_sequence_id FROM npc_candidate \
             JOIN npc_round ON npc_round.id = npc_candidate.round_id \
             WHERE npc_candidate.execution_sequence_id = ?",
            sequence_id,
        )
        .fetch_optional(&mut **transaction)
        .await
        .context("finding the discussion this speech continues")?;
        let root = own.as_ref().map_or(sequence_id, |own| own.root_sequence_id);
        let own_round = own.map(|own| own.id);
        // The selected set's own utterances are its progress, not an outside
        // change, unless something else changed the scene since it was chosen.
        if let Some(own_round) = own_round {
            sqlx::query!(
                "UPDATE npc_round SET scene_revision = ?1 WHERE id = ?2 AND scene_revision = ?1 - 1",
                revision,
                own_round,
            )
            .execute(&mut **transaction)
            .await
            .context("recording the selected set's progress")?;
        }
        Self::reopen_round(
            transaction,
            group_id,
            root,
            revision,
            own_round,
            Some(source),
        )
        .await
    }

    /// Committed fiction or state in the group changed, so work collected
    /// against the old revision is stale.
    pub(super) async fn advance_scene_revision(
        transaction: &mut Transaction<'_, Sqlite>,
        group_id: i64,
    ) -> Result<i64> {
        sqlx::query_scalar!(
            "UPDATE \"group\" SET scene_revision = scene_revision + 1 WHERE id = ? \
             RETURNING scene_revision",
            group_id,
        )
        .fetch_one(&mut **transaction)
        .await
        .context("advancing the group's scene revision")
    }

    /// Supersede the group's open round (carrying the speech it had not yet
    /// answered), then open a round at `revision` answering that speech plus
    /// `new_source`, for the NPCs who heard any of it and are still present.
    /// It stays ineligible until the sequence that produced each source
    /// speech has finished successfully.
    async fn reopen_round(
        transaction: &mut Transaction<'_, Sqlite>,
        group_id: i64,
        root: i64,
        revision: i64,
        keep_round: Option<i64>,
        new_source: Option<i64>,
    ) -> Result<()> {
        let mut sources = Vec::new();
        // `keep_round` is the selected set executing this very speech.
        let open_rounds = sqlx::query!(
            "SELECT id, root_sequence_id FROM npc_round              WHERE group_id = ?1 AND state IN ('collecting', 'selected') AND id IS NOT ?2",
            group_id,
            keep_round,
        )
        .fetch_all(&mut **transaction)
        .await
        .context("finding the group's open rounds")?;
        for open in open_rounds {
            sources.extend(
                sqlx::query_scalar!(
                    "SELECT source_tool_execution_id FROM npc_round_source WHERE round_id = ?",
                    open.id,
                )
                .fetch_all(&mut **transaction)
                .await
                .context("carrying an open round's unanswered speech")?,
            );
            sqlx::query!(
                "UPDATE npc_round SET state = 'superseded' WHERE id = ?",
                open.id
            )
            .execute(&mut **transaction)
            .await
            .context("superseding an open round")?;
            Self::settle_idle_discussion(transaction, open.root_sequence_id).await?;
        }
        sources.extend(new_source);
        sources.sort_unstable();
        sources.dedup();
        let mut roster = Vec::new();
        for source in &sources {
            roster.extend(
                sqlx::query_scalar!(
                    "SELECT DISTINCT npc_agent.character_id AS \"character_id!: i64\" \
                     FROM action \
                     JOIN speech ON speech.action_id = action.id \
                     JOIN speech_message ON speech_message.speech_action_id = speech.action_id \
                     JOIN history_entry ON history_entry.id = speech_message.message_id \
                     JOIN agent_history ON agent_history.branch_id = history_entry.branch_id \
                     JOIN npc_agent ON npc_agent.agent_id = agent_history.agent_id \
                     JOIN character_group ON character_group.character_id = npc_agent.character_id \
                     WHERE action.requesting_tool_execution_id = ? \
                       AND npc_agent.character_id <> speech.speaker_character_id \
                       AND character_group.group_id = ?",
                    source,
                    group_id,
                )
                .fetch_all(&mut **transaction)
                .await
                .context("finding the NPCs who heard the speech")?,
            );
        }
        roster.sort_unstable();
        roster.dedup();
        if roster.is_empty() {
            return Ok(());
        }
        let round_id = sqlx::query_scalar!(
            "INSERT INTO npc_round (root_sequence_id, group_id, scene_revision, state) \
             VALUES (?, ?, ?, 'collecting') RETURNING id",
            root,
            group_id,
            revision,
        )
        .fetch_one(&mut **transaction)
        .await
        .context("opening a response round")?;
        for source in sources {
            sqlx::query!(
                "INSERT INTO npc_round_source (round_id, source_tool_execution_id) VALUES (?, ?)",
                round_id,
                source,
            )
            .execute(&mut **transaction)
            .await
            .context("recording the speech a round answers")?;
        }
        for character_id in roster {
            sqlx::query!(
                "INSERT INTO npc_candidate (round_id, character_id) VALUES (?, ?)",
                round_id,
                character_id,
            )
            .execute(&mut **transaction)
            .await
            .context("inviting an NPC to answer")?;
        }
        Ok(())
    }

    /// A group's characters moved on, so its unanswered round no longer applies.
    pub(super) async fn retire_group_rounds(
        transaction: &mut Transaction<'_, Sqlite>,
        group_id: i64,
    ) -> Result<()> {
        sqlx::query!(
            "UPDATE npc_round SET state = 'superseded' \
             WHERE group_id = ? AND state IN ('collecting', 'selected')",
            group_id,
        )
        .execute(&mut **transaction)
        .await
        .context("superseding a retired group's round")?;
        Ok(())
    }

    /// A discussion with no open round has run its course.
    async fn settle_idle_discussion(
        transaction: &mut Transaction<'_, Sqlite>,
        root: i64,
    ) -> Result<()> {
        sqlx::query!(
            "UPDATE npc_discussion SET state = 'settled' \
             WHERE root_sequence_id = ?1 AND state = 'ready' \
               AND NOT EXISTS (SELECT 1 FROM npc_round WHERE root_sequence_id = ?1 \
                               AND state IN ('collecting', 'selected', 'executing'))",
            root,
        )
        .execute(&mut **transaction)
        .await
        .context("settling an idle discussion")?;
        Ok(())
    }

    /// Finish a human turn, making the rounds its speech opened eligible with
    /// the budget it spent, or failing them if the turn failed.
    pub async fn finish_root_turn(
        &self,
        sequence_id: i64,
        error: Option<&str>,
        spent: u32,
    ) -> Result<()> {
        let mut transaction = self.pool.begin().await.context("finishing a human turn")?;
        Self::finish_sequence_in(&mut transaction, sequence_id, error).await?;
        if error.is_some() {
            sqlx::query!(
                "UPDATE npc_round SET state = 'failed' \
                 WHERE root_sequence_id = ? AND state IN ('collecting', 'selected', 'executing')",
                sequence_id,
            )
            .execute(&mut *transaction)
            .await
            .context("failing the rounds of a failed turn")?;
        } else {
            let spent = i64::from(spent);
            sqlx::query!(
                "INSERT INTO npc_discussion (root_sequence_id, spent_total, state) \
                 SELECT ?1, ?2, 'ready' WHERE EXISTS (SELECT 1 FROM npc_round WHERE root_sequence_id = ?1) \
                 ON CONFLICT(root_sequence_id) DO UPDATE SET spent_total = excluded.spent_total",
                sequence_id,
                spent,
            )
            .execute(&mut *transaction)
            .await
            .context("saving the discussion budget")?;
        }
        transaction
            .commit()
            .await
            .context("committing a human turn's end")
    }

    /// The NPC's own facts and what it can see where it stands.
    pub async fn npc_scene(&self, character_id: i64) -> Result<NpcScene> {
        let you = sqlx::query!(
            "SELECT character.name, character.description, character.background, character.motive,                     character.ambition, location.id AS location_id, location.name AS location,                     location.description AS location_description, character_group.group_id              FROM character              JOIN character_group ON character_group.character_id = character.id              JOIN \"group\" ON \"group\".id = character_group.group_id              JOIN location ON location.id = \"group\".location_id              WHERE character.id = ? AND character.role = 'npc'",
            character_id,
        )
        .fetch_one(&self.pool)
        .await
        .with_context(|| format!("loading NPC {character_id}'s surroundings"))?;
        let present = sqlx::query_as!(
            PersonRow,
            "SELECT character.name, character.description FROM character_group              JOIN character ON character.id = character_group.character_id              WHERE character_group.group_id = ? AND character.id <> ? ORDER BY character.id",
            you.group_id,
            character_id,
        )
        .fetch_all(&self.pool)
        .await
        .context("loading who is present")?
        .into_iter()
        .map(|row| Person {
            name: row.name,
            description: row.description,
        })
        .collect();
        Ok(NpcScene {
            you: Person {
                name: you.name,
                description: you.description,
            },
            background: you.background,
            motive: you.motive,
            ambition: you.ambition,
            location: you.location,
            location_description: you.location_description,
            items: Self::location_items(&self.pool, you.location_id).await?,
            present,
        })
    }

    /// How many inferences the discussion has already spent.
    pub async fn discussion_spent(&self, root_sequence_id: i64) -> Result<u32> {
        let spent = sqlx::query_scalar!(
            "SELECT spent_total FROM npc_discussion WHERE root_sequence_id = ?",
            root_sequence_id,
        )
        .fetch_one(&self.pool)
        .await
        .context("loading the discussion budget")?;
        u32::try_from(spent).context("discussion budget exceeds u32")
    }

    /// The next finite NPC step for this world, if any work is eligible.
    pub async fn next_npc_step(&self, world_id: i64) -> Result<Option<NpcStep>> {
        let Some(row) = sqlx::query!(
            "SELECT npc_round.id, npc_round.root_sequence_id, npc_round.group_id, \
                    npc_round.state, npc_round.scene_revision, \
                    \"group\".scene_revision AS current_revision \
             FROM npc_round \
             JOIN \"group\" ON \"group\".id = npc_round.group_id \
             JOIN npc_discussion ON npc_discussion.root_sequence_id = npc_round.root_sequence_id \
             WHERE \"group\".world_id = ? AND \"group\".retired_at IS NULL \
               AND npc_round.state IN ('collecting', 'selected') \
               AND npc_discussion.state = 'ready'                AND (npc_round.state = 'selected' OR NOT EXISTS (                    SELECT 1 FROM npc_round AS earlier                    WHERE earlier.group_id = npc_round.group_id                      AND earlier.state IN ('selected', 'executing'))) \
               AND NOT EXISTS ( \
                   SELECT 1 FROM npc_round_source \
                   JOIN action ON action.requesting_tool_execution_id = npc_round_source.source_tool_execution_id \
                   JOIN speech ON speech.action_id = action.id \
                   JOIN speech_batch ON speech_batch.id = speech.batch_id \
                   JOIN sequence ON sequence.id = speech_batch.sequence_id \
                   WHERE npc_round_source.round_id = npc_round.id \
                     AND (sequence.finished_at IS NULL OR sequence.error IS NOT NULL)) \
             ORDER BY npc_round.id LIMIT 1",
            world_id,
        )
        .fetch_optional(&self.pool)
        .await
        .context("looking for eligible NPC work")?
        else {
            return Ok(None);
        };
        let round = NpcRound {
            id: row.id,
            root_sequence_id: row.root_sequence_id,
            group_id: row.group_id,
            world_id,
        };
        if row.scene_revision != row.current_revision {
            return Ok(Some(NpcStep::Refresh(round)));
        }
        if row.state == "selected" {
            return Ok(Some(NpcStep::Execute(round)));
        }
        let waiting = sqlx::query_scalar!(
            "SELECT character_id FROM npc_candidate \
             WHERE round_id = ? AND decision_invocation_id IS NULL \
             ORDER BY character_id LIMIT 1",
            round.id,
        )
        .fetch_optional(&self.pool)
        .await
        .context("looking for a waiting NPC")?;
        Ok(Some(match waiting {
            Some(character_id) => NpcStep::Decide {
                round,
                character_id,
            },
            None => NpcStep::Select(round),
        }))
    }

    /// The scene changed under an open round: answer the same speech afresh,
    /// for whoever is still present.
    pub async fn refresh_round(&self, round: &NpcRound) -> Result<()> {
        let mut transaction = self
            .pool
            .begin()
            .await
            .context("refreshing a stale round")?;
        let revision = sqlx::query_scalar!(
            "SELECT scene_revision FROM \"group\" WHERE id = ?",
            round.group_id,
        )
        .fetch_one(&mut *transaction)
        .await
        .context("reading the group's scene revision")?;
        Self::reopen_round(
            &mut transaction,
            round.group_id,
            round.root_sequence_id,
            revision,
            None,
            None,
        )
        .await?;
        transaction
            .commit()
            .await
            .context("committing a refreshed round")
    }

    /// Begin one NPC's decision: its own sequence, caused by the speech it
    /// answers, and its decision invocation, attached to the candidate.
    pub async fn claim_npc_decision(
        &self,
        round: &NpcRound,
        character_id: i64,
    ) -> Result<DecisionClaim> {
        let mut transaction = self
            .pool
            .begin()
            .await
            .context("claiming an NPC decision")?;
        let sequence_id = Self::begin_caused_sequence(
            &mut transaction,
            round.world_id,
            "npc response",
            round.id,
            None,
        )
        .await?;
        let agent_id = sqlx::query_scalar!(
            "SELECT agent_id FROM npc_agent WHERE character_id = ?",
            character_id,
        )
        .fetch_one(&mut *transaction)
        .await
        .context("finding the NPC's agent")?;
        let branch_id = super::history::branch_for_agent(&mut transaction, agent_id).await?;
        let invocation_id = sqlx::query_scalar!(
            "INSERT INTO invocation (sequence_id, agent_id, branch_id) VALUES (?, ?, ?) RETURNING id",
            sequence_id,
            agent_id,
            branch_id,
        )
        .fetch_one(&mut *transaction)
        .await
        .context("starting the decision invocation")?;
        let claimed = sqlx::query!(
            "UPDATE npc_candidate SET decision_invocation_id = ? \
             WHERE round_id = ? AND character_id = ? AND decision_invocation_id IS NULL",
            invocation_id,
            round.id,
            character_id,
        )
        .execute(&mut *transaction)
        .await
        .context("attaching the decision to its candidate")?;
        ensure!(
            claimed.rows_affected() == 1,
            "NPC {character_id} was already claimed in round {}",
            round.id
        );
        transaction
            .commit()
            .await
            .context("committing a decision claim")?;
        Ok(DecisionClaim {
            root_sequence_id: round.root_sequence_id,
            sequence_id,
            invocation_id,
            branch: AgentBranch {
                world_id: round.world_id,
                agent_id,
                branch_id,
            },
        })
    }

    /// A sequence caused by the speech a round answers (its sources), or by the
    /// winners' proposals when `proposals` is given.
    async fn begin_caused_sequence(
        transaction: &mut Transaction<'_, Sqlite>,
        world_id: i64,
        trigger: &str,
        round_id: i64,
        proposals: Option<&[i64]>,
    ) -> Result<i64> {
        let sequence_id = sqlx::query_scalar!(
            "INSERT INTO sequence (world_id, trigger) VALUES (?, ?) RETURNING id",
            world_id,
            trigger,
        )
        .fetch_one(&mut **transaction)
        .await
        .with_context(|| format!("starting {trigger} sequence in world {world_id}"))?;
        let causes = match proposals {
            Some(proposals) => proposals.to_vec(),
            None => sqlx::query_scalar!(
                "SELECT source_tool_execution_id FROM npc_round_source WHERE round_id = ?",
                round_id,
            )
            .fetch_all(&mut **transaction)
            .await
            .context("loading what the round answers")?,
        };
        for cause in causes {
            sqlx::query!(
                "INSERT INTO sequence_cause (child_sequence_id, source_tool_execution_id) VALUES (?, ?)",
                sequence_id,
                cause,
            )
            .execute(&mut **transaction)
            .await
            .context("linking a sequence to its cause")?;
        }
        Ok(sequence_id)
    }

    /// Record a valid decision's result in the NPC's history and finish its
    /// invocation and sequence, with the discussion's budget checkpoint, in one
    /// transaction.
    pub async fn finish_npc_decision(
        &self,
        claim: &DecisionClaim,
        inference_id: i64,
        call: &ToolCall,
        result: &str,
        spent: u32,
    ) -> Result<()> {
        let mut transaction = self
            .pool
            .begin()
            .await
            .context("finishing an NPC decision")?;
        let execution_id = sqlx::query_scalar!(
            "INSERT INTO tool_execution (inference_id, ordinal, status) VALUES (?, 0, 'running') RETURNING id",
            inference_id,
        )
        .fetch_one(&mut *transaction)
        .await
        .context("recording the decision call")?;
        let (_, content_id) = Self::insert_message(
            &mut transaction,
            claim.branch.branch_id,
            &Message::tool_result(call.id.clone(), result.to_string()),
            Some(inference_id),
            Some(execution_id),
            None,
            crate::inspection::MessageOrigin::Conversation,
        )
        .await?;
        sqlx::query!(
            "UPDATE tool_execution SET status = 'completed', result_content_id = ?, \
             finished_at = CURRENT_TIMESTAMP WHERE id = ?",
            content_id,
            execution_id,
        )
        .execute(&mut *transaction)
        .await
        .context("completing the decision call")?;
        sqlx::query!(
            "UPDATE invocation SET finished_at = CURRENT_TIMESTAMP, outcome = 'settled' WHERE id = ?",
            claim.invocation_id,
        )
        .execute(&mut *transaction)
        .await
        .context("finishing the decision invocation")?;
        Self::finish_sequence_in(&mut transaction, claim.sequence_id, None).await?;
        Self::save_discussion_budget(&mut transaction, claim.root_sequence_id, spent).await?;
        transaction
            .commit()
            .await
            .context("committing an NPC decision")
    }

    async fn save_discussion_budget(
        transaction: &mut Transaction<'_, Sqlite>,
        root: i64,
        spent: u32,
    ) -> Result<()> {
        let spent = i64::from(spent);
        sqlx::query!(
            "UPDATE npc_discussion SET spent_total = ? WHERE root_sequence_id = ?",
            spent,
            root,
        )
        .execute(&mut **transaction)
        .await
        .context("saving the discussion budget")?;
        Ok(())
    }

    /// A step failed: preserve what committed, end the step's records with the
    /// real error, and fail the discussion and its remaining rounds. `calls`
    /// names a decision inference whose tool call got no result.
    pub async fn fail_npc_discussion(
        &self,
        root_sequence_id: i64,
        failing_sequence_id: i64,
        unfinished: Option<(i64, Option<i64>)>,
        error: &str,
    ) -> Result<()> {
        let mut transaction = self
            .pool
            .begin()
            .await
            .context("failing an NPC discussion")?;
        if let Some((invocation_id, inference_id)) = unfinished {
            if let Some(inference_id) = inference_id {
                Self::close_tool_calls(
                    &mut transaction,
                    inference_id,
                    &CallClosure::Failed {
                        failing_execution_id: None,
                        error,
                    },
                )
                .await?;
            }
            sqlx::query!(
                "UPDATE invocation SET finished_at = CURRENT_TIMESTAMP, outcome = 'error' \
                 WHERE id = ? AND finished_at IS NULL",
                invocation_id,
            )
            .execute(&mut *transaction)
            .await
            .context("ending the failed invocation")?;
        }
        Self::finish_sequence_in(&mut transaction, failing_sequence_id, Some(error)).await?;
        sqlx::query!(
            "UPDATE npc_discussion SET state = 'failed', failure_sequence_id = ? \
             WHERE root_sequence_id = ? AND state <> 'failed'",
            failing_sequence_id,
            root_sequence_id,
        )
        .execute(&mut *transaction)
        .await
        .context("failing the discussion")?;
        sqlx::query!(
            "UPDATE npc_round SET state = 'failed' \
             WHERE root_sequence_id = ? AND state IN ('collecting', 'selected', 'executing')",
            root_sequence_id,
        )
        .execute(&mut *transaction)
        .await
        .context("failing the discussion's rounds")?;
        transaction
            .commit()
            .await
            .context("committing a failed discussion")
    }

    /// Stop a discussion whose step failed before it had a sequence of its
    /// own to carry the error. The failure becomes a finished, failed sequence
    /// caused by the speech the round answers, so players see it where that
    /// speech was said. Returns that sequence, or `None` if the discussion had
    /// already failed.
    pub async fn ensure_discussion_failed(
        &self,
        round: &NpcRound,
        error: &str,
    ) -> Result<Option<i64>> {
        let mut transaction = self
            .pool
            .begin()
            .await
            .context("failing an NPC discussion")?;
        let state = sqlx::query_scalar!(
            "SELECT state FROM npc_discussion WHERE root_sequence_id = ?",
            round.root_sequence_id,
        )
        .fetch_one(&mut *transaction)
        .await
        .context("reading the discussion")?;
        let failure = if state == "failed" {
            None
        } else {
            let sequence_id = Self::begin_caused_sequence(
                &mut transaction,
                round.world_id,
                "npc discussion failed",
                round.id,
                None,
            )
            .await?;
            Self::finish_sequence_in(&mut transaction, sequence_id, Some(error)).await?;
            sqlx::query!(
                "UPDATE npc_discussion SET state = 'failed', failure_sequence_id = ? WHERE root_sequence_id = ?",
                sequence_id,
                round.root_sequence_id,
            )
            .execute(&mut *transaction)
            .await
            .context("failing the discussion")?;
            Some(sequence_id)
        };
        sqlx::query!(
            "UPDATE npc_round SET state = 'failed' WHERE root_sequence_id = ? AND state IN ('collecting', 'selected', 'executing')",
            round.root_sequence_id,
        )
        .execute(&mut *transaction)
        .await
        .context("failing the discussion's rounds")?;
        transaction
            .commit()
            .await
            .context("committing a failed discussion")?;
        Ok(failure)
    }

    async fn round_decisions(&self, round_id: i64) -> Result<Vec<Decided>> {
        let rows = sqlx::query!(
            "SELECT npc_candidate.character_id, inference.output AS \"output!: i64\" \
             FROM npc_candidate \
             JOIN inference ON inference.invocation_id = npc_candidate.decision_invocation_id \
                           AND inference.ordinal = 0 \
             WHERE npc_candidate.round_id = ? \
             ORDER BY npc_candidate.character_id",
            round_id,
        )
        .fetch_all(&self.pool)
        .await
        .context("loading the round's decisions")?;
        let mut decided = Vec::with_capacity(rows.len());
        for row in rows {
            let (MessageContent::ToolCalls(calls), _) = self.recorded_content(row.output).await?
            else {
                bail!(
                    "NPC {}'s recorded decision is not a tool call",
                    row.character_id
                );
            };
            let (_, decision) = Decision::parse(&Content::ToolCalls(calls))
                .with_context(|| format!("decoding NPC {}'s decision", row.character_id))?;
            decided.push(Decided {
                character_id: row.character_id,
                decision,
            });
        }
        Ok(decided)
    }

    /// Choose the winners of a fully collected round: the highest eagerness,
    /// with every speech proposal tied at that level winning together. A round
    /// where nobody proposed ends without further work.
    pub async fn select_round(&self, round: &NpcRound) -> Result<()> {
        let decided = self.round_decisions(round.id).await?;
        let top = decided
            .iter()
            .filter_map(|candidate| match &candidate.decision {
                Decision::Propose(proposal) => Some(proposal.eagerness),
                Decision::Nothing => None,
            })
            .max();
        let winners: Vec<i64> = decided
            .iter()
            .filter(|candidate| {
                matches!(&candidate.decision, Decision::Propose(proposal) if Some(proposal.eagerness) == top)
            })
            .map(|candidate| candidate.character_id)
            .collect();
        let mut transaction = self
            .pool
            .begin()
            .await
            .context("selecting a round's winners")?;
        for character_id in &winners {
            sqlx::query!(
                "UPDATE npc_candidate SET selected = 1 WHERE round_id = ? AND character_id = ?",
                round.id,
                character_id,
            )
            .execute(&mut *transaction)
            .await
            .context("marking a winner")?;
        }
        let state = if winners.is_empty() {
            "settled"
        } else {
            "selected"
        };
        let moved = sqlx::query!(
            "UPDATE npc_round SET state = ? WHERE id = ? AND state = 'collecting'",
            state,
            round.id,
        )
        .execute(&mut *transaction)
        .await
        .context("advancing the round")?;
        ensure!(
            moved.rows_affected() == 1,
            "round {} changed while selecting",
            round.id
        );
        if winners.is_empty() {
            Self::settle_idle_discussion(&mut transaction, round.root_sequence_id).await?;
        }
        transaction
            .commit()
            .await
            .context("committing a round's selection")
    }

    /// Start taking the next selected utterance to the GM: its own sequence,
    /// caused by the NPC's proposal, with the proposal submitted as a pending
    /// action for that NPC.
    pub async fn claim_npc_execution(&self, round: &NpcRound) -> Result<ExecutionClaim> {
        let decided = self.round_decisions(round.id).await?;
        let mut transaction = self
            .pool
            .begin()
            .await
            .context("claiming a round's execution")?;
        let moved = sqlx::query!(
            "UPDATE npc_round SET state = 'executing' WHERE id = ? AND state = 'selected'",
            round.id,
        )
        .execute(&mut *transaction)
        .await
        .context("starting the round's execution")?;
        ensure!(
            moved.rows_affected() == 1,
            "round {} is not selected",
            round.id
        );
        let selected = sqlx::query_scalar!(
            "SELECT character_id FROM npc_candidate WHERE round_id = ? AND selected = 1              ORDER BY character_id",
            round.id,
        )
        .fetch_all(&mut *transaction)
        .await
        .context("listing the selected replies")?;
        let character_id = sqlx::query_scalar!(
            "SELECT character_id FROM npc_candidate              WHERE round_id = ? AND selected = 1 AND execution_sequence_id IS NULL              ORDER BY character_id LIMIT 1",
            round.id,
        )
        .fetch_optional(&mut *transaction)
        .await
        .context("finding the next selected reply")?
        .with_context(|| format!("round {} has no selected reply left", round.id))?;
        let Some(Decision::Propose(proposal)) = decided
            .into_iter()
            .find(|candidate| candidate.character_id == character_id)
            .map(|candidate| candidate.decision)
        else {
            bail!("selected NPC {character_id} has no proposal");
        };
        let execution_id = sqlx::query_scalar!(
            "SELECT tool_execution.id FROM npc_candidate              JOIN inference ON inference.invocation_id = npc_candidate.decision_invocation_id                            AND inference.ordinal = 0              JOIN tool_execution ON tool_execution.inference_id = inference.id AND tool_execution.ordinal = 0              WHERE npc_candidate.round_id = ? AND npc_candidate.character_id = ?",
            round.id,
            character_id,
        )
        .fetch_one(&mut *transaction)
        .await
        .context("finding the proposal's call")?;
        let sequence_id = Self::begin_caused_sequence(
            &mut transaction,
            round.world_id,
            "npc speech",
            round.id,
            Some(&[execution_id]),
        )
        .await?;
        sqlx::query!(
            "UPDATE npc_candidate SET execution_sequence_id = ? WHERE round_id = ? AND character_id = ?",
            sequence_id,
            round.id,
            character_id,
        )
        .execute(&mut *transaction)
        .await
        .context("recording the execution sequence")?;
        let pending = Self::create_pending_action_in(
            &mut transaction,
            &CharacterActor {
                world_id: round.world_id,
                character_id,
            },
            sequence_id,
            execution_id,
            (proposal.tool, &proposal.arguments),
        )
        .await?;
        transaction
            .commit()
            .await
            .context("committing a round's execution claim")?;
        Ok(ExecutionClaim {
            round_id: round.id,
            root_sequence_id: round.root_sequence_id,
            sequence_id,
            pending,
            tied: selected.len() > 1,
        })
    }

    /// The GM has dealt with this utterance: end the step with the
    /// discussion's budget checkpoint. The round stays selected while chosen
    /// replies remain.
    pub async fn finish_npc_execution(&self, claim: &ExecutionClaim, spent: u32) -> Result<()> {
        let mut transaction = self
            .pool
            .begin()
            .await
            .context("finishing a round's execution")?;
        Self::finish_sequence_in(&mut transaction, claim.sequence_id, None).await?;
        sqlx::query!(
            "UPDATE npc_round SET state = CASE WHEN EXISTS (                  SELECT 1 FROM npc_candidate WHERE round_id = ?1 AND selected = 1                    AND execution_sequence_id IS NULL) THEN 'selected' ELSE 'settled' END              WHERE id = ?1 AND state = 'executing'",
            claim.round_id,
        )
        .execute(&mut *transaction)
        .await
        .context("returning the round to the worker")?;
        Self::save_discussion_budget(&mut transaction, claim.root_sequence_id, spent).await?;
        Self::settle_idle_discussion(&mut transaction, claim.root_sequence_id).await?;
        transaction
            .commit()
            .await
            .context("committing a round's execution")
    }
}

#[derive(sqlx::FromRow)]
struct PersonRow {
    name: String,
    description: String,
}
