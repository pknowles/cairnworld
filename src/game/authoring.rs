//! The tools of world creation's object phase (design.md's Storyteller world
//! creation): the Storyteller creates, changes and deletes objects, and the
//! Questioner reads them, marks them reviewed and says when they are whole.
//! `create_object` and `update_object` take the kind as an argument; creating
//! requires a description, which every kind has, and changing carries no field
//! text of its own, so each field is described once. There is no tool for a
//! group: an NPC joins one or starts one, and a group goes with its last NPC.
//! Every tool is built for one world, so a call can only touch that world.

use std::sync::Arc;

use anyhow::Result;
use schemars::{JsonSchema, schema_for};
use serde::{Deserialize, de::DeserializeOwned};

use crate::{
    inspection::DevEvent,
    llm::ToolDefinition,
    prompts,
    store::{Change, DeleteOptions, Fields, ObjectKind, Rejected, Store},
    tools::{Tool, ToolOutcome},
    world::World,
};

#[derive(Deserialize, JsonSchema)]
struct CreateObject {
    object: ObjectKind,
    #[serde(flatten)]
    fields: Fields,
}

#[derive(Deserialize, JsonSchema)]
struct UpdateObject {
    object: ObjectKind,
    id: i64,
    #[serde(flatten)]
    fields: Fields,
}

#[derive(Deserialize, JsonSchema)]
struct DeleteObject {
    object: ObjectKind,
    id: i64,
    #[serde(flatten)]
    options: DeleteOptions,
}

/// The tools for the Storyteller to create, change and delete the world's
/// objects. A rejected call (a name already used, an ID of nothing, a delete
/// something depends on) is an ordinary tool result.
pub(super) fn storyteller_tools(store: &Store, world: &Arc<World>) -> Result<Vec<Tool>> {
    let world = (store, world);
    let prompts = prompts::current()?;
    let tools = &prompts.storyteller.tools;
    let mut list = Vec::new();
    let mut create = tools.tool(
        "create_object",
        serde_json::to_value(schema_for!(CreateObject)).expect("object schema should serialize"),
    )?;
    drop_null_types(&mut create.schema);
    // Every kind it creates has a description, so the schema can say it is needed.
    create.schema["required"]
        .as_array_mut()
        .expect("the schema lists what is required")
        .push("description".into());
    remove_group_kind(&mut create.schema);
    list.push(changer::<CreateObject>(
        world,
        create,
        ["object"]
            .map(String::from)
            .into_iter()
            .chain(Fields::names())
            .collect(),
        Box::new(|call| Change::Create {
            kind: call.object,
            fields: &call.fields,
        }),
    ));
    let mut update = tools.tool(
        "update_object",
        serde_json::to_value(schema_for!(UpdateObject)).expect("object schema should serialize"),
    )?;
    drop_null_types(&mut update.schema);
    remove_group_kind(&mut update.schema);
    list.push(changer::<UpdateObject>(
        world,
        update,
        ["object", "id"]
            .map(String::from)
            .into_iter()
            .chain(Fields::names())
            .collect(),
        Box::new(|call| Change::Update {
            kind: call.object,
            id: call.id,
            fields: &call.fields,
        }),
    ));
    let mut delete = tools.tool(
        "delete_object",
        serde_json::to_value(schema_for!(DeleteObject)).expect("object schema should serialize"),
    )?;
    drop_null_types(&mut delete.schema);
    remove_group_kind(&mut delete.schema);
    list.push(changer::<DeleteObject>(
        world,
        delete,
        ["object", "id"]
            .map(String::from)
            .into_iter()
            .chain(DeleteOptions::names())
            .collect(),
        Box::new(|call| Change::Delete {
            kind: call.object,
            id: call.id,
            options: &call.options,
        }),
    ));
    Ok(list)
}

/// A group is not created, changed or deleted through these tools, so they do
/// not offer it as a kind.
fn remove_group_kind(schema: &mut serde_json::Value) {
    if let Some(kinds) = schema["properties"]["object"]["enum"].as_array_mut() {
        kinds.retain(|kind| kind != "group");
    }
}

