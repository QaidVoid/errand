//! The listener that admits a session's connections, gates them, and tunnels.
//!
//! One `Broker` serves one session: it is started when the session's sandbox
//! is launched and closed when the sandbox is stopped. Its allowlist, its
//! proxy credential, and its provider nonces are honoured only until then,
//! on any connection, including one opened before the close.

use std::collections::BTreeMap;
use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::oneshot;

use super::{
    DIAL_BUDGET, DIAL_TIMEOUT, JudgedResolver, LOOKUP_TIMEOUT, NoAddress, ProviderRoute,
    ProviderState, ProviderUpstream, Target, carries_proxy_credential, host_allowed, parse_connect,
    parse_forward, public_addresses,
};
use crate::config::redact::{MINTED_HEX_DIGITS, MINTED_PREFIX};
use crate::log::Logger;
use crate::log::fields;

/// What every session's broker is built from, fixed when the daemon starts.
///
/// The allowlist, the ports, and where each provider is are the daemon's.
/// What a session is handed to reach them is not: [`BrokerTemplate::start`]
/// mints a proxy credential and a nonce per provider for each session, so
/// nothing read out of one session is worth anything to another, or outlives
/// it.
#[derive(Clone)]
pub struct BrokerTemplate {
    /// The egress allowlist, by host.
    pub allow: Vec<String>,
    /// The upstream ports a broker will open.
    pub ports: Vec<u16>,
    /// Whether a literal internal address may be dialled.
    pub allow_internal: bool,
    /// The logger each session's broker writes to, under its session's name.
    pub log: Logger,
    /// The providers a broker stands in front of.
    pub providers: Vec<ProviderUpstream>,
    /// The variable the agent reads each provider's key from, by provider
    /// name, for every brokered provider that names one.
    pub credential_names: BTreeMap<String, String>,
}

/// How long the accept loop waits before trying again after a failed accept,
/// such as the daemon running out of file descriptors for a moment.
const ACCEPT_RETRY: Duration = Duration::from_secs(1);

/// One session's running broker, and what the session is handed to reach it.
pub struct SessionBroker {
    /// The running broker. Closing or dropping it refuses every request made
    /// after, on any connection, so everything below is worthless from then.
    pub broker: Broker,
    /// The loopback port the broker listens on.
    pub port: u16,
    /// The proxy credential this session's tools send.
    pub proxy_secret: String,
    /// What stands in for each provider's credential, by provider name.
    pub nonces: BTreeMap<String, String>,
    /// The variable the agent reads each provider's key from, by provider
    /// name, as the template has it.
    pub credential_names: BTreeMap<String, String>,
}

impl BrokerTemplate {
    /// Starts a broker for `session_id`, with its own listener, its own proxy
    /// credential, and its own nonce for every provider.
    pub async fn start(&self, session_id: &str) -> std::io::Result<SessionBroker> {
        let mut nonces = BTreeMap::new();
        let routes = self
            .providers
            .iter()
            .map(|provider| {
                let nonce = fresh_secret();
                nonces.insert(provider.name.clone(), nonce.clone());
                ProviderRoute {
                    prefix: provider.prefix.clone(),
                    upstream: provider.upstream.clone(),
                    nonce,
                    credential: provider.credential.clone(),
                }
            })
            .collect();
        let proxy_secret = fresh_secret();
        let mut broker = Broker::new(
            self.allow.clone(),
            self.ports.clone(),
            self.log.with(fields([("session", session_id.into())])),
            routes,
            self.allow_internal,
            proxy_authorization(&proxy_secret),
        );
        // The listener sits on loopback; the address a sandbox dials is only
        // mapped back to this host from inside it.
        let port = broker.listen("127.0.0.1").await?;
        Ok(SessionBroker {
            broker,
            port,
            proxy_secret,
            nonces,
            credential_names: self.credential_names.clone(),
        })
    }
}

