//! The local web interface.
//!
//! Serves the built assets and a small API over them. It holds no state of
//! its own: sessions, their output, and their history all live in the session
//! layer, and a browser is just another view attached to them.
//!
//! There is no authentication, deliberately. The bind address is the access
//! control, and it is checked before anything is served.

use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};

use axum::extract::{Path as PathParam, Query, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use serde_json::{Value, json};

use crate::config::schema::WebConfig;
use crate::log::LogValue;
use crate::log::Logger;
use crate::log::fields;
use crate::log::now_ms;
use crate::sandbox::paths::host_path_under;
use crate::session::event::SessionEvent;
use crate::session::files::MAX_INLINE_BYTES;
use crate::session::files::{read_directory, read_file_for_display};
use crate::session::manager::StartOutcome;
use crate::session::manager::{DetachedRequest, SessionManager};
use crate::session::record::transcript_path;
use crate::session::session::IncomingMessage;
use crate::session::transcript::Transcript;
use crate::web::address::check_bind_address;
use crate::web::view::{NameLookup, WebView, wire};

/// The identity a request from the interface acts under.
///
/// Reaching the interface already means being on the operator's own network,
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

/// Resolves a request path inside the assets root, refusing any escape.
fn normalize_under(root: &Path, wanted: &str) -> Option<PathBuf> {
    // A request path always names something under the root; the leading
    // slash is the URL's, not the start of an absolute host path.
    let wanted = wanted.trim_start_matches('/');
    let mut resolved = root.to_path_buf();
    for component in Path::new(wanted).components() {
        match component {
            Component::CurDir => {}
            Component::Normal(part) => resolved.push(part),
            // Absolute or prefix-bearing requests leave the root by
            // construction, and a parent that escapes it is refused below.
            Component::ParentDir => {
                resolved.pop();
            }
            _ => return None,
        }
    }
    resolved.starts_with(root).then_some(resolved)
}

/// The interface, bound to one address.
pub struct WebServer {
    config: WebConfig,
    sessions: Arc<SessionManager>,
    assets: PathBuf,
    log: Logger,
    guild_id: Option<String>,
    names: Option<NameLookup>,
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
    #[serde(default)]
    project: Option<String>,
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
    pub fn new(
        config: WebConfig,
        sessions: Arc<SessionManager>,
        assets: PathBuf,
        log: Logger,
        guild_id: Option<String>,
        names: Option<NameLookup>,
    ) -> Arc<Self> {
        Arc::new(Self {
            config,
            sessions,
            assets,
            log,
            guild_id,
            names,
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

        if !self.assets.join("index.html").exists() {
            return Err(WebInterfaceError(format!(
                "the interface is not built. Run `deno task build:web` to produce {}.",
                self.assets.display()
            )));
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
            .route("/api/interface", get(describe))
            .route("/api/sessions", get(list_sessions).post(start_session))
            .route("/api/sessions/{id}/send", post(send))
            .route("/api/sessions/{id}/stream", get(stream_session))
            .route("/api/sessions/{id}/transcript", get(transcript))
            .route("/api/sessions/{id}/tree", get(tree))
            .route("/api/sessions/{id}/file", get(file))
            .route("/api/sessions/{id}/download", get(download))
            .fallback(serve_asset)
            .with_state(state)
    }

    /// Refuses anything that would change something, when configured to
    /// observe.
    fn observing(&self) -> Option<Response> {
        self.config.observer.then(|| {
            refused(
                json!({ "error": "the interface is an observer and cannot change anything" }),
                StatusCode::FORBIDDEN,
            )
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
    fn locate(&self, id: &str, requested: &str) -> Result<(String, String), Response> {
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

        let Some(host) = host_path_under(&project, &project, &relative) else {
            return Err(refused(
                json!({ "error": "outside this session's project" }),
                StatusCode::FORBIDDEN,
            ));
        };
        let relative = if relative == "." { "" } else { &relative };
        Ok((host, relative.to_owned()))
    }
}

/// What the interface is allowed to do, so it can show only what works.
///
/// An observer that still drew a composer would be offering something every
/// attempt at which is refused.
async fn describe(State(server): State<ServerState>) -> Response {
    ok(json!({
        "observer": server.config.observer,
        "guildId": server.guild_id,
    }))
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
    body: Option<axum::Json<StartBody>>,
) -> Response {
    if let Some(refusal) = server.observing() {
        return refusal;
    }

    let Some(axum::Json(body)) = body else {
        return refused(
            json!({ "error": "a prompt is required" }),
            StatusCode::BAD_REQUEST,
        );
    };
    let prompt = body.prompt.unwrap_or_default().trim().to_owned();
    let project = body.project.unwrap_or_default().trim().to_owned();
    if prompt.is_empty() {
        return refused(
            json!({ "error": "a prompt is required" }),
            StatusCode::BAD_REQUEST,
        );
    }

    let outcome = server
        .sessions
        .start_detached(DetachedRequest {
            project,
            prompt,
            owner_id: WEB_ACTOR.to_owned(),
            owner_name: Some("the interface".to_owned()),
        })
        .await;

    match outcome {
        StartOutcome::Started { session } => ok(json!({ "id": session.id() })),
        StartOutcome::Refused { reason } => {
            refused(json!({ "error": reason }), StatusCode::CONFLICT)
        }
    }
}

/// Hands a message to a live session, as if posted in its thread.
async fn send(
    State(server): State<ServerState>,
    PathParam(id): PathParam<String>,
    body: Option<axum::Json<SendBody>>,
) -> Response {
    if let Some(refusal) = server.observing() {
        return refusal;
    }

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

    // The same path a message in a thread takes, so prompts queue and
    // commands apply exactly as they do there.
    let delivered = server
        .sessions
        .deliver_to_session(
            &id,
            IncomingMessage {
                id: format!("web-{}", now_ms()),
                author_id: WEB_ACTOR.to_owned(),
                author_name: Some("the interface".to_owned()),
                content: text,
                attachments: Vec::new(),
            },
        )
        .await;

    if delivered {
        ok(json!({ "accepted": true, "detail": "sent" }))
    } else {
        refused(
            json!({ "accepted": false, "detail": "no such live session" }),
            StatusCode::NOT_FOUND,
        )
    }
}

/// Streams a live session to a browser, replaying its record from the start.
async fn stream_session(
    State(server): State<ServerState>,
    PathParam(id): PathParam<String>,
) -> Response {
    let (view, receiver) = WebView::new(server.names.clone());
    let Some(attached) = server.sessions.attach_view(&id, view.clone()).await else {
        return refused(
            json!({ "error": "no such live session" }),
            StatusCode::NOT_FOUND,
        );
    };

    // The browser closing drops the stream, which is the only signal that a
    // view has gone. Detaching then keeps the fan out from growing forever.
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_millis(1_000)).await;
            if view.is_closed() {
                attached.detach();
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

    let stored = Transcript::new(transcript_path(&record.state_dir), None).read();

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

    match std::fs::metadata(&located.0) {
        Ok(metadata) if metadata.is_dir() => {
            let entries = read_directory(&located.0, &located.1).unwrap_or_default();
            ok(json!(
                entries
                    .into_iter()
                    .map(|entry| json!({
                        "name": entry.name,
                        "path": entry.path,
                        "directory": entry.directory,
                        "size": entry.size,
                    }))
                    .collect::<Vec<_>>()
            ))
        }
        Ok(_) => refused(
            json!({ "error": "not a directory" }),
            StatusCode::BAD_REQUEST,
        ),
        Err(error) => refused(json!({ "error": error.to_string() }), StatusCode::NOT_FOUND),
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

    match std::fs::metadata(&located.0) {
        Ok(metadata) if metadata.is_dir() => refused(
            json!({ "error": "is a directory" }),
            StatusCode::BAD_REQUEST,
        ),
        Ok(_) => match read_file_for_display(&located.0, &located.1, MAX_INLINE_BYTES) {
            Ok(contents) => ok(json!({
                "path": contents.path,
                "size": contents.size,
                "binary": contents.binary,
                "truncated": contents.truncated,
                "text": contents.text,
                "language": contents.language,
            })),
            Err(error) => refused(json!({ "error": error.to_string() }), StatusCode::NOT_FOUND),
        },
        Err(error) => refused(json!({ "error": error.to_string() }), StatusCode::NOT_FOUND),
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

    let Ok(file) = tokio::fs::File::open(&located.0).await else {
        return refused(json!({ "error": "no such file" }), StatusCode::NOT_FOUND);
    };

    let name = located
        .1
        .rsplit('/')
        .next()
        .unwrap_or("file")
        .replace('"', "");
    let chunks = futures_util::stream::try_unfold(file, |mut file| async move {
        use tokio::io::AsyncReadExt;
        let mut buffer = vec![0u8; 65_536];
        match AsyncReadExt::read(&mut file, &mut buffer).await {
            Ok(0) => Ok(None),
            Ok(read) => {
                buffer.truncate(read);
                Ok(Some((buffer, file)))
            }
            Err(error) => Err(error),
        }
    });
    Response::builder()
        .header(header::CONTENT_TYPE, "application/octet-stream")
        .header(
            header::CONTENT_DISPOSITION,
            format!("attachment; filename=\"{name}\""),
        )
        .body(axum::body::Body::from_stream(chunks))
        .unwrap_or_else(|_| unreachable!("a download response always builds"))
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
    let Some(resolved) = normalize_under(&server.assets, wanted) else {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    };

    match tokio::fs::read(&resolved).await {
        Ok(bytes) => (
            [(
                header::CONTENT_TYPE,
                content_type(&resolved.to_string_lossy()),
            )],
            bytes,
        )
            .into_response(),
        Err(_) => match tokio::fs::read(server.assets.join("index.html")).await {
            Ok(bytes) => ([(header::CONTENT_TYPE, TYPES[0].1)], bytes).into_response(),
            Err(_) => (StatusCode::NOT_FOUND, "not found").into_response(),
        },
    }
}

#[cfg(test)]
mod tests;