type Wrap<T> = Box<dyn for<'a> Fn(&'a T) -> Change<'a> + Send + Sync>;

fn changer<T>(
    (store, world): (&Store, &Arc<World>),
    definition: ToolDefinition,
    allowed: Vec<String>,
    wrap: Wrap<T>,
) -> Tool
where
    T: DeserializeOwned + Send + 'static,
{
    let store = store.clone();
    let world = Arc::clone(world);
    let wrap = Arc::new(wrap);
    Tool::new(definition, move |context| {
        let arguments = context.arguments.to_string();
        let store = store.clone();
        let world = Arc::clone(&world);
        let allowed = allowed.clone();
        let wrap = Arc::clone(&wrap);
        Box::pin(async move {
            let call: T = match parse(&arguments, &allowed) {
                Ok(call) => call,
                Err(error) => return Ok(ToolOutcome::rejected(error)),
            };
            let change = wrap(&call);
            let done = change.past_tense();
            match store.change_object(world.id, change).await {
                Ok(changed) => {
                    world.publish(DevEvent::Changed {
                        sequence_id: None,
                        inference_id: None,
                    });
                    let group = changed
                        .group
                        .map(|group| format!(" in new group {group}"))
                        .unwrap_or_default();
                    Ok(ToolOutcome::Completed(format!(
                        "{done} {} {}{group}. The world now has {}.",
                        changed.kind,
                        changed.id,
                        serde_json::to_string(&changed.counts)?
                    )))
                }
                Err(error) => rejection_or_failure(error),
            }
        })
    })
}

/// An argument that may be left out is already absent from `required`, so
/// its type need not also allow null, which costs context in every request.
fn drop_null_types(schema: &mut serde_json::Value) {
    let Some(properties) = schema["properties"].as_object_mut() else {
        return;
    };
    for property in properties.values_mut() {
        if let Some(types) = property["type"].as_array()
            && let [kind, null] = types.as_slice()
            && null == "null"
        {
            property["type"] = kind.clone();
        }
    }
}

/// Read a call's arguments, refusing any the tool does not take (serde would
/// otherwise drop them silently).
fn parse<T: DeserializeOwned>(arguments: &str, allowed: &[String]) -> Result<T> {
    let value: serde_json::Value = serde_json::from_str(arguments)?;
    if let Some(map) = value.as_object()
        && let Some(unknown) = map.keys().find(|key| !allowed.contains(key))
    {
        anyhow::bail!("`{unknown}` is not an argument of this tool");
    }
    Ok(serde_json::from_value(value)?)
}

