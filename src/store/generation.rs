//! A world's initialization by the Questioner and Storyteller (design.md's
//! Storyteller world creation). `world.generation_stage` selects the next
//! finite step; each step's outputs commit together with the next stage, so a
//! restart resumes at the stored one. `advance_generation` is the single
//! definition of the allowed transitions.

use anyhow::{Context, Result, bail, ensure};
use sqlx::{Sqlite, Transaction};

use super::{AgentBranch, PlayerAgent, Store, User};
use crate::inspection::GenerationProgress;

/// The name a world has until the Storyteller names it.
const UNNAMED_WORLD: &str = "Unnamed world";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GenerationStage {
    /// The Questioner answers the meta-questions.
    Meta,
    /// The Questioner asks the Storyteller questions, each answered within its
    /// turn, until it declares the world summary complete.
    Ask,
    /// The Storyteller replies with the world name.
    Name,
    /// The Storyteller creates objects with the authoring tools.
    CreateObjects,
    /// The Questioner reviews the objects.
    Review,
    Failed,
    /// Ready to play, however the world was filled in.
    Complete,
}

impl GenerationStage {
    /// What a user is told is happening in this stage.
    fn phase(self) -> &'static str {
        match self {
            Self::Meta => "Preparing the setting",
            Self::Ask => "Developing the world summary",
            Self::Name => "Naming the world",
            Self::CreateObjects => "Creating locations, characters and items",
            Self::Review => "Reviewing the objects",
            Self::Failed => "Stopped",
            Self::Complete => "Complete",
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Meta => "meta",
            Self::Ask => "ask",
            Self::Name => "name",
            Self::CreateObjects => "create_objects",
            Self::Review => "review",
            Self::Failed => "failed",
            Self::Complete => "complete",
        }
    }

    fn parse(stage: &str) -> Result<Self> {
        Ok(match stage {
            "meta" => Self::Meta,
            "ask" => Self::Ask,
            "name" => Self::Name,
            "create_objects" => Self::CreateObjects,
            "review" => Self::Review,
            "failed" => Self::Failed,
            "complete" => Self::Complete,
            other => bail!("unknown generation stage `{other}`"),
        })
    }
}

/// What one finished step produced, which decides the next stage.
pub enum Advance {
    MetaQuestionsAnswered,
    /// The Questioner is satisfied with the world summary.
    WorldSummaryComplete,
    Name(String),
    /// The Storyteller finished creating the summary's objects.
    ObjectsCreated,
    /// Every core object is reviewed and the world is whole.
    CoreObjectsCreated,
}

/// Everything one generation step needs, read at once.
pub struct Generation {
    pub stage: GenerationStage,
    /// The optional seed prompt the owner gave when creating the world.
    pub seed: String,
    /// The latest world summary; empty before the first round.
    pub summary: String,
    /// Inferences spent so far, restored into the shared budget.
    pub spent: u32,
    pub questioner: AgentBranch,
    pub storyteller: AgentBranch,
}

impl Store {
    /// Create a world to be initialized, owned by and joined by `owner` with a
    /// blank Adventurer that is in no group yet: character creation waits for
    /// the world to be ready.
    pub async fn create_world_to_generate(&self, owner: &User, seed: &str) -> Result<PlayerAgent> {
        let mut transaction = self
            .pool
            .begin()
            .await
            .with_context(|| format!("starting world creation for {}", owner.email))?;
        let stage = GenerationStage::Meta.as_str();
        let world_id = sqlx::query!(
            "INSERT INTO world (name, initial_prompt, generation_stage) VALUES (?, ?, ?)",
            UNNAMED_WORLD,
            seed,
            stage,
        )
        .execute(&mut *transaction)
        .await
        .context("creating the world")?
        .last_insert_rowid();
        sqlx::query!(
            "INSERT INTO world_owner (world_id, user_id) VALUES (?, ?)",
            world_id,
            owner.id,
        )
        .execute(&mut *transaction)
        .await
        .with_context(|| format!("recording owner for world {world_id}"))?;
        let joined = Self::join_world_unplaced(&mut transaction, owner, world_id).await?;
        Self::create_role_agent(&mut transaction, world_id, "questioner").await?;
        Self::create_role_agent(&mut transaction, world_id, "storyteller").await?;
        sqlx::query!(
            "INSERT INTO world_generation (world_id) VALUES (?)",
            world_id
        )
        .execute(&mut *transaction)
        .await
        .with_context(|| format!("recording generation progress for world {world_id}"))?;
        transaction
            .commit()
            .await
            .with_context(|| format!("committing world {world_id}"))?;
        Ok(PlayerAgent {
            member_id: joined.member_id,
            world_id,
            user_id: owner.id,
            character_id: joined.character_id,
            agent_id: joined.agent_id,
        })
    }

