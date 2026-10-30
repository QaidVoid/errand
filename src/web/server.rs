//! The local web interface.
//!
//! Serves the built assets and a small API over them. It holds no state of
//! its own: sessions, their output, and their history all live in the session
//! layer, and a browser is just another view attached to them.
//!
//! The bind address is always checked before anything is served. Without a
//! login configured it is the whole access control. With one, reading stays
//! open and changing anything takes a Discord login, after which a visitor
//! acts as their own account.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::extract::{Path as PathParam, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use serde_json::{Value, json};

use crate::config::redact::{ByteScrubber, redact_text};
use crate::config::schema::WebConfig;
use crate::log::LogValue;
use crate::log::Logger;
use crate::log::fields;
use crate::log::now_ms;
use crate::provider::models::AvailableModel;
use crate::sandbox::paths;
use crate::sandbox::paths::host_path_under;
use crate::session::commands::{COMMANDS, CommandAccess};
use crate::session::event::SessionEvent;
use crate::session::files::MAX_INLINE_BYTES;
use crate::session::files::{read_directory, read_file_for_display};
use crate::session::manager::StartOutcome;
use crate::session::manager::{Delivery, DetachedRequest, SessionManager};
use crate::session::record::{record_dir, transcript_path};
use crate::session::session::IncomingMessage;
use crate::session::transcript::{Transcript, UPLOADS_DIRNAME, is_kept_name};
use crate::web::address::check_bind_address;
use crate::web::login::{Authorized, LoginFailure, Logins};
use crate::web::view::{NameLookup, WebView, wire};

/// The identity a request from the interface acts under when no login is
/// configured.
///
/// Reaching the interface then means being on the operator's own network,
/// so a request carries operator authority. Giving it a name rather than
/// borrowing somebody's account id keeps that visible in a session's own
/// records.
pub const WEB_ACTOR: &str = "web-interface";

/// Raised when the interface cannot be served, with what to do about it.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct WebInterfaceError(pub String);

/// Content types for what the build produces.
const TYPES: [(&str, &str); 7] = [
    (".html", "text/html; charset=utf-8"),
    (".js", "text/javascript; charset=utf-8"),
    (".css", "text/css; charset=utf-8"),
    (".svg", "image/svg+xml"),
    (".json", "application/json; charset=utf-8"),
    (".woff2", "font/woff2"),
    (".map", "application/json; charset=utf-8"),
];

fn content_type(path: &str) -> &str {
    TYPES
        .iter()
        .find(|(extension, _)| path.ends_with(extension))
        .map_or("application/octet-stream", |(_, kind)| kind)
}

#[expect(clippy::needless_pass_by_value)]
fn json(body: Value, status: StatusCode) -> Response {
    (
        status,
        [(header::CONTENT_TYPE, "application/json; charset=utf-8")],
        body.to_string(),
    )
        .into_response()
}

fn ok(body: Value) -> Response {
    json(body, StatusCode::OK)
}

fn refused(body: Value, status: StatusCode) -> Response {
    json(body, status)
}

include!(concat!(env!("OUT_DIR"), "/interface.rs"));

/// The built interface, as a table of paths and bytes.
///
/// Compiled in rather than read from disk, so the binary is the whole daemon
/// wherever it is put. Injected so a test decides what the interface holds,
/// and so a build made without one is the same shape as a build with one.
pub type Assets = &'static [(&'static str, &'static [u8])];

/// What this build carries, which is empty when it was built without one.
pub const BUILT_INTERFACE: Assets = BUNDLED;

/// One file of the interface, or nothing when the bundle does not hold it.
fn bundled(assets: Assets, name: &str) -> Option<&'static [u8]> {
    let wanted = name.trim_start_matches('/');
    assets
        .iter()
        .find(|(path, _)| *path == wanted)
        .map(|(_, bytes)| *bytes)
}

/// Reduces a request path to the name the bundle would hold it under.
///
/// Normalised rather than searched for: a request that climbs above the
/// bundle names nothing in it, and one that merely spells a name oddly should
/// still find it.
fn normalize_request(wanted: &str) -> Option<String> {
    let mut parts: Vec<&str> = Vec::new();
    for component in wanted.trim_start_matches('/').split('/') {
        match component {
            "" | "." => {}
            ".." => return None,
            part => parts.push(part),
        }
    }
    (!parts.is_empty()).then(|| parts.join("/"))
}

