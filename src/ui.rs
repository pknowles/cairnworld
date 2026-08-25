use leptos::prelude::*;
use serde::{Deserialize, Serialize};

#[cfg(feature = "hydrate")]
use {
    std::{cell::RefCell, rc::Rc},
    wasm_bindgen::{JsCast, closure::Closure},
    wasm_bindgen_futures::{JsFuture, spawn_local},
    web_sys::{Event, MessageEvent, Response, WebSocket},
};

/// Messages accepted from the browser's player-chat transport.
#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientEvent {
    Message { text: String },
}

/// Player-visible events emitted by the server's player-chat transport.
#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerEvent {
    History { entries: Vec<ChatEntry> },
    Entry { role: ChatRole, text: String },
    CanAct { value: bool },
    Error { message: String },
}

/// The source of a player-visible chat entry.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ChatRole {
    User,
    Assistant,
    Narration,
    Notice,
}

/// A durable or live entry rendered in the player's chat transcript.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct ChatEntry {
    pub role: ChatRole,
    pub text: String,
}

/// Waits once for the server's model lifecycle transition, then reloads into
/// the server-rendered game page with its durable history.
#[island]
pub fn GameLoading() -> impl IntoView {
    let message = RwSignal::new("Preparing the game world…".to_string());

    #[cfg(feature = "hydrate")]
    wait_for_game(message);

    view! {
        <main class="min-h-dvh bg-base-200 p-4 sm:p-8">
            <section class="card mx-auto max-w-xl bg-base-100 shadow-xl">
                <div class="card-body">
                    <h1 class="card-title text-2xl">"Cairnworld"</h1>
                    <span class="loading loading-spinner loading-md text-primary" aria-hidden="true"></span>
                    <p role="status">{move || message.get()}</p>
                </div>
            </section>
        </main>
    }
}

#[cfg(feature = "hydrate")]
fn wait_for_game(message: RwSignal<String>) {
    let Some(window) = web_sys::window() else {
        message.set("Could not start the game-ready check: no browser window is available.".into());
        return;
    };
    let request = window.fetch_with_str("/game-status");
    spawn_local(async move {
        let response = match JsFuture::from(request).await {
            Ok(response) => match response.dyn_into::<Response>() {
                Ok(response) => response,
                Err(_) => {
                    message
                        .set("The game-ready check returned an invalid browser response.".into());
                    return;
                }
            },
            Err(error) => {
                message.set(format!(
                    "Could not check whether the game is ready: {error:?}"
                ));
                return;
            }
        };
        if response.ok() {
            if let Err(error) = window.location().reload() {
                message.set(format!(
                    "The game is ready, but this page could not reload: {error:?}"
                ));
            }
        } else {
            let status = response.status();
            let body = match response.text() {
                Ok(body) => body,
                Err(error) => {
                    message.set(format!(
                        "The game could not start (HTTP {status}); obtaining the server error failed: {error:?}"
                    ));
                    return;
                }
            };
            match JsFuture::from(body).await {
                Ok(body) => message.set(format!(
                    "The game could not start (HTTP {status}): {}",
                    body.as_string().unwrap_or_else(|| "the server returned a non-text error".into())
                )),
                Err(error) => message.set(format!(
                    "The game could not start (HTTP {status}); reading the server error failed: {error:?}"
                )),
            }
        }
    });
}

#[island]
pub fn PlayerChat(world_id: i64, history: Vec<ChatEntry>) -> impl IntoView {
    #[cfg(not(feature = "hydrate"))]
    let _ = world_id;
    let entries = RwSignal::new(history);
    let can_act = RwSignal::new(false);
    let input = NodeRef::<leptos::html::Input>::new();

    #[cfg(feature = "hydrate")]
    let socket = connect_player_chat(world_id, entries, can_act);

    let on_submit = move |event: leptos::ev::SubmitEvent| {
        event.prevent_default();
        #[cfg(feature = "hydrate")]
        {
            let Some(input_element) = input.get() else {
                return;
            };
            let text = input_element.value();
            if text.trim().is_empty() {
                return;
            }
            let socket = socket.borrow();
            let Some(socket) = socket.as_ref() else {
                return;
            };
            if socket.ready_state() != WebSocket::OPEN {
                return;
            }
            let event = ClientEvent::Message { text: text.clone() };
            match serde_json::to_string(&event).and_then(|event| {
                socket.send_with_str(&event).map_err(|error| {
                    serde_json::Error::io(std::io::Error::other(format!(
                        "sending browser chat message: {error:?}"
                    )))
                })
            }) {
                Ok(()) => {
                    entries.update(|entries| {
                        entries.push(ChatEntry {
                            role: ChatRole::User,
                            text,
                        })
                    });
                    input_element.set_value("");
                    can_act.set(false);
                }
                Err(error) => add_notice(entries, format!("Could not send message: {error}")),
            }
        }
    };

    view! {
        <ol id="chat" class="flex min-h-0 flex-1 flex-col gap-3 overflow-y-auto px-1 py-4" aria-live="polite">
            {move || entries.get().into_iter().map(|entry| {
                let class = match entry.role {
                    ChatRole::User => "chat chat-end",
                    ChatRole::Assistant => "chat chat-start",
                    ChatRole::Narration => "rounded-box bg-base-200 px-4 py-3 text-sm text-base-content/80",
                    ChatRole::Notice => "alert alert-warning text-sm",
                };
                let bubble = match entry.role {
                    ChatRole::User => "chat-bubble chat-bubble-primary",
                    ChatRole::Assistant => "chat-bubble",
                    ChatRole::Narration | ChatRole::Notice => "",
                };
                view! { <li class=class data-role=chat_role_name(entry.role)><div class=bubble>{entry.text}</div></li> }
            }).collect_view()}
        </ol>
        <form id="message" class="join w-full" on:submit=on_submit>
            <input class="input join-item min-w-0 flex-1" node_ref=input name="text" autocomplete="off" placeholder="Write a message…" disabled=move || !can_act.get()/>
            <button class="btn btn-primary join-item" type="submit" disabled=move || !can_act.get()>"Send"</button>
        </form>
    }
}

