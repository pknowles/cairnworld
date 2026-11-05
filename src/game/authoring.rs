//! The tools of world creation's object phase (design.md's Storyteller world
//! creation): the Storyteller creates, changes and deletes objects, and the
//! Questioner reads them, marks them reviewed and says when they are whole.
//! Three tools with an `object` kind stand in for the declared per-kind
//! Create/Update/Delete tools, so the model is shown each field once. Every tool
//! is built for one world, so a call can only touch that world.

use std::sync::Arc;

use anyhow::Result;
use schemars::{JsonSchema, schema_for};
use serde::{Deserialize, de::DeserializeOwned};

use crate::{
    inspection::DevEvent,
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
    let with = |extra: &[&str], names: Vec<String>| -> Vec<String> {
        extra
            .iter()
            .map(ToString::to_string)
            .chain(names)
            .collect()
    };
    Ok(vec![
        changer::<CreateObject>(
            world,
            "create_object",
            "Created",
            with(&["object"], Fields::names()),
            |call| Change::Create {
                kind: call.object,
                fields: &call.fields,
            },
        )?,
        changer::<UpdateObject>(
            world,
            "update_object",
            "Changed",
            with(&["object", "id"], Fields::names()),
            |call| Change::Update {
                kind: call.object,
                id: call.id,
                fields: &call.fields,
            },
        )?,
        changer::<DeleteObject>(
            world,
            "delete_object",
            "Deleted",
            with(&["object", "id"], DeleteOptions::names()),
            |call| Change::Delete {
                kind: call.object,
                id: call.id,
                options: &call.options,
            },
        )?,
    ])
}

fn changer<T>(
    (store, world): (&Store, &Arc<World>),
    name: &'static str,
    done: &'static str,
    allowed: Vec<String>,
    wrap: for<'a> fn(&'a T) -> Change<'a>,
) -> Result<Tool>
where
    T: DeserializeOwned + JsonSchema + Send + 'static,
{
    let mut definition = prompts::current()?.storyteller.tools.tool(
        name,
        serde_json::to_value(schema_for!(T)).expect("object schema should serialize"),
    )?;
    drop_null_types(&mut definition.schema);
    let store = store.clone();
    let world = Arc::clone(world);
    Ok(Tool::new(definition, move |context| {
        let arguments = context.arguments.to_string();
        let store = store.clone();
        let world = Arc::clone(&world);
        let allowed = allowed.clone();
        Box::pin(async move {
            let call: T = match parse(&arguments, &allowed) {
                Ok(call) => call,
                Err(error) => return Ok(ToolOutcome::rejected(error)),
            };
            match store.change_object(world.id, wrap(&call)).await {
                Ok(changed) => {
                    world.publish(DevEvent::Changed {
                        sequence_id: None,
                        inference_id: None,
                    });
                    Ok(ToolOutcome::Completed(format!(
                        "{done} {} {}. The world now has {}.",
                        changed.kind,
                        changed.id,
                        serde_json::to_string(&changed.counts)?
                    )))
                }
                Err(error) => rejection_or_failure(error),
            }
        })
    }))
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
    let read_allowed = allowed.clone();
    let read = Tool::new(
        tools.tool(
            "read_object",
            serde_json::to_value(schema_for!(ObjectId)).expect("schema should serialize"),
        )?,
        move |context| {
            let arguments = context.arguments.to_string();
            let store = read_store.clone();
            let allowed = read_allowed.clone();
            Box::pin(async move {
                let object: ObjectId = match parse(&arguments, &allowed) {
                    Ok(object) => object,
                    Err(error) => return Ok(ToolOutcome::rejected(error)),
                };
                match store.read_object(world_id, object.object, object.id).await {
                    Ok(text) => Ok(ToolOutcome::Completed(text)),
                    Err(error) => rejection_or_failure(error),
                }
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
