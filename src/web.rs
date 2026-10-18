use std::{net::SocketAddr, sync::Arc};

use anyhow::{Context, Result};
use axum::{
    Json, Router,
    extract::{
        Form, Path, Query, State,
        ws::{Message as WebSocketMessage, WebSocket, WebSocketUpgrade},
    },
    http::{HeaderMap, StatusCode, header::HOST},
    response::{Html, IntoResponse, Redirect},
    routing::{get, post},
};
use cairnworld::ui::{
    ChatActivity, ChatEntry, ChatRole, ChatTranscript, ClientEvent, DeveloperModeEnableButton,
    GameLoading, PlayerChat, ServerEvent,
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
    inspection::{ObjectType, RawHistoryPageResponse, WorldObjectCursor},
    llm::Backend,
    mistralrs_backend::MistralRsBackend,
    scenario::Scenario,
    settings,
    store::{PlayerAgent, PlayerChatEntry, Store, World, WorldMember, WorldObjectsQuery},
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

/// The complete route table, shared by the real server and route tests so
/// both exercise the identical handlers and state.
fn router(app: App, sessions: SqliteStore, secure_cookies: bool) -> Router {
    Router::new()
        .route("/", get(landing))
        .route("/auth/google", get(begin_login))
        .route("/auth/google/callback", get(finish_login))
        .route("/game-status", get(game_status))
        .route("/logout", post(logout))
        .route("/profile", post(update_display_name))
        .route("/worlds", post(create_bread_thief_world))
        .route("/world/{world_id}", get(world_detail))
        .route("/world/{world_id}/developer-mode", post(set_developer_mode))
        .route(
            "/world/{world_id}/inspect/sequences",
            get(inspect_sequences),
        )
        .route(
            "/world/{world_id}/inspect/sequences/{sequence_id}",
            get(inspect_sequence),
        )
        .route(
            "/world/{world_id}/inspect/inferences/{inference_id}",
            get(inspect_inference),
        )
        .route("/world/{world_id}/inspect/agents", get(inspect_agents))
        .route(
            "/world/{world_id}/inspect/agents/{agent_id}/history",
            get(inspect_agent_history),
        )
        .route(
            "/world/{world_id}/inspect/agents/{agent_id}/branches",
            get(inspect_agent_branches),
        )
        .route("/world/{world_id}/inspect/objects", get(inspect_objects))
        .route("/world/{world_id}/inspect/events", get(inspect_events))
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
        .with_state(app)
        .layer(
            SessionManagerLayer::new(sessions)
                .with_secure(secure_cookies)
                // Google returns through a top-level cross-site GET, for which an
                // OAuth login session must use Lax rather than the library's Strict default.
                .with_same_site(SameSite::Lax)
                .with_expiry(Expiry::OnInactivity(Duration::days(30))),
        )
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
    let secure_cookies = config.google_redirect_url.starts_with("https://");
    let app = router(
        App {
            store,
            google,
            game,
        },
        sessions,
        secure_cookies,
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
        true,
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

#[derive(Deserialize)]
struct DeveloperModeForm {
    enabled: bool,
}

/// Owner-only toggle for the world's inspection views. Recording is unaffected.
async fn set_developer_mode(
    State(app): State<App>,
    session: Session,
    Path(world_id): Path<i64>,
    Form(form): Form<DeveloperModeForm>,
) -> Result<Redirect, WebError> {
    let owner = session_user(&app.store, &session).await?;
    app.store
        .set_developer_mode(owner.id, world_id, form.enabled)
        .await?;
    Ok(Redirect::to(&format!("/world/{world_id}")))
}

#[derive(Deserialize)]
struct InspectPage {
    before: Option<i64>,
    limit: Option<i64>,
}

impl InspectPage {
    fn limit(&self) -> i64 {
        self.limit.unwrap_or(50).clamp(1, 200)
    }
}

/// Raw history paging uses an opaque cursor (`store::RawHistoryCursor`), not
/// a bare id, since a page boundary must name the exact branch/position to
/// resume from rather than a single history-entry id.
#[derive(Deserialize)]
struct RawHistoryPage {
    /// Which of the agent's branches the *first* page pages - required for a
    /// world_gm agent (see `gm_branch_graph`), ignored once `after` names a
    /// page to resume, which already carries its own branch.
    branch: Option<i64>,
    entry: Option<i64>,
    /// JSON-encoded `RawHistoryCursor`, as returned by a previous page.
    after: Option<String>,
    limit: Option<i64>,
}

/// Stable filtering and paging for the current-world object index. `type` and
/// `id` are also the direct-object URL fields, so a relationship link fetches
/// its precise target rather than relying on the browser's currently loaded
/// page.
#[derive(Deserialize)]
struct WorldObjectsPage {
    #[serde(rename = "type")]
    object_type: Option<String>,
    id: Option<i64>,
    search: Option<String>,
    after: Option<String>,
    limit: Option<i64>,
}

impl WorldObjectsPage {
    fn object_type(&self) -> Result<Option<ObjectType>, WebError> {
        self.object_type
            .as_deref()
            .map(|value| {
                ObjectType::parse(value).ok_or_else(|| {
                    WebError::bad_request(format!("unknown world object type {value:?}"))
                })
            })
            .transpose()
    }

    fn after(&self) -> Result<Option<WorldObjectCursor>, WebError> {
        self.after
            .as_deref()
            .map(|cursor| {
                serde_json::from_str(cursor).map_err(|error| {
                    WebError::bad_request(format!("invalid world object cursor: {error}"))
                })
            })
            .transpose()
    }

    fn limit(&self) -> i64 {
        self.limit.unwrap_or(50).clamp(1, 200)
    }
}

impl RawHistoryPage {
    fn limit(&self) -> i64 {
        self.limit.unwrap_or(50).clamp(1, 200)
    }

    fn after(&self) -> Result<Option<crate::store::RawHistoryCursor>, WebError> {
        self.after
            .as_deref()
            .map(|cursor| {
                serde_json::from_str(cursor).map_err(|error| {
                    WebError::bad_request(format!("invalid raw history cursor: {error}"))
                })
            })
            .transpose()
    }
}

/// Every inspection route resolves the signed-in viewer, then defers the
/// active-membership-and-enabled-mode check to the store query itself.
async fn inspect_sequences(
    State(app): State<App>,
    session: Session,
    Path(world_id): Path<i64>,
    Query(page): Query<InspectPage>,
) -> Result<impl IntoResponse, WebError> {
    let viewer = session_user(&app.store, &session).await?;
    Ok(Json(
        app.store
            .world_sequences(viewer.id, world_id, page.before, page.limit())
            .await?,
    ))
}

async fn inspect_sequence(
    State(app): State<App>,
    session: Session,
    Path((world_id, sequence_id)): Path<(i64, i64)>,
) -> Result<impl IntoResponse, WebError> {
    let viewer = session_user(&app.store, &session).await?;
    Ok(Json(
        app.store
            .sequence_tree(viewer.id, world_id, sequence_id)
            .await?,
    ))
}

async fn inspect_inference(
    State(app): State<App>,
    session: Session,
    Path((world_id, inference_id)): Path<(i64, i64)>,
) -> Result<impl IntoResponse, WebError> {
    let viewer = session_user(&app.store, &session).await?;
    Ok(Json(
        app.store
            .inference_evidence(viewer.id, world_id, inference_id)
            .await?,
    ))
}

async fn inspect_agents(
    State(app): State<App>,
    session: Session,
    Path(world_id): Path<i64>,
) -> Result<impl IntoResponse, WebError> {
    let viewer = session_user(&app.store, &session).await?;
    Ok(Json(app.store.world_agents(viewer.id, world_id).await?))
}

async fn inspect_agent_history(
    State(app): State<App>,
    session: Session,
    Path((world_id, agent_id)): Path<(i64, i64)>,
    Query(page): Query<RawHistoryPage>,
) -> Result<impl IntoResponse, WebError> {
    let viewer = session_user(&app.store, &session).await?;
    let cursor = match page.after()? {
        Some(cursor) => {
            app.store
                .resume_raw_history_cursor(viewer.id, world_id, agent_id, cursor)
                .await?
        }
        None => match (page.branch, page.entry) {
            (Some(branch_id), Some(entry_id)) => {
                app.store
                    .raw_history_cursor_at(viewer.id, world_id, agent_id, branch_id, entry_id)
                    .await?
            }
            (_, Some(_)) => {
                return Err(WebError::bad_request(
                    "a raw history entry needs its branch",
                ));
            }
            (branch_id, None) => {
                app.store
                    .first_raw_history_cursor(viewer.id, world_id, agent_id, branch_id)
                    .await?
            }
        },
    };
    let (entries, cursor) = app
        .store
        .agent_raw_history(viewer.id, cursor, page.limit())
        .await?;
    Ok(Json(RawHistoryPageResponse { entries, cursor }))
}

/// The whole branch DAG an agent's history can form (a world_gm agent, once
/// groups have split/merged) or its trivial one-branch form (every other
/// agent kind) - fetched once, separate from any branch's own paged entries,
/// so the browser can render every branch as its own linked section.
async fn inspect_agent_branches(
    State(app): State<App>,
    session: Session,
    Path((world_id, agent_id)): Path<(i64, i64)>,
) -> Result<impl IntoResponse, WebError> {
    let viewer = session_user(&app.store, &session).await?;
    Ok(Json(
        app.store
            .gm_branch_graph(viewer.id, world_id, agent_id)
            .await?,
    ))
}

async fn inspect_objects(
    State(app): State<App>,
    session: Session,
    Path(world_id): Path<i64>,
    Query(page): Query<WorldObjectsPage>,
) -> Result<impl IntoResponse, WebError> {
    let viewer = session_user(&app.store, &session).await?;
    let object_type = page.object_type()?;
    let after = page.after()?;
    let limit = page.limit();
    Ok(Json(
        app.store
            .world_objects(
                viewer.id,
                world_id,
                WorldObjectsQuery {
                    object_type,
                    object_id: page.id,
                    search: page.search,
                    after,
                    limit,
                },
            )
            .await?,
    ))
}

/// Live Developer Mode events for one world: inference lifecycle, streamed
/// tokens, change wake-ups and the developer_mode toggle itself, so an
/// already-open game page updates without a reload. Read-only from the
/// browser's side - unlike the player chat socket, this never accepts an
/// incoming message.
///
/// Subscribes before running the initial access query (design.md's Live
/// inspection protocol): a membership/mode change racing the subscribe can
/// then never fall in the gap between "checked access" and "started
/// listening" for this connection.
async fn inspect_events(
    State(app): State<App>,
    session: Session,
    Path(world_id): Path<i64>,
    websocket: WebSocketUpgrade,
) -> Result<axum::response::Response, WebError> {
    let viewer = session_user(&app.store, &session).await?;
    let events = app.store.subscribe_dev_events(world_id);
    let access = app.store.access_for(viewer.id, world_id).await?;
    Ok(websocket.on_upgrade(move |socket| {
        dev_events_connection(socket, events, app.store, viewer.id, world_id, access)
    }))
}

/// Re-runs `Store::access_for` before every private outbound frame - the one
/// indexed query HTTP inspection routes also use - rather than trusting any
/// cached mode/membership flag. `AccessChanged`/`DeveloperModeChanged` are
/// wake-ups only; the recheck they trigger is the actual authorization
/// decision, per design.md's Live inspection protocol. A plain query then a
/// plain `socket.send` - never a transaction or lock held across the send.
async fn dev_events_connection(
    mut socket: WebSocket,
    mut events: tokio::sync::broadcast::Receiver<crate::store::DevEvent>,
    store: Store,
    viewer_id: i64,
    world_id: i64,
    initial_access: crate::store::Access,
) {
    let mut developer_mode = initial_access.developer_mode;
    if !initial_access.active_member {
        return;
    }
    loop {
        // `recheck` is `None` once the connection must close (removed
        // membership or a failed access query - the latter terminates
        // visibly via the logged error rather than silently); `Some(extra)`
        // carries whichever event, if any, this iteration should still send
        // after the mode toggle itself, now that access is current again.
        let recheck = async |socket: &mut WebSocket,
                             developer_mode: &mut bool,
                             extra: Option<crate::store::DevEvent>,
                             context: &str|
               -> Result<bool> {
            let access = store
                .access_for(viewer_id, world_id)
                .await
                .inspect_err(|error| {
                    tracing::error!(
                        viewer_id, world_id, error = %format!("{error:#}"),
                        "terminating live inspection: access check failed {context}"
                    );
                })?;
            if !access.active_member {
                return Ok(false);
            }
            let mode_changed = *developer_mode != access.developer_mode;
            *developer_mode = access.developer_mode;
            if mode_changed {
                send_dev_event(
                    socket,
                    crate::store::DevEvent::DeveloperModeChanged {
                        enabled: *developer_mode,
                    },
                )
                .await?;
            }
            if !*developer_mode {
                return Ok(true);
            }
            if let Some(extra) = extra {
                send_dev_event(socket, extra).await?;
            }
            Ok(true)
        };
        let result = match events.recv().await {
            Ok(event) => {
                // A dropped AccessChanged cannot authorize later private
                // frames: this recheck, not the received event, is what
                // actually authorizes forwarding it.
                let extra = (!matches!(
                    event,
                    crate::store::DevEvent::DeveloperModeChanged { .. }
                        | crate::store::DevEvent::AccessChanged
                ))
                .then_some(event);
                recheck(&mut socket, &mut developer_mode, extra, "after an event").await
            }
            Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                recheck(
                    &mut socket,
                    &mut developer_mode,
                    Some(crate::store::DevEvent::ResyncRequired { skipped }),
                    "after broadcast lag",
                )
                .await
            }
            Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
        };
        match result {
            Ok(true) => {}
            Ok(false) => return,
            Err(error) => {
                tracing::error!(viewer_id, world_id, error = %error, "terminating live inspection");
                return;
            }
        }
    }
}

async fn send_dev_event(socket: &mut WebSocket, event: crate::store::DevEvent) -> Result<()> {
    let text = serde_json::to_string(&event).context("encoding live inspection event")?;
    socket
        .send(WebSocketMessage::Text(text.into()))
        .await
        .context("sending live inspection event")
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
            let developer_mode = app
                .store
                .world(world_id)
                .await?
                .with_context(|| format!("active membership references missing world {world_id}"))?
                .developer_mode;
            render_page(
                "Cairnworld",
                true,
                move || view! { <GamePage world_id=member.world_id character_id=member.character_id history=history developer_mode/> },
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
    // A connection is registered after authentication and deregistered on
    // every exit including error/ban/logout or transport error (design.md's
    // Membership and presence). The whole remaining body runs inside this
    // block so every one of its early `return`/`?` exits still reaches the
    // single disconnect call below - matching agent.rs's InvocationGuard
    // pattern for a guaranteed exit action without async work in Drop.
    let connection_id = game.connect(&member).await?;
    let result = play_connected(&mut socket, &game, &member, after_message_id).await;
    game.disconnect(&member, connection_id)
        .await
        .context("releasing player chat connection")?;
    result
}

async fn play_connected(
    socket: &mut WebSocket,
    game: &Arc<Game<MistralRsBackend>>,
    member: &PlayerAgent,
    after_message_id: Option<i64>,
) -> Result<()> {
    let mut broadcasts = match game.subscribe(member).await {
        Ok(broadcasts) => broadcasts,
        Err(error) => {
            send_event(
                socket,
                &ServerEvent::Error {
                    message: format!("{error:#}"),
                },
            )
            .await?;
            return Ok(());
        }
    };
    match game.opening_complete(member).await {
        Ok(true) => {}
        Ok(false) => {
            send_event(
                socket,
                &ServerEvent::Activity {
                    activity: ChatActivity::PreparingOpening,
                },
            )
            .await?
        }
        Err(error) => {
            send_event(
                socket,
                &ServerEvent::Error {
                    message: format!("{error:#}"),
                },
            )
            .await?;
            return Ok(());
        }
    }
    match ensure_opening(game, member.clone()).await {
        Ok(()) => {}
        Err(error) => {
            send_event(
                socket,
                &ServerEvent::Error {
                    message: format!("{error:#}"),
                },
            )
            .await?;
            return Ok(());
        }
    }
    let mut cursor = after_message_id;
    cursor = send_new_entries(socket, game, member, cursor).await?;
    send_event(socket, &ServerEvent::CanAct { value: true }).await?;
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
                        send_event(socket, &ServerEvent::Error {
                            message: format!("invalid browser chat message: {error}"),
                        }).await?;
                        continue;
                    }
                };
                if text.trim().is_empty() {
                    send_event(socket, &ServerEvent::Error {
                        message: "browser chat message cannot be empty".into(),
                    }).await?;
                    continue;
                }
                send_event(socket, &ServerEvent::Activity {
                    activity: ChatActivity::Responding,
                }).await?;
                send_event(socket, &ServerEvent::CanAct { value: false }).await?;
                let result = game.player_message(member.clone(), &text).await;
                cursor = send_ready_broadcasts(socket, game, member, &mut broadcasts, cursor).await?;
                if let Err(error) = result {
                    send_event(socket, &ServerEvent::Error { message: format!("{error:#}") }).await?;
                    send_event(socket, &ServerEvent::CanAct { value: true }).await?;
                    continue;
                }
                cursor = send_new_entries(socket, game, member, cursor).await?;
                send_event(socket, &ServerEvent::CanAct { value: true }).await?;
            }
            narration = broadcasts.recv() => match narration {
                Ok(_) => {
                    cursor = send_new_entries(socket, game, member, cursor).await?;
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                    send_event(socket, &ServerEvent::Error {
                        message: format!("{skipped} earlier live messages were missed; reload the page to recover the stored history."),
                    }).await?;
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => return Ok(()),
            }
        }
    }
}

