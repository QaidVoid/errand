//! The listener that admits a session's connections, gates them, and tunnels.
//!
//! One `Broker` serves one session's namespace: the allowlist is fixed for the
//! life of that session.

use std::sync::Arc;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::oneshot;

use super::{
    ALLOWED_UPSTREAM_PORTS, ProviderRoute, ProviderState, host_allowed, parse_connect,
    public_address,
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
        let provider_port = if self.routes.is_empty() {
            0
        } else {
            self.start_provider().await
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
            client: reqwest::Client::new(),
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

    async fn start_provider(&mut self) -> u16 {
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
            client: reqwest::Client::new(),
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
    if !ALLOWED_UPSTREAM_PORTS.contains(&target.port) || !host_allowed(&target.host, &state.allow) {
        state.log.info(
            "egress refused",
            &fields([
                ("host", target.host.as_str().into()),
                ("port", i64::from(target.port).into()),
            ]),
        );
        let _ = refuse(&mut writer, 403, "not on the egress allowlist").await;
        return;
    }

    // Where the name actually points is checked, not just whether it is
    // allowed: the broker runs on the host, so dialling the host's own network
    // through it is a way back in that the namespace was built to close.
    let address = public_address(&target.host, state.allow_internal, state.resolve.as_ref()).await;
    let Some(address) = address else {
        state.log.warn(
            "egress refused a host-internal target",
            &fields([("host", target.host.as_str().into())]),
        );
        let _ = refuse(&mut writer, 403, "not a public host").await;
        return;
    };

    let upstream = tokio::net::TcpStream::connect((address.as_str(), target.port)).await;
    match upstream {
        Ok(upstream) => {
            if writer
                .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
                .await
                .is_err()
            {
                return;
            }
            state.log.info(
                "egress allowed",
                &fields([
                    ("host", target.host.as_str().into()),
                    ("port", i64::from(target.port).into()),
                ]),
            );
            let (mut up_reader, mut up_writer) = upstream.into_split();
            let outbound = tokio::io::copy(&mut reader, &mut up_writer);
            let inbound = tokio::io::copy(&mut up_reader, &mut writer);
            let _ = tokio::join!(outbound, inbound);
        }
        Err(error) => {
            let _ = refuse(&mut writer, 502, "upstream unreachable").await;
            state.log.warn(
                "upstream connect failed",
                &fields([
                    ("host", target.host.as_str().into()),
                    ("detail", error.to_string().into()),
                ]),
            );
        }
    }
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

/// Reads the whole CONNECT request head, up to and including the blank line.
///
/// Read one byte at a time so nothing past the head is consumed: what follows
/// is the tunnelled bytes, and reading even one of them here would strip it
/// from the stream the client expects to carry its TLS. Reading only the
/// request line, and leaving the remaining headers in the socket, is worse
/// still: those leftover header bytes would then be piped to the upstream
/// ahead of the TLS `ClientHello` and corrupt the connection. The returned text
/// keeps CRs so the caller splits on either line ending.
pub async fn read_request_head<R: tokio::io::AsyncRead + Unpin>(conn: &mut R) -> Option<String> {
    let mut byte = [0_u8; 1];
    let mut bytes: Vec<u8> = Vec::new();
    // A head larger than this is not one the broker will honour.
    while bytes.len() < 8192 {
        match conn.read(&mut byte).await {
            Ok(0) | Err(_) => return None,
            Ok(_) => bytes.push(byte[0]),
        }
        let len = bytes.len();
        // End of head: a blank line, as CRLFCRLF or a bare LFLF.
        if bytes[len - 1] == 0x0a {
            if len >= 2 && bytes[len - 2] == 0x0a {
                break;
            }
            if len >= 4
                && bytes[len - 2] == 0x0d
                && bytes[len - 3] == 0x0a
                && bytes[len - 4] == 0x0d
            {
                break;
            }
        }
    }
    Some(String::from_utf8_lossy(&bytes).into_owned())
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
