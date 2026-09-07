use std::{net::SocketAddr, sync::Arc};

use anyhow::{Context, Result};
use axum::{
    Router,
    extract::{
        Form, Path, Query, State,
        ws::{Message as WebSocketMessage, WebSocket, WebSocketUpgrade},
    },
    http::{HeaderMap, StatusCode, header::HOST},
    response::{Html, IntoResponse, Redirect},
    routing::{get, post},
};
use cairnworld::ui::{
    ChatActivity, ChatEntry, ChatRole, ChatTranscript, ClientEvent, GameLoading, PlayerChat,
    ServerEvent,
};
use hydration_context::SsrSharedContext;
use leptos::prelude::*;
use openidconnect::{
    AuthorizationCode, ClientId, ClientSecret, CsrfToken, EndpointMaybeSet, EndpointNotSet,
    EndpointSet, IssuerUrl, Nonce, PkceCodeChallenge, PkceCodeVerifier, RedirectUrl, Scope,
    TokenResponse,
    core::{CoreAuthenticationFlow, CoreClient, CoreProviderMetadata},
    reqwest,
};
use serde::Deserialize;
use time::Duration;
use tokio::sync::{Notify, RwLock};
use tower_http::services::ServeDir;
use tower_sessions::{Expiry, Session, SessionManagerLayer, cookie::SameSite};
use tower_sessions_sqlx_store::{SqliteStore, sqlx::SqlitePool};

use crate::{
    game::Game,
    llm::{Backend, Content},
    mistralrs_backend::MistralRsBackend,
    scenario::Scenario,
    settings,
    store::{PlayerAgent, PlayerChatEntry, Store, World, WorldMember},
};

const USER_ID: &str = "user_id";
const OAUTH_STATE: &str = "oauth_state";
const OAUTH_NONCE: &str = "oauth_nonce";
const OAUTH_VERIFIER: &str = "oauth_verifier";
const WORLD_DETAIL_ROW_CLASSES: &str =
    "flex flex-wrap items-center justify-between gap-3 rounded-box bg-base-200 p-3";
type GoogleClient = CoreClient<
    EndpointSet,
    EndpointNotSet,
    EndpointNotSet,
    EndpointNotSet,
    EndpointMaybeSet,
    EndpointMaybeSet,
>;

#[derive(Clone)]
struct Google {
    client: GoogleClient,
    http: reqwest::Client,
}

impl Google {
    async fn discover(config: &settings::Web) -> Result<Self> {
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .context("building Google OIDC HTTP client")?;
        let provider = CoreProviderMetadata::discover_async(
            IssuerUrl::new("https://accounts.google.com".to_string())
                .context("parsing Google OIDC issuer URL")?,
            &http,
        )
        .await
        .context("discovering Google OIDC metadata")?;
        let client = CoreClient::from_provider_metadata(
            provider,
            ClientId::new(config.google_client_id.clone()),
            Some(ClientSecret::new(config.google_client_secret.clone())),
        )
        .set_redirect_uri(
            RedirectUrl::new(config.google_redirect_url.clone())
                .context("parsing Google OIDC redirect URL")?,
        );
        Ok(Self { client, http })
    }
}

#[derive(Clone)]
struct App {
    store: Store,
    google: Google,
    game: GameLoad,
}

/// The web-visible lifetime of the one in-process game model.
///
/// HTTP authentication and world browsing do not depend on GPU initialization,
/// so they start while the model is loading. A failed load remains visible to
/// both the terminal and the browser instead of looking like a hung server.
#[derive(Clone)]
pub struct GameLoad {
    state: Arc<RwLock<GameLoadState>>,
    changed: Arc<Notify>,
}

enum GameLoadState {
    Loading,
    Ready(Arc<Game<MistralRsBackend>>),
    Failed(String),
}

pub enum GameAvailability {
    Loading,
    Ready(Arc<Game<MistralRsBackend>>),
    Failed(String),
}

impl GameLoad {
    pub fn loading() -> Self {
        Self {
            state: Arc::new(RwLock::new(GameLoadState::Loading)),
            changed: Arc::new(Notify::new()),
        }
    }

    pub async fn finish(&self, result: Result<Arc<Game<MistralRsBackend>>>) {
        let state = match result {
            Ok(game) => GameLoadState::Ready(game),
            Err(error) => GameLoadState::Failed(format!("{error:#}")),
        };
        *self.state.write().await = state;
        self.changed.notify_waiters();
    }

    pub async fn availability(&self) -> GameAvailability {
        match &*self.state.read().await {
            GameLoadState::Loading => GameAvailability::Loading,
            GameLoadState::Ready(game) => GameAvailability::Ready(Arc::clone(game)),
            GameLoadState::Failed(error) => GameAvailability::Failed(error.clone()),
        }
    }

    pub async fn wait_until_ready(&self) -> GameAvailability {
        loop {
            let changed = self.changed.notified();
            match self.availability().await {
                GameAvailability::Loading => changed.await,
                availability => return availability,
            }
        }
    }
}

