//! Starting the daemon: what runs, and in what order.
//!
//! Order matters. Configuration is validated, the lock is taken, the backend
//! is probed and its gaps reported, leftover sandboxes are swept, and only
//! then does the connection open. Nothing that could start an agent happens
//! before the isolation contract has been checked and reported.

use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use serenity::client::Context;
use serenity::client::EventHandler as SerenityEventHandler;
use serenity::model::application::Interaction;
use serenity::model::channel::Channel;
use serenity::model::id::ChannelId;

use crate::agent::protocol::AgentImage;
use crate::chat::commands::TranslatedCommand;
use crate::chat::commands::{acknowledge, register_commands};
use crate::chat::gateway::{Gateway, GatewayHandlers};
use crate::chat::render::{MESSAGE_LIMIT, split_message, when_relative, when_relative_plain};
use crate::chat::threads::ChatThreadFactory;
use crate::chat::threads::plain;
use crate::config::load::Environment;
use crate::config::redact::{redact_text, secret_values};
use crate::config::schema::{Config, EgressMode};
use crate::daemon::SlashCommand;
use crate::daemon::StartError;
use crate::daemon::{Daemon, DaemonOptions};
use crate::daemon::{create_sandbox, probe_sandbox};
use crate::lock::DaemonLock;
use crate::lock::acquire_lock;
use crate::log::now_ms;
use crate::log::{LogValue, Logger, fields};
use crate::memory::store::MemoryStore;
use crate::provider::gateway::{GATEWAY_USAGE, fetch_gateway_usage};
use crate::provider::models::{
    agent_directory, available_models, model_by_id, read_models, read_store,
};
use crate::provider::usage::Fetch;
use crate::provider::usage::HttpRequest;
use crate::provider::usage::HttpResponse;
use crate::provider::usage::QUOTA_TTL_MS;
use crate::provider::usage::Quota;
use crate::provider::usage::Window;
use crate::provider::usage::{
    FetchError, QuotaGate, UNKNOWN_QUOTA, UsageSource, is_spent, quota_message, spent_message,
    usage_status,
};
use crate::provider::vision::HttpPost;
use crate::provider::vision::image_describer;
use crate::provider::zai::{fetch_quota, meters_usage};
use crate::sandbox::bailey::ProviderBrokering;
use crate::sandbox::bailey::{EGRESS_MAP_ADDRESS, provider_prefix};
use crate::sandbox::broker::Broker;
use crate::sandbox::broker::ProviderRoute;
use crate::sandbox::paths;
use crate::session::manager::{CreatedThread, FoundView, ThreadFactory};
use crate::session::session::DescribeImages;
use crate::session::session::IncomingMessage;
use crate::session::session::Unavailable;
use crate::session::views::SessionView;
use crate::web::server::WEB_ACTOR;
use crate::web::server::WebServer;
use crate::web::view::NameLookup;

/// Filename of the memory database inside the state directory.
pub const MEMORY_FILENAME: &str = "memory.db";

/// How long the chat service has to answer a login.
const READY_TIMEOUT_MS: u64 = 30_000;

/// The one real fetch of a provider endpoint, over HTTPS.
struct HttpFetch;

impl Fetch for HttpFetch {
    async fn fetch(&self, url: String, request: HttpRequest) -> Result<HttpResponse, FetchError> {
        let client = reqwest::Client::new();
        let mut sent = client.get(&url);
        for (name, value) in &request.headers {
            sent = sent.header(name.as_str(), value.as_str());
        }
        let answer = sent
            .timeout(std::time::Duration::from_millis(request.timeout_ms))
            .send()
            .await
            .map_err(|error| FetchError(error.to_string()))?;
        let status = answer.status().as_u16();
        let body = answer.json::<serde_json::Value>().await.ok();
        Ok(HttpResponse { status, body })
    }
}

/// Powers off through logind, which is what a desktop session uses.
///
/// Its policy allows an active local session and asks for authentication
/// otherwise, so this works when the daemon was started from a logged-in seat
/// and fails with a reason when it was not.
async fn power_off() -> Option<String> {
    let output = tokio::process::Command::new("loginctl")
        .arg("poweroff")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .await
        .ok()?;
    if output.status.code() == Some(0) {
        return None;
    }

    let said = String::from_utf8_lossy(&output.stderr);
    let said = said.trim().lines().next().unwrap_or_default();
    let code = output.status.code().unwrap_or(-1);
    Some(if said.is_empty() {
        format!("could not power off: it exited with {code}")
    } else {
        format!("could not power off: {said}")
    })
}

/// Renders a reset time, or nothing where the provider did not give one.
fn when(at: Option<i64>, render: fn(i64) -> String) -> Option<String> {
    at.map(render)
}