/// The client the broker calls providers with, carrying the real key.
///
/// It follows no redirect. A redirect carries the key to a host nobody
/// judged, in a header reqwest does not strip across hosts, so a provider's
/// redirect reaches the session as it was sent. It dials only addresses its
/// resolver judged, and never through a proxy from the daemon's environment,
/// which would resolve the name itself. The tunnel path dials directly too.
pub(super) fn provider_client(resolve: Option<super::Resolve>) -> std::io::Result<reqwest::Client> {
    reqwest::Client::builder()
        .connect_timeout(DIAL_TIMEOUT)
        .redirect(reqwest::redirect::Policy::none())
        .no_proxy()
        .dns_resolver(Arc::new(JudgedResolver { resolve }))
        .build()
        .map_err(std::io::Error::other)
}

/// The `Proxy-Authorization` value a session's tools send for `secret`.
pub(super) fn proxy_authorization(secret: &str) -> String {
    format!("Basic {}", STANDARD.encode(format!("session:{secret}")))
}

/// A stand-in for a credential, for one session.
///
/// Drawn from the system generator rather than anything derived from what it
/// stands for, so holding it says nothing about the key behind it. It is
/// honoured only by the broker it was minted for, which ends with the session.
/// It carries [`MINTED_PREFIX`], so an echoed one reads as errand's rather
/// than as a provider's key, and redacted text is scrubbed of it by shape.
fn fresh_secret() -> String {
    use std::fmt::Write as _;
    let mut bytes = [0u8; MINTED_HEX_DIGITS / 2];
    getrandom::fill(&mut bytes).expect("the system generator is available");
    bytes
        .iter()
        .fold(MINTED_PREFIX.to_owned(), |mut out, byte| {
            let _ = write!(out, "{byte:02x}");
            out
        })
}

/// A running proxy that admits only allowlisted hosts on allowed ports.
pub struct Broker {
    allow: Vec<String>,
    ports: Vec<u16>,
    log: Logger,
    routes: Vec<ProviderRoute>,
    allow_internal: bool,
    proxy_authorization: String,
    accept_shutdown: Option<oneshot::Sender<()>>,
    provider_shutdown: Option<oneshot::Sender<()>>,
    closed: Arc<AtomicBool>,
}