#[derive(Deserialize)]
struct Callback {
    code: String,
    state: String,
}

#[derive(Deserialize)]
struct InvitationForm {
    max_uses: Option<i64>,
}

#[derive(Deserialize)]
struct DisplayNameForm {
    display_name: String,
}

/// Serve the OAuth-only browser entrypoint. The Google exchange is deliberately
/// performed by the OpenID Connect crate; Cairnworld only persists the verified
/// email identity it receives and resolves access from that identity.
pub async fn serve(store: Store, config: &settings::Web, game: GameLoad) -> Result<()> {
    let bind: SocketAddr = config
        .bind
        .parse()
        .with_context(|| format!("parsing web.bind `{}`", config.bind))?;
    let google = Google::discover(config).await?;
    let sessions = session_store(store.pool()).await?;
    tracing::info!(
        bind = %bind,
        redirect_url = %config.google_redirect_url,
        "web configuration is ready; binding listener"
    );
    let app = Router::new()
        .route("/", get(landing))
        .route("/auth/google", get(begin_login))
        .route("/auth/google/callback", get(finish_login))
        .route("/game-status", get(game_status))
        .route("/logout", post(logout))
        .route("/profile", post(update_display_name))
        .route("/worlds", post(create_bread_thief_world))
        .route("/world/{world_id}", get(world_detail))
        .route("/world/{world_id}/invitations", post(create_invitation))
        .route(
            "/world/{world_id}/invitations/{token}/revoke",
            post(revoke_invitation),
        )
        .route("/world/{world_id}/members/{user_id}", post(remove_member))
        .route(
            "/world/{world_id}/characters",
            post(create_player_character),
        )
        .route(
            "/invite/{token}",
            get(invitation_page).post(accept_invitation),
        )
        .route(
            "/world/{world_id}/characters/{character_id}/play",
            get(game_page),
        )
        .route(
            "/world/{world_id}/characters/{character_id}/ws",
            get(game_socket),
        )
        .nest_service("/pkg", ServeDir::new("target/site/pkg"))
        .nest_service("/media", ServeDir::new("media"))
        .with_state(App {
            store,
            google,
            game,
        })
        .layer(
            SessionManagerLayer::new(sessions)
                .with_secure(config.google_redirect_url.starts_with("https://"))
                // Google returns through a top-level cross-site GET, for which an
                // OAuth login session must use Lax rather than the library's Strict default.
                .with_same_site(SameSite::Lax)
                .with_expiry(Expiry::OnInactivity(Duration::days(30))),
        );
    let listener = tokio::net::TcpListener::bind(bind)
        .await
        .with_context(|| format!("binding web server at {bind}"))?;
    tracing::info!(address = %listener.local_addr()?, "web server is listening");
    axum::serve(listener, app.into_make_service())
        .await
        .context("serving web application")
}

async fn session_store(pool: SqlitePool) -> Result<SqliteStore> {
    let store = SqliteStore::new(pool);
    store.migrate().await.context("migrating session store")?;
    Ok(store)
}

async fn landing(State(app): State<App>, session: Session) -> Result<Html<String>, WebError> {
    let user = match session.get::<i64>(USER_ID).await? {
        Some(id) => app.store.user(id).await?,
        None => None,
    };
    let worlds = match &user {
        Some(user) => app.store.owned_worlds(user.id).await?,
        None => vec![],
    };
    Ok(Html(render_page(
        "Cairnworld",
        false,
        move || view! { <Landing user=user worlds=worlds/> },
    )))
}

async fn begin_login(session: Session, State(app): State<App>) -> Result<Redirect, WebError> {
    let (challenge, verifier) = PkceCodeChallenge::new_random_sha256();
    let (url, state, nonce) = app
        .google
        .client
        .authorize_url(
            CoreAuthenticationFlow::AuthorizationCode,
            CsrfToken::new_random,
            Nonce::new_random,
        )
        .add_scope(Scope::new("email".to_string()))
        .add_scope(Scope::new("profile".to_string()))
        .set_pkce_challenge(challenge)
        .url();
    session.insert(OAUTH_STATE, state.secret()).await?;
    session.insert(OAUTH_NONCE, nonce.secret()).await?;
    session.insert(OAUTH_VERIFIER, verifier.secret()).await?;
    Ok(Redirect::temporary(url.as_str()))
}

