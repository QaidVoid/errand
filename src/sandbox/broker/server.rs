//! The listener that admits a session's connections, gates them, and tunnels.
//!
//! One `Broker` serves one session's namespace: the allowlist is fixed for the
//! life of that session.

use std::sync::Arc;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::oneshot;

use super::{
    ALLOWED_UPSTREAM_PORTS, ConnectTarget, DIAL_TIMEOUT, LOOKUP_TIMEOUT, ProviderRoute,
    ProviderState, host_allowed, parse_connect, public_addresses,
};
use crate::log::Logger;
use crate::log::fields;

/// A running CONNECT proxy that admits only allowlisted hosts.
pub struct Broker {
    allow: Vec<String>,
    log: Logger,
    routes: Vec<ProviderRoute>,
    allow_internal: bool,
    accept_shutdown: Option<oneshot::Sender<()>>,
    provider_shutdown: Option<oneshot::Sender<()>>,
    closed: bool,
}

impl Broker {
    /// A broker over `allow`, optionally answering as the providers in
    /// `routes`.
    pub fn new(
        allow: Vec<String>,
        log: Logger,
        routes: Vec<ProviderRoute>,
        allow_internal: bool,
    ) -> Self {
        Self {
            allow,
            log,
            routes,
            allow_internal,
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
            allow_internal: self.allow_internal,
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
                tokio::spawn(handle(stream, state));
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
            allow_internal: self.allow_internal,
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

/// Handles one client connection: gate, serve the provider, or refuse.
async fn handle(stream: TcpStream, state: Arc<ProviderState>) {
    let (mut reader, mut writer) = stream.into_split();
    let Some(head) = read_request_head(&mut reader).await else {
        return;
    };
    let request_line = head.split('\n').next().unwrap_or("").to_owned();
    let Some(target) = parse_connect(&request_line) else {
        // Not a tunnel. With a provider route this is the session calling the
        // provider, which is served rather than refused: the head already read
        // is replayed so the server sees the request whole.
        if !state.routes.is_empty() {
            let _ = serve_provider(&mut reader, &mut writer, &head, state.provider_port).await;
            return;
        }
        let _ = refuse(&mut writer, 400, "the broker speaks only CONNECT").await;
        return;
    };
    let Some(upstream) = admit(&mut writer, &target, &state).await else {
        return;
    };
    if writer
        .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
        .await
        .is_err()
    {
        return;
    }
    let (mut up_reader, mut up_writer) = upstream.into_split();
    let outbound = tokio::io::copy(&mut reader, &mut up_writer);
    let inbound = tokio::io::copy(&mut up_reader, &mut writer);
    let _ = tokio::join!(outbound, inbound);
}

/// Gates a target and dials it, or refuses the client and says why.
async fn admit<W: tokio::io::AsyncWrite + Unpin>(
    writer: &mut W,
    target: &ConnectTarget,
    state: &ProviderState,
) -> Option<TcpStream> {
    let host = target.host.as_str();
    let port = i64::from(target.port);
    if !ALLOWED_UPSTREAM_PORTS.contains(&target.port) || !host_allowed(host, &state.allow) {
        state.log.info(
            "egress refused",
            &fields([("host", host.into()), ("port", port.into())]),
        );
        let _ = refuse(writer, 403, "not on the egress allowlist").await;
        return None;
    }

    // Where the name actually points is checked, not just whether it is
    // allowed: the broker runs on the host, so dialling the host's own network
    // through it is a way back in that the namespace was built to close.
    let lookup = public_addresses(host, state.allow_internal, state.resolve.as_ref());
    let Ok(addresses) = tokio::time::timeout(LOOKUP_TIMEOUT, lookup).await else {
        state
            .log
            .warn("egress lookup timed out", &fields([("host", host.into())]));
        let _ = refuse(writer, 504, "the name did not resolve in time").await;
        return None;
    };
    if addresses.is_empty() {
        state.log.warn(
            "egress refused a target with no public address",
            &fields([("host", host.into())]),
        );
        let _ = refuse(writer, 403, "not a public host").await;
        return None;
    }

    match dial(&addresses, target.port).await {
        Ok(upstream) => {
            state.log.info(
                "egress allowed",
                &fields([("host", host.into()), ("port", port.into())]),
            );
            Some(upstream)
        }
        Err(error) => {
            let _ = refuse(writer, 502, "upstream unreachable").await;
            state.log.warn(
                "upstream connect failed",
                &fields([("host", host.into()), ("detail", error.to_string().into())]),
            );
            None
        }
    }
}

/// Dials each address in turn, giving each [`DIAL_TIMEOUT`] to accept.
pub(super) async fn dial(addresses: &[String], port: u16) -> std::io::Result<TcpStream> {
    let mut last = std::io::Error::other("no address to dial");
    for address in addresses {
        let connect = TcpStream::connect((address.as_str(), port));
        match tokio::time::timeout(DIAL_TIMEOUT, connect).await {
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
async fn serve_provider(
    reader: &mut tokio::net::tcp::OwnedReadHalf,
    writer: &mut tokio::net::tcp::OwnedWriteHalf,
    head: &str,
    provider_port: u16,
) -> std::io::Result<()> {
    let Ok(inner) = tokio::net::TcpStream::connect(("127.0.0.1", provider_port)).await else {
        return refuse(writer, 502, "the provider endpoint is not up").await;
    };
    let (mut inner_reader, mut inner_writer) = inner.into_split();
    inner_writer.write_all(head.as_bytes()).await?;
    let outbound = tokio::io::copy(reader, &mut inner_writer);
    let inbound = tokio::io::copy(&mut inner_reader, writer);
    let _ = tokio::join!(outbound, inbound);
    Ok(())
}

/// Largest request head the broker will read before giving up on one.
///
/// A CONNECT head is a line and a handful of headers. Anything longer is not a
/// client that means to tunnel.
pub const MAX_HEAD_BYTES: usize = 8192;

/// Reads the whole CONNECT request head, up to and including the blank line.
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
) -> std::io::Result<()> {
    // The client may have gone; the refusal is best-effort.
    let _ = writer
        .write_all(format!("HTTP/1.1 {code} {reason}\r\n\r\n").as_bytes())
        .await;
    writer.shutdown().await
}