/// Every provider on this host whose window can be asked about.
///
/// The configured one first, because it is what a session runs on unless the
/// opening message says otherwise, and so it is the one worth putting under
/// the bot's name. A defined provider is asked only where it says it serves a
/// usage endpoint: a base URL that does not is simply not asked, rather than
/// probed.
type BoxedGateRead =
    Box<dyn Fn() -> Pin<Box<dyn Future<Output = Option<Quota>> + Send>> + Send + Sync>;

fn usage_sources(config: &Config) -> Vec<UsageSource<BoxedGateRead>> {
    let mut sources = Vec::new();
    if meters_usage(&config.agent.provider) {
        let credential = config.agent.credential.clone();
        sources.push(UsageSource {
            provider: config.agent.provider.clone(),
            gate: QuotaGate::new(
                Box::new(move || {
                    let credential = credential.clone();
                    Box::pin(async move {
                        let fetch = HttpFetch;
                        fetch_quota(&credential, &fetch, 10_000).await
                    }) as Pin<Box<dyn Future<Output = Option<Quota>> + Send>>
                }) as BoxedGateRead,
                now_ms,
            ),
        });
    }

    for (name, definition) in &config.agent.providers {
        let Some(fields) = definition.as_object() else {
            continue;
        };
        if fields.get("usage").and_then(serde_json::Value::as_str) != Some(GATEWAY_USAGE) {
            continue;
        }
        let (Some(base_url), Some(credential)) = (
            fields.get("baseUrl").and_then(serde_json::Value::as_str),
            fields.get("credential").and_then(serde_json::Value::as_str),
        ) else {
            continue;
        };
        let base_url = base_url.to_owned();
        let credential = credential.to_owned();
        sources.push(UsageSource {
            provider: name.clone(),
            gate: QuotaGate::new(
                Box::new(move || {
                    let base_url = base_url.clone();
                    let credential = credential.clone();
                    Box::pin(async move {
                        let fetch = HttpFetch;
                        fetch_gateway_usage(&base_url, &credential, &fetch, 10_000).await
                    }) as Pin<Box<dyn Future<Output = Option<Quota>> + Send>>
                }) as BoxedGateRead,
                now_ms,
            ),
        });
    }
    sources
}

/// The defined providers the daemon can stand in front of.
///
/// A definition is brokerable when it says where the provider is and what the
/// key to it is: without the base URL there is nowhere to forward, and
/// without the credential there is nothing to put on. One that says neither
/// is left alone, and the agent reaches it however it would have.
fn brokerable_providers(
    defined: &serde_json::Map<String, serde_json::Value>,
) -> Vec<(String, String, String)> {
    let mut found = Vec::new();
    for (name, definition) in defined {
        let Some(fields) = definition.as_object() else {
            continue;
        };
        let (Some(upstream), Some(credential)) = (
            fields.get("baseUrl").and_then(serde_json::Value::as_str),
            fields.get("credential").and_then(serde_json::Value::as_str),
        ) else {
            continue;
        };
        if upstream.trim().is_empty() || credential.trim().is_empty() {
            continue;
        }
        found.push((
            name.clone(),
            upstream.trim().to_owned(),
            credential.to_owned(),
        ));
    }
    found
}

/// A per-run stand-in for the provider credential.
///
/// Drawn from the system generator rather than anything derived from the
/// credential, so holding the nonce says nothing about the key it stands
/// for. It lasts as long as the daemon: the broker is the only thing that
/// honours it, and a restart brings a new one.
fn provider_nonce() -> String {
    use std::fmt::Write as _;
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).expect("the system generator is available");
    bytes.iter().fold(String::new(), |mut out, byte| {
        let _ = write!(out, "{byte:02x}");
        out
    })
}

/// The lower-cased host of a base URL, for the egress allowlist.
fn host_of(base_url: &str) -> Option<String> {
    let rest = base_url
        .strip_prefix("https://")
        .or_else(|| base_url.strip_prefix("http://"))?;
    let host: String = rest
        .split(['/', ':', '?', '#'])
        .next()
        .unwrap_or_default()
        .to_lowercase();
    (!host.is_empty()).then_some(host)
}

/// What the daemon reports when it stops.
///
/// These are a contract with whoever runs the daemon: the table in
/// `docs/start.md` names them, so a service can tell a refusal from a crash.
/// The numbers are chosen rather than incidental.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
pub enum Exit {
    /// Served, and stopped when it was asked to.
    Served = 0,
    /// Something failed while running that was not a refusal.
    Failed = 1,
    /// The configuration, the sandbox backend, or the token was refused.
    Refused = 2,
    /// The backend cannot enforce a guarantee the configuration demands.
    EnforcementGap = 3,
    /// Another daemon already holds this state directory.
    AlreadyRunning = 4,
}

