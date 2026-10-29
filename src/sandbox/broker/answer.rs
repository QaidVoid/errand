//! What a provider's answer is relayed to the session as.
//!
//! The broker puts the operator's real credential on the request, so an
//! upstream that quotes what it was sent, as verbose validation errors and
//! error pages do, would hand the session the key. What the session gets back
//! is therefore scrubbed of that route's credential: the body as it streams,
//! and every header value. An answer the upstream compressed anyway cannot be
//! scrubbed and is refused. The body is
//! bounded, and framing that a rewrite would make false is dropped.

use futures_util::StreamExt;

use super::{HOP_BY_HOP, ProviderRoute};
use crate::config::redact::REDACTION;
use crate::log::{Logger, fields};

/// The largest answer relayed to a session, declared or streamed.
///
/// A ceiling rather than a setting: nothing a provider legitimately answers
/// comes near it, and without one an upstream could stream without end.
pub(super) const MAX_ANSWER_BYTES: u64 = 64 * 1024 * 1024;

/// Headers whose value describes the body as the upstream sent it, and so
/// is false once the body is rechunked or rewritten on this hop.
const BODY_FRAMING: [&str; 3] = ["content-length", "etag", "content-md5"];

/// Replaces every occurrence of a credential in a stream of bytes, including
/// one split across chunks.
///
/// Holds back only the end of a chunk that could be the start of the
/// credential, never all of it, and emits everything else at once, so a
/// streamed answer is held up only while a match is still possible.
pub(super) struct Scrubber {
    needle: Vec<u8>,
    held: Vec<u8>,
}

impl Scrubber {
    /// A scrubber for `secret`. An empty one matches nothing, so the scrubber
    /// relays everything unchanged.
    pub(super) fn new(secret: &str) -> Self {
        Self {
            needle: secret.as_bytes().to_vec(),
            held: Vec::new(),
        }
    }

    /// Takes the next chunk and returns what can be emitted now.
    pub(super) fn push(&mut self, chunk: &[u8]) -> Vec<u8> {
        if self.needle.is_empty() {
            return chunk.to_vec();
        }
        self.held.extend_from_slice(chunk);
        let (mut out, rest) = replace_all(&self.held, &self.needle);
        let keep = (1..self.needle.len().min(rest.len() + 1))
            .rev()
            .find(|&length| rest.ends_with(&self.needle[..length]))
            .unwrap_or(0);
        out.extend_from_slice(&rest[..rest.len() - keep]);
        self.held = rest[rest.len() - keep..].to_vec();
        out
    }

    /// What is still held once the stream has ended.
    pub(super) fn finish(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.held)
    }
}

/// `haystack` with every whole occurrence of `needle` replaced, as the
/// replaced part and the tail after the last occurrence, unscanned for a
/// partial one.
fn replace_all<'a>(haystack: &'a [u8], needle: &[u8]) -> (Vec<u8>, &'a [u8]) {
    let mut out = Vec::with_capacity(haystack.len());
    let mut rest = haystack;
    while let Some(at) = rest
        .windows(needle.len())
        .position(|window| window == needle)
    {
        out.extend_from_slice(&rest[..at]);
        out.extend_from_slice(REDACTION.as_bytes());
        rest = &rest[at + needle.len()..];
    }
    (out, rest)
}

