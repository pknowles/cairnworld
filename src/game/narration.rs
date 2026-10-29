//! The GM ends every turn with exactly one narrate tool, which says who
//! perceives what. The four tools differ only in their audience arguments, so
//! the choice of audience is a tool-call shape the model is trained on rather
//! than a convention in free text. Each text is recorded once and delivered to
//! its validated recipients.

use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, ensure};
use schemars::{JsonSchema, schema_for};
use serde::Deserialize;

use super::Game;
use crate::{
    llm::{Backend, ToolDefinition},
    prompts,
    store::{Narration, NarrationText, PendingAction},
    tools::{Tool, ToolFuture, ToolOutcome},
};

/// Where the GM's narration goes, and what must be settled before it.
pub(super) struct NarrationScope {
    pub world_id: i64,
    /// A character of the group being narrated. The group is read from them
    /// when the narrate call is made, since a split or merge earlier in the
    /// same turn moves characters between groups.
    pub in_group: i64,
    /// The character whose action or arrival is being narrated, when there is
    /// one. Only then is `narrate_actor_and_others` available.
    pub actor: Option<i64>,
    /// The actions the GM was presented this turn: the one it is judging and any
    /// reactions it asked for. Each must be approved or rejected before
    /// narrating. The list grows during the turn.
    pub presented: PresentedActions,
}

/// Actions presented to the GM in one turn, shared by the tools that resolve
/// and narrate them.
pub(super) type PresentedActions = Arc<Mutex<Vec<PendingAction>>>;