async fn finish_login(
    State(app): State<App>,
    headers: HeaderMap,
    session: Session,
    Query(callback): Query<Callback>,
) -> Result<Redirect, WebError> {
    let callback_host = headers
        .get(HOST)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("<missing or invalid>");
    let session_loaded = session.id().is_some();
    tracing::info!(
        callback_host,
        session_loaded,
        "received Google OAuth callback"
    );
    let expected_state = session
        .remove::<String>(OAUTH_STATE)
        .await?
        .with_context(|| {
            format!(
                "Google callback has no login state; callback host was `{callback_host}`, and the server {}load a valid session cookie and record",
                if session_loaded {
                    "did"
                } else {
                    "did not"
                }
            )
        })?;
    if callback.state != expected_state {
        return Err(WebError::bad_request(
            "Google callback state does not match login state",
        ));
    }
    let nonce = Nonce::new(
        session
            .remove::<String>(OAUTH_NONCE)
            .await?
            .context("Google callback has no login nonce")?,
    );
    let verifier = PkceCodeVerifier::new(
        session
            .remove::<String>(OAUTH_VERIFIER)
            .await?
            .context("Google callback has no PKCE verifier")?,
    );
    let tokens = app
        .google
        .client
        .exchange_code(AuthorizationCode::new(callback.code))
        .context("preparing Google authorization-code exchange")?
        .set_pkce_verifier(verifier)
        .request_async(&app.google.http)
        .await
        .context("exchanging Google authorization code")?;
    let claims = tokens
        .id_token()
        .context("Google did not return an ID token")?
        .claims(&app.google.client.id_token_verifier(), &nonce)
        .context("verifying Google ID token")?;
    if claims.email_verified() != Some(true) {
        return Err(WebError::bad_request(
            "Google account email is not verified",
        ));
    }
    let email = claims
        .email()
        .context("Google ID token contains no email address")?
        .as_str();
    let display_name = claims
        .name()
        .and_then(|name| name.get(None))
        .map(|name| name.as_str().to_string())
        .unwrap_or_else(|| email.to_string());
    let user = app.store.find_or_create_user(email, &display_name).await?;
    session.insert(USER_ID, user.id).await?;
    Ok(Redirect::to("/"))
}

async fn logout(session: Session) -> Result<Redirect, WebError> {
    session.remove::<i64>(USER_ID).await?;
    Ok(Redirect::to("/"))
}

async fn update_display_name(
    State(app): State<App>,
    session: Session,
    Form(form): Form<DisplayNameForm>,
) -> Result<Redirect, WebError> {
    let name = form.display_name.trim();
    if name.is_empty() {
        return Err(WebError::bad_request("display name cannot be empty"));
    }
    let user = session_user(&app.store, &session).await?;
    app.store.rename_user(user.id, name).await?;
    Ok(Redirect::to("/"))
}

async fn create_bread_thief_world(
    State(app): State<App>,
    session: Session,
) -> Result<Redirect, WebError> {
    let user = session_user(&app.store, &session).await?;
    let scenario = Scenario::read(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/scenarios/bread_thief.json"
    ))
    .context("reading checked-in Bread Thief scenario")?;
    let installed = app.store.install_scenario(&user, &scenario).await?;
    Ok(Redirect::to(&format!("/world/{}", installed.world_id)))
}

async fn world_detail(
    State(app): State<App>,
    session: Session,
    Path(world_id): Path<i64>,
) -> Result<Html<String>, WebError> {
    let viewer = session_user(&app.store, &session).await?;
    if !app.store.has_active_membership(viewer.id, world_id).await? {
        return Err(WebError::forbidden(format!(
            "user {} has no active access to world {world_id}",
            viewer.id
        )));
    }
    let world = app
        .store
        .world(world_id)
        .await?
        .with_context(|| format!("active membership references missing world {world_id}"))?;
    let members = app.store.world_members(world_id).await?;
    let invitations = if world.owner_id == viewer.id {
        app.store.invitations(viewer.id, world_id).await?
    } else {
        vec![]
    };
    Ok(Html(render_page(
        "Cairnworld",
        false,
        move || view! { <WorldDetail world=world members=members invitations=invitations viewer_id=viewer.id/> },
    )))
}

async fn create_invitation(
    State(app): State<App>,
    session: Session,
    Path(world_id): Path<i64>,
    Form(form): Form<InvitationForm>,
) -> Result<Redirect, WebError> {
    let owner = session_user(&app.store, &session).await?;
    app.store
        .create_invitation(owner.id, world_id, form.max_uses)
        .await?;
    Ok(Redirect::to(&format!("/world/{world_id}")))
}

async fn revoke_invitation(
    State(app): State<App>,
    session: Session,
    Path((world_id, token)): Path<(i64, String)>,
) -> Result<Redirect, WebError> {
    let owner = session_user(&app.store, &session).await?;
    app.store
        .revoke_invitation(owner.id, world_id, &token)
        .await?;
    Ok(Redirect::to(&format!("/world/{world_id}")))
}

async fn remove_member(
    State(app): State<App>,
    session: Session,
    Path((world_id, user_id)): Path<(i64, i64)>,
) -> Result<Redirect, WebError> {
    let owner = session_user(&app.store, &session).await?;
    app.store.remove_member(owner.id, world_id, user_id).await?;
    Ok(Redirect::to(&format!("/world/{world_id}")))
}

async fn create_player_character(
    State(app): State<App>,
    session: Session,
    Path(world_id): Path<i64>,
) -> Result<Redirect, WebError> {
    let user = session_user(&app.store, &session).await?;
    app.store.create_player_character(user.id, world_id).await?;
    Ok(Redirect::to(&format!("/world/{world_id}")))
}