    /// The agent for one of a world's two generation roles, with its one chat.
    async fn create_role_agent(
        transaction: &mut Transaction<'_, Sqlite>,
        world_id: i64,
        role: &str,
    ) -> Result<()> {
        let agent_id = sqlx::query!("INSERT INTO agent (world_id) VALUES (?)", world_id)
            .execute(&mut **transaction)
            .await
            .with_context(|| format!("creating the {role} agent for world {world_id}"))?
            .last_insert_rowid();
        Self::create_agent_history(transaction, agent_id).await?;
        sqlx::query!(
            "INSERT INTO world_role_agent (world_id, role, agent_id) VALUES (?, ?, ?)",
            world_id,
            role,
            agent_id,
        )
        .execute(&mut **transaction)
        .await
        .with_context(|| format!("linking the {role} agent for world {world_id}"))?;
        Ok(())
    }

    /// The agent for one of a world's generation roles.
    #[cfg(test)]
    pub async fn role_agent(&self, world_id: i64, role: &str) -> Result<i64> {
        sqlx::query_scalar!(
            "SELECT agent_id FROM world_role_agent WHERE world_id = ? AND role = ?",
            world_id,
            role,
        )
        .fetch_one(&self.pool)
        .await
        .with_context(|| format!("finding the {role} of world {world_id}"))
    }

    /// Whether the world is ready to play, however it was filled in.
    pub async fn world_is_ready(&self, world_id: i64) -> Result<bool> {
        let complete = GenerationStage::Complete.as_str();
        sqlx::query_scalar!(
            "SELECT generation_stage = ? AS \"ready!: bool\" FROM world WHERE id = ?",
            complete,
            world_id,
        )
        .fetch_one(&self.pool)
        .await
        .with_context(|| format!("checking whether world {world_id} is ready"))
    }

    /// The world's next generation step, if it is still being initialized.
    /// Where a world being made has got.
    pub async fn generation_progress(&self, world_id: i64) -> Result<GenerationProgress> {
        let stage =
            sqlx::query_scalar!("SELECT generation_stage FROM world WHERE id = ?", world_id)
                .fetch_one(&self.pool)
                .await
                .with_context(|| format!("loading the stage of world {world_id}"))?;
        let stage = GenerationStage::parse(&stage)?;
        let inferences = sqlx::query_scalar!(
            "SELECT COUNT(*) FROM inference JOIN sequence ON sequence.id = inference.sequence_id \
             WHERE sequence.world_id = ? AND sequence.trigger = 'world generation'",
            world_id,
        )
        .fetch_one(&self.pool)
        .await
        .with_context(|| format!("counting the inferences of world {world_id}"))?;
        let entries = self.review_list(world_id).await?;
        let count = |kind: &str| entries.iter().filter(|entry| entry.kind == kind).count();
        let kinds = [
            ("location", "locations"),
            ("connection", "connections"),
            ("npc", "NPCs"),
            ("item_type", "item types"),
            ("item", "items"),
        ];
        let objects: Vec<String> = kinds
            .iter()
            .filter(|(kind, _)| count(kind) > 0)
            .map(|(kind, name)| format!("{} {name}", count(kind)))
            .collect();
        Ok(GenerationProgress {
            phase: stage.phase().to_string(),
            inferences,
            objects: if objects.is_empty() {
                "none yet".to_string()
            } else {
                objects.join(", ")
            },
            finished: matches!(stage, GenerationStage::Complete | GenerationStage::Failed),
        })
    }