impl Exit {
    /// The number the process exits with.
    pub fn code(self) -> i32 {
        self as i32
    }
}

/// Runs the daemon until it is told to stop.
///
/// Serving is the steady state, so this returns only on a signal or when the
/// connection has been lost for good.
pub async fn serve(config: Config, log: Logger) -> Exit {
    let secrets = secret_values(&config);

    // Taken before anything connects or spawns, so a second daemon fails fast
    // instead of racing the first one for every message that arrives.
    let _ = std::fs::create_dir_all(&config.state_dir);
    let pid =
        i32::try_from(std::process::id()).expect("a pid fits an i32 everywhere the daemon runs");
    let mut lock = match acquire_lock(&config.state_dir, pid) {
        Ok(lock) => lock,
        Err(error) => {
            log.error(&error.to_string(), &fields([]));
            return Exit::AlreadyRunning;
        }
    };

    let mut served_channel = ChannelId::new(config.chat.channel_id.parse().unwrap_or(0));
    let code = run(&config, &log, &secrets, &mut lock, &mut served_channel).await;
    // Released on every path, including a startup that never got as far as
    // connecting. A lock left behind is taken over next time because its
    // holder is gone, but leaving one is still a puzzle for whoever finds it.
    lock.release();
    code
}

/// Refuses to start when house rules were named but cannot be read.
///
/// Named but unreadable is a refusal, not a warning. An operator who
/// configured house rules believes every session carries them, and a typo that
/// only ever showed up as a line in a log would leave that belief standing
/// while no session actually got them.
fn check_house_rules(config: &Config, log: &Logger) -> Result<(), Exit> {
    let Some(rules_path) = &config.agent.rules_path else {
        return Ok(());
    };
    match std::fs::read_to_string(rules_path) {
        Ok(_) => {
            log.info(
                "house rules will be given to every session",
                &fields([("path", LogValue::from(rules_path.as_str()))]),
            );
            Ok(())
        }
        Err(error) => {
            log.error(
                &format!("agent.rulesPath cannot be read: {rules_path}"),
                &fields([]),
            );
            log.error(&error.to_string(), &fields([]));
            Err(Exit::Refused)
        }
    }
}

/// Keeps the usage window shown under the bot's name up to date.
///
/// A reconnect resets the presence, so the status is put back by the same call
/// that first set it rather than only at startup. The gate holds its answer
/// for `QUOTA_TTL_MS` and stops asking entirely once the window is spent, so
/// refreshing on that same interval adds no requests the daemon was not
/// already making. A window that cannot be read clears the status rather than
/// leaving a stale number under the bot's name.
fn refresh_presence(
    sources: &Arc<tokio::sync::Mutex<Vec<UsageSource<BoxedGateRead>>>>,
    gateway: &Arc<Gateway>,
) {
    let status_sources = Arc::clone(sources);
    let status_gateway = Arc::clone(gateway);
    tokio::spawn(async move {
        loop {
            let mut windows = Vec::new();
            {
                let mut sources = status_sources.lock().await;
                for source in sources.iter_mut() {
                    let quota = source.gate.current().await;
                    windows.push(quota.map(|quota| {
                        let relative = quota.resets_at.map(|at| when_relative_plain(at, now_ms()));
                        Window {
                            provider: source.provider.clone(),
                            quota,
                            relative,
                        }
                    }));
                }
            }
            if let Some(status) = usage_status(&windows.into_iter().flatten().collect::<Vec<_>>()) {
                status_gateway.set_status(Some(&status));
            } else {
                status_gateway.set_status(None);
            }
            tokio::time::sleep(std::time::Duration::from_millis(QUOTA_TTL_MS as u64)).await;
        }
    });
}

/// The broker a session's egress is forced through, when one is asked for.
///
/// Nothing here is set under any other egress mode, so the three travel
/// together rather than as three options the caller has to keep in step.
struct Brokered {
    /// The running broker, held so shutdown can close it.
    broker: Broker,
    /// The loopback port the sandbox reaches it on.
    proxy_port: u16,
    /// What stands in for each provider credential inside a sandbox.
    brokering: Option<ProviderBrokering>,
}