async fn invitation_page(
    State(app): State<App>,
    session: Session,
    Path(token): Path<String>,
) -> Result<Html<String>, WebError> {
    let user = match session.get::<i64>(USER_ID).await? {
        Some(id) => app.store.user(id).await?,
        None => None,
    };
    Ok(Html(render_page(
        "Cairnworld invitation",
        false,
        move || view! { <InvitationPage token=token user=user/> },
    )))
}

async fn accept_invitation(
    State(app): State<App>,
    session: Session,
    Path(token): Path<String>,
) -> Result<Redirect, WebError> {
    let user = session_user(&app.store, &session).await?;
    let member = app.store.accept_invitation(&user, &token).await?;
    Ok(Redirect::to(&format!("/world/{}", member.world_id)))
}

async fn game_page(
    State(app): State<App>,
    session: Session,
    Path((world_id, character_id)): Path<(i64, i64)>,
) -> Result<Html<String>, WebError> {
    let member = active_player_character(&app.store, &session, world_id, character_id).await?;
    Ok(Html(match app.game.availability().await {
        GameAvailability::Loading => {
            render_page("Cairnworld", true, || view! { <GameLoadingPage/> })
        }
        GameAvailability::Ready(_) => {
            let history = app.store.player_chat(&member).await?;
            render_page(
                "Cairnworld",
                true,
                move || view! { <GamePage world_id=member.world_id character_id=member.character_id history=history/> },
            )
        }
        GameAvailability::Failed(error) => return Err(WebError::unavailable(error)),
    }))
}

async fn game_status(State(app): State<App>) -> Result<StatusCode, WebError> {
    match app.game.wait_until_ready().await {
        GameAvailability::Ready(_) => Ok(StatusCode::NO_CONTENT),
        GameAvailability::Failed(error) => Err(WebError::unavailable(error)),
        GameAvailability::Loading => unreachable!("GameLoad waits until it leaves Loading"),
    }
}

async fn game_socket(
    State(app): State<App>,
    session: Session,
    Path((world_id, character_id)): Path<(i64, i64)>,
    Query(cursor): Query<ChatCursor>,
    websocket: WebSocketUpgrade,
) -> Result<axum::response::Response, WebError> {
    let member = active_player_character(&app.store, &session, world_id, character_id).await?;
    tracing::info!(
        world_id,
        user_id = member.user_id,
        "player chat websocket requested"
    );
    let game = match app.game.availability().await {
        GameAvailability::Loading => {
            return Err(WebError::unavailable("game model is still loading"));
        }
        GameAvailability::Ready(game) => game,
        GameAvailability::Failed(error) => return Err(WebError::unavailable(error)),
    };
    Ok(websocket.on_upgrade(move |socket| play(socket, game, member, cursor.after_message_id)))
}

#[derive(Deserialize)]
struct ChatCursor {
    after_message_id: Option<i64>,
}

async fn active_player_character(
    store: &Store,
    session: &Session,
    world_id: i64,
    character_id: i64,
) -> std::result::Result<PlayerAgent, WebError> {
    let user_id = session_user(store, session).await?.id;
    store
        .active_player_character(user_id, world_id, character_id)
        .await
        .map_err(WebError::from)?
        .ok_or_else(|| {
            WebError::forbidden(format!(
                "user {user_id} has no active access to character {character_id} in world {world_id}"
            ))
        })
}

async fn session_user(
    store: &Store,
    session: &Session,
) -> std::result::Result<crate::store::User, WebError> {
    let user_id = session
        .get::<i64>(USER_ID)
        .await
        .context("loading browser session")
        .map_err(WebError::from)?
        .ok_or_else(|| WebError::unauthenticated("sign in before opening a world"))?;
    store
        .user(user_id)
        .await
        .map_err(WebError::from)?
        .ok_or_else(|| {
            WebError::unauthenticated(format!("browser session references missing user {user_id}"))
        })
}

async fn play(
    socket: WebSocket,
    game: Arc<Game<MistralRsBackend>>,
    member: PlayerAgent,
    after_message_id: Option<i64>,
) {
    if let Err(error) = play_connection(socket, game, member, after_message_id).await {
        tracing::error!(error = %format!("{error:#}"), "player chat websocket ended with an error");
    }
}

