//! A world's initialization by the Questioner and Storyteller (design.md's
//! Storyteller world creation). `world.generation_stage` selects the next
//! finite step; each step's outputs commit together with the next stage, so a
//! restart resumes at the stored one. `advance_generation` is the single
//! definition of the allowed transitions.

use anyhow::{Context, Result, bail, ensure};
use sqlx::{Sqlite, Transaction};

use super::{AgentBranch, PlayerAgent, Store, User};

/// The name a world has until the Storyteller names it.
const UNNAMED_WORLD: &str = "Unnamed world";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GenerationStage {
    /// The Questioner answers the meta-questions.
    Meta,
    /// The Questioner asks questions or declares the world complete.
    Ask,
    /// The Storyteller answers the current question as text.
    Answer,
    /// The Storyteller replies with the world summary.
    Summarize,
    /// The Storyteller replies with the world name.
    Name,
    /// The Storyteller creates objects with the authoring tools.
    Build,
    /// The Questioner reviews the objects.
    Review,
    Failed,
    /// Ready to play, however the world was filled in.
    Complete,
}

impl GenerationStage {
    fn as_str(self) -> &'static str {
        match self {
            Self::Meta => "meta",
            Self::Ask => "ask",
            Self::Answer => "answer",
            Self::Summarize => "summarize",
            Self::Name => "name",
            Self::Build => "build",
            Self::Review => "review",
            Self::Failed => "failed",
            Self::Complete => "complete",
        }
    }

    fn parse(stage: &str) -> Result<Self> {
        Ok(match stage {
            "meta" => Self::Meta,
            "ask" => Self::Ask,
            "answer" => Self::Answer,
            "summarize" => Self::Summarize,
            "name" => Self::Name,
            "build" => Self::Build,
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
    /// The Questioner's questions for the Storyteller to answer.
    Questions(Vec<String>),
    /// The Questioner is satisfied.
    WorldSummaryComplete,
    QuestionAnswered,
    Summary(String),
    Name(String),
    /// The Storyteller finished a round of creating objects.
    Built,
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
    pub questions: Vec<String>,
    pub next_question: usize,
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
    pub async fn next_generation(&self, world_id: i64) -> Result<Option<Generation>> {
        let Some(row) = sqlx::query!(
            "SELECT world.generation_stage, world.initial_prompt, world.storyteller_summary, \
                    world_generation.questions, world_generation.next_question, \
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
                | GenerationStage::Answer
                | GenerationStage::Summarize
                | GenerationStage::Name
                | GenerationStage::Build
                | GenerationStage::Review
        ) {
            return Ok(None);
        }
        Ok(Some(Generation {
            stage,
            seed: row.initial_prompt,
            summary: row.storyteller_summary,
            questions: serde_json::from_str(&row.questions)
                .with_context(|| format!("decoding the questions of world {world_id}"))?,
            next_question: usize::try_from(row.next_question)
                .context("the next question is negative")?,
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
            "SELECT world.generation_stage, world_generation.questions, world_generation.next_question \
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
            (
                stage @ (GenerationStage::Ask | GenerationStage::Review),
                Advance::Questions(questions),
            ) => {
                ensure!(!questions.is_empty(), "the Questioner asked no questions");
                let questions = serde_json::to_string(&questions).context("encoding questions")?;
                sqlx::query!(
                    "UPDATE world_generation SET questions = ?, next_question = 0 WHERE world_id = ?",
                    questions,
                    world_id,
                )
                .execute(&mut *transaction)
                .await
                .context("recording the round's questions")?;
                if stage == GenerationStage::Review {
                    GenerationStage::Build
                } else {
                    GenerationStage::Answer
                }
            }
            (GenerationStage::Ask, Advance::WorldSummaryComplete) => {
                // The last round's questions are answered; building starts fresh.
                sqlx::query!(
                    "UPDATE world_generation SET questions = '[]', next_question = 0 WHERE world_id = ?",
                    world_id,
                )
                .execute(&mut *transaction)
                .await
                .context("clearing the answered questions")?;
                GenerationStage::Name
            }
            (GenerationStage::Answer, Advance::QuestionAnswered) => {
                let answered = current.next_question + 1;
                let asked: Vec<String> = serde_json::from_str(&current.questions)
                    .context("decoding the round's questions")?;
                sqlx::query!(
                    "UPDATE world_generation SET next_question = ? WHERE world_id = ?",
                    answered,
                    world_id,
                )
                .execute(&mut *transaction)
                .await
                .context("recording the answered question")?;
                if usize::try_from(answered).is_ok_and(|answered| answered >= asked.len()) {
                    GenerationStage::Summarize
                } else {
                    GenerationStage::Answer
                }
            }
            (GenerationStage::Summarize, Advance::Summary(summary)) => {
                ensure!(
                    !summary.trim().is_empty(),
                    "the Storyteller's summary is empty"
                );
                sqlx::query!(
                    "UPDATE world SET storyteller_summary = ? WHERE id = ?",
                    summary,
                    world_id,
                )
                .execute(&mut *transaction)
                .await
                .context("storing the world summary")?;
                GenerationStage::Ask
            }
            (GenerationStage::Name, Advance::Name(name)) => {
                let name = name.trim();
                ensure!(!name.is_empty(), "the Storyteller's world name is empty");
                sqlx::query!("UPDATE world SET name = ? WHERE id = ?", name, world_id)
                    .execute(&mut *transaction)
                    .await
                    .context("storing the world name")?;
                GenerationStage::Build
            }
            (GenerationStage::Build, Advance::Built) => {
                sqlx::query!(
                    "UPDATE world_generation SET questions = '[]', next_question = 0 WHERE world_id = ?",
                    world_id,
                )
                .execute(&mut *transaction)
                .await
                .context("clearing the answered questions")?;
                GenerationStage::Review
            }
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
            .advance_generation(world_id, Advance::Summary("too early".into()), 3)
            .await
            .unwrap_err();
        assert!(format!("{error:#}").contains("does not apply"));
        let generation = store.next_generation(world_id).await.unwrap().unwrap();
        assert_eq!(generation.stage, GenerationStage::Meta);
        assert_eq!(generation.spent, 0, "a rejected step spends nothing");
        assert!(generation.summary.is_empty());
    }

    #[tokio::test]
    async fn every_round_uses_the_same_one_chat_per_role_and_progress_is_kept() {
        let (store, _db, world_id) = new_world("generation-rounds").await;
        let first = store.next_generation(world_id).await.unwrap().unwrap();

        store
            .advance_generation(world_id, Advance::MetaQuestionsAnswered, 1)
            .await
            .unwrap();
        let asked = vec!["one?".to_string(), "two?".to_string()];
        store
            .advance_generation(world_id, Advance::Questions(asked.clone()), 2)
            .await
            .unwrap();
        let round_one = store.next_generation(world_id).await.unwrap().unwrap();
        assert_eq!(round_one.stage, GenerationStage::Answer);
        assert_eq!(round_one.questions, asked);
        assert_eq!(round_one.next_question, 0);
        assert_eq!(round_one.spent, 2);
        assert_eq!(
            round_one.storyteller.branch_id(),
            first.storyteller.branch_id()
        );
        assert_eq!(
            round_one.storyteller.agent_id(),
            first.storyteller.agent_id()
        );
        assert_eq!(
            round_one.questioner.branch_id(),
            first.questioner.branch_id()
        );

        store
            .advance_generation(world_id, Advance::QuestionAnswered, 3)
            .await
            .unwrap();
        assert_eq!(stage(&store, world_id).await, GenerationStage::Answer);
        store
            .advance_generation(world_id, Advance::QuestionAnswered, 4)
            .await
            .unwrap();
        assert_eq!(stage(&store, world_id).await, GenerationStage::Summarize);
        store
            .advance_generation(world_id, Advance::Summary("A flooded city.".into()), 5)
            .await
            .unwrap();
        let after = store.next_generation(world_id).await.unwrap().unwrap();
        assert_eq!(after.stage, GenerationStage::Ask);
        assert_eq!(after.summary, "A flooded city.");

        store
            .advance_generation(world_id, Advance::Questions(asked), 6)
            .await
            .unwrap();
        let round_two = store.next_generation(world_id).await.unwrap().unwrap();
        assert_eq!(
            round_two.storyteller.branch_id(),
            round_one.storyteller.branch_id(),
            "the Storyteller keeps its one chat"
        );
        assert_eq!(
            round_two.summary, "A flooded city.",
            "the summary carries over"
        );
    }

    #[tokio::test]
    async fn an_empty_summary_or_name_is_refused_and_a_name_ends_the_steps() {
        let (store, _db, world_id) = new_world("generation-empty").await;
        store
            .advance_generation(world_id, Advance::MetaQuestionsAnswered, 1)
            .await
            .unwrap();
        store
            .advance_generation(world_id, Advance::Questions(vec!["q?".into()]), 2)
            .await
            .unwrap();
        store
            .advance_generation(world_id, Advance::QuestionAnswered, 3)
            .await
            .unwrap();
        assert!(
            store
                .advance_generation(world_id, Advance::Summary("  \n".into()), 4)
                .await
                .is_err()
        );
        assert_eq!(stage(&store, world_id).await, GenerationStage::Summarize);

        store
            .advance_generation(world_id, Advance::Summary("Summary.".into()), 4)
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
        assert_eq!(stage(&store, world_id).await, GenerationStage::Build);
        assert_eq!(
            store.world(world_id).await.unwrap().unwrap().name,
            "Saltmarsh"
        );
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