/// Starts the egress broker and works out what a sandbox is told about it.
///
/// Under `egress.mode = proxy` every session's outbound is forced through one
/// broker the daemon runs here on the host. Its allowlist is the provider,
/// which a session cannot work without, plus whatever the operator named. The
/// provider host is read from the model store the agent would reach it at.
///
/// Returns nothing when the mode asks for no broker, and an exit code when
/// one was asked for and could not be had.
async fn start_broker(config: &Config, log: &Logger) -> Result<Option<Brokered>, Exit> {
    if config.sandbox.egress.mode != EgressMode::Proxy {
        return Ok(None);
    }
    let env = host_environment();
    let store = agent_directory(&env);
    let provider_base = model_by_id(
        &read_models(store.as_deref(), config.agent.provider.as_str()),
        config.agent.model.as_deref(),
    )
    .and_then(|model| model.base_url.clone());
    let provider_host = provider_base.as_deref().and_then(host_of);
    let mut allow: Vec<String> = provider_host
        .as_ref()
        .map(|host| vec![host.clone()])
        .unwrap_or_default();
    allow.extend(config.sandbox.egress.allow.iter().cloned());
    if allow.is_empty() {
        log.error(
            "egress.mode is proxy but no host is allowed: name the provider host or set egress.allow",
            &fields([]),
        );
        return Err(Exit::Refused);
    }
    // The credential is held back from the session and put on here
    // instead, so what a sandbox carries is a nonce that is worth nothing
    // anywhere else. One route per provider whose upstream and credential
    // the daemon knows, each with a nonce of its own.
    let mut nonces = BTreeMap::new();
    let mut routes: Vec<ProviderRoute> = Vec::new();
    if let Some(provider_base) = &provider_base {
        let nonce = provider_nonce();
        nonces.insert(config.agent.provider.clone(), nonce.clone());
        routes.push(ProviderRoute {
            prefix: provider_prefix(&config.agent.provider),
            upstream: provider_base.clone(),
            nonce,
            credential: config.agent.credential.clone(),
        });
    }
    for (name, upstream, credential) in brokerable_providers(&config.agent.providers) {
        let nonce = provider_nonce();
        nonces.insert(name.clone(), nonce.clone());
        routes.push(ProviderRoute {
            prefix: provider_prefix(&name),
            upstream,
            nonce,
            credential,
        });
    }
    let brokering = (!nonces.is_empty()).then(|| ProviderBrokering {
        credential_name: config.agent.credential_name.clone(),
        provider: config.agent.provider.clone(),
        nonces,
    });
    let mut broker_instance = Broker::new(
        allow.clone(),
        log.clone(),
        routes.clone(),
        config.sandbox.egress.allow_internal,
    );
    // The listener sits on loopback; 169.254.169.1 is only what bailey
    // tells the sandbox to dial, mapped back to this host from inside.
    let proxy_port = match broker_instance.listen("127.0.0.1").await {
        Ok(port) => {
            log.info(
                "egress is brokered",
                &fields([
                    (
                        "via",
                        LogValue::from(format!("{EGRESS_MAP_ADDRESS}:{port}")),
                    ),
                    ("allow", LogValue::from(allow.join(", "))),
                ]),
            );
            port
        }
        Err(error) => {
            log.error(
                &format!("the egress broker could not be started: {error}"),
                &fields([]),
            );
            return Err(Exit::Refused);
        }
    };
    if routes.is_empty() {
        log.warn(
            "the model store does not say where the provider is, so the credential is given to the session",
            &fields([]),
        );
    } else {
        log.info(
            "provider credentials stay outside the sandbox",
            &fields([("providers", LogValue::from(routes.len()))]),
        );
    }
    Ok(Some(Brokered {
        broker: broker_instance,
        proxy_port,
        brokering,
    }))
}