/// Runs a command typed outside any session as the channel runs it, for the
/// account given by id and name, and answers with what the channel says back.
pub type ChannelCommand = Arc<
    dyn Fn(
            String,
            String,
            String,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = String> + Send>>
        + Send
        + Sync,
>;

/// The interface, bound to one address.
pub struct WebServer {
    config: WebConfig,
    sessions: Arc<SessionManager>,
    assets: Assets,
    log: Logger,
    guild_id: Option<String>,
    names: Option<NameLookup>,
    logins: Option<Logins>,
    commands: Option<ChannelCommand>,
    streams: Arc<std::sync::atomic::AtomicUsize>,
    started: Mutex<HashMap<String, i64>>,
    openings: Mutex<HashMap<String, String>>,
    shutdown: Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
    bound: Mutex<Option<u16>>,
}

/// What the interface lists for each session.
#[expect(clippy::too_many_arguments, clippy::needless_pass_by_value)]
fn summary(
    id: &str,
    project: &str,
    owner: &str,
    busy: bool,
    ended: bool,
    live: bool,
    opening: Option<String>,
    thread_id: Option<String>,
    started_at: i64,
    last_active_at: i64,
) -> Value {
    json!({
        "id": id,
        "project": project,
        "owner": owner,
        "busy": busy,
        "ended": ended,
        "live": live,
        "opening": opening,
        "threadId": thread_id,
        "startedAt": started_at,
        "lastActiveAt": last_active_at,
    })
}

/// The query a tree, file, or download request carries.
#[derive(serde::Deserialize)]
struct PathQuery {
    #[serde(rename = "path", default)]
    path: Option<String>,
}

#[derive(serde::Deserialize)]
struct StartBody {
    #[serde(default)]
    prompt: Option<String>,
}

#[derive(serde::Deserialize)]
struct SendBody {
    #[serde(default)]
    text: Option<String>,
}

/// The server every route handler is given.
pub type ServerState = Arc<WebServer>;

impl WebServer {
    /// An interface over one session manager and one built bundle.
    ///
    /// `commands` runs what is typed on the home page as a command, which
    /// would otherwise start a session to answer it.
    #[expect(clippy::too_many_arguments)]
    pub fn new(
        config: WebConfig,
        sessions: Arc<SessionManager>,
        assets: Assets,
        log: Logger,
        guild_id: Option<String>,
        names: Option<NameLookup>,
        logins: Option<Logins>,
        commands: Option<ChannelCommand>,
    ) -> Arc<Self> {
        Arc::new(Self {
            config,
            sessions,
            assets,
            log,
            guild_id,
            names,
            logins,
            commands,
            streams: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            started: Mutex::new(HashMap::new()),
            openings: Mutex::new(HashMap::new()),
            shutdown: Mutex::new(None),
            bound: Mutex::new(None),
        })
    }

    /// The address it is listening on, once started.
    pub fn url(&self) -> String {
        format!("http://{}:{}", self.config.host, self.config.port)
    }

    /// Whether the interface may only watch.
    pub fn observer(&self) -> bool {
        self.config.observer
    }

    /// The port the listener actually took, which a test needs when it asked
    /// for an ephemeral one.
    #[allow(
        dead_code,
        reason = "read by this module's tests, which assert on state the daemon never asks for"
    )]
    pub fn bound_port(&self) -> Option<u16> {
        *self.bound.lock().expect("the bound lock")
    }

    /// Starts listening.
    ///
    /// Fails with [`WebInterfaceError`] when the address is not private, or
    /// the interface has not been built. Neither is worth serving something
    /// broken over.
    pub async fn start(self: &Arc<Self>) -> Result<(), WebInterfaceError> {
        let verdict = check_bind_address(&self.config.host);
        if !verdict.allowed {
            return Err(WebInterfaceError(format!(
                "refusing to serve the interface: {}",
                verdict.reason
            )));
        }

        if bundled(self.assets, "index.html").is_none() {
            return Err(WebInterfaceError(
                "this build carries no interface. Build it with `cd web && bun install && bun \
                 run build`, then build the daemon again."
                    .to_owned(),
            ));
        }

        let app = self.router();
        let address = format!("{}:{}", self.config.host, self.config.port);
        let listener = tokio::net::TcpListener::bind(&address)
            .await
            .map_err(|error| {
                WebInterfaceError(format!("could not listen on {address}: {error}"))
            })?;

        match listener.local_addr() {
            Ok(address) => *self.bound.lock().expect("the bound lock") = Some(address.port()),
            Err(error) => {
                return Err(WebInterfaceError(format!("could not listen: {error}")));
            }
        }

        let (shutdown, receiver) = tokio::sync::oneshot::channel::<()>();
        *self.shutdown.lock().expect("the shutdown lock") = Some(shutdown);

        let log = self.log.clone();
        let url = self.url();
        let observer = self.config.observer;
        tokio::spawn(async move {
            log.info(
                "web interface listening",
                &fields([
                    ("url", LogValue::from(url)),
                    ("observer", LogValue::from(observer)),
                ]),
            );
            let serving = axum::serve(listener, app).with_graceful_shutdown(async {
                let _ = receiver.await;
            });
            if let Err(error) = serving.await {
                log.error(
                    "the web interface stopped",
                    &fields([("detail", LogValue::from(error.to_string()))]),
                );
            }
        });
        Ok(())
    }

    /// Stops listening.
    pub fn stop(&self) {
        if let Some(shutdown) = self.shutdown.lock().expect("the shutdown lock").take() {
            let _ = shutdown.send(());
        }
    }

    fn router(self: &Arc<Self>) -> axum::Router {
        let state: ServerState = Arc::clone(self);
        axum::Router::new()
            .route("/auth/login", get(begin_login))
            .route("/auth/callback", get(finish_login))
            .route("/auth/logout", post(logout))
            .route("/api/interface", get(describe))
            .route("/api/models", get(models))
            .route("/api/commands", get(commands))
            .route("/api/sessions", get(list_sessions).post(start_session))
            .route("/api/command", post(channel_command))
            .route("/api/sessions/{id}/send", post(send))
            .route("/api/sessions/{id}/stream", get(stream_session))
            .route("/api/sessions/{id}/transcript", get(transcript))
            .route("/api/sessions/{id}/tree", get(tree))
            .route("/api/sessions/{id}/file", get(file))
            .route("/api/sessions/{id}/download", get(download))
            .route("/api/sessions/{id}/uploads/{file}", get(upload))
            .fallback(serve_asset)
            .with_state(state)
    }

    /// Who a request that would change something acts as, or why it may
    /// not.
    #[expect(
        clippy::result_large_err,
        reason = "the refusal is a ready response, which is the point of the shape"
    )]
    async fn actor(&self, headers: &HeaderMap) -> Result<Actor, Response> {
        if self.config.observer {
            return Err(refused(
                json!({ "error": "the interface is an observer and cannot change anything" }),
                StatusCode::FORBIDDEN,
            ));
        }
        let Some(logins) = &self.logins else {
            return Ok(Actor {
                id: WEB_ACTOR.to_owned(),
                name: "the interface".to_owned(),
            });
        };
        let signed = match logins.authorize(headers, self.guild_id.as_deref()).await {
            Authorized::Signed(signed) => signed,
            Authorized::Anonymous => {
                return Err(refused(
                    json!({ "error": "log in with Discord to do this" }),
                    StatusCode::UNAUTHORIZED,
                ));
            }
            Authorized::Unconfirmed(detail) => {
                self.log.warn(
                    "web action refused: membership could not be confirmed",
                    &fields([("detail", LogValue::from(detail))]),
                );
                return Err(refused(
                    json!({ "error": "Discord could not confirm you are still in the server; try again in a moment" }),
                    StatusCode::SERVICE_UNAVAILABLE,
                ));
            }
        };
        // Phrased as the channel's own refusal is not: the visitor already
        // knows who they are, and saying which list they missed tells them
        // nothing the configuration exists to hide.
        if !signed.may_act {
            return Err(refused(
                json!({ "error": "this account may not drive sessions here" }),
                StatusCode::FORBIDDEN,
            ));
        }
        Ok(Actor {
            id: signed.visitor.id,
            name: signed.visitor.name,
        })
    }

    /// What a stopped session was asked, read from its transcript once.
    fn opening_of(&self, session_id: &str, state_dir: &str) -> Option<String> {
        if let Some(known) = self
            .openings
            .lock()
            .expect("the openings lock")
            .get(session_id)
        {
            return Some(known.clone());
        }

        let found = Transcript::new(transcript_path(state_dir), None).opening();
        if let Some(found) = found.as_ref().filter(|found| !found.is_empty()) {
            self.openings
                .lock()
                .expect("the openings lock")
                .insert(session_id.to_owned(), found.clone());
        }
        found
    }

    /// Resolves a project-relative path for a session.
    ///
    /// Goes through the session so the containment is the one the sandbox
    /// applies, rather than a second implementation that could be looser.
    #[expect(
        clippy::result_large_err,
        reason = "the refusal is a ready response, which is the point of the shape"
    )]
    fn locate(&self, id: &str, requested: &str) -> Result<Located, Response> {
        let relative = match requested.trim() {
            "" => ".".to_owned(),
            trimmed => trimmed.to_owned(),
        };
        let session = self.sessions.for_session(id);

        // A session that has stopped keeps its project, so its files stay
        // readable without having to start a sandbox just to look at them.
        let record = if session.is_some() {
            None
        } else {
            self.sessions
                .resumable()
                .into_iter()
                .find(|candidate| candidate.session_id == id)
        };

        let project = match (&session, &record) {
            (Some(session), _) => session.project().path.clone(),
            (None, Some(record)) => record.project_path.clone(),
            (None, None) => {
                return Err(refused(
                    json!({ "error": "no such session" }),
                    StatusCode::NOT_FOUND,
                ));
            }
        };

        if host_path_under(&project, &project, &relative).is_none() {
            return Err(refused(
                json!({ "error": "outside this session's project" }),
                StatusCode::FORBIDDEN,
            ));
        }
        let relative = if relative == "." { "" } else { &relative };
        Ok(Located {
            root: project,
            relative: relative.to_owned(),
        })
    }
}