impl Broker {
    /// A broker over `allow` and `ports`, optionally answering as the
    /// providers in `routes`. Proxy requests must carry `proxy_authorization`,
    /// supplied as a complete `Basic <base64(session:secret)>` header value.
    pub fn new(
        allow: Vec<String>,
        ports: Vec<u16>,
        log: Logger,
        routes: Vec<ProviderRoute>,
        allow_internal: bool,
        proxy_authorization: String,
    ) -> Self {
        Self {
            allow,
            ports,
            log,
            routes,
            allow_internal,
            proxy_authorization,
            accept_shutdown: None,
            provider_shutdown: None,
            closed: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Binds to a loopback port and starts admitting connections. Returns the
    /// port.
    ///
    /// Runs on the async runtime, which owns the accept loop and the provider
    /// endpoint from here on.
    pub async fn listen(&mut self, host: &str) -> std::io::Result<u16> {
        let client = provider_client(None)?;
        let provider_port = if self.routes.is_empty() {
            0
        } else {
            self.start_provider(client.clone()).await?
        };
        let listener = TcpListener::bind((host, 0)).await?;
        let port = listener.local_addr()?.port();
        let state = Arc::new(ProviderState {
            routes: self.routes.clone(),
            allow: self.allow.clone(),
            ports: self.ports.clone(),
            allow_internal: self.allow_internal,
            proxy_authorization: self.proxy_authorization.clone(),
            log: self.log.clone(),
            resolve: None,
            provider_port,
            client,
            closed: Arc::clone(&self.closed),
        });
        let (shutdown_sender, mut shutdown_receiver) = oneshot::channel::<()>();
        tokio::spawn(async move {
            loop {
                // A close is seen ahead of a connection already queued, so
                // nothing is admitted once the session has been stopped.
                let accepted = tokio::select! {
                    biased;
                    _ = &mut shutdown_receiver => break,
                    accepted = listener.accept() => accepted,
                };
                let (stream, _peer) = match accepted {
                    Ok(accepted) => accepted,
                    Err(error) => {
                        state.log.warn(
                            "the egress broker could not accept a connection, and tries again",
                            &fields([
                                ("detail", error.to_string().into()),
                                (
                                    "retry_ms",
                                    u64::try_from(ACCEPT_RETRY.as_millis())
                                        .unwrap_or(u64::MAX)
                                        .into(),
                                ),
                            ]),
                        );
                        tokio::time::sleep(ACCEPT_RETRY).await;
                        continue;
                    }
                };
                let state = Arc::clone(&state);
                tokio::spawn(handle(
                    stream,
                    state,
                    |addresses, port, deadline| async move { dial(&addresses, port, deadline).await },
                ));
            }
        });
        self.accept_shutdown = Some(shutdown_sender);
        Ok(port)
    }

    async fn start_provider(&mut self, client: reqwest::Client) -> std::io::Result<u16> {
        let (shutdown_sender, shutdown_receiver) = oneshot::channel::<()>();
        let app = axum::Router::new().fallback(
            |axum::extract::State(state): axum::extract::State<Arc<ProviderState>>,
             request: axum::extract::Request| async move {
                super::serve_provider_request(state, request).await
            },
        );
        let state = Arc::new(ProviderState {
            routes: self.routes.clone(),
            allow: self.allow.clone(),
            ports: self.ports.clone(),
            allow_internal: self.allow_internal,
            proxy_authorization: self.proxy_authorization.clone(),
            log: self.log.clone(),
            resolve: None,
            provider_port: 0,
            client,
            closed: Arc::clone(&self.closed),
        });
        let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
        let port = listener.local_addr()?.port();
        self.provider_shutdown = Some(shutdown_sender);
        tokio::spawn(async move {
            let shutdown = async {
                let _ = shutdown_receiver.await;
            };
            axum::serve(listener, app.with_state(state))
                .with_graceful_shutdown(shutdown)
                .await
                .expect("the provider server serves");
        });
        Ok(port)
    }

    /// Stops accepting, closes the listener, and refuses any request still to
    /// come on a connection accepted before. A tunnel already running is left
    /// to finish; it ends when the session's processes do.
    pub fn close(&mut self) {
        if self.closed.swap(true, Ordering::SeqCst) {
            return;
        }
        if let Some(shutdown) = self.accept_shutdown.take() {
            let _ = shutdown.send(());
        }
        if let Some(shutdown) = self.provider_shutdown.take() {
            let _ = shutdown.send(());
        }
    }
}

/// A broker that goes out of scope stops listening, so a launch that fails
/// after starting one leaves nothing behind that still honours its nonces.
impl Drop for Broker {
    fn drop(&mut self) {
        self.close();
    }
}

/// Handles one client connection with an injected stream and upstream dial.
/// Provider requests retain their separate loopback transport and nonce check.
pub(super) async fn handle<S, U, F>(
    mut stream: S,
    state: Arc<ProviderState>,
    dial_upstream: impl FnOnce(Vec<String>, u16, tokio::time::Instant) -> F,
) where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
    U: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
    F: Future<Output = std::io::Result<U>>,
{
    let Some(head) = read_request_head(&mut stream).await else {
        return;
    };
    // A connection opened before the session was stopped may still be held
    // open by it, so a request is weighed against the close when it arrives.
    if state.closed.load(Ordering::SeqCst) {
        let _ = refuse(&mut stream, 503, "this session has stopped", &[]).await;
        return;
    }
    if let Some(target) = parse_connect(head.lines().next().unwrap_or("")) {
        if !authenticate_proxy(&mut stream, &head, &target, &state).await {
            return;
        }
        let Some(mut upstream) = admit(&mut stream, &target, &state, dial_upstream).await else {
            return;
        };
        if stream
            .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
            .await
            .is_ok()
        {
            let _ = tokio::io::copy_bidirectional(&mut stream, &mut upstream).await;
        }
        return;
    }
    if let Some(forward) = parse_forward(&head) {
        if !authenticate_proxy(&mut stream, &head, &forward.target, &state).await {
            return;
        }
        let Some(mut upstream) = admit(&mut stream, &forward.target, &state, dial_upstream).await
        else {
            return;
        };
        if upstream.write_all(forward.head.as_bytes()).await.is_ok() {
            let _ = tokio::io::copy_bidirectional(&mut stream, &mut upstream).await;
        }
        return;
    }
    // Neither proxied. With a provider route this is the session calling the
    // provider, which is served rather than refused: the head already read is
    // replayed so the server sees the request whole.
    if !state.routes.is_empty() {
        let _ = serve_provider(&mut stream, &head, state.provider_port).await;
        return;
    }
    let _ = refuse(&mut stream, 400, "the broker speaks only as a proxy", &[]).await;
}

/// Challenges an unauthenticated proxy request before target admission.
async fn authenticate_proxy<W: tokio::io::AsyncWrite + Unpin>(
    writer: &mut W,
    head: &str,
    target: &Target,
    state: &ProviderState,
) -> bool {
    if carries_proxy_credential(head, &state.proxy_authorization) {
        return true;
    }
    state.log.info(
        "proxy authentication refused",
        &fields([
            ("host", target.host.as_str().into()),
            ("port", i64::from(target.port).into()),
        ]),
    );
    let _ = refuse(
        writer,
        407,
        "Proxy Authentication Required",
        &[("Proxy-Authenticate", "Basic realm=\"errand\"")],
    )
    .await;
    false
}

/// Gates a target and dials it, or refuses the client and says why.
async fn admit<W, U, F>(
    writer: &mut W,
    target: &Target,
    state: &ProviderState,
    dial_upstream: impl FnOnce(Vec<String>, u16, tokio::time::Instant) -> F,
) -> Option<U>
where
    W: tokio::io::AsyncWrite + Unpin,
    F: Future<Output = std::io::Result<U>>,
{
    let host = target.host.as_str();
    let port = i64::from(target.port);
    if !state.ports.contains(&target.port) || !host_allowed(host, &state.allow) {
        state.log.info(
            "egress refused",
            &fields([("host", host.into()), ("port", port.into())]),
        );
        let _ = refuse(writer, 403, "not on the egress allowlist", &[]).await;
        return None;
    }

    // Where the name actually points is checked, not just whether it is
    // allowed: the broker runs on the host, so dialling the host's own network
    // through it is a way back in that the namespace was built to close.
    let lookup = public_addresses(host, state.allow_internal, state.resolve.as_ref());
    let Ok(resolved) = tokio::time::timeout(LOOKUP_TIMEOUT, lookup).await else {
        state
            .log
            .warn("egress lookup timed out", &fields([("host", host.into())]));
        let _ = refuse(writer, 504, "the name did not resolve in time", &[]).await;
        return None;
    };
    let addresses = match resolved {
        Ok(addresses) => addresses,
        Err(NoAddress::Unresolved(why)) => {
            state.log.warn(
                "egress could not resolve a name on this host",
                &fields([("host", host.into()), ("detail", why.into())]),
            );
            let _ = refuse(writer, 502, "the name did not resolve", &[]).await;
            return None;
        }
        Err(NoAddress::Internal) => {
            state.log.warn(
                "egress refused a host-internal target",
                &fields([("host", host.into())]),
            );
            let _ = refuse(writer, 403, "not a public host", &[]).await;
            return None;
        }
    };

    let deadline = tokio::time::Instant::now() + DIAL_BUDGET;
    match dial_upstream(addresses, target.port, deadline).await {
        // The lookup and the dial take seconds, and the session may have been
        // stopped while they ran. A tunnel is not opened for a stopped one.
        Ok(_) if state.closed.load(Ordering::SeqCst) => {
            state.log.info(
                "egress refused, since the session stopped while it was dialled",
                &fields([("host", host.into()), ("port", port.into())]),
            );
            let _ = refuse(writer, 503, "this session has stopped", &[]).await;
            None
        }
        Ok(upstream) => {
            state.log.info(
                "egress allowed",
                &fields([("host", host.into()), ("port", port.into())]),
            );
            Some(upstream)
        }
        Err(error) => {
            let _ = refuse(writer, 502, "upstream unreachable", &[]).await;
            state.log.warn(
                "upstream connect failed",
                &fields([("host", host.into()), ("detail", error.to_string().into())]),
            );
            None
        }
    }
}

/// Dials each address in turn, giving each [`DIAL_TIMEOUT`] to accept, and
/// stops at `deadline` whether or not any are left.
pub(super) async fn dial(
    addresses: &[String],
    port: u16,
    deadline: tokio::time::Instant,
) -> std::io::Result<TcpStream> {
    let mut last = std::io::Error::other("no address to dial");
    for (tried, address) in addresses.iter().enumerate() {
        let now = tokio::time::Instant::now();
        if now >= deadline {
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                format!(
                    "the dial deadline passed with {tried} of {} addresses tried; last: {last}",
                    addresses.len()
                ),
            ));
        }
        let connect = TcpStream::connect((address.as_str(), port));
        let attempt_ends = deadline.min(now + DIAL_TIMEOUT);
        match tokio::time::timeout_at(attempt_ends, connect).await {
            Ok(Ok(stream)) => return Ok(stream),
            Ok(Err(error)) => last = error,
            Err(_) => {
                last = std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    format!("{address} did not accept in time"),
                );
            }
        }
    }
    Err(last)
}