async fn play_connection(
    mut socket: WebSocket,
    game: Arc<Game<MistralRsBackend>>,
    member: PlayerAgent,
    after_message_id: Option<i64>,
) -> Result<()> {
    tracing::info!(
        world_id = member.world_id,
        user_id = member.user_id,
        "player chat websocket connected"
    );
    let mut broadcasts = match game.subscribe(&member).await {
        Ok(broadcasts) => broadcasts,
        Err(error) => {
            send_event(
                &mut socket,
                &ServerEvent::Error {
                    message: format!("{error:#}"),
                },
            )
            .await?;
            return Ok(());
        }
    };
    match game.opening_complete(&member).await {
        Ok(true) => {}
        Ok(false) => {
            send_event(
                &mut socket,
                &ServerEvent::Activity {
                    activity: ChatActivity::PreparingOpening,
                },
            )
            .await?
        }
        Err(error) => {
            send_event(
                &mut socket,
                &ServerEvent::Error {
                    message: format!("{error:#}"),
                },
            )
            .await?;
            return Ok(());
        }
    }
    match ensure_opening(&game, member.clone()).await {
        Ok(()) => {}
        Err(error) => {
            send_event(
                &mut socket,
                &ServerEvent::Error {
                    message: format!("{error:#}"),
                },
            )
            .await?;
            return Ok(());
        }
    }
    for entry in game.player_chat_after(&member, after_message_id).await? {
        let role = match entry.role {
            crate::llm::Role::User => ChatRole::User,
            crate::llm::Role::Assistant => ChatRole::Assistant,
            crate::llm::Role::Narration => ChatRole::Narration,
            crate::llm::Role::System | crate::llm::Role::Tool => continue,
        };
        send_event(
            &mut socket,
            &ServerEvent::Entry {
                role,
                text: entry.text,
            },
        )
        .await?;
    }
    send_event(&mut socket, &ServerEvent::CanAct { value: true }).await?;
    tracing::info!(
        world_id = member.world_id,
        user_id = member.user_id,
        "player chat is ready for input"
    );
    loop {
        tokio::select! {
            incoming = socket.recv() => {
                let Some(incoming) = incoming else {
                    return Ok(());
                };
                let WebSocketMessage::Text(text) = incoming.context("receiving browser websocket message")? else {
                    continue;
                };
                let ClientEvent::Message { text } = match serde_json::from_str(&text) {
                    Ok(event) => event,
                    Err(error) => {
                        send_event(&mut socket, &ServerEvent::Error {
                            message: format!("invalid browser chat message: {error}"),
                        }).await?;
                        continue;
                    }
                };
                if text.trim().is_empty() {
                    send_event(&mut socket, &ServerEvent::Error {
                        message: "browser chat message cannot be empty".into(),
                    }).await?;
                    continue;
                }
                send_event(&mut socket, &ServerEvent::Activity {
                    activity: ChatActivity::Responding,
                }).await?;
                send_event(&mut socket, &ServerEvent::CanAct { value: false }).await?;
                let reply = match game.player_message(member.clone(), &text).await {
                    Ok(response) => match response.content {
                        Content::Text(text) => text,
                        Content::ToolCalls(_) => "The player agent did not finish its turn.".to_string(),
                    },
                    Err(error) => {
                        send_ready_broadcasts(&mut socket, &mut broadcasts).await?;
                        send_event(&mut socket, &ServerEvent::Error { message: format!("{error:#}") }).await?;
                        send_event(&mut socket, &ServerEvent::CanAct { value: true }).await?;
                        continue;
                    }
                };
                send_ready_broadcasts(&mut socket, &mut broadcasts).await?;
                if !reply.is_empty() {
                send_event(&mut socket, &ServerEvent::Entry { role: ChatRole::Assistant, text: reply }).await?;
                }
                send_event(&mut socket, &ServerEvent::CanAct { value: true }).await?;
            }
            narration = broadcasts.recv() => match narration {
                Ok(narration) => {
                    send_event(&mut socket, &ServerEvent::Entry { role: ChatRole::Narration, text: narration }).await?;
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                    send_event(&mut socket, &ServerEvent::Error {
                        message: format!("{skipped} earlier live messages were missed; reload the page to recover the stored history."),
                    }).await?;
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => return Ok(()),
            }
        }
    }
}

/// Wait for the one stored opening turn, if this blank Adventurer has not
/// already received it. A reconnect is only a viewer and never creates a
/// second game event.
async fn ensure_opening<B>(game: &Arc<Game<B>>, member: PlayerAgent) -> Result<()>
where
    B: Backend + Send + Sync + 'static,
{
    tracing::info!(
        world_id = member.world_id,
        user_id = member.user_id,
        "waiting for server-owned player agent opening turn"
    );
    game.wait_for_opening(member).await
}

async fn send_ready_broadcasts(
    socket: &mut WebSocket,
    broadcasts: &mut tokio::sync::broadcast::Receiver<String>,
) -> Result<()> {
    loop {
        match broadcasts.try_recv() {
            Ok(narration) => {
                send_event(
                    socket,
                    &ServerEvent::Entry {
                        role: ChatRole::Narration,
                        text: narration,
                    },
                )
                .await?
            }
            Err(tokio::sync::broadcast::error::TryRecvError::Empty) => return Ok(()),
            Err(tokio::sync::broadcast::error::TryRecvError::Lagged(skipped)) => {
                send_event(socket, &ServerEvent::Error {
                    message: format!("{skipped} earlier live messages were missed; reload the page to recover the stored history."),
                }).await?;
            }
            Err(tokio::sync::broadcast::error::TryRecvError::Closed) => return Ok(()),
        }
    }
}

async fn send_event(socket: &mut WebSocket, event: &ServerEvent) -> Result<()> {
    if let ServerEvent::Error { message } = event {
        tracing::error!(error = message, "reporting player chat failure to browser");
    }
    let event = serde_json::to_string(event).context("encoding browser websocket event")?;
    socket
        .send(WebSocketMessage::Text(event.into()))
        .await
        .context("sending browser websocket event")
}

#[component]
fn Page(title: &'static str, hydrate: bool, children: Children) -> impl IntoView {
    let hydration = hydrate.then(hydration_options);
    view! {
        <html lang="en" data-theme="forest">
            <head>
                <meta charset="utf-8"/>
                <meta name="viewport" content="width=device-width, initial-scale=1"/>
                <title>{title}</title>
                <link rel="stylesheet" href="/pkg/cairnworld.css"/>
                {move || hydration.clone().map(|options| view! { <HydrationScripts options islands=true/> })}
            </head>
            <body>
                {move || hydrate.then(|| view! {
                    <noscript>
                        <p class="alert alert-error m-4">
                            "Cairnworld's live chat requires JavaScript. Enable it and reload this page."
                        </p>
                    </noscript>
                })}
                {children()}
            </body>
        </html>
    }
}

fn hydration_options() -> LeptosOptions {
    LeptosOptions::builder()
        .output_name("cairnworld")
        .site_root("target/site")
        .site_pkg_dir("pkg")
        .build()
}

fn render_page<IV: IntoView + 'static>(
    title: &'static str,
    hydrate: bool,
    content: impl FnOnce() -> IV + Send + 'static,
) -> String {
    let owner = Owner::new_root(Some(Arc::new(SsrSharedContext::new())));
    let content = owner.with(|| view! { <Page title hydrate>{content()}</Page> }.to_html());
    format!("<!doctype html>{}", content)
}