impl Located {
    /// What the path is, found the way it will be read: beneath the project
    /// and refusing links. Asking the host path would follow a link the agent
    /// planted, and say whether its target exists anywhere on the host.
    fn metadata(&self) -> std::io::Result<std::fs::Metadata> {
        paths::open_beneath(&self.root, &self.relative, &paths::OpenOptions::read())
            .and_then(|file| file.metadata())
    }
}

/// A path inside one session's project.
///
/// The root is what a reader resolves beneath and the relative path is what
/// it asks for. Everything opened is opened beneath the one through the other.
struct Located {
    /// The session's project directory.
    root: String,
    /// The path under the root, as the browser asked for it.
    relative: String,
}

/// The most live streams open at once, across every session.
pub const MAX_STREAMS: usize = 64;

/// The most a message from the interface may say, in characters.
///
/// What a Discord message may hold, so what is sent here can be mirrored into
/// the thread as one message rather than as a wall of them.
pub const MAX_MESSAGE_CHARS: usize = 4_000;

fn too_long(text: &str) -> Option<Response> {
    (text.chars().count() > MAX_MESSAGE_CHARS).then(|| {
        refused(
            json!({ "error": format!("a message may be at most {MAX_MESSAGE_CHARS} characters") }),
            StatusCode::PAYLOAD_TOO_LARGE,
        )
    })
}