/// Everything between taking the lock and giving it back.
///
/// Wiring is linear and each piece names itself, so the length is the
/// startup order, not a tangle; splitting it would hide that order.
#[expect(clippy::too_many_lines)]
async fn run(
    config: &Config,
    log: &Logger,
    secrets: &[String],
    lock: &mut DaemonLock,
    served_channel: &mut ChannelId,
) -> Exit {
    // Asked before anything starts: without it the daemon runs but cannot
    // read a session's own files, which is a puzzle rather than a failure.
    if !paths::containment_is_enforced() {
        log.error(
            "this kernel does not support openat2, which is how a session's files are kept \
             inside its project; Linux 5.6 or newer is required",
            &fields([]),
        );
        return Exit::Refused;
    }

    if let Err(code) = check_house_rules(config, log) {
        return code;
    }

    let brokered = match start_broker(config, log).await {
        Ok(brokered) => brokered,
        Err(code) => return code,
    };
    let egress_proxy_port = brokered.as_ref().map(|brokered| brokered.proxy_port);
    let brokering = brokered
        .as_ref()
        .and_then(|brokered| brokered.brokering.clone());
    let mut broker = brokered.map(|brokered| brokered.broker);

    // The sandbox is checked before the chat service is touched, so a missing
    // image or an unenforceable guarantee fails immediately rather than after
    // a login round trip.
    let sandbox = Arc::new(create_sandbox(
        config,
        log.clone(),
        egress_proxy_port,
        brokering,
    ));
    let report = match probe_sandbox(sandbox.as_ref(), config, log).await {
        Ok(report) => report,
        Err(error) => {
            log.error(&error.to_string(), &fields([]));
            return match error {
                StartError::Unavailable(_) => Exit::Refused,
                StartError::EnforcementGap(_) => Exit::EnforcementGap,
            };
        }
    };

    // The connection signals readiness during connect, before the thread
    // factory and the daemon exist, so the handlers reach them through
    // holders rather than closing over bindings that are not initialised
    // yet.
    let sessions_holder: Arc<tokio::sync::Mutex<Option<Arc<Daemon>>>> =
        Arc::new(tokio::sync::Mutex::new(None));

    // Every thread's outbox follows this one flag, so a reconnect drains what
    // buffered while the gateway was down without the daemon having to keep a
    // register of which threads are live.
    let (connection, watching) = tokio::sync::watch::channel(true);
    let connection = Arc::new(connection);

    let gateway = Gateway::new(
        config.chat.clone(),
        GatewayHandlers {
            on_message: {
                let holder = Arc::clone(&sessions_holder);
                Arc::new(move |raw, decision| {
                    let holder = Arc::clone(&holder);
                    tokio::spawn(async move {
                        let guard = holder.lock().await;
                        if let Some(daemon) = guard.as_ref() {
                            daemon.handle(raw, decision).await;
                        }
                    });
                })
            },
            on_command: {
                let holder = Arc::clone(&sessions_holder);
                Arc::new(
                    move |command: TranslatedCommand, ack: Arc<dyn Fn(&str) + Send + Sync>| {
                        let holder = Arc::clone(&holder);
                        tokio::spawn(async move {
                            let answer = match guard_sessions(&holder).await {
                                Some(daemon) => {
                                    daemon
                                        .run_command(&SlashCommand {
                                            thread_id: command.thread_id.clone(),
                                            user_id: command.user_id.clone(),
                                            user_name: command.user_name.clone(),
                                            content: command.content.clone(),
                                        })
                                        .await
                                }
                                None => "the daemon is not ready".to_owned(),
                            };
                            ack(&answer);
                            // The service reports an interaction nobody answered
                            // as the bot being broken, so a private nod always
                            // goes out even when the answer ran long.
                            let _ = acknowledge;
                        });
                    },
                )
            },
            on_thread_closed: {
                let holder = Arc::clone(&sessions_holder);
                Arc::new(move |thread_id| {
                    let holder = Arc::clone(&holder);
                    tokio::spawn(async move {
                        if let Some(daemon) = guard_sessions(&holder).await {
                            daemon.thread_closed(&thread_id).await;
                        }
                    });
                })
            },
            on_withdrawn: {
                let holder = Arc::clone(&sessions_holder);
                Arc::new(move |message_id, thread_id| {
                    let holder = Arc::clone(&holder);
                    tokio::spawn(async move {
                        if let Some(daemon) = guard_sessions(&holder).await {
                            daemon.withdraw(&message_id, thread_id.as_deref()).await;
                        }
                    });
                })
            },
            on_connected: {
                let log = log.clone();
                let connection = Arc::clone(&connection);
                Arc::new(move || {
                    let _ = connection.send(true);
                    log.info("connected", &fields([]));
                })
            },
            on_disconnected: {
                let log = log.clone();
                let connection = Arc::clone(&connection);
                Arc::new(move || {
                    let _ = connection.send(false);
                    log.warn(
                        "disconnected; sessions keep running and output is buffered",
                        &fields([]),
                    );
                })
            },
            on_gave_up: {
                let log = log.clone();
                Arc::new(move |attempts| {
                    log.error(
                        "reconnection failed for good; the daemon gives up",
                        &fields([("attempts", LogValue::from(i64::from(attempts)))]),
                    );
                    std::process::exit(2);
                })
            },
        },
        log.clone(),
    );

    // A real serenity client owns the socket; the gateway is its handler.
    let mut client = match serenity::Client::builder(&config.chat.token, Gateway::intents())
        .event_handler(GatewayHandler(Arc::clone(&gateway)))
        .await
    {
        Ok(client) => client,
        Err(error) => {
            if Gateway::login_is_permanent(&error) {
                log.error(
                    "the chat service rejected the bot token or the intents; set chat.token and enable the Message Content intent",
                    &fields([]),
                );
                return Exit::Refused;
            }
            log.error(
                "the daemon failed to start",
                &fields([("detail", LogValue::from(error.to_string()))]),
            );
            return Exit::Failed;
        }
    };
    let http = Arc::clone(&client.http);
    tokio::spawn(async move {
        if let Err(error) = client.start().await {
            eprintln!("the chat connection ended: {error}");
            std::process::exit(2);
        }
    });
    if let Err(error) = gateway.wait_ready(READY_TIMEOUT_MS).await {
        log.error(
            &format!("{error}; the chat service did not answer"),
            &fields([]),
        );
        return Exit::Refused;
    }

    let thread_factory = Arc::new(ChatThreadFactory::new(
        ChannelId::new(config.chat.channel_id.parse().unwrap_or(0)),
        Arc::clone(&http),
        log.clone(),
        config.output.forward_tool_output,
        watching,
    ));

    let memory = Arc::new(
        MemoryStore::open(std::path::Path::new(&config.state_dir).join(MEMORY_FILENAME))
            .expect("the memory store opens"),
    );

    // Every provider this host can ask about, because somebody deciding what
    // to start wants to know which one has room.
    let sources = usage_sources(config);
    let sources = Arc::new(tokio::sync::Mutex::new(sources));
    if !sources.lock().await.is_empty() {
        refresh_presence(&sources, &gateway);
    }

    let env = host_environment();
    let store = agent_directory(&env);
    let read = read_store(store.as_deref(), config.agent.provider.as_str());
    if read.skipped > 0 {
        // Said once, at startup: the store is written by something other than
        // errand, and an operator can only fix what they are told about.
        log.warn(
            "the model store holds entries that are not models, and they were skipped",
            &fields([
                ("provider", LogValue::from(config.agent.provider.as_str())),
                ("skipped", LogValue::from(read.skipped)),
            ]),
        );
    }
    let models = read.models;

    let delegate = config.agent.delegate.clone();
    let delegate_base_url = delegate.as_ref().and_then(|delegate| {
        delegate
            .base_url
            .clone()
            .or_else(|| {
                model_by_id(&models, Some(&delegate.model)).and_then(|model| model.base_url.clone())
            })
            .or_else(|| {
                config.agent.model.as_deref().and_then(|model| {
                    model_by_id(&models, Some(model)).and_then(|m| m.base_url.clone())
                })
            })
    });
    if let Some(delegate) = &delegate
        && delegate_base_url.is_none()
    {
        log.warn(
            "delegation is configured but there is nowhere to send it",
            &fields([("model", LogValue::from(delegate.model.clone()))]),
        );
    }

    let describer = image_describer(&config.agent, store.as_deref(), HttpPost);
    if let Some(describer) = &describer {
        log.info(
            "images will be described for this model",
            &fields([
                (
                    "model",
                    LogValue::from(config.agent.model.clone().unwrap_or_default()),
                ),
                ("by", LogValue::from(describer.model.clone())),
            ]),
        );
    }

    // A request from the interface acts as an operator, which is the
    // authority that reaching a private listener already implies. An observer
    // gets none.
    let web_operates = config.web.as_ref().is_some_and(|web| !web.observer);
    let mut operator_ids = config.chat.operator_user_ids.clone();
    if web_operates {
        operator_ids.push(WEB_ACTOR.to_owned());
    }

    let unavailable: Option<Unavailable> = if sources.lock().await.is_empty() {
        None
    } else {
        let sources = Arc::clone(&sources);
        // Wrapping gates in Arc needs interior mutability per gate; the gates
        // are per provider and their caching is internal, so they are driven
        // through a mutex.
        Some(Arc::new(move |provider: &str| {
            let sources = Arc::clone(&sources);
            let provider = provider.to_owned();
            Box::pin(async move {
                let mut sources = sources.lock().await;
                let source = sources
                    .iter_mut()
                    .find(|candidate| candidate.provider == provider)?;
                let window = source.gate.current().await;
                window.filter(is_spent).map(|window| {
                    spent_message(&provider, when(window.resets_at, when_relative).as_deref())
                })
            })
        }))
    };

    let daemon = Arc::new(Daemon::new(DaemonOptions {
        config: config.clone(),
        sandbox,
        threads: {
            struct FactoryAdapter(Arc<ChatThreadFactory>);
            impl ThreadFactory for FactoryAdapter {
                fn create(
                    self: Arc<Self>,
                    message: IncomingMessage,
                    name: String,
                ) -> Pin<Box<dyn Future<Output = Result<CreatedThread, String>> + Send>>
                {
                    let starter =
                        serenity::model::id::MessageId::new(message.id.parse().unwrap_or(0));
                    let factory = Arc::clone(&self.0);
                    Box::pin(async move {
                        let (id, thread) = factory.create(starter, &name).await?;
                        Ok(CreatedThread {
                            id,
                            view: Arc::new(thread) as Arc<dyn SessionView>,
                        })
                    })
                }

                fn open(
                    self: Arc<Self>,
                    name: String,
                    opener: String,
                ) -> Pin<Box<dyn Future<Output = Result<CreatedThread, String>> + Send>>
                {
                    let factory = Arc::clone(&self.0);
                    Box::pin(async move {
                        let (id, thread) = factory.open(&name, &opener).await?;
                        Ok(CreatedThread {
                            id,
                            view: Arc::new(thread) as Arc<dyn SessionView>,
                        })
                    })
                }

                fn port_for(self: Arc<Self>, thread_id: String) -> FoundView {
                    let factory = Arc::clone(&self.0);
                    Box::pin(async move {
                        let id = thread_id.parse().ok()?;
                        factory
                            .port_for(id)
                            .await
                            .map(|thread| Arc::new(thread) as Arc<dyn SessionView>)
                    })
                }
            }
            Arc::new(FactoryAdapter(thread_factory)) as Arc<dyn ThreadFactory>
        },
        log: log.clone(),
        reply_in_channel: {
            let http = Arc::clone(&http);
            let secrets = secrets.to_vec();
            let log = log.clone();
            let served = *served_channel;
            Arc::new(move |message: IncomingMessage, text: String| {
                let http = Arc::clone(&http);
                let secrets = secrets.clone();
                let log = log.clone();
                Box::pin(async move {
                    reply_in_channel(&http, &log, &secrets, served, &message, &text).await;
                })
            })
        },
        memory: Some(Arc::clone(&memory)),
        power_off: Some(Arc::new(|| {
            Box::pin(power_off()) as Pin<Box<dyn Future<Output = Option<String>> + Send>>
        })),
        describe_usage: if sources.lock().await.is_empty() {
            None
        } else {
            let sources = Arc::clone(&sources);
            Some(Arc::new(move || {
                let sources = Arc::clone(&sources);
                Box::pin(async move {
                    let mut sources = sources.lock().await;
                    let mut lines = Vec::new();
                    for source in sources.iter_mut() {
                        let window = source.gate.current().await;
                        lines.push(match window {
                            None => format!("{}: {UNKNOWN_QUOTA}", source.provider),
                            Some(window) => quota_message(
                                &source.provider,
                                &window,
                                when(window.resets_at, when_relative).as_deref(),
                            ),
                        });
                    }
                    lines.join("\n")
                })
            }))
        },
        describe_images: describer.map(|describer| {
            let describer = Arc::new(describer);
            Arc::new(move |images: Vec<AgentImage>, question: String| {
                let describer = Arc::clone(&describer);
                Box::pin(async move {
                    describer
                        .describe(images, &question)
                        .await
                        .map_err(|error| error.to_string())
                }) as Pin<Box<dyn Future<Output = Result<String, String>> + Send>>
            }) as DescribeImages
        }),
        public_url: config.web.as_ref().and_then(|web| web.public_url.clone()),
        operator_ids: Some(operator_ids),
        available_models: available_models(&config.agent, store.as_deref()),
        delegate_base_url,
        unavailable,
    }));
    *sessions_holder.lock().await = Some(Arc::clone(&daemon));

    if daemon.start(Some(report)).await.is_err() {
        return Exit::Failed;
    }

    // Registered after startup, so a bot invited without the commands scope
    // reports that clearly instead of failing before it can serve anything.
    let served = ChannelId::new(config.chat.channel_id.parse().unwrap_or(0));
    let guild_id = served
        .to_channel(&http)
        .await
        .ok()
        .and_then(|channel| match channel {
            Channel::Guild(guild) => Some(guild.guild_id),
            _ => None,
        });
    if let Some(guild_id) = guild_id {
        daemon.set_guild(guild_id.get().to_string());
        match register_commands(&http, guild_id, log).await {
            Ok(()) => {}
            Err(error) => {
                log.warn(&error.to_string(), &fields([]));
                log.warn(
                    "slash commands are unavailable; the ! commands still work",
                    &fields([]),
                );
            }
        }
    }

    // Started after the daemon is accepting, so the interface never lists a
    // session the daemon is not yet ready to act on.
    let mut web: Option<Arc<WebServer>> = None;
    if let Some(web_config) = config.web.clone() {
        // Resolved lazily per name, so a mention reads as the person it names.
        let memory_for_names = Arc::clone(&memory);
        let names: NameLookup =
            Arc::new(move |id| memory_for_names.display_name(id).ok().flatten());
        let web_server = WebServer::new(
            web_config,
            daemon.sessions(),
            crate::web::server::BUILT_INTERFACE,
            log.clone(),
            guild_id.map(|guild| guild.get().to_string()),
            Some(names),
        );
        match web_server.start().await {
            Ok(()) => {
                log.info(
                    "ACCESS: anyone who can reach the interface acts with operator authority",
                    &fields([]),
                );
                log.info(
                    "  the agent's sandbox is unaffected by this; the interface is not sandboxed",
                    &fields([]),
                );
                if web_server.observer() {
                    log.info(
                        "  the interface is an observer and cannot change anything",
                        &fields([]),
                    );
                }
                web = Some(web_server);
            }
            Err(error) => {
                log.error(&error.to_string(), &fields([]));
                log.warn("continuing without the web interface", &fields([]));
            }
        }
    }

    log.info(
        "accepting messages",
        &fields([("channel", LogValue::from(config.chat.channel_id.as_str()))]),
    );

    wait_for_signal().await;

    log.info(
        "shutting down",
        &fields([("reason", LogValue::from("signal"))]),
    );
    if let Some(mut broker) = broker.take() {
        broker.close();
    }
    if let Some(web) = &web {
        web.stop();
    }
    daemon.shutdown().await;
    lock.release();
    Exit::Served
}