/// Forwards every stored chat entry after `cursor` as a real `ServerEvent::Entry`
/// (its durable id and inspection sequence id included, unlike a raw broadcast
/// string), and returns the new cursor. Reuses the same query SSR and page
/// reconnect use, so a live turn is inspectable immediately, not only after a
/// reload re-resolves it.
async fn send_new_entries(
    socket: &mut WebSocket,
    game: &Arc<Game<MistralRsBackend>>,
    member: &PlayerAgent,
    cursor: Option<i64>,
) -> Result<Option<i64>> {
    let mut cursor = cursor;
    for entry in game.player_chat_after(member, cursor).await? {
        cursor = Some(entry.id);
        let role = match entry.role {
            crate::llm::Role::User => ChatRole::User,
            crate::llm::Role::Assistant => ChatRole::Assistant,
            crate::llm::Role::Narration => ChatRole::Narration,
            crate::llm::Role::System | crate::llm::Role::Tool => continue,
        };
        send_event(
            socket,
            &ServerEvent::Entry {
                role,
                text: entry.text,
                id: Some(entry.id),
                sequence_id: entry.sequence_id,
            },
        )
        .await?;
    }
    Ok(cursor)
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
    game: &Arc<Game<MistralRsBackend>>,
    member: &PlayerAgent,
    broadcasts: &mut tokio::sync::broadcast::Receiver<String>,
    cursor: Option<i64>,
) -> Result<Option<i64>> {
    let mut cursor = cursor;
    loop {
        match broadcasts.try_recv() {
            Ok(_) => cursor = send_new_entries(socket, game, member, cursor).await?,
            Err(tokio::sync::broadcast::error::TryRecvError::Empty) => return Ok(cursor),
            Err(tokio::sync::broadcast::error::TryRecvError::Lagged(skipped)) => {
                send_event(socket, &ServerEvent::Error {
                    message: format!("{skipped} earlier live messages were missed; reload the page to recover the stored history."),
                }).await?;
            }
            Err(tokio::sync::broadcast::error::TryRecvError::Closed) => return Ok(cursor),
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
    let developer_mode_path = format!("{world_path}/developer-mode");
    view! {
        <main class="min-h-dvh bg-base-200 p-4 sm:p-8">
            <section class="card mx-auto max-w-5xl bg-base-100 shadow-xl"><div class="card-body gap-6">
            <div class="flex flex-wrap items-center justify-between gap-4"><h1 class="card-title text-3xl">{world.name}</h1></div>
            {world.developer_mode.then(|| view! {
                <div class="alert alert-error">"Developer Mode is on: every player can see complete internal game state."</div>
            })}
            {is_owner.then(|| view! {
                <div>{if world.developer_mode {
                    view! {
                        <form action=developer_mode_path.clone() method="post">
                            <input type="hidden" name="enabled" value="false"/>
                            <button class="btn btn-sm" type="submit">"Disable Developer Mode"</button>
                        </form>
                    }.into_any()
                } else {
                    view! { <DeveloperModeEnableButton world_id/> }.into_any()
                }}</div>
            })}
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
fn GamePage(
    world_id: i64,
    character_id: i64,
    history: Vec<PlayerChatEntry>,
    #[prop(default = false)] developer_mode: bool,
) -> impl IntoView {
    let after_message_id = history.last().map(|entry| entry.id);
    let history = player_chat_entries(history);
    view! {
        <PlayerChat world_id character_id after_message_id developer_mode>
            <ChatTranscript history developer_mode/>
        </PlayerChat>
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
            id: Some(entry.id),
            sequence_id: entry.sequence_id,
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

    /// Browser fixtures use the production page shell, GamePage, generated
    /// island loader, assets, and authorized inspection routes. Only model/chat
    /// events are scripted in Node, so these UI checks need neither GPU nor OAuth.
    #[tokio::test]
    #[ignore = "requires built frontend and Chrome; run npm run test:hydration"]
    async fn browser_checks() {
        use crate::llm::{Content, Message, Response, Role, Sampling, Usage};
        use crate::store::{InferenceInput, InferenceOutcome};
        use std::{collections::VecDeque, sync::Mutex};

        struct ScriptedBackend {
            responses: Mutex<VecDeque<Response>>,
            requests: Arc<Mutex<Vec<crate::llm::Request>>>,
        }

        impl ScriptedBackend {
            fn new(
                responses: impl IntoIterator<Item = Response>,
            ) -> (Self, Arc<Mutex<Vec<crate::llm::Request>>>) {
                let requests = Arc::new(Mutex::new(Vec::new()));
                (
                    Self {
                        responses: Mutex::new(responses.into_iter().collect()),
                        requests: Arc::clone(&requests),
                    },
                    requests,
                )
            }
        }

        impl crate::llm::Backend for ScriptedBackend {
            async fn complete(
                &self,
                request: crate::llm::Request,
                _on_token: impl FnMut(&str) + Send,
            ) -> Result<Response> {
                self.requests.lock().unwrap().push(request);
                self.responses
                    .lock()
                    .unwrap()
                    .pop_front()
                    .context("browser fixture received an unexpected inference")
            }
        }

        #[derive(Deserialize)]
        struct BrowserPage {
            history: Vec<ChatEntry>,
            developer_mode: bool,
            world_id: i64,
        }

        async fn render_browser_page(Json(page): Json<BrowserPage>) -> Html<String> {
            let history = page
                .history
                .into_iter()
                .enumerate()
                .map(|(index, entry)| PlayerChatEntry {
                    id: entry.id.unwrap_or(index as i64 + 1),
                    role: match entry.role {
                        ChatRole::User => Role::User,
                        ChatRole::Assistant => Role::Assistant,
                        ChatRole::Narration => Role::Narration,
                        ChatRole::Notice => panic!("fixture notices must use the chat event path"),
                    },
                    text: entry.text,
                    sequence_id: entry.sequence_id,
                })
                .collect();
            Html(render_page("Cairnworld", true, move || {
                view! {
                    <GamePage world_id=page.world_id character_id=2 history developer_mode=page.developer_mode/>
                }
            }))
        }

        let (app, store, _db) = test_app().await;
        let owner = store
            .find_or_create_user("browser@example.test", "Browser owner")
            .await
            .unwrap();
        let scenario = Scenario::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/scenarios/bread_thief.json"
        ))
        .unwrap();
        let installed = store.install_scenario(&owner, &scenario).await.unwrap();
        let world_id = installed.world_id;
        store
            .set_developer_mode(owner.id, world_id, true)
            .await
            .unwrap();
        // A second, separately owned world that starts with Developer Mode
        // off, so check_world_detail.cjs can exercise the real enable flow
        // (the shared world above already has it on for the other checks).
        let modal_world_id = store
            .install_scenario(&owner, &scenario)
            .await
            .unwrap()
            .world_id;
        // A second, non-owner active member of the shared world: real
        // membership/access revocation (a live ban) has an owner and a
        // removable target, and only the owner may call set_developer_mode
        // or remove_member, so check_developer_mode.cjs's live revocation
        // checks need a distinct session from `owner`'s.
        let viewer = store
            .find_or_create_user("browser-viewer@example.test", "Browser viewer")
            .await
            .unwrap();
        let invitation = store
            .create_invitation(owner.id, world_id, None)
            .await
            .unwrap();
        let viewer_character_id = store
            .accept_invitation(&viewer, &invitation.token)
            .await
            .unwrap()
            .character_id;

        // A real split, so check_developer_mode.cjs can exercise the GM's
        // branch-graph DAG view (D6), not just the trivial one-branch case
        // every other agent still uses.
        let original_group_id = store
            .character_group_id(installed.member.character_id)
            .await
            .unwrap();
        let toma_id: i64 = sqlx::query_scalar("SELECT id FROM character WHERE name = 'Toma'")
            .fetch_one(&store.pool())
            .await
            .unwrap();
        let mara_id: i64 = sqlx::query_scalar("SELECT id FROM character WHERE name = 'Mara'")
            .fetch_one(&store.pool())
            .await
            .unwrap();
        let gm_history = store.group_gm_history(original_group_id).await.unwrap();
        let split_trigger = store
            .begin_sequence(world_id, "browser inspection split fixture")
            .await
            .unwrap();
        let split_invocation = store
            .start_invocation(
                Some(split_trigger.id),
                gm_history.agent_id,
                Some(gm_history.branch_id),
                None,
                None,
            )
            .await
            .unwrap();
        let split_inference = store
            .start_inference(InferenceInput {
                agent_id: gm_history.agent_id,
                sequence_id: Some(split_trigger.id),
                invocation_id: split_invocation,
                segments: &[],
                request: &crate::llm::Request {
                    messages: vec![Message::text(Role::User, "split")],
                    tools: Vec::new(),
                    sampling: Sampling::default(),
                },
                model: "browser-test",
            })
            .await
            .unwrap();
        let split_executions = store
            .create_tool_executions(split_inference, 1)
            .await
            .unwrap();
        store
            .apply_topology_changes(
                split_inference,
                vec![(
                    "split-1".to_string(),
                    split_executions[0],
                    crate::tools::TopologyChange::Split {
                        group_id: original_group_id,
                        partitions: vec![
                            crate::tools::GroupPartition {
                                character_ids: vec![
                                    installed.member.character_id,
                                    mara_id,
                                    viewer_character_id,
                                ],
                                name: "player side".into(),
                                description: "outside".into(),
                            },
                            crate::tools::GroupPartition {
                                character_ids: vec![toma_id],
                                name: "Toma side".into(),
                                description: "inside".into(),
                            },
                        ],
                        continue_with_character_id: Some(installed.member.character_id),
                    },
                )],
            )
            .await
            .unwrap();
        // This fixture used to construct an inference, tool execution and
        // reply directly in Store. E2 must inspect the records normal play
        // creates, so exercise Game's ordinary player-message loop instead.
        // These responses were captured from one local configured-model run
        // with forced compaction. Replaying them keeps this browser test
        // repeatable while request assembly, tool execution, result delivery
        // and recording stay in production code.
        let browser_limits = crate::settings::Limits {
            // The recorded opening narration exceeds this threshold and
            // creates one deferred summary. The short post-tool reply does
            // not, so this fixture is one complete turn rather than a chain
            // of summaries of summaries.
            compact_before_next_input_tokens: 700,
            keep_tail_messages: 1,
            ..Default::default()
        };
        let responses: Vec<Response> =
            serde_json::from_str(include_str!("../testdata/real_model_creation_round.json"))
                .unwrap();
        let (backend, captured_requests) = ScriptedBackend::new(responses);
        let scheduler = crate::inference::InferenceScheduler::new(backend, browser_limits).unwrap();
        let game = Arc::new(Game::new(
            store.clone(),
            scheduler.foreground(),
            browser_limits,
            "browser-test".into(),
            Sampling::default(),
        ));
        let creation = store
            .begin_sequence(world_id, "browser completed creation")
            .await
            .unwrap();
        store
            .roll_hit_protection(&installed.member, creation.id, None)
            .await
            .unwrap();
        store
            .roll_attributes(&installed.member, creation.id, None)
            .await
            .unwrap();
        game.player_message(
            installed.member.clone(),
            "The required rolls are complete. Begin play now.",
        )
        .await
        .unwrap();
        let sequence = store
            .world_sequences(owner.id, world_id, None, 1)
            .await
            .unwrap()
            .into_iter()
            .next()
            .unwrap();
        let nodes = store
            .sequence_tree(owner.id, world_id, sequence.id)
            .await
            .unwrap();
        let inference_id = nodes[0].inference.id;
        let recorded_request = nodes[0].inference.request.clone();
        assert_eq!(
            recorded_request,
            captured_requests.lock().unwrap()[0],
            "the inspection record must preserve the request Game sent to the model"
        );
        let agent_id = installed.member.agent_id;
        // Live update checks drive Store's test-only event hooks one step at
        // a time. Give them their own valid, un-compacted history instead of
        // fabricating a new inference from the action above after its real
        // deferred compaction has changed the branch.
        let live_agent_id = store.create_agent(world_id).await.unwrap();
        let live_sequence = store
            .begin_sequence(world_id, "browser live inspection fixture")
            .await
            .unwrap();
        let live_message_id = store
            .append_message_with_sequence(
                live_agent_id,
                &Message::text(Role::User, "I am ready for a live inspection test."),
                live_sequence.id,
            )
            .await
            .unwrap();
        // Drives one live inference's real store writes step by step so a
        // browser check can observe the actual /inspect/events broadcast at
        // each stage (started, mid-stream tokens, completed) without a real
        // model - Store/Game/routes/broadcast are the same production code
        // either way, only what triggers them is a test fixture. Real state
        // per design.md's Live inspection protocol: not a second event
        // transport, just driving the one real one from JS at chosen points.
        #[derive(Deserialize)]
        struct LiveInferenceStep {
            agent_id: i64,
            sequence_id: i64,
            #[serde(default)]
            parent_tool_execution_id: Option<i64>,
            #[serde(default)]
            inference_id: Option<i64>,
            #[serde(default)]
            token: Option<String>,
            #[serde(default)]
            offset: Option<usize>,
            #[serde(default)]
            content: Option<String>,
        }
        let live_store = store.clone();
        let start_live_inference = move |Json(step): Json<LiveInferenceStep>| {
            let store = live_store.clone();
            async move {
                let segments = store
                    .live_segments(store.agent_branch(step.agent_id).await.unwrap())
                    .await
                    .unwrap();
                let request = store
                    .request_for_segments(step.agent_id, &segments, Sampling::default())
                    .await
                    .unwrap();
                let invocation_id = store
                    .start_invocation(
                        Some(step.sequence_id),
                        step.agent_id,
                        None,
                        step.parent_tool_execution_id,
                        None,
                    )
                    .await
                    .unwrap();
                let inference_id = store
                    .start_inference(InferenceInput {
                        agent_id: step.agent_id,
                        sequence_id: Some(step.sequence_id),
                        invocation_id,
                        segments: &segments,
                        request: &request,
                        model: "browser-test",
                    })
                    .await
                    .unwrap();
                Json(inference_id)
            }
        };
        let live_store = store.clone();
        let token_live_inference = move |Json(step): Json<LiveInferenceStep>| {
            let store = live_store.clone();
            async move {
                let world_id = store
                    .world_id_for_inference(step.inference_id.unwrap())
                    .await
                    .unwrap();
                store.publish_token(
                    world_id,
                    step.inference_id.unwrap(),
                    step.offset.unwrap(),
                    &step.token.unwrap(),
                );
            }
        };
        let live_store = store.clone();
        let finish_live_inference = move |Json(step): Json<LiveInferenceStep>| {
            let store = live_store.clone();
            async move {
                store
                    .finish_inference(
                        step.inference_id.unwrap(),
                        InferenceOutcome::Response(Response {
                            content: Content::Text(
                                step.content.unwrap_or_else(|| "Live reply.".to_string()),
                            ),
                            reasoning: String::new(),
                            usage: Usage {
                                input_tokens: 1,
                                output_tokens: 2,
                            },
                        }),
                        5,
                    )
                    .await
                    .unwrap();
            }
        };

        // More than one object page is required to verify that an ordinary
        // browser pagination action preserves the mounted chat draft. These
        // rows are fixture-only current state, inserted after the scripted
        // game path has recorded its evidence so they cannot alter that path.
        let fixture_item_type: i64 =
            sqlx::query_scalar("SELECT id FROM item_type WHERE world_id = ? AND name = 'gear'")
                .bind(world_id)
                .fetch_one(&store.pool())
                .await
                .unwrap();
        let fixture_location: i64 =
            sqlx::query_scalar("SELECT id FROM location WHERE world_id = ? ORDER BY id LIMIT 1")
                .bind(world_id)
                .fetch_one(&store.pool())
                .await
                .unwrap();
        for number in 0..60 {
            let item_id = sqlx::query_scalar::<_, i64>(
                "INSERT INTO item (world_id, item_type_id, name, description) \
                 VALUES (?, ?, ?, '') RETURNING id",
            )
            .bind(world_id)
            .bind(fixture_item_type)
            .bind(format!("pagination fixture item {number}"))
            .fetch_one(&store.pool())
            .await
            .unwrap();
            sqlx::query("INSERT INTO item_location (item_id, location_id) VALUES (?, ?)")
                .bind(item_id)
                .bind(fixture_location)
                .execute(&store.pool())
                .await
                .unwrap();
        }

        let app = app
            .route("/browser/render", post(render_browser_page))
            .route(
                "/browser/loading",
                get(|| async {
                    Html(render_page("Cairnworld", true, || view! { <GameLoading/> }))
                }),
            )
            .route("/browser/inference/start", post(start_live_inference))
            .route("/browser/inference/token", post(token_live_inference))
            .route("/browser/inference/finish", post(finish_live_inference))
            .route("/favicon.ico", get(|| async { StatusCode::NO_CONTENT }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let fixture = serde_json::json!({
            "base": base, "cookie": session_cookie(&store, owner.id).await,
            "viewer_id": viewer.id, "viewer_cookie": session_cookie(&store, viewer.id).await,
            "world_id": world_id, "sequence_id": sequence.id,
            "inference_id": inference_id,
            "request": recorded_request,
            "tool_call_id": "call-d95cbfac-d6c0-4d92-a5ab-58498440d128",
            "agent_id": agent_id, "modal_world_id": modal_world_id,
            "live_agent_id": live_agent_id, "live_sequence_id": live_sequence.id,
            "live_message_id": live_message_id,
            "gm_agent_id": gm_history.agent_id,
        })
        .to_string();
        let (shutdown, stopped) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(async {
                    stopped.await.unwrap();
                })
                .await
                .unwrap();
        });
        let mut outputs = Vec::new();
        for script in [
            "check_hydration.cjs",
            "check_player_chat.cjs",
            "check_developer_mode.cjs",
            "check_world_detail.cjs",
        ] {
            let output = tokio::process::Command::new("node")
                .arg(format!("scripts/{script}"))
                .env("CAIRNWORLD_BROWSER_FIXTURE", &fixture)
                .kill_on_drop(true)
                .output()
                .await
                .unwrap();
            println!("{script}: {}", output.status);
            if !output.status.success() {
                eprintln!("{}", String::from_utf8_lossy(&output.stderr));
            }
            outputs.push((script, output));
        }
        let mut negatives = Vec::new();
        for kind in ["exception", "console", "resource"] {
            let output = tokio::process::Command::new("node")
                .arg("scripts/check_hydration.cjs")
                .env("CAIRNWORLD_BROWSER_FIXTURE", &fixture)
                .env("CAIRNWORLD_INJECT_BROWSER_ERROR", kind)
                .kill_on_drop(true)
                .output()
                .await
                .unwrap();
            negatives.push((kind, output));
        }
        shutdown.send(()).unwrap();
        server.await.unwrap();
        for (script, output) in outputs {
            assert!(
                output.status.success(),
                "{script}: {}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
        for (kind, output) in negatives {
            assert!(
                !output.status.success(),
                "injected {kind} error must fail the command"
            );
            assert!(
                String::from_utf8_lossy(&output.stderr).contains("injected"),
                "{kind}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            println!("injected {kind} error: correctly rejected");
        }
    }

    /// The GPU test covers E2's direct model-to-browser path: it opens in a real browser
    /// the exact record written by a normal Mistral-backed Game invocation.
    /// Ordinary browser checks remain model-free so they can run on every
    /// development machine.
    #[tokio::test]
    #[ignore = "device/browser test: needs a CUDA GPU, dev-qwen35, built frontend and Chrome"]
    async fn real_model_browser_opens_exact_inference_evidence() {
        use crate::{inference::InferenceScheduler, settings::Settings};

        let game_load = GameLoad::loading();
        let (app, store, _db) = test_app_with_game(game_load.clone()).await;
        let owner = store
            .find_or_create_user("real-browser@example.test", "Real browser")
            .await
            .unwrap();
        let scenario = Scenario::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/scenarios/bread_thief.json"
        ))
        .unwrap();
        let installed = store.install_scenario(&owner, &scenario).await.unwrap();
        store
            .set_developer_mode(owner.id, installed.world_id, true)
            .await
            .unwrap();

        let settings = Settings::for_device_test("dev-qwen35").unwrap();
        let model = settings.model(None).unwrap();
        let model_path = model.path.clone();
        let sampling = crate::llm::Sampling {
            temperature: 0.0,
            ..settings.sampling(&model)
        };
        let mut backend = MistralRsBackend::load(
            &model.path,
            model.chat_template.as_deref(),
            model.source_model.as_deref(),
            settings.limits,
            false,
        )
        .await
        .unwrap();
        let observed_prompts = backend.observe_prompts();
        let game = Arc::new(Game::new(
            store.clone(),
            InferenceScheduler::new(backend, settings.limits)
                .unwrap()
                .foreground(),
            settings.limits,
            model_path,
            sampling,
        ));
        game_load.finish(Ok(Arc::clone(&game))).await;
        game.wait_for_opening(installed.member.clone())
            .await
            .unwrap();

        let creation = store
            .begin_sequence(installed.world_id, "real-browser completed creation")
            .await
            .unwrap();
        store
            .roll_hit_protection(&installed.member, creation.id, None)
            .await
            .unwrap();
        store
            .roll_attributes(&installed.member, creation.id, None)
            .await
            .unwrap();
        for attempt in 1..=3 {
            game.player_message(
                installed.member.clone(),
                "The required rolls are complete. Begin play now.",
            )
            .await
            .unwrap_or_else(|error| panic!("creation follow-up {attempt} failed: {error:#}"));
            if store.is_ready_to_begin(&installed.member).await.unwrap() {
                break;
            }
        }
        assert!(
            store.is_ready_to_begin(&installed.member).await.unwrap(),
            "the real model did not call ready_to_begin after the completed rolls"
        );
        let sequence = store
            .world_sequences(owner.id, installed.world_id, None, 8)
            .await
            .unwrap()
            .into_iter()
            .find(|sequence| sequence.trigger == "player message")
            .unwrap();
        let nodes = store
            .sequence_tree(owner.id, installed.world_id, sequence.id)
            .await
            .unwrap();
        let inference_id = nodes
            .iter()
            .find(|node| {
                matches!(
                    &node.inference.outcome,
                    crate::inspection::RecordedOutcome::Response(response)
                        if matches!(&response.content, crate::llm::Content::ToolCalls(calls)
                            if calls.iter().any(|call| call.name == "ready_to_begin"))
                )
            })
            .unwrap_or_else(|| {
                panic!("creation sequence did not record ready_to_begin: {nodes:#?}")
            })
            .inference
            .id;
        let recorded = store.reconstruct_inference(inference_id).await.unwrap();
        let prompt = recorded.model_prompt.clone().unwrap();
        let evidence = recorded.prompt_evidence.clone().unwrap();
        {
            let observed_prompts = observed_prompts.lock().unwrap();
            let observed = observed_prompts
                .iter()
                .find(|observed| observed.request == recorded.request)
                .unwrap_or_else(|| {
                    panic!("the captured tool inference was not observed at the model boundary")
                });
            assert_eq!(recorded.request, observed.request);
            assert_eq!(prompt, observed.prompt);
            assert_eq!(
                evidence.tokens_hash,
                crate::store::PromptEvidence::tokens_hash(&observed.tokens)
            );
        }

        // This is current world state, deliberately outside the immutable
        // recipe retained for a past inference. The old record must remain
        // exactly reconstructable after it changes.
        sqlx::query("UPDATE world SET gm_notes = ? WHERE id = ?")
            .bind(r#"{"changed_after_capture":true}"#)
            .bind(installed.world_id)
            .execute(&store.pool())
            .await
            .unwrap();
        assert_eq!(
            store.reconstruct_inference(inference_id).await.unwrap(),
            recorded,
            "current world notes changed a saved inference"
        );

        let after_message_id = store
            .player_chat(&installed.member)
            .await
            .unwrap()
            .last()
            .unwrap()
            .id;
        let cookie = session_cookie(&store, owner.id).await;
        assert_eq!(
            request(
                &app,
                "GET",
                &format!(
                    "/world/{}/inspect/sequences/{}",
                    installed.world_id, sequence.id
                ),
                Some(&cookie),
            )
            .await,
            StatusCode::OK,
            "the browser must be able to load the real tool sequence"
        );
        assert_eq!(
            request(
                &app,
                "GET",
                &format!(
                    "/world/{}/inspect/inferences/{inference_id}",
                    installed.world_id
                ),
                Some(&cookie),
            )
            .await,
            StatusCode::OK,
            "the browser must be able to load the real tool inference"
        );
        let app = app.route("/favicon.ico", get(|| async { StatusCode::NO_CONTENT }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let fixture = serde_json::json!({
            "base": base,
            "cookie": cookie,
            "world_id": installed.world_id,
            "character_id": installed.member.character_id,
            "after_message_id": after_message_id,
            "sequence_id": sequence.id,
            "inference_id": inference_id,
            "tool": "ready_to_begin",
            "request_json": serde_json::to_string(&recorded.request).unwrap(),
            "model_prompt": prompt,
            "prompt_evidence": evidence,
        })
        .to_string();
        let (shutdown, stopped) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(async { stopped.await.unwrap() })
                .await
                .unwrap();
        });
        let output = tokio::process::Command::new("node")
            .arg("scripts/check_real_model_developer_evidence.cjs")
            .env("CAIRNWORLD_BROWSER_FIXTURE", fixture)
            .kill_on_drop(true)
            .output()
            .await
            .unwrap();
        shutdown.send(()).unwrap();
        server.await.unwrap();
        assert!(
            output.status.success(),
            "real-model Developer Mode browser check: {}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    /// A real router over a real store and a real session middleware, wired
    /// exactly as `serve` wires it. `Google` never talks to the network: its
    /// provider metadata is constructed offline with `ProviderMetadata::new`,
    /// the same public constructor `discover_async` itself builds on, since
    /// route tests never exercise the OAuth handlers.
    async fn test_app() -> (Router, Store, crate::store::TestDatabase) {
        test_app_with_game(GameLoad::loading()).await
    }

    async fn test_app_with_game(game: GameLoad) -> (Router, Store, crate::store::TestDatabase) {
        use openidconnect::{
            AuthUrl, EmptyAdditionalProviderMetadata, JsonWebKeySetUrl, ResponseTypes,
            core::{CoreJwsSigningAlgorithm, CoreResponseType, CoreSubjectIdentifierType},
        };

        let db = crate::store::TestDatabase::new("web-test");
        let store = Store::open(db.path()).await.unwrap();
        let sessions = session_store(store.pool()).await.unwrap();
        let metadata = CoreProviderMetadata::new(
            IssuerUrl::new("https://accounts.example.test".into()).unwrap(),
            AuthUrl::new("https://accounts.example.test/auth".into()).unwrap(),
            JsonWebKeySetUrl::new("https://accounts.example.test/jwks".into()).unwrap(),
            vec![ResponseTypes::new(vec![CoreResponseType::Code])],
            vec![CoreSubjectIdentifierType::Public],
            vec![CoreJwsSigningAlgorithm::RsaSsaPkcs1V15Sha256],
            EmptyAdditionalProviderMetadata {},
        );
        let client = CoreClient::from_provider_metadata(
            metadata,
            ClientId::new("test-client".into()),
            Some(ClientSecret::new("test-secret".into())),
        )
        .set_redirect_uri(RedirectUrl::new("https://example.test/callback".into()).unwrap());
        let google = Google {
            client,
            http: reqwest::Client::new(),
        };
        let app = router(
            App {
                store: store.clone(),
                google,
                game,
            },
            sessions,
            true,
        );
        (app, store, db)
    }

    /// A `Cookie` header selecting a real, saved session for `user_id`, so
    /// route tests exercise the same session middleware and store-backed
    /// lookup a browser request would.
    async fn session_cookie(store: &Store, user_id: i64) -> String {
        let sessions = session_store(store.pool()).await.unwrap();
        let session = Session::new(None, Arc::new(sessions), None);
        session.insert(USER_ID, user_id).await.unwrap();
        session.save().await.unwrap();
        format!("id={}", session.id().unwrap())
    }

    async fn request(
        app: &Router,
        method: &str,
        uri: &str,
        cookie: Option<&str>,
    ) -> axum::http::StatusCode {
        form_request(app, method, uri, cookie, None).await
    }

    /// `form` is sent as an `application/x-www-form-urlencoded` body, matching
    /// every POST route's real `Form<T>` extractor.
    async fn form_request(
        app: &Router,
        method: &str,
        uri: &str,
        cookie: Option<&str>,
        form: Option<&str>,
    ) -> axum::http::StatusCode {
        use tower::ServiceExt;
        let mut builder = axum::http::Request::builder().method(method).uri(uri);
        if let Some(cookie) = cookie {
            builder = builder.header("cookie", cookie);
        }
        let body = if let Some(form) = form {
            builder = builder.header("content-type", "application/x-www-form-urlencoded");
            axum::body::Body::from(form.to_string())
        } else {
            axum::body::Body::empty()
        };
        let request = builder.body(body).unwrap();
        app.clone().oneshot(request).await.unwrap().status()
    }

    /// A world with an owner, an active player, and a removed player, in one
    /// world, plus a second world the first world's owner does not own -
    /// enough identities to exercise every authorization row in the
    /// Developer Mode test matrix.
    async fn authorization_fixture(
        store: &Store,
    ) -> (
        i64,
        crate::store::User,
        crate::store::User,
        crate::store::User,
        i64,
    ) {
        let owner = store
            .find_or_create_user("owner@example.test", "Owner")
            .await
            .unwrap();
        let scenario = Scenario::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/scenarios/bread_thief.json"
        ))
        .unwrap();
        let installed = store.install_scenario(&owner, &scenario).await.unwrap();
        let world_id = installed.world_id;
        let active_player = store
            .find_or_create_user("player@example.test", "Player")
            .await
            .unwrap();
        let invitation = store
            .create_invitation(owner.id, world_id, None)
            .await
            .unwrap();
        store
            .accept_invitation(&active_player, &invitation.token)
            .await
            .unwrap();
        let removed_player = store
            .find_or_create_user("removed@example.test", "Removed")
            .await
            .unwrap();
        let removed_invitation = store
            .create_invitation(owner.id, world_id, None)
            .await
            .unwrap();
        store
            .accept_invitation(&removed_player, &removed_invitation.token)
            .await
            .unwrap();
        store
            .remove_member(owner.id, world_id, removed_player.id)
            .await
            .unwrap();
        let other_world = store.create_world("other").await.unwrap();
        (world_id, owner, active_player, removed_player, other_world)
    }

    #[tokio::test]
    async fn inspect_routes_reject_every_unauthorized_case_at_the_http_boundary() {
        let (app, store, _db) = test_app().await;
        let (world_id, owner, active_player, removed_player, other_world) =
            authorization_fixture(&store).await;

        let inspect_paths = [
            format!("/world/{world_id}/inspect/sequences"),
            format!("/world/{world_id}/inspect/agents"),
            format!("/world/{world_id}/inspect/objects"),
        ];

        for path in &inspect_paths {
            assert_eq!(
                request(&app, "GET", path, None).await,
                StatusCode::UNAUTHORIZED,
                "unauthenticated request to {path} must be rejected"
            );
            let stranger = session_cookie(&store, 999_999).await;
            assert_eq!(
                request(&app, "GET", path, Some(&stranger)).await,
                StatusCode::UNAUTHORIZED,
                "a session naming no real user must not return data for {path}"
            );
            let active = session_cookie(&store, active_player.id).await;
            assert_eq!(
                request(&app, "GET", path, Some(&active)).await,
                StatusCode::INTERNAL_SERVER_ERROR,
                "an active member must be rejected while developer mode is off for {path}"
            );
            let removed = session_cookie(&store, removed_player.id).await;
            assert_eq!(
                request(&app, "GET", path, Some(&removed)).await,
                StatusCode::INTERNAL_SERVER_ERROR,
                "a removed member must be rejected for {path}"
            );
        }

        // A non-owner cannot enable developer mode.
        let active = session_cookie(&store, active_player.id).await;
        assert_eq!(
            form_request(
                &app,
                "POST",
                &format!("/world/{world_id}/developer-mode"),
                Some(&active),
                Some("enabled=true"),
            )
            .await,
            StatusCode::INTERNAL_SERVER_ERROR,
            "a non-owner cannot toggle developer mode"
        );
        store
            .set_developer_mode(owner.id, world_id, true)
            .await
            .unwrap();

        for path in &inspect_paths {
            let active = session_cookie(&store, active_player.id).await;
            assert_eq!(
                request(&app, "GET", path, Some(&active)).await,
                StatusCode::OK,
                "an active member must succeed once developer mode is on for {path}"
            );
            let removed = session_cookie(&store, removed_player.id).await;
            assert_eq!(
                request(&app, "GET", path, Some(&removed)).await,
                StatusCode::INTERNAL_SERVER_ERROR,
                "a removed member stays rejected even with developer mode on for {path}"
            );
        }

        // A foreign world's sequence/inference/agent id is rejected even for
        // this world's own owner and even with developer mode on.
        let owner_session = session_cookie(&store, owner.id).await;
        let other_agent = store.create_agent(other_world).await.unwrap();
        assert_eq!(
            request(
                &app,
                "GET",
                &format!("/world/{world_id}/inspect/agents/{other_agent}/history"),
                Some(&owner_session),
            )
            .await,
            StatusCode::INTERNAL_SERVER_ERROR,
            "an agent from another world must be rejected"
        );
        assert_eq!(
            request(
                &app,
                "GET",
                &format!("/world/{world_id}/inspect/agents/{other_agent}/branches"),
                Some(&owner_session),
            )
            .await,
            StatusCode::INTERNAL_SERVER_ERROR,
            "an agent from another world must be rejected for its branch graph too"
        );
    }

    #[test]
    fn game_page_renders_stored_chat_entries_with_their_visible_roles() {
        let html = render_page("Cairnworld", true, || {
            view! { <GamePage world_id=7 character_id=8 history=vec![
                PlayerChatEntry { id: 1, role: crate::llm::Role::User, text: "I look around.".into(), sequence_id: Some(1) },
                PlayerChatEntry { id: 2, role: crate::llm::Role::Narration, text: "Toma watches.".into(), sequence_id: None },
            ]/> }
        });
        assert!(html.contains("I look around."));
        assert!(html.contains("Toma watches."));
        assert!(html.contains("data-role=\"user\""));
        assert!(html.contains("data-role=\"narration\""));
        assert!(
            !html.contains("data-inspect-sequence"),
            "an ordinary player's page must never carry a sequence id"
        );
        assert!(!html.contains("Developer Mode"));
    }

    #[test]
    fn developer_mode_game_page_exposes_inspect_links_for_entries_with_a_sequence() {
        let html = render_page("Cairnworld", true, || {
            view! { <GamePage world_id=7 character_id=8 developer_mode=true history=vec![
                PlayerChatEntry { id: 1, role: crate::llm::Role::User, text: "I look around.".into(), sequence_id: Some(42) },
                PlayerChatEntry { id: 2, role: crate::llm::Role::Narration, text: "Toma watches.".into(), sequence_id: None },
            ]/> }
        });
        assert!(
            html.contains("Developer Mode"),
            "the indicator badge is shown"
        );
        assert!(
            html.contains("data-inspect-sequence=\"42\""),
            "the entry with a resolved sequence carries an inspect link"
        );
        // The narration entry has no sequence, so it gets no inspect link -
        // its role's presence confirms it still rendered otherwise.
        assert!(html.contains("data-role=\"narration\""));
        assert_eq!(
            html.matches("data-inspect-sequence=\"").count(),
            1,
            "only the entry with a real sequence id is inspectable"
        );
    }

    #[test]
    fn world_detail_gives_each_active_viewer_their_own_entry_link() {
        fn detail(viewer_id: i64) -> String {
            render_page("Cairnworld", false, move || {
                view! { <WorldDetail
                    world=World { id: 7, name: "Bread Thief".into(), owner_id: 1, developer_mode: false }
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
    fn developer_mode_indicator_and_toggle_reflect_state_and_ownership() {
        fn detail(developer_mode: bool, viewer_id: i64) -> String {
            render_page("Cairnworld", true, move || {
                view! { <WorldDetail
                    world=World { id: 7, name: "Bread Thief".into(), owner_id: 1, developer_mode }
                    members=vec![]
                    invitations=vec![]
                    viewer_id
                /> }
            })
        }

        // Off, owner: no indicator, sees the owner-only enable control. The
        // real <dialog>'s enable form exists in the DOM (so showModal() can
        // reveal it instantly) but the dialog itself has no `open` attribute
        // - it is not an active, already-visible control.
        let off_owner = detail(false, 1);
        assert!(!off_owner.contains("Developer Mode is on"));
        assert!(off_owner.contains("Enable Developer Mode"));
        assert!(off_owner.contains("action=\"/world/7/developer-mode\""));
        assert!(
            off_owner.contains("<dialog") && !off_owner.contains("<dialog open"),
            "the warning dialog must render closed, not already open"
        );

        // Off, non-owner: no indicator, no toggle at all.
        let off_player = detail(false, 2);
        assert!(!off_player.contains("Developer Mode is on"));
        assert!(!off_player.contains("Enable Developer Mode"));

        // On, non-owner: the indicator is visible to every active member, but
        // only the owner gets a control - a non-owner cannot disable it either.
        let on_player = detail(true, 2);
        assert!(on_player.contains("Developer Mode is on"));
        assert!(!on_player.contains("action=\"/world/7/developer-mode\""));

        // On, owner: the indicator plus a plain disable form - no warning
        // needed to turn it back off.
        let on_owner = detail(true, 1);
        assert!(on_owner.contains("Developer Mode is on"));
        assert!(on_owner.contains("action=\"/world/7/developer-mode\""));
        assert!(on_owner.contains("Disable Developer Mode"));
        assert!(!on_owner.contains("Enable Developer Mode"));
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
            id: None,
            sequence_id: None,
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