/// Who a change is made as.
struct Actor {
    id: String,
    name: String,
}

/// What the interface is allowed to do, so it can show only what works.
///
/// An observer that still drew a composer would be offering something every
/// attempt at which is refused.
async fn describe(State(server): State<ServerState>, headers: HeaderMap) -> Response {
    let signed = server
        .logins
        .as_ref()
        .and_then(|logins| logins.signed(&headers));
    let may_act = !server.config.observer
        && match &server.logins {
            None => true,
            Some(_) => signed.as_ref().is_some_and(|signed| signed.may_act),
        };
    ok(json!({
        "observer": server.config.observer,
        "guildId": server.guild_id,
        "login": server.logins.is_some(),
        "visitor": signed.as_ref().map(|signed| &signed.visitor),
        "mayAct": may_act,
    }))
}

/// The models a session can be started on or switched to.
///
/// Named the way `--model` and `!model` take them, so what the interface
/// offers is exactly what either would accept.
async fn models(State(server): State<ServerState>) -> Response {
    let catalog = server.sessions.catalog();
    let names: Vec<String> = catalog
        .models()
        .iter()
        .map(AvailableModel::qualified)
        .collect();
    ok(json!({
        "default": server.sessions.default_model(),
        "models": names,
        "aliases": catalog.aliases(),
    }))
}

/// The commands a session answers to, from the same table the thread's help
/// is written from.
///
/// Leaves out what only the daemon answers, in the channel: a session never
/// sees those, so offering one here would offer something that does nothing.
async fn commands() -> Response {
    let listed: Vec<Value> = COMMANDS
        .iter()
        .filter(|(_, meta)| meta.access != CommandAccess::Host)
        .map(|(name, meta)| {
            json!({
                "name": name,
                "argument": meta.argument,
                "summary": meta.summary,
                "group": meta.group.title(),
            })
        })
        .collect();
    ok(Value::Array(listed))
}

/// Sends the browser to Discord to log in.
async fn begin_login(State(server): State<ServerState>) -> Response {
    let Some(logins) = &server.logins else {
        return refused(
            json!({ "error": "this interface has no login" }),
            StatusCode::NOT_FOUND,
        );
    };
    let Some((state, url)) = logins.begin() else {
        server
            .log
            .warn("web login refused: too many in progress", &fields([]));
        return refused(
            json!({ "error": "too many logins are in progress; try again in a few minutes" }),
            StatusCode::SERVICE_UNAVAILABLE,
        );
    };
    Response::builder()
        .status(StatusCode::SEE_OTHER)
        .header(header::LOCATION, url)
        .header(header::SET_COOKIE, logins.state_cookie(&state))
        .body(axum::body::Body::empty())
        .unwrap_or_else(|_| unreachable!("a redirect always builds"))
}

/// The query Discord returns the browser with.
#[derive(serde::Deserialize)]
struct CallbackQuery {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
}