    pub async fn next_generation(&self, world_id: i64) -> Result<Option<Generation>> {
        let Some(row) = sqlx::query!(
            "SELECT world.generation_stage, world.initial_prompt, world.storyteller_summary, \
                    world_generation.spent, \
                    questioner.agent_id AS questioner_agent_id, \
                    questioner_chat.branch_id AS questioner_branch_id, \
                    storyteller.agent_id AS storyteller_agent_id, \
                    storyteller_chat.branch_id AS storyteller_branch_id \
             FROM world \
             JOIN world_generation ON world_generation.world_id = world.id \
             JOIN world_role_agent AS questioner \
               ON questioner.world_id = world.id AND questioner.role = 'questioner' \
             JOIN agent_history AS questioner_chat ON questioner_chat.agent_id = questioner.agent_id \
             JOIN world_role_agent AS storyteller \
               ON storyteller.world_id = world.id AND storyteller.role = 'storyteller' \
             JOIN agent_history AS storyteller_chat ON storyteller_chat.agent_id = storyteller.agent_id \
             WHERE world.id = ?",
            world_id,
        )
        .fetch_optional(&self.pool)
        .await
        .with_context(|| format!("loading generation of world {world_id}"))?
        else {
            return Ok(None);
        };
        let stage = GenerationStage::parse(&row.generation_stage)?;
        if !matches!(
            stage,
            GenerationStage::Meta
                | GenerationStage::Ask
                | GenerationStage::Name
                | GenerationStage::CreateObjects
                | GenerationStage::Review
        ) {
            return Ok(None);
        }
        Ok(Some(Generation {
            stage,
            seed: row.initial_prompt,
            summary: row.storyteller_summary,
            spent: u32::try_from(row.spent).context("the spent inferences are out of range")?,
            questioner: AgentBranch {
                world_id,
                agent_id: row.questioner_agent_id,
                branch_id: row.questioner_branch_id,
            },
            storyteller: AgentBranch {
                world_id,
                agent_id: row.storyteller_agent_id,
                branch_id: row.storyteller_branch_id,
            },
        }))
    }

    /// Commit a finished step's outputs with the stage that follows it, and the
    /// inferences spent so far. A step that does not apply at the stored stage
    /// is an error and changes nothing.
    pub async fn advance_generation(
        &self,
        world_id: i64,
        advance: Advance,
        spent: u32,
    ) -> Result<()> {
        let mut transaction = self
            .pool
            .begin()
            .await
            .with_context(|| format!("advancing generation of world {world_id}"))?;
        let current = sqlx::query!(
            "SELECT world.generation_stage \
             FROM world JOIN world_generation ON world_generation.world_id = world.id \
             WHERE world.id = ?",
            world_id,
        )
        .fetch_one(&mut *transaction)
        .await
        .with_context(|| format!("loading generation of world {world_id}"))?;
        let stage = GenerationStage::parse(&current.generation_stage)?;
        let next = match (stage, advance) {
            (GenerationStage::Meta, Advance::MetaQuestionsAnswered) => GenerationStage::Ask,
            (GenerationStage::Ask, Advance::WorldSummaryComplete) => GenerationStage::Name,
            (GenerationStage::Name, Advance::Name(name)) => {
                let name = name.trim();
                ensure!(!name.is_empty(), "the Storyteller's world name is empty");
                sqlx::query!("UPDATE world SET name = ? WHERE id = ?", name, world_id)
                    .execute(&mut *transaction)
                    .await
                    .context("storing the world name")?;
                GenerationStage::CreateObjects
            }
            (GenerationStage::CreateObjects, Advance::ObjectsCreated) => GenerationStage::Review,
            (GenerationStage::Review, Advance::CoreObjectsCreated) => {
                let problems = Self::core_problems(&mut transaction, world_id).await?;
                ensure!(
                    problems.is_empty(),
                    "the world is not whole: {}",
                    problems.join(" ")
                );
                // Until entry placement exists, whoever waits for the world
                // starts in its first group.
                Self::place_waiting_adventurers(&mut transaction, world_id).await?;
                GenerationStage::Complete
            }
            (stage, _) => bail!(
                "that generation step does not apply to world {world_id} at stage `{}`",
                stage.as_str()
            ),
        };
        Self::set_generation_stage(&mut transaction, world_id, next, spent).await?;
        transaction
            .commit()
            .await
            .with_context(|| format!("committing generation of world {world_id}"))
    }