#[component]
fn Landing(user: Option<crate::store::User>, worlds: Vec<World>) -> impl IntoView {
    let account = user.map(|user| user.display_name);
    view! {
        <main class="min-h-dvh bg-base-200 px-4 py-8 sm:px-8 lg:px-12">
            <section class="hero mx-auto max-w-6xl overflow-hidden rounded-box bg-base-100 shadow-2xl">
                <div class="hero-content grid gap-8 p-0 lg:grid-cols-2">
                    <img class="h-full min-h-64 w-full object-cover" src="/media/banner.png" alt="Cairnworld adventurers"/>
                    <div class="p-6 sm:p-10">
                        <h1 class="text-4xl font-bold sm:text-5xl">"Cairnworld"</h1>
                        <p class="py-5 text-lg text-base-content/80">"A small shared RPG world, played by chatting."</p>
            {match account {
                Some(name) => view! {
                    <p class="mb-5">{format!("Signed in as {name}.")}</p>
                    <form class="join mb-5" action="/profile" method="post">
                        <input class="input join-item w-full" name="display_name" value=name.clone() aria-label="Display name"/>
                        <button class="btn join-item" type="submit">"Save name"</button>
                    </form>
                    <form class="mb-5" action="/worlds" method="post"><button class="btn btn-primary" type="submit">"Create Bread Thief world"</button></form>
                    <ul class="menu mb-5 rounded-box bg-base-200">{worlds.into_iter().map(|world| view! {
                        <li><a href=format!("/world/{}", world.id)>{world.name}</a></li>
                    }).collect_view()}</ul>
                    <form action="/logout" method="post"><button class="btn btn-ghost btn-sm" type="submit">"Log out"</button></form>
                }.into_any(),
                None => view! { <a class="btn btn-primary" href="/auth/google">"Log in with Google"</a> }.into_any(),
            }}
                    </div>
                </div>
            </section>
        </main>
    }
}