/// Completes a login and returns the browser to the interface.
async fn finish_login(
    State(server): State<ServerState>,
    headers: HeaderMap,
    Query(query): Query<CallbackQuery>,
) -> Response {
    let Some(logins) = &server.logins else {
        return refused(
            json!({ "error": "this interface has no login" }),
            StatusCode::NOT_FOUND,
        );
    };
    let finished = logins
        .finish(
            query.state.as_deref(),
            logins.state_of(&headers).as_deref(),
            query.code.as_deref(),
            query.error.as_deref(),
            server.guild_id.as_deref(),
        )
        .await;
    let mut redirect = Response::builder()
        .status(StatusCode::SEE_OTHER)
        .header(header::SET_COOKIE, logins.cleared_state_cookie());
    // A failed attempt leaves any login the browser already holds alone:
    // backing out of Discord's page should not log somebody out.
    redirect = match finished {
        Ok((token, visitor)) => {
            server.log.info(
                "web login",
                &fields([("account", LogValue::from(visitor.id))]),
            );
            redirect
                .header(header::LOCATION, "/")
                .header(header::SET_COOKIE, logins.login_cookie(&token))
        }
        Err(failure) => {
            let detail = match &failure {
                LoginFailure::Failed(detail) => detail.clone(),
                LoginFailure::Denied => "declined on Discord".to_owned(),
                LoginFailure::Expired => {
                    "the login was not started here or took too long".to_owned()
                }
            };
            server.log.warn(
                "web login failed",
                &fields([("detail", LogValue::from(detail))]),
            );
            redirect.header(header::LOCATION, format!("/?login={}", failure.code()))
        }
    };
    redirect
        .body(axum::body::Body::empty())
        .unwrap_or_else(|_| unreachable!("a redirect always builds"))
}

/// Ends the browser's login.
async fn logout(State(server): State<ServerState>, headers: HeaderMap) -> Response {
    let Some(logins) = &server.logins else {
        return ok(json!({ "loggedOut": true }));
    };
    logins.end(&headers);
    Response::builder()
        .header(header::SET_COOKIE, logins.cleared_login_cookie())
        .header(header::CONTENT_TYPE, "application/json; charset=utf-8")
        .body(axum::body::Body::from(
            json!({ "loggedOut": true }).to_string(),
        ))
        .unwrap_or_else(|_| unreachable!("a logout response always builds"))
}

/// Every session the interface knows, live and stopped together.
async fn list_sessions(State(server): State<ServerState>) -> Response {
    let mut listed = Vec::new();
    for session in server.sessions.sessions() {
        let id = session.id().to_owned();
        let busy = server.sessions.is_busy(&id);
        let ended = session.is_ended().await;
        let started_at = {
            let mut started = server.started.lock().expect("the started lock");
            *started.entry(id.clone()).or_insert_with(now_ms)
        };
        let opening = session.opening();
        listed.push(summary(
            &id,
            &session.project().name,
            session.owner_id(),
            busy,
            ended,
            true,
            (!opening.is_empty()).then_some(opening),
            server.sessions.thread_id_for(&id),
            started_at,
            session.last_active_at().await,
        ));
    }

    // Threads whose sandbox has gone are still listed, because the agent's
    // history outlives it and sending to one picks the conversation back up.
    for record in server.sessions.resumable() {
        listed.push(summary(
            &record.session_id,
            &record.project_name,
            &record.owner_id,
            false,
            false,
            false,
            server.opening_of(&record.session_id, &record.state_dir),
            Some(record.thread_id.clone()),
            record.updated_at,
            record.updated_at,
        ));
    }

    ok(Value::Array(listed))
}

/// Starts a session that belongs to no thread.
async fn start_session(
    State(server): State<ServerState>,
    headers: HeaderMap,
    body: Option<axum::Json<StartBody>>,
) -> Response {
    let actor = match server.actor(&headers).await {
        Ok(actor) => actor,
        Err(refusal) => return refusal,
    };

    let Some(axum::Json(body)) = body else {
        return refused(
            json!({ "error": "a prompt is required" }),
            StatusCode::BAD_REQUEST,
        );
    };
    let prompt = body.prompt.unwrap_or_default().trim().to_owned();
    if prompt.is_empty() {
        return refused(
            json!({ "error": "a prompt is required" }),
            StatusCode::BAD_REQUEST,
        );
    }
    if let Some(refusal) = too_long(&prompt) {
        return refusal;
    }

    let outcome = server
        .sessions
        .start_detached(DetachedRequest {
            prompt,
            owner_id: actor.id,
            owner_name: Some(actor.name),
        })
        .await;

    match outcome {
        StartOutcome::Started { session } => ok(json!({ "id": session.id() })),
        StartOutcome::Refused { reason } => {
            refused(json!({ "error": reason }), StatusCode::CONFLICT)
        }
    }
}

/// Runs a command typed on the home page as the channel would, rather than
/// starting a session to answer it: a listing is answered here, and a picture
/// or a clip is posted to the channel, addressed to whoever asked.
async fn channel_command(
    State(server): State<ServerState>,
    headers: HeaderMap,
    body: Option<axum::Json<SendBody>>,
) -> Response {
    let actor = match server.actor(&headers).await {
        Ok(actor) => actor,
        Err(refusal) => return refusal,
    };
    let text = body
        .and_then(|axum::Json(body)| body.text)
        .unwrap_or_default()
        .trim()
        .to_owned();
    if !text.starts_with('!') {
        return refused(
            json!({ "error": "a command starts with !" }),
            StatusCode::BAD_REQUEST,
        );
    }
    if let Some(refusal) = too_long(&text) {
        return refusal;
    }
    let Some(commands) = server.commands.clone() else {
        return refused(
            json!({ "error": "commands cannot be run from here" }),
            StatusCode::SERVICE_UNAVAILABLE,
        );
    };
    let answer = commands(actor.id, actor.name, text).await;
    ok(json!({ "answer": answer }))
}