    /// Store the world summary the Storyteller just wrote, replacing the last.
    pub async fn save_world_summary(&self, world_id: i64, summary: &str) -> Result<()> {
        ensure!(
            !summary.trim().is_empty(),
            "the Storyteller's summary is empty"
        );
        sqlx::query!(
            "UPDATE world SET storyteller_summary = ? WHERE id = ?",
            summary,
            world_id,
        )
        .execute(&self.pool)
        .await
        .with_context(|| format!("storing the summary of world {world_id}"))?;
        Ok(())
    }

    /// Mark a world's initialization failed. The error stays on the failing
    /// step's sequence.
    pub async fn fail_generation(&self, world_id: i64) -> Result<()> {
        let mut transaction = self
            .pool
            .begin()
            .await
            .with_context(|| format!("failing generation of world {world_id}"))?;
        let spent = sqlx::query_scalar!(
            "SELECT spent FROM world_generation WHERE world_id = ?",
            world_id,
        )
        .fetch_one(&mut *transaction)
        .await
        .with_context(|| format!("loading generation of world {world_id}"))?;
        let spent = u32::try_from(spent).context("the spent inferences are out of range")?;
        Self::set_generation_stage(&mut transaction, world_id, GenerationStage::Failed, spent)
            .await?;
        transaction
            .commit()
            .await
            .with_context(|| format!("committing the failure of world {world_id}"))
    }

    async fn set_generation_stage(
        transaction: &mut Transaction<'_, Sqlite>,
        world_id: i64,
        stage: GenerationStage,
        spent: u32,
    ) -> Result<()> {
        let stage = stage.as_str();
        let spent = i64::from(spent);
        sqlx::query!(
            "UPDATE world SET generation_stage = ? WHERE id = ?",
            stage,
            world_id,
        )
        .execute(&mut **transaction)
        .await
        .with_context(|| format!("setting the generation stage of world {world_id}"))?;
        sqlx::query!(
            "UPDATE world_generation SET spent = ? WHERE world_id = ?",
            spent,
            world_id,
        )
        .execute(&mut **transaction)
        .await
        .with_context(|| format!("recording the spent inferences of world {world_id}"))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{scenario::Scenario, store::TestDatabase};

    async fn new_world(name: &str) -> (Store, TestDatabase, i64) {
        let db = TestDatabase::new(name);
        let store = Store::open(db.path()).await.unwrap();
        let owner = store
            .find_or_create_user("maker@example.test", "Maker")
            .await
            .unwrap();
        let member = store
            .create_world_to_generate(&owner, "a seed")
            .await
            .unwrap();
        (store, db, member.world_id)
    }

    async fn stage(store: &Store, world_id: i64) -> GenerationStage {
        store
            .next_generation(world_id)
            .await
            .unwrap()
            .unwrap()
            .stage
    }

    #[tokio::test]
    async fn only_a_scenario_world_is_ready_before_generation_finishes() {
        let (store, _db, generated) = new_world("generation-ready").await;
        let owner = store
            .find_or_create_user("maker@example.test", "Maker")
            .await
            .unwrap();
        let scenario = Scenario::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/testdata/hut_scenario.json"
        ))
        .unwrap();
        let installed = store.install_scenario(&owner, &scenario).await.unwrap();

