//! The tools of world creation's object phase (design.md's Storyteller world
//! creation): the Storyteller creates objects, and the Questioner reads them,
//! marks them reviewed and says when they are whole. Every tool is built for
//! one world, so a call can only touch that world.

use std::sync::Arc;

use anyhow::Result;
use schemars::{JsonSchema, schema_for};
use serde::{Deserialize, de::DeserializeOwned};

use crate::{
    inspection::DevEvent,
    prompts,
    scenario::{Connection, Group, ItemType, Location, Npc},
    store::{
        Change, ConnectionDelete, ConnectionUpdate, Delete, GroupUpdate, ItemTypeUpdate,
        ItemUpdate, LocationDelete, LocationUpdate, NameOnly, NewItem, NewObject, NpcDelete,
        NpcUpdate, ObjectKind, Rejected, Store, Update,
    },
    tools::{Tool, ToolOutcome},
    world::World,
};

/// The tools for the Storyteller to create, change and delete the world's
/// objects. A rejected call (a name already used, a reference to nothing, a
/// delete something depends on) is an ordinary tool result.
pub(super) fn storyteller_tools(store: &Store, world: &Arc<World>) -> Result<Vec<Tool>> {
    let s = (store, world);
    Ok(vec![
        changer::<Location>(s, "create_location", |o| {
            Change::Create(NewObject::Location(o))
        })?,
        changer::<Connection>(s, "create_connection", |o| {
            Change::Create(NewObject::Connection(o))
        })?,
        changer::<Group>(s, "create_group", |o| Change::Create(NewObject::Group(o)))?,
        changer::<Npc>(s, "create_npc", |o| Change::Create(NewObject::Npc(o)))?,
        changer::<ItemType>(s, "create_item_type", |o| {
            Change::Create(NewObject::ItemType(o))
        })?,
        changer::<NewItem>(s, "create_item", |o| Change::Create(NewObject::Item(o)))?,
        changer::<LocationUpdate>(s, "update_location", |o| {
            Change::Update(Update::Location(o))
        })?,
        changer::<ConnectionUpdate>(s, "update_connection", |o| {
            Change::Update(Update::Connection(o))
        })?,
        changer::<GroupUpdate>(s, "update_group", |o| Change::Update(Update::Group(o)))?,
        changer::<NpcUpdate>(s, "update_npc", |o| Change::Update(Update::Npc(o)))?,
        changer::<ItemTypeUpdate>(s, "update_item_type", |o| {
            Change::Update(Update::ItemType(o))
        })?,
        changer::<ItemUpdate>(s, "update_item", |o| Change::Update(Update::Item(o)))?,
        changer::<LocationDelete>(s, "delete_location", |o| {
            Change::Delete(Delete::Location(o))
        })?,
        changer::<ConnectionDelete>(s, "delete_connection", |o| {
            Change::Delete(Delete::Connection(o))
        })?,
        changer::<NpcDelete>(s, "delete_npc", |o| Change::Delete(Delete::Npc(o)))?,
        changer::<NameOnly>(s, "delete_item_type", |o| {
            Change::Delete(Delete::ItemType(o))
        })?,
        changer::<NameOnly>(s, "delete_item", |o| Change::Delete(Delete::Item(o)))?,
    ])
}

fn changer<T>(
    (store, world): (&Store, &Arc<World>),
    name: &'static str,
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
        Box::pin(async move {
            let object: T = match parse(&arguments) {
                Ok(object) => object,
                Err(error) => return Ok(ToolOutcome::rejected(error)),
            };
            match store.change_object(world.id, wrap(&object)).await {
                Ok(counts) => {
                    world.publish(DevEvent::Changed {
                        sequence_id: None,
                        inference_id: None,
                    });
                    Ok(ToolOutcome::Completed(format!(
                        "Done. The world now has {}.",
                        serde_json::to_string(&counts)?
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

/// Read a call's arguments. The notes are not the Storyteller's to write here.
fn parse<T: DeserializeOwned>(arguments: &str) -> Result<T> {
    let value: serde_json::Value = serde_json::from_str(arguments)?;
    anyhow::ensure!(value.get("notes").is_none(), "notes cannot be set here");
    Ok(serde_json::from_value(value)?)
}

/// What the Questioner uses to look at the world and to end the phase. Asking
/// questions is the other way a review ends; that tool is shared with the
/// summary phase.
pub(super) fn review_tools(store: &Store, world_id: i64) -> Result<Vec<Tool>> {
    let tools = &prompts::current()?.questioner.tools;
    let read_store = store.clone();
    let read = Tool::new(
        tools.tool(
            "read_object",
            serde_json::to_value(schema_for!(ObjectName)).expect("schema should serialize"),
        )?,
        move |context| {
            let arguments = context.arguments.to_string();
            let store = read_store.clone();
            Box::pin(async move {
                let object: ObjectName = match parse(&arguments) {
                    Ok(object) => object,
                    Err(error) => return Ok(ToolOutcome::rejected(error)),
                };
                match store.read_object(world_id, object.kind, &object.name).await {
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
            Box::pin(async move {
                let mark: MarkReviewed = match parse(&arguments) {
                    Ok(mark) => mark,
                    Err(error) => return Ok(ToolOutcome::rejected(error)),
                };
                match store
                    .set_reviewed(world_id, mark.kind, &mark.name, mark.reviewed)
                    .await
                {
                    Ok(()) => Ok(ToolOutcome::Completed(format!(
                        "{:?} {} is now marked {}.",
                        mark.kind,
                        mark.name,
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
#[serde(deny_unknown_fields)]
struct ObjectName {
    kind: ObjectKind,
    name: String,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct MarkReviewed {
    kind: ObjectKind,
    name: String,
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