pub fn chat_role_name(role: ChatRole) -> &'static str {
    match role {
        ChatRole::User => "user",
        ChatRole::Assistant => "assistant",
        ChatRole::Narration => "narration",
        ChatRole::Notice => "notice",
    }
}

#[cfg(feature = "hydrate")]
fn connect_player_chat(
    world_id: i64,
    entries: RwSignal<Vec<ChatEntry>>,
    can_act: RwSignal<bool>,
) -> Rc<RefCell<Option<WebSocket>>> {
    let socket = Rc::new(RefCell::new(None));
    let socket_for_connection = socket.clone();
    {
        let Some(window) = web_sys::window() else {
            add_notice(
                entries,
                "Could not open chat: no browser window is available.".into(),
            );
            return socket;
        };
        let location = window.location();
        let scheme = if location.protocol().ok().as_deref() == Some("https:") {
            "wss"
        } else {
            "ws"
        };
        let host = match location.host() {
            Ok(host) => host,
            Err(_) => {
                add_notice(
                    entries,
                    "Could not determine the browser host for chat.".into(),
                );
                return socket;
            }
        };
        let url = format!("{scheme}://{host}/world/{world_id}/ws");
        let socket = match WebSocket::new(&url) {
            Ok(socket) => socket,
            Err(error) => {
                add_notice(
                    entries,
                    format!("Could not open chat connection: {error:?}"),
                );
                return socket;
            }
        };

        let on_open_entries = entries;
        let on_open = Closure::<dyn FnMut(Event)>::new(move |_| {
            add_notice(
                on_open_entries,
                "Connected. Your guide is preparing the opening message…".into(),
            );
        });
        socket.set_onopen(Some(on_open.as_ref().unchecked_ref()));
        on_open.forget();

        let on_message_entries = entries;
        let on_message_can_act = can_act;
        let on_message = Closure::<dyn FnMut(Event)>::new(move |event: Event| {
            let Some(message) = event.dyn_ref::<MessageEvent>() else {
                return;
            };
            let text: String = match message.data().as_string() {
                Some(text) => text,
                None => {
                    add_notice(
                        on_message_entries,
                        "Chat server sent a non-text message.".into(),
                    );
                    return;
                }
            };
            match serde_json::from_str::<ServerEvent>(&text) {
                Ok(ServerEvent::History { entries: history }) => entries.set(history),
                Ok(ServerEvent::Entry { role, text }) => on_message_entries.update(|entries| {
                    entries.push(ChatEntry { role, text });
                }),
                Ok(ServerEvent::CanAct { value }) => on_message_can_act.set(value),
                Ok(ServerEvent::Error { message }) => add_notice(on_message_entries, message),
                Err(error) => add_notice(
                    on_message_entries,
                    format!("Chat server sent an invalid message: {error}"),
                ),
            }
        });
        socket.set_onmessage(Some(on_message.as_ref().unchecked_ref()));
        // The browser owns this callback until the document unloads with its
        // WebSocket. wasm-bindgen otherwise drops it as this function returns.
        on_message.forget();

        let on_close_entries = entries;
        let on_close_can_act = can_act;
        let on_close = Closure::<dyn FnMut(Event)>::new(move |_| {
            on_close_can_act.set(false);
            add_notice(
                on_close_entries,
                "Connection closed. Reload to reconnect.".into(),
            );
        });
        socket.set_onclose(Some(on_close.as_ref().unchecked_ref()));
        on_close.forget();

        *socket_for_connection.borrow_mut() = Some(socket);
    }

    socket
}

#[cfg(feature = "hydrate")]
fn add_notice(entries: RwSignal<Vec<ChatEntry>>, text: String) {
    entries.update(|entries| {
        entries.push(ChatEntry {
            role: ChatRole::Notice,
            text,
        })
    });
}