/// The narrate tools of one GM turn, and the narration its call captured.
pub(super) struct NarrationTools {
    pub tools: Vec<Tool>,
    /// What the narrate call said.
    captured: Arc<Mutex<Option<Draft>>>,
    /// Whether the game shares one detailed account among the player characters.
    shares: bool,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct NarrateGroup {
    text: String,
    #[serde(default)]
    exclude_character_ids: Vec<i64>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct NarrateActorAndOthers {
    actor_text: String,
    others_text: String,
    #[serde(default)]
    exclude_character_ids: Vec<i64>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct NarrateCharacters {
    character_ids: Vec<i64>,
    text: String,
}

/// A narrate call's texts and every character it names, before validation
/// against the group.
struct Draft {
    named: Vec<i64>,
    texts: Vec<NarrationText>,
    /// Addressed to chosen characters, so never shared beyond them.
    private: bool,
}

fn nonempty(text: &str) -> Result<()> {
    ensure!(!text.trim().is_empty(), "narration text is empty");
    Ok(())
}

fn group_draft(arguments: &str) -> Result<Draft> {
    let call: NarrateGroup =
        serde_json::from_str(arguments).context("parsing narrate_group arguments")?;
    nonempty(&call.text)?;
    Ok(Draft {
        named: call.exclude_character_ids.clone(),
        texts: vec![NarrationText {
            text: call.text,
            only: None,
            except: call.exclude_character_ids,
        }],
        private: false,
    })
}

fn actor_and_others_draft(arguments: &str, actor: i64) -> Result<Draft> {
    let call: NarrateActorAndOthers =
        serde_json::from_str(arguments).context("parsing narrate_actor_and_others arguments")?;
    nonempty(&call.actor_text)?;
    nonempty(&call.others_text)?;
    let mut except = call.exclude_character_ids.clone();
    except.push(actor);
    Ok(Draft {
        named: call.exclude_character_ids.clone(),
        texts: vec![
            NarrationText {
                text: call.actor_text,
                only: Some(vec![actor]),
                except: call.exclude_character_ids,
            },
            NarrationText {
                text: call.others_text,
                only: None,
                except,
            },
        ],
        private: false,
    })
}

fn nothing_draft(arguments: &str) -> Result<Draft> {
    let arguments: serde_json::Map<String, serde_json::Value> =
        serde_json::from_str(arguments).context("parsing narrate_nothing arguments")?;
    ensure!(arguments.is_empty(), "narrate_nothing takes no arguments");
    Ok(Draft {
        named: Vec::new(),
        texts: Vec::new(),
        private: false,
    })
}

/// Turns one narrate tool's arguments into its texts.
type Parse = Arc<dyn Fn(&str) -> Result<Draft> + Send + Sync>;

fn characters_draft(arguments: &str) -> Result<Draft> {
    let call: NarrateCharacters =
        serde_json::from_str(arguments).context("parsing narrate_characters arguments")?;
    nonempty(&call.text)?;
    ensure!(
        !call.character_ids.is_empty(),
        "narrate_characters needs at least one character"
    );
    Ok(Draft {
        named: call.character_ids.clone(),
        texts: vec![NarrationText {
            text: call.text,
            only: Some(call.character_ids),
            except: Vec::new(),
        }],
        private: true,
    })
}

impl NarrationTools {
    pub fn new<B>(game: &Arc<Game<B>>, scope: NarrationScope) -> Result<Self>
    where
        B: Backend + Send + Sync + 'static,
    {
        let prompts = prompts::current()?;
        let tools = &prompts.gm.tools;
        let captured = Arc::new(Mutex::new(None));
        let scope = Arc::new(scope);
        let tool = |definition: ToolDefinition, parse: Parse| {
            narration_tool(game, definition, &scope, &captured, parse)
        };
        let mut narrate = vec![
            tool(
                tools.tool("narrate_group", schema(schema_for!(NarrateGroup)))?,
                Arc::new(group_draft),
            ),
            tool(
                tools.tool("narrate_characters", schema(schema_for!(NarrateCharacters)))?,
                Arc::new(characters_draft),
            ),
            tool(
                tools.tool("narrate_nothing", prompts::no_arguments())?,
                Arc::new(nothing_draft),
            ),
        ];
        if let Some(actor) = scope.actor {
            narrate.insert(
                1,
                tool(
                    tools.tool(
                        "narrate_actor_and_others",
                        schema(schema_for!(NarrateActorAndOthers)),
                    )?,
                    Arc::new(move |arguments| actor_and_others_draft(arguments, actor)),
                ),
            );
        }
        Ok(Self {
            tools: narrate,
            captured,
            shares: game.rules.characters_share_narration,
        })
    }

    /// What the GM tells itself when it answers in plain text.
    pub fn reminder(&self) -> Result<String> {
        let names: Vec<String> = self
            .tools
            .iter()
            .map(|tool| tool.definition().name)
            .collect();
        prompts::fill(
            &prompts::current()?.gm.narrate_reminder,
            &[("tools", &names.join(", "))],
        )
    }

    /// The narration the GM's call captured, to deliver in `group_id` from
    /// `sequence_id`. A turn that ended through a narrate tool always has one.
    pub fn finish(
        self,
        (group_id, sequence_id): (i64, i64),
        inference_id: i64,
        actor: Option<i64>,
    ) -> Result<Narration> {
        let Draft { texts, private, .. } = self
            .captured
            .lock()
            .expect("narration lock poisoned")
            .take()
            .context("the GM's turn ended without a narrate call")?;
        Ok(Narration {
            group_id,
            inference_id,
            sequence_id,
            actor,
            opens_scene: false,
            players_share: self.shares && !private,
            texts,
        })
    }
}

fn schema(schema: schemars::Schema) -> serde_json::Value {
    serde_json::to_value(schema).expect("narration schema should serialize")
}

fn narration_tool<B>(
    game: &Arc<Game<B>>,
    definition: ToolDefinition,
    scope: &Arc<NarrationScope>,
    captured: &Arc<Mutex<Option<Draft>>>,
    parse: Parse,
) -> Tool
where
    B: Backend + Send + Sync + 'static,
{
    let game = Arc::clone(game);
    let scope = Arc::clone(scope);
    let captured = Arc::clone(captured);
    Tool::terminal(definition, move |context| {
        let arguments = context.arguments.to_string();
        let game = Arc::clone(&game);
        let scope = Arc::clone(&scope);
        let captured = Arc::clone(&captured);
        let parse = Arc::clone(&parse);
        Box::pin(async move {
            let draft = match parse(&arguments) {
                Ok(draft) => draft,
                Err(error) => return Ok(ToolOutcome::rejected(error)),
            };
            let group_id = game.store.character_group_id(scope.in_group).await?;
            let present = game.store.group_character_ids(group_id).await?;
            if let Some(stranger) = draft.named.iter().find(|id| !present.contains(id)) {
                return Ok(ToolOutcome::Rejected(format!(
                    "Tool arguments were rejected: character {stranger} is not in this group"
                )));
            }
            let presented: Vec<i64> = scope
                .presented
                .lock()
                .expect("presented actions lock poisoned")
                .iter()
                .map(|action| action.id)
                .collect();
            for action_id in presented {
                if game
                    .store
                    .has_pending_action(scope.world_id, action_id)
                    .await?
                {
                    return Ok(ToolOutcome::Rejected(format!(
                        "Tool arguments were rejected: action {action_id} is not yet approved or rejected; resolve it before narrating"
                    )));
                }
            }
            let nothing = draft.texts.is_empty();
            *captured.lock().expect("narration lock poisoned") = Some(draft);
            Ok(ToolOutcome::Completed(
                if nothing {
                    "Nothing narrated."
                } else {
                    "Narration recorded."
                }
                .to_string(),
            ))
        }) as ToolFuture
    })
}

impl<B> Game<B>
where
    B: Backend + Send + Sync + 'static,
{
    pub(super) async fn deliver_narration(
        &self,
        world_id: i64,
        narration: &Narration,
    ) -> Result<()> {
        let world = self.world(world_id)?;
        self.store
            .append_group_narration(world_id, narration)
            .await
            .context("storing GM narration before the player agent follows up")?;
        // A player may disconnect while the GM resolves. The narration remains
        // stored for reload; active viewers receive this exact same text.
        let _no_live_viewers = world.broadcasts.send(crate::world::ChatChange::Entries);
        Ok(())
    }
}