/// The provider's answer as the session receives it: the same status, its
/// headers scrubbed of the route's credential and stripped of hop-by-hop and
/// stale framing, and its body scrubbed as it streams and cut at `limit`.
pub(super) fn relay(
    answered: reqwest::Response,
    route: &ProviderRoute,
    log: &Logger,
    limit: u64,
) -> axum::response::Response {
    use axum::response::IntoResponse;

    let provider = route.prefix.as_str();
    let declared = answered
        .headers()
        .get(http::header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.trim().parse::<u64>().ok());
    if declared.is_some_and(|length| length > limit) {
        log.warn(
            "a provider answer was refused for its declared size",
            &fields([("provider", provider.into()), ("limit_bytes", limit.into())]),
        );
        return (
            axum::http::StatusCode::BAD_GATEWAY,
            "the provider's answer is larger than the broker relays\n",
        )
            .into_response();
    }

    // The upstream was asked for identity encoding so the bytes can be read.
    // One that compresses anyway is refused rather than relayed: inflating it
    // is not something this broker does, and bytes it cannot scrub could
    // hand the session the real key, to use outside the route and after it
    // stops.
    let encoded = answered
        .headers()
        .get_all(http::header::CONTENT_ENCODING)
        .iter()
        .any(|value| {
            value.to_str().map_or(true, |codings| {
                codings
                    .split(',')
                    .map(str::trim)
                    .any(|coding| !coding.is_empty() && !coding.eq_ignore_ascii_case("identity"))
            })
        });
    // An answer with no body has nothing to scrub, and may carry the coding
    // of the representation it stands for, as a 304 does.
    let bodiless = matches!(answered.status().as_u16(), 204 | 304);
    if encoded && !bodiless {
        log.warn(
            "a provider answered compressed though asked not to, so the answer was refused \
             rather than relayed unscrubbed",
            &fields([("provider", provider.into())]),
        );
        return (
            axum::http::StatusCode::BAD_GATEWAY,
            "the provider answered compressed, which the broker cannot scrub\n",
        )
            .into_response();
    }
    let mut builder = axum::http::Response::builder().status(answered.status().as_u16());
    for (name, value) in answered.headers() {
        if HOP_BY_HOP.contains(&name.as_str()) || BODY_FRAMING.contains(&name.as_str()) {
            continue;
        }
        let mut scrubber = Scrubber::new(&route.credential);
        let mut relayed = scrubber.push(value.as_bytes());
        relayed.extend(scrubber.finish());
        // A valid value with part of it replaced by printable ASCII is still
        // valid, so this cannot fail.
        let relayed =
            http::HeaderValue::from_bytes(&relayed).expect("a scrubbed header value stays valid");
        builder = builder.header(name, relayed);
    }

    let scrubber = Scrubber::new(&route.credential);
    let body = scrubbed_body(answered, scrubber, limit, log.clone(), provider.to_owned());
    builder
        .body(axum::body::Body::from_stream(body))
        .unwrap_or_else(|error| {
            (axum::http::StatusCode::BAD_GATEWAY, error.to_string()).into_response()
        })
}

/// The answer's body through `scrubber`, ending in an error once more than
/// `limit` bytes would reach the session, with the cut or a failed read
/// logged. What is counted is what is relayed, since a scrubbed body can be
/// longer than the upstream's.
fn scrubbed_body(
    answered: reqwest::Response,
    scrubber: Scrubber,
    limit: u64,
    log: Logger,
    provider: String,
) -> impl futures_util::Stream<Item = std::io::Result<Vec<u8>>> {
    futures_util::stream::unfold(
        Some((answered.bytes_stream(), scrubber, 0_u64)),
        move |state| {
            let log = log.clone();
            let provider = provider.clone();
            async move {
                let (mut upstream, mut scrubber, seen) = state?;
                match upstream.next().await {
                    Some(Ok(chunk)) => {
                        let out = scrubber.push(&chunk);
                        let seen = seen.saturating_add(out.len() as u64);
                        if seen > limit {
                            log.warn(
                                "a provider answer was cut for its size",
                                &fields([
                                    ("provider", provider.as_str().into()),
                                    ("limit_bytes", limit.into()),
                                ]),
                            );
                            let cut = std::io::Error::other(
                                "the provider's answer is larger than the broker relays",
                            );
                            return Some((Err(cut), None));
                        }
                        Some((Ok(out), Some((upstream, scrubber, seen))))
                    }
                    Some(Err(error)) => {
                        log.warn(
                            "a provider answer failed part way",
                            &fields([
                                ("provider", provider.as_str().into()),
                                ("detail", error.to_string().into()),
                            ]),
                        );
                        Some((Err(std::io::Error::other(error.to_string())), None))
                    }
                    None => {
                        let tail = scrubber.finish();
                        (!tail.is_empty()).then_some((Ok(tail), None))
                    }
                }
            }
        },
    )
}

#[cfg(test)]
mod tests;
