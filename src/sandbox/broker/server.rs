//! The listener that admits a session's connections, gates them, and tunnels.
//!
//! One `Broker` serves one session's namespace: the allowlist is fixed for the
//! life of that session.

use std::future::Future;
use std::sync::Arc;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::oneshot;

use super::{
    DIAL_BUDGET, DIAL_TIMEOUT, LOOKUP_TIMEOUT, NoAddress, ProviderRoute, ProviderState, Target,
    carries_proxy_credential, host_allowed, parse_connect, parse_forward, public_addresses,
};
use crate::log::Logger;
use crate::log::fields;

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
    closed: bool,
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
            closed: false,
        }
    }

    /// Binds to a loopback port and starts admitting connections. Returns the
    /// port.
    ///
    /// Runs on the async runtime, which owns the accept loop and the provider
    /// endpoint from here on.
    pub async fn listen(&mut self, host: &str) -> std::io::Result<u16> {
        let client = reqwest::Client::builder()
            .connect_timeout(DIAL_TIMEOUT)
            .build()
            .map_err(std::io::Error::other)?;
        let provider_port = if self.routes.is_empty() {
            0
        } else {
            self.start_provider(client.clone()).await
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
        });
        let (shutdown_sender, mut shutdown_receiver) = oneshot::channel::<()>();
        tokio::spawn(async move {
            loop {
                let accepted = tokio::select! {
                    accepted = listener.accept() => accepted,
                    _ = &mut shutdown_receiver => break,
                };
                let Ok((stream, _peer)) = accepted else { break };
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

    async fn start_provider(&mut self, client: reqwest::Client) -> u16 {
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
        });
        let listener = TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("a loopback port for the provider endpoint");
        let port = listener.local_addr().expect("a local address").port();
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
        port
    }

    /// Stops accepting and closes the listener. A tunnel already running is
    /// left to finish, as it is under the listener this replaces.
    pub fn close(&mut self) {
        if self.closed {
            return;
        }
        self.closed = true;
        if let Some(shutdown) = self.accept_shutdown.take() {
            let _ = shutdown.send(());
        }
        if let Some(shutdown) = self.provider_shutdown.take() {
            let _ = shutdown.send(());
        }
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