/// Hands a message to a live session, as if posted in its thread.
async fn send(
    State(server): State<ServerState>,
    PathParam(id): PathParam<String>,
    headers: HeaderMap,
    body: Option<axum::Json<SendBody>>,
) -> Response {
    let actor = match server.actor(&headers).await {
        Ok(actor) => actor,
        Err(refusal) => return refusal,
    };

    let Some(axum::Json(body)) = body else {
        return refused(
            json!({ "error": "nothing to send" }),
            StatusCode::BAD_REQUEST,
        );
    };
    let text = body.text.unwrap_or_default().trim().to_owned();
    if text.is_empty() {
        return refused(
            json!({ "error": "nothing to send" }),
            StatusCode::BAD_REQUEST,
        );
    }
    if let Some(refusal) = too_long(&text) {
        return refusal;
    }

    // The same path a message in a thread takes, so prompts queue and
    // commands apply exactly as they do there.
    let delivered = server
        .sessions
        .deliver_to_session(
            &id,
            IncomingMessage {
                id: format!("web-{}", now_ms()),
                author_id: actor.id,
                channel_id: String::new(),
                author_name: Some(actor.name),
                content: text,
                attachments: Vec::new(),
                from_interface: true,
            },
        )
        .await;

    match delivered {
        Delivery::Sent => ok(json!({ "accepted": true, "detail": "sent" })),
        Delivery::NoSuchSession => refused(
            json!({ "accepted": false, "error": "no such session" }),
            StatusCode::NOT_FOUND,
        ),
        Delivery::NotInvited => refused(
            json!({
                "accepted": false,
                "error": "this session is stopped, and only its owner or someone they invited can pick it up again",
            }),
            StatusCode::FORBIDDEN,
        ),
    }
}

/// Streams a live session to a browser, replaying its record from the start.
async fn stream_session(
    State(server): State<ServerState>,
    PathParam(id): PathParam<String>,
) -> Response {
    use std::sync::atomic::Ordering;

    // Anyone may watch, and each watcher holds a replay, a queue, and a task
    // until it goes away. A ceiling keeps a crowd of them, or one client that
    // opens streams and never reads them, from taking the daemon with it.
    if server.streams.fetch_add(1, Ordering::SeqCst) >= MAX_STREAMS {
        server.streams.fetch_sub(1, Ordering::SeqCst);
        server.log.warn(
            "web stream refused: too many open",
            &fields([("max", LogValue::from(MAX_STREAMS))]),
        );
        return refused(
            json!({ "error": "too many people are watching right now; try again shortly" }),
            StatusCode::SERVICE_UNAVAILABLE,
        );
    }
    let (view, receiver) = WebView::new(server.names.clone());
    let Some(attached) = server.sessions.attach_view(&id, view.clone()).await else {
        server.streams.fetch_sub(1, Ordering::SeqCst);
        return refused(
            json!({ "error": "no such live session" }),
            StatusCode::NOT_FOUND,
        );
    };

    // The browser closing drops the stream, which is the only signal that a
    // view has gone. Detaching then keeps the fan out from growing forever.
    let streams = Arc::clone(&server.streams);
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_millis(1_000)).await;
            if view.is_closed() {
                attached.detach();
                streams.fetch_sub(1, Ordering::SeqCst);
                break;
            }
        }
    });

    let body = futures_util::stream::unfold(receiver, |mut receiver| async move {
        receiver
            .recv()
            .await
            .map(|chunk| (Ok::<_, String>(chunk), receiver))
    });
    Response::builder()
        .header(header::CONTENT_TYPE, "text/event-stream")
        .header(header::CACHE_CONTROL, "no-cache")
        .body(axum::body::Body::from_stream(body))
        .unwrap_or_else(|_| unreachable!("a stream response always builds"))
}

/// What a stopped session said, read back from where it was written down.
async fn transcript(
    State(server): State<ServerState>,
    PathParam(id): PathParam<String>,
) -> Response {
    let record = server
        .sessions
        .resumable()
        .into_iter()
        .find(|candidate| candidate.session_id == id);
    let Some(record) = record else {
        // A live session has its history on its stream, so there is nothing
        // to read back for it.
        return if server.sessions.for_session(&id).is_some() {
            ok(json!({ "entries": [], "dropped": 0 }))
        } else {
            refused(json!({ "error": "no such session" }), StatusCode::NOT_FOUND)
        };
    };

    let path = transcript_path(&record.state_dir);
    let Ok(stored) = blocking(move || Transcript::new(path, None).read()).await else {
        return refused(
            json!({ "error": "the transcript could not be read" }),
            StatusCode::INTERNAL_SERVER_ERROR,
        );
    };

    // The turn is carried on each entry rather than announced between them,
    // because a stored transcript is read in one piece rather than streamed.
    let mut entries = Vec::new();
    let mut usage = None;
    let mut grouped = false;
    for held in &stored.entries {
        // Usage is state rather than an item in the conversation, so only the
        // latest is reported, through the same field a live session uses. A
        // stopped session has no other way to say what it cost.
        if let SessionEvent::Usage { usage: latest } = &held.entry {
            usage = Some(serde_json::to_value(latest).unwrap_or(Value::Null));
            continue;
        }
        grouped |= held.turn.is_some();
        entries.push(wire(&held.entry, held.at, held.turn, server.names.as_ref()));
    }

    let mut body = json!({
        "entries": entries,
        "dropped": stored.dropped,
        "grouped": grouped,
    });
    if let Some(usage) = usage {
        body["usage"] = usage;
    }
    ok(body)
}