/// Hands a connection to the provider server, head and all.
///
/// The head was read to find out whether this was a tunnel, so it is written
/// on before the two are joined; everything after it is still in the socket
/// and flows through untouched, body and event stream alike.
async fn serve_provider<S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin>(
    stream: &mut S,
    head: &str,
    provider_port: u16,
) -> std::io::Result<()> {
    let Ok(mut inner) = TcpStream::connect(("127.0.0.1", provider_port)).await else {
        return refuse(stream, 502, "the provider endpoint is not up", &[]).await;
    };
    inner.write_all(head.as_bytes()).await?;
    tokio::io::copy_bidirectional(stream, &mut inner).await?;
    Ok(())
}

/// Largest request head the broker will read before giving up on one.
///
/// A CONNECT head is a line and a handful of headers. Anything longer is not a
/// client that means to tunnel.
pub const MAX_HEAD_BYTES: usize = 8192;

/// Reads the whole request head, up to and including the blank line.
///
/// Read one byte at a time so nothing past the head is consumed: what follows
/// is the tunnelled bytes, and reading even one of them here would strip it
/// from the stream the client expects to carry its TLS. Reading only the
/// request line, and leaving the remaining headers in the socket, is worse
/// still: those leftover header bytes would then be piped to the upstream
/// ahead of the TLS `ClientHello` and corrupt the connection. The returned text
/// keeps CRs so the caller splits on either line ending.
///
/// Returns nothing when the head does not end within [`MAX_HEAD_BYTES`].
/// Handing back what had accumulated would let a client that never sent a
/// blank line be served as though it had, with whatever the cut left behind
/// read as a complete request.
pub async fn read_request_head<R: tokio::io::AsyncRead + Unpin>(conn: &mut R) -> Option<String> {
    let mut byte = [0_u8; 1];
    let mut bytes: Vec<u8> = Vec::new();
    while bytes.len() < MAX_HEAD_BYTES {
        match conn.read(&mut byte).await {
            Ok(0) | Err(_) => return None,
            Ok(_) => bytes.push(byte[0]),
        }
        let len = bytes.len();
        // End of head: a blank line, as CRLFCRLF or a bare LFLF.
        if bytes[len - 1] == 0x0a {
            if len >= 2 && bytes[len - 2] == 0x0a {
                return Some(String::from_utf8_lossy(&bytes).into_owned());
            }
            if len >= 4
                && bytes[len - 2] == 0x0d
                && bytes[len - 3] == 0x0a
                && bytes[len - 4] == 0x0d
            {
                return Some(String::from_utf8_lossy(&bytes).into_owned());
            }
        }
    }
    None
}

/// Answers a refused connection with the reason, then closes it.
async fn refuse<W: tokio::io::AsyncWrite + Unpin>(
    writer: &mut W,
    code: u16,
    reason: &str,
    headers: &[(&str, &str)],
) -> std::io::Result<()> {
    let mut head = format!("HTTP/1.1 {code} {reason}\r\n");
    for (name, value) in headers {
        head.push_str(name);
        head.push_str(": ");
        head.push_str(value);
        head.push_str("\r\n");
    }
    // Without these an HTTP/1.1 client takes the connection as kept alive.
    // git probes the proxy without a credential first, and on a 407 it then
    // retries on a socket the broker has already closed and aborts, where an
    // explicit close makes it reconnect with the credential.
    head.push_str("Connection: close\r\nContent-Length: 0\r\n\r\n");
    // The client may have gone; the refusal is best-effort.
    let _ = writer.write_all(head.as_bytes()).await;
    writer.shutdown().await
}