#[component]
fn WorldDetail(
    world: World,
    members: Vec<WorldMember>,
    invitations: Vec<crate::store::Invitation>,
    viewer_id: i64,
) -> impl IntoView {
    let is_owner = world.owner_id == viewer_id;
    let world_id = world.id;
    let world_path = format!("/world/{world_id}");
    let member_path = format!("{world_path}/members");
    let invitation_path = format!("{world_path}/invitations");
    view! {
        <main class="min-h-dvh bg-base-200 p-4 sm:p-8">
            <section class="card mx-auto max-w-5xl bg-base-100 shadow-xl"><div class="card-body gap-6">
            <div class="flex flex-wrap items-center justify-between gap-4"><h1 class="card-title text-3xl">{world.name}</h1></div>
            <section><h2 class="mb-3 text-xl font-semibold">"Players"</h2>
            <ul class="space-y-2">{members.into_iter().map(move |member| {
                let is_active = member.access == "active";
                let remove = is_owner && member.user_id != viewer_id && is_active;
                let enter = member.user_id == viewer_id && is_active;
                let create = member.user_id == viewer_id && is_active;
                let player = format!("{} ({})", member.display_name, member.access);
                let remove_path = format!("{member_path}/{}", member.user_id);
                view! { <li class=WORLD_DETAIL_ROW_CLASSES>
                    <div class="w-full flex flex-wrap items-center justify-between gap-3">
                        <span>{player}</span>
                        {create.then(|| view! { <form action=format!("{world_path}/characters") method="post"><button class="btn btn-sm" type="submit">"Create character"</button></form> })}
                        {remove.then(|| view! { <form action=remove_path method="post"><button class="btn btn-error btn-sm" type="submit">"Remove"</button></form> })}
                    </div>
                    <ul class="w-full space-y-2 pl-6">{member.characters.into_iter().map({
                        let world_path = world_path.clone();
                        move |character| {
                        let enter_path = format!("{world_path}/characters/{}/play", character.id);
                        view! { <li class="flex flex-wrap items-center justify-between gap-3">
                            <span>{character.name}</span>
                            {enter.then(|| view! { <a class="btn btn-primary btn-sm" href=enter_path>"Enter world"</a> })}
                        </li> }}
                    }).collect_view()}</ul>
                </li> }
            }).collect_view()}</ul></section>
            {is_owner.then(|| view! {
                <section class="border-t border-base-300 pt-6">
                    <h2 class="mb-3 text-xl font-semibold">"Invitation links"</h2>
                    <form class="join mb-4" action=invitation_path.clone() method="post">
                        <input class="input join-item w-full" name="max_uses" type="number" min="1" placeholder="Maximum uses (optional)"/>
                        <button class="btn join-item" type="submit">"Create link"</button>
                    </form>
                    <ul class="space-y-2">{invitations.into_iter().map(move |invitation| {
                        let public_invitation_path = format!("/invite/{}", invitation.token);
                        let public_invitation_href = public_invitation_path.clone();
                        let revoke_path = format!("{invitation_path}/{}/revoke", invitation.token);
                        view! {
                        <li class=WORLD_DETAIL_ROW_CLASSES><a class="link link-primary break-all" href=public_invitation_href>{public_invitation_path}</a>
                        <span class="badge badge-ghost">{invitation.max_uses.map(|max| (max - invitation.uses).to_string()).unwrap_or_else(|| "unlimited".to_string())}</span>
                        <form action=revoke_path method="post"><button class="btn btn-error btn-sm" type="submit">"Revoke"</button></form></li>
                    }}).collect_view()}</ul>
                </section>
            })}
            </div></section>
        </main>
    }
}

#[component]
fn InvitationPage(token: String, user: Option<crate::store::User>) -> impl IntoView {
    view! { <main class="min-h-dvh bg-base-200 p-4 sm:p-8"><section class="card mx-auto max-w-xl bg-base-100 shadow-xl"><div class="card-body">
        <h1 class="card-title text-3xl">"Cairnworld invitation"</h1>
        {match user {
            Some(user) => view! { <p>{format!("Signed in as {}.", user.display_name)}</p><form action=format!("/invite/{token}") method="post"><button class="btn btn-primary" type="submit">"Join world"</button></form> }.into_any(),
            None => view! { <a class="btn btn-primary" href="/auth/google">"Log in with Google to join"</a> }.into_any(),
        }}
    </div></section></main> }
}

#[component]
fn GamePage(world_id: i64, character_id: i64, history: Vec<PlayerChatEntry>) -> impl IntoView {
    let after_message_id = history.last().map(|entry| entry.id);
    let history = player_chat_entries(history);
    view! {
        <main class="h-dvh bg-base-200 p-3 sm:p-6">
            <section class="mx-auto flex h-[calc(100dvh-1.5rem)] max-w-5xl flex-col rounded-box bg-base-100 shadow-xl sm:h-[calc(100dvh-3rem)]">
                <header class="navbar border-b border-base-300 px-4"><h1 class="text-xl font-semibold">"Cairnworld"</h1><span class="ml-auto badge badge-primary badge-outline">"Adventure chat"</span></header>
                <div class="flex min-h-0 flex-1 flex-col p-3 sm:p-5"><PlayerChat world_id character_id after_message_id><ChatTranscript history/></PlayerChat></div>
            </section>
        </main>
    }
}

fn player_chat_entries(history: Vec<PlayerChatEntry>) -> Vec<ChatEntry> {
    history
        .into_iter()
        .map(|entry| ChatEntry {
            role: match entry.role {
                crate::llm::Role::User => ChatRole::User,
                crate::llm::Role::Assistant => ChatRole::Assistant,
                crate::llm::Role::Narration => ChatRole::Narration,
                crate::llm::Role::System | crate::llm::Role::Tool => {
                    unreachable!("player chat entries are user, assistant, or narration")
                }
            },
            text: entry.text,
        })
        .collect()
}

#[component]
fn GameLoadingPage() -> impl IntoView {
    view! { <GameLoading/> }
}

struct WebError {
    status: StatusCode,
    error: anyhow::Error,
}

impl WebError {
    fn bad_request(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            error: anyhow::anyhow!(message.into()),
        }
    }

    fn unauthenticated(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::UNAUTHORIZED,
            error: anyhow::anyhow!(message.into()),
        }
    }

    fn forbidden(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::FORBIDDEN,
            error: anyhow::anyhow!(message.into()),
        }
    }

    fn unavailable(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::SERVICE_UNAVAILABLE,
            error: anyhow::anyhow!(message.into()),
        }
    }
}