/// Hands the gateway to the chat library, which holds handlers behind an Arc
/// of its own.
struct GatewayHandler(Arc<Gateway>);

#[serenity::async_trait]
impl SerenityEventHandler for GatewayHandler {
    async fn message(&self, ctx: Context, message: serenity::model::channel::Message) {
        self.0.message(ctx, message).await;
    }

    async fn interaction_create(&self, ctx: Context, interaction: Interaction) {
        self.0.interaction_create(ctx, interaction).await;
    }

    async fn message_delete(
        &self,
        ctx: Context,
        channel_id: ChannelId,
        deleted_message_id: serenity::model::id::MessageId,
        guild_id: Option<serenity::model::id::GuildId>,
    ) {
        self.0
            .message_delete(ctx, channel_id, deleted_message_id, guild_id)
            .await;
    }

    async fn thread_update(
        &self,
        ctx: Context,
        old: Option<serenity::model::channel::GuildChannel>,
        new: serenity::model::channel::GuildChannel,
    ) {
        self.0.thread_update(ctx, old, new).await;
    }

    async fn thread_delete(
        &self,
        ctx: Context,
        thread: serenity::model::channel::PartialGuildChannel,
        full: Option<serenity::model::channel::GuildChannel>,
    ) {
        self.0.thread_delete(ctx, thread, full).await;
    }