        assert!(!store.world_is_ready(generated).await.unwrap());
        assert!(store.world_is_ready(installed.world_id).await.unwrap());
        assert!(
            store
                .next_generation(installed.world_id)
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn a_step_that_does_not_apply_changes_nothing() {
        let (store, _db, world_id) = new_world("generation-order").await;
        let error = store
            .advance_generation(world_id, Advance::ObjectsCreated, 3)
            .await
            .unwrap_err();
        assert!(format!("{error:#}").contains("does not apply"));
        let generation = store.next_generation(world_id).await.unwrap().unwrap();
        assert_eq!(generation.stage, GenerationStage::Meta);
        assert_eq!(generation.spent, 0, "a rejected step spends nothing");
        assert!(generation.summary.is_empty());
    }

    #[tokio::test]
    async fn the_summary_is_replaced_and_both_roles_keep_their_one_chat() {
        let (store, _db, world_id) = new_world("generation-rounds").await;
        let first = store.next_generation(world_id).await.unwrap().unwrap();

        store
            .advance_generation(world_id, Advance::MetaQuestionsAnswered, 1)
            .await
            .unwrap();
        store
            .save_world_summary(world_id, "A flooded city.")
            .await
            .unwrap();
        store
            .save_world_summary(world_id, "A drowned port.")
            .await
            .unwrap();
        let asking = store.next_generation(world_id).await.unwrap().unwrap();
        assert_eq!(asking.stage, GenerationStage::Ask);
        assert_eq!(asking.summary, "A drowned port.", "the newest summary wins");
        assert_eq!(asking.spent, 1);
        assert_eq!(
            asking.storyteller.branch_id(),
            first.storyteller.branch_id()
        );
        assert_eq!(asking.questioner.branch_id(), first.questioner.branch_id());
        assert!(store.save_world_summary(world_id, "  \n").await.is_err());
    }

    #[tokio::test]
    async fn a_name_ends_the_summary_and_an_empty_one_is_refused() {
        let (store, _db, world_id) = new_world("generation-empty").await;
        store
            .advance_generation(world_id, Advance::MetaQuestionsAnswered, 1)
            .await
            .unwrap();
        store
            .advance_generation(world_id, Advance::WorldSummaryComplete, 5)
            .await
            .unwrap();
        assert!(
            store
                .advance_generation(world_id, Advance::Name(" ".into()), 6)
                .await
                .is_err()
        );
        store
            .advance_generation(world_id, Advance::Name(" Saltmarsh \n".into()), 6)
            .await
            .unwrap();
        assert_eq!(
            stage(&store, world_id).await,
            GenerationStage::CreateObjects
        );
        assert_eq!(
            store.world(world_id).await.unwrap().unwrap().name,
            "Saltmarsh"
        );
    }

    /// Stopping the server while a world is being made is not a failure: the
    /// step that was running runs again when the server is started again.
    #[tokio::test]
    async fn a_world_being_made_continues_after_the_server_stops() {
        let (store, _db, world_id) = new_world("generation-restart").await;
        store
            .advance_generation(world_id, Advance::MetaQuestionsAnswered, 7)
            .await
            .unwrap();
        let before = store.next_generation(world_id).await.unwrap().unwrap();
        let sequence = store
            .begin_sequence(world_id, "world generation")
            .await
            .unwrap();
        // The process stops here: the sequence never finishes.
        store.close_interrupted_turns().await.unwrap();
        let world = store.world(world_id).await.unwrap().unwrap();
        assert_eq!(world.failure, None, "an interrupted step is not a failure");
        let after = store.next_generation(world_id).await.unwrap().unwrap();
        assert_eq!(after.stage, before.stage, "the same step comes next");
        assert_eq!(after.spent, before.spent);
        let _ = sequence;
    }

    #[tokio::test]
    async fn a_failed_world_has_no_step_left_and_keeps_what_it_spent() {
        let (store, _db, world_id) = new_world("generation-failed").await;
        store
            .advance_generation(world_id, Advance::MetaQuestionsAnswered, 7)
            .await
            .unwrap();
        store.fail_generation(world_id).await.unwrap();
        assert!(store.next_generation(world_id).await.unwrap().is_none());
        assert!(!store.world_is_ready(world_id).await.unwrap());
    }
}