impl<E> From<E> for WebError
where
    E: Into<anyhow::Error>,
{
    fn from(error: E) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            error: error.into(),
        }
    }
}

impl IntoResponse for WebError {
    fn into_response(self) -> axum::response::Response {
        tracing::error!(status = %self.status, error = %format!("{:#}", self.error), "web request failed");
        (self.status, format!("{:#}", self.error)).into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn game_page_renders_stored_chat_entries_with_their_visible_roles() {
        let html = render_page("Cairnworld", true, || {
            view! { <GamePage world_id=7 character_id=8 history=vec![
                PlayerChatEntry { id: 1, role: crate::llm::Role::User, text: "I look around.".into() },
                PlayerChatEntry { id: 2, role: crate::llm::Role::Narration, text: "Toma watches.".into() },
            ]/> }
        });
        assert!(html.contains("I look around."));
        assert!(html.contains("Toma watches."));
        assert!(html.contains("data-role=\"user\""));
        assert!(html.contains("data-role=\"narration\""));
    }

    #[test]
    fn world_detail_gives_each_active_viewer_their_own_entry_link() {
        fn detail(viewer_id: i64) -> String {
            render_page("Cairnworld", false, move || {
                view! { <WorldDetail
                    world=World { id: 7, name: "Bread Thief".into(), owner_id: 1 }
                    members=vec![
                        WorldMember { user_id: 1, display_name: "Owner".into(), access: "active".into(), characters: vec![crate::store::WorldCharacter { id: 8, name: "Rook".into() }, crate::store::WorldCharacter { id: 11, name: "Lark".into() }] },
                        WorldMember { user_id: 2, display_name: "Invitee".into(), access: "active".into(), characters: vec![crate::store::WorldCharacter { id: 9, name: "Moth".into() }] },
                        WorldMember { user_id: 3, display_name: "Removed".into(), access: "removed".into(), characters: vec![crate::store::WorldCharacter { id: 10, name: "Ash".into() }] },
                    ]
                    invitations=vec![crate::store::Invitation { token: "invite".into(), world_id: 7, max_uses: Some(2), uses: 1 }]
                    viewer_id
                /> }
            })
        }

        let owner = detail(1);
        let invitee = detail(2);
        for html in [&owner, &invitee] {
            assert!(html.contains("Owner (active)"));
            assert!(html.contains("Invitee (active)"));
            assert!(html.contains("Removed (removed)"));
            assert_eq!(
                html.matches("href=\"/world/7/characters/").count(),
                if html == &owner { 2 } else { 1 }
            );
        }
        let owner_link = owner.find("href=\"/world/7/characters/8/play\"").unwrap();
        assert!(owner.find("Rook").unwrap() < owner_link);
        assert!(owner_link < owner.find("Moth").unwrap());
        assert!(owner.contains("href=\"/world/7/characters/11/play\""));
        assert!(owner.contains("action=\"/world/7/characters\""));
        let invitee_link = invitee.find("href=\"/world/7/characters/9/play\"").unwrap();
        assert!(invitee.find("Moth").unwrap() < invitee_link);
        assert!(invitee_link < invitee.find("Ash").unwrap());
        assert!(owner.contains("action=\"/world/7/members/2\""));
        assert!(owner.contains("action=\"/world/7/invitations\""));
        assert!(owner.contains("action=\"/world/7/invitations/invite/revoke\""));
    }

    #[test]
    fn hydrated_pages_load_the_wasm_file_emitted_by_cargo_leptos() {
        let html = render_page("Cairnworld", true, || view! { <GameLoadingPage/> });
        assert!(html.contains("/pkg/cairnworld.wasm"));
        assert!(!html.contains("/pkg/cairnworld_bg.wasm"));
    }

    #[test]
    fn hydrated_page_explains_when_the_live_chat_cannot_start_without_javascript() {
        let html = render_page("Cairnworld", true, || view! { <GameLoadingPage/> });
        assert!(html.contains("Cairnworld's live chat requires JavaScript"));
    }

    #[test]
    fn socket_protocol_distinguishes_narration_from_agent_replies() {
        let event = ServerEvent::Entry {
            role: ChatRole::Narration,
            text: "Toma watches.".into(),
        };
        assert_eq!(
            serde_json::to_value(event).unwrap(),
            serde_json::json!({
                "type": "entry",
                "role": "narration",
                "text": "Toma watches.",
            })
        );
        let client: ClientEvent =
            serde_json::from_str(r#"{"type":"message","text":"I take the flour sack."}"#).unwrap();
        assert!(
            matches!(client, ClientEvent::Message { text } if text == "I take the flour sack.")
        );
    }

    #[test]
    fn socket_protocol_reports_server_work_separately_from_chat_entries() {
        let event = ServerEvent::Activity {
            activity: ChatActivity::Responding,
        };
        assert_eq!(
            serde_json::to_value(event).unwrap(),
            serde_json::json!({
                "type": "activity",
                "activity": "responding",
            })
        );
    }
}