/// What the Questioner uses to look at the world and to end the phase. Asking
/// questions is the other way a review ends; that tool is shared with the
/// summary phase.
pub(super) fn review_tools(store: &Store, world_id: i64) -> Result<Vec<Tool>> {
    let tools = &prompts::current()?.questioner.tools;
    let allowed = ["object", "id", "reviewed"].map(String::from).to_vec();
    let read_store = store.clone();
    let read_allowed = vec!["objects".to_string()];
    let read = Tool::new(
        tools.tool(
            "read_object",
            serde_json::to_value(schema_for!(ReadObjects)).expect("schema should serialize"),
        )?,
        move |context| {
            let arguments = context.arguments.to_string();
            let store = read_store.clone();
            let allowed = read_allowed.clone();
            Box::pin(async move {
                let read: ReadObjects = match parse(&arguments, &allowed) {
                    Ok(read) => read,
                    Err(error) => return Ok(ToolOutcome::rejected(error)),
                };
                if read.objects.is_empty() {
                    return Ok(ToolOutcome::Rejected("Name at least one object.".into()));
                }
                let mut texts = Vec::new();
                for object in read.objects {
                    match store.read_object(world_id, object.object, object.id).await {
                        Ok(text) => texts.push(text),
                        Err(error) => return rejection_or_failure(error),
                    }
                }
                Ok(ToolOutcome::Completed(texts.join(
                    "

",
                )))
            })
        },
    );
    let mark_store = store.clone();
    let mark = Tool::new(
        tools.tool(
            "mark_object_reviewed",
            serde_json::to_value(schema_for!(MarkReviewed)).expect("schema should serialize"),
        )?,
        move |context| {
            let arguments = context.arguments.to_string();
            let store = mark_store.clone();
            let allowed = allowed.clone();
            Box::pin(async move {
                let mark: MarkReviewed = match parse(&arguments, &allowed) {
                    Ok(mark) => mark,
                    Err(error) => return Ok(ToolOutcome::rejected(error)),
                };
                match store
                    .set_reviewed(world_id, mark.object, mark.id, mark.reviewed)
                    .await
                {
                    Ok(()) => Ok(ToolOutcome::Completed(format!(
                        "{:?} {} is now marked {}.",
                        mark.object,
                        mark.id,
                        if mark.reviewed {
                            "reviewed"
                        } else {
                            "unreviewed"
                        }
                    ))),
                    Err(error) => rejection_or_failure(error),
                }
            })
        },
    );
    let done_store = store.clone();
    let done = Tool::terminal(
        tools.tool("core_objects_created", prompts::no_arguments())?,
        move |_| {
            let store = done_store.clone();
            Box::pin(async move {
                let problems = store.core_object_problems(world_id).await?;
                Ok(if problems.is_empty() {
                    ToolOutcome::Completed("The world is recorded as complete.".into())
                } else {
                    ToolOutcome::Rejected(format!(
                        "The world is not ready yet. {}",
                        problems.join(" ")
                    ))
                })
            })
        },
    );
    Ok(vec![read, mark, done])
}

#[derive(Deserialize, JsonSchema)]
struct ObjectId {
    object: ObjectKind,
    id: i64,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ReadObjects {
    objects: Vec<ObjectId>,
}

#[derive(Deserialize, JsonSchema)]
struct MarkReviewed {
    object: ObjectKind,
    id: i64,
    reviewed: bool,
}

/// A refusal of the world's rules goes back to the model; anything else is a
/// failure of the game.
fn rejection_or_failure(error: anyhow::Error) -> Result<ToolOutcome> {
    match error.downcast_ref::<Rejected>() {
        Some(rejected) => Ok(ToolOutcome::Rejected(rejected.0.clone())),
        None => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{Store, TestDatabase};

    /// Every tool stays visible to the agent, so what they cost in every request
    /// has to fit the smallest context the game supports (4096 tokens).
    #[tokio::test]
    async fn the_storyteller_tools_stay_small_enough_for_a_small_context() {
        let db = TestDatabase::new("tool-size");
        let store = Store::open(db.path()).await.unwrap();
        let owner = store
            .find_or_create_user("maker@example.test", "Maker")
            .await
            .unwrap();
        let member = store.create_world_to_generate(&owner, "x").await.unwrap();
        let worlds = World::load(&store).await.unwrap();
        let world = World::get(&worlds, member.world_id).unwrap();
        let sizes: Vec<(String, usize)> = storyteller_tools(&store, &world)
            .unwrap()
            .iter()
            .map(|tool| {
                let definition = tool.definition();
                (
                    definition.name.clone(),
                    serde_json::to_string(&definition).unwrap().len(),
                )
            })
            .collect();
        println!("STORYTELLER_TOOL_SIZES {sizes:?}");
        for tool in storyteller_tools(&store, &world).unwrap() {
            println!(
                "TOOL {}",
                serde_json::to_string(&tool.definition()).unwrap()
            );
        }
        let characters: usize = sizes.iter().map(|(_, size)| size).sum();
        println!("STORYTELLER_TOOL_CHARACTERS {characters}");
        assert!(characters < 4_200, "the tools are {characters} characters");
    }
}