/// Lists a directory inside a session's project.
async fn tree(
    State(server): State<ServerState>,
    PathParam(id): PathParam<String>,
    Query(query): Query<PathQuery>,
) -> Response {
    let located = match server.locate(&id, query.path.as_deref().unwrap_or("")) {
        Ok(located) => located,
        Err(refusal) => return refusal,
    };

    let listed = blocking(move || {
        let metadata = located.metadata()?;
        if !metadata.is_dir() {
            return Ok(None);
        }
        Ok::<_, std::io::Error>(Some(
            read_directory(&located.root, &located.relative).unwrap_or_default(),
        ))
    })
    .await;
    match listed {
        Ok(Ok(Some(entries))) => ok(json!(
            entries
                .into_iter()
                .map(|entry| json!({
                    "name": entry.name,
                    "path": entry.path,
                    "directory": entry.directory,
                    "size": entry.size,
                }))
                .collect::<Vec<_>>()
        )),
        Ok(Ok(None)) => refused(
            json!({ "error": "not a directory" }),
            StatusCode::BAD_REQUEST,
        ),
        _ => refused(
            json!({ "error": "no such directory" }),
            StatusCode::NOT_FOUND,
        ),
    }
}

/// Reads one file inside a session's project, for display.
async fn file(
    State(server): State<ServerState>,
    PathParam(id): PathParam<String>,
    Query(query): Query<PathQuery>,
) -> Response {
    let located = match server.locate(&id, query.path.as_deref().unwrap_or("")) {
        Ok(located) => located,
        Err(refusal) => return refusal,
    };

    let secrets = server.sessions.secrets_for(&id);
    // Every way of not finding one is answered the same, so the reason is
    // not kept: it is the error text that would describe the host.
    let read = blocking(move || {
        let metadata = located.metadata().map_err(drop)?;
        if metadata.is_dir() {
            return Ok(None);
        }
        read_file_for_display(&located.root, &located.relative, MAX_INLINE_BYTES)
            .map(Some)
            .map_err(drop)
    })
    .await;
    match read {
        // The agent writes these, and it holds whatever credentials its
        // sandbox was given. The transcript is scrubbed for the same reason,
        // and a file is no less public.
        Ok(Ok(Some(contents))) => ok(json!({
            "path": contents.path,
            "size": contents.size,
            "binary": contents.binary,
            "truncated": contents.truncated,
            "text": redact_text(&contents.text, &secrets),
            "language": contents.language,
        })),
        Ok(Ok(None)) => refused(
            json!({ "error": "is a directory" }),
            StatusCode::BAD_REQUEST,
        ),
        _ => refused(json!({ "error": "no such file" }), StatusCode::NOT_FOUND),
    }
}

/// Sends one file inside a session's project to the browser.
async fn download(
    State(server): State<ServerState>,
    PathParam(id): PathParam<String>,
    Query(query): Query<PathQuery>,
) -> Response {
    let located = match server.locate(&id, query.path.as_deref().unwrap_or("")) {
        Ok(located) => located,
        Err(refusal) => return refusal,
    };

    let Ok(file) = paths::open_beneath(
        &located.root,
        &located.relative,
        &paths::OpenOptions::read(),
    ) else {
        return refused(json!({ "error": "no such file" }), StatusCode::NOT_FOUND);
    };
    let file = tokio::fs::File::from_std(file);

    let name = download_name(&located.relative);
    // Scrubbed as it goes, for the same reason the file view is.
    let scrubber = ByteScrubber::new(&server.sessions.secrets_for(&id));
    let chunks = futures_util::stream::try_unfold(
        (Some(file), scrubber),
        |(file, mut scrubber)| async move {
            use tokio::io::AsyncReadExt;
            let Some(mut file) = file else {
                return Ok(None);
            };
            let mut buffer = vec![0u8; 65_536];
            match AsyncReadExt::read(&mut file, &mut buffer).await {
                Ok(0) => {
                    let rest = scrubber.finish();
                    Ok(Some((rest, (None, scrubber))))
                }
                Ok(read) => {
                    let passed = scrubber.push(&buffer[..read]);
                    Ok(Some((passed, (Some(file), scrubber))))
                }
                Err(error) => Err::<_, std::io::Error>(error),
            }
        },
    );
    Response::builder()
        .header(header::CONTENT_TYPE, "application/octet-stream")
        .header(
            header::CONTENT_DISPOSITION,
            format!("attachment; filename=\"{name}\""),
        )
        .body(axum::body::Body::from_stream(chunks))
        .unwrap_or_else(|_| unreachable!("a download response always builds"))
}