    async fn shard_stage_update(
        &self,
        ctx: Context,
        event: serenity::gateway::ShardStageUpdateEvent,
    ) {
        self.0.shard_stage_update(ctx, event).await;
    }

    async fn ready(&self, ctx: Context, ready: serenity::model::gateway::Ready) {
        self.0.ready(ctx, ready).await;
    }
}

/// Reads the daemon's sessions behind the holder, for the gateway callbacks.
async fn guard_sessions(holder: &tokio::sync::Mutex<Option<Arc<Daemon>>>) -> Option<Arc<Daemon>> {
    holder.lock().await.clone()
}

/// The environment the process runs with, as the daemon reads it.
fn host_environment() -> Environment {
    std::env::vars().collect::<BTreeMap<_, _>>()
}

/// Says the refusal where it was asked, redacted, in pieces the service
/// takes.
///
/// Every step here used to fail into silence, so an answer the daemon had
/// already worked out simply never arrived and nothing said why. A reply
/// that cannot be delivered is worth a line: it is the difference between a
/// command that did nothing and one that was never heard.
async fn reply_in_channel(
    http: &serenity::http::Http,
    log: &Logger,
    secrets: &[String],
    channel_id: ChannelId,
    message: &IncomingMessage,
    text: &str,
) {
    let starter = channel_id
        .message(
            http,
            serenity::model::id::MessageId::new(message.id.parse().unwrap_or(0)),
        )
        .await
        .ok();
    let chunks = split_message(&redact_text(text, secrets), MESSAGE_LIMIT);
    if chunks.is_empty() {
        return;
    }

    let said = |error: serenity::Error| {
        log.warn(
            "the reply could not be sent",
            &fields([("detail", LogValue::from(error.to_string()))]),
        );
    };

    let Some(starter) = starter else {
        // Nothing to hang it off, so it is said in the channel: the answer
        // matters more than what it is attached to.
        for chunk in chunks {
            let _ = channel_id
                .send_message(http, plain(&chunk))
                .await
                .map_err(said);
        }
        return;
    };

    if chunks.len() == 1 {
        let reply = plain(&chunks[0]).reference_message(&starter);
        let _ = channel_id.send_message(http, reply).await.map_err(said);
        return;
    }

    // A long answer goes in a thread of its own rather than filling the
    // channel with it.
    let thread = channel_id
        .create_thread_from_message(
            http,
            starter.id,
            serenity::builder::CreateThread::new("answer"),
        )
        .await
        .map_err(said);
    let Ok(thread) = thread else { return };
    for chunk in chunks {
        let _ = thread.send_message(http, plain(&chunk)).await.map_err(said);
    }
}

/// Waits for the signals that stop the daemon.
async fn wait_for_signal() {
    use tokio::signal::unix::{SignalKind, signal};
    let mut sigterm = signal(SignalKind::terminate()).expect("SIGTERM is supported");
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        _ = sigterm.recv() => {}
    }
}