/// Runs file work on the blocking pool.
///
/// Reading a project is ordinary file access, which blocks. On the async
/// workers a slow disk, or a directory with a million entries in it, would
/// hold up every other request and the chat connection with them.
/// A file a session posted, as kept beside its record.
///
/// Only a picture or a clip is shown in place. Anything else is offered as a
/// download, an SVG included, since a browser runs the script one can carry.
/// Scrubbed like a download, so a secret that slipped into one is not served.
async fn upload(
    State(server): State<ServerState>,
    PathParam((id, file)): PathParam<(String, String)>,
) -> Response {
    let missing = || refused(json!({ "error": "no such file" }), StatusCode::NOT_FOUND);
    if !is_kept_name(&file) {
        return missing();
    }
    let Some(state_dir) = server.sessions.state_dir_of(&id) else {
        return missing();
    };
    let path = std::path::Path::new(&record_dir(&state_dir))
        .join(UPLOADS_DIRNAME)
        .join(&file);
    let read = blocking(move || {
        use std::io::Read;
        use std::os::unix::fs::OpenOptionsExt;
        let mut bytes = Vec::new();
        std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(nix::fcntl::OFlag::O_NOFOLLOW.bits())
            .open(path)
            .and_then(|mut opened| opened.read_to_end(&mut bytes))
            .map(|_| bytes)
    })
    .await;
    let Ok(Ok(bytes)) = read else {
        return missing();
    };

    let mut scrubber = ByteScrubber::new(&server.sessions.secrets_for(&id));
    let mut body = scrubber.push(&bytes);
    body.extend(scrubber.finish());

    let shown = shown_type(&file);
    let disposition = match shown {
        Some(_) => "inline".to_owned(),
        None => format!("attachment; filename=\"{}\"", download_name(&file)),
    };
    Response::builder()
        .header(
            header::CONTENT_TYPE,
            shown.unwrap_or("application/octet-stream"),
        )
        .header(header::CONTENT_DISPOSITION, disposition)
        .header(header::X_CONTENT_TYPE_OPTIONS, "nosniff")
        .header(header::CACHE_CONTROL, "private, max-age=86400, immutable")
        .body(axum::body::Body::from(body))
        .unwrap_or_else(|_| unreachable!("an upload response always builds"))
}

/// The type a kept file is shown in place as, by its extension, or nothing
/// for a file that is only offered as a download.
fn shown_type(file: &str) -> Option<&'static str> {
    let extension = file.rsplit_once('.')?.1.to_ascii_lowercase();
    Some(match extension.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        "ogg" | "oga" | "opus" => "audio/ogg",
        "flac" => "audio/flac",
        "m4a" => "audio/mp4",
        _ => return None,
    })
}

async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> T + Send + 'static,
) -> Result<T, tokio::task::JoinError> {
    tokio::task::spawn_blocking(work).await
}

/// The name a download is saved under, made safe to put in a header.
///
/// The agent names its files, and a newline or a quote in one would either
/// break the header or end the name early.
fn download_name(relative: &str) -> String {
    let name: String = relative
        .rsplit('/')
        .next()
        .unwrap_or_default()
        .chars()
        .map(|c| {
            if c.is_ascii_graphic() && c != '"' && c != '\\' || c == ' ' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if name.trim().is_empty() {
        "file".to_owned()
    } else {
        name
    }
}

/// Serves the built bundle, falling back to the application itself.
///
/// Normalised and then checked, so a traversal in the request cannot reach
/// outside the built assets. Anything unrecognised is the interface, so a
/// reload of a deep path still lands on the application rather than on a
/// blank 404.
async fn serve_asset(
    State(server): State<ServerState>,
    request: axum::extract::Request,
) -> Response {
    let wanted = request.uri().path();
    if wanted.starts_with("/api/") {
        return refused(json!({ "error": "no such route" }), StatusCode::NOT_FOUND);
    }
    let wanted = if wanted == "/" { "/index.html" } else { wanted };
    // Normalised before it is looked up, so a traversal cannot name a file
    // the bundle does not offer under a path that reads as though it does.
    let Some(resolved) = normalize_request(wanted) else {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    };

    if let Some(bytes) = bundled(server.assets, &resolved) {
        return ([(header::CONTENT_TYPE, content_type(&resolved))], bytes).into_response();
    }
    match bundled(server.assets, "index.html") {
        Some(bytes) => ([(header::CONTENT_TYPE, TYPES[0].1)], bytes).into_response(),
        None => (StatusCode::NOT_FOUND, "not found").into_response(),
    }
}

#[cfg(test)]
mod tests;
