//! A session's GitHub credential minted from a GitHub App, in place of the
//! daemon's own token.
//!
//! The daemon signs a short JSON Web Token with the App's private key and
//! trades it for an installation token, which covers only the installation's
//! repositories, carries only the permissions asked for, and expires within
//! the hour. A session holds that, never the daemon's token, so what one
//! prompt injection can take is an hour of a narrowed credential rather than
//! the operator's GitHub identity.

use std::collections::BTreeMap;

use base64::Engine as _;
use serde_json::{Value, json};

use crate::config::schema::GithubAppConfig;
use crate::session::pr::{Api, ApiCall};

/// Where in a session's state directory the token is kept, for the `gh`
/// wrapper to read before every call.
///
/// A file rather than the environment, because a token renewed while the
/// agent runs cannot reach a process environment fixed at launch.
pub const TOKEN_FILENAME: &str = "github-token";

/// How long before a token expires it is renewed.
pub const RENEW_BEFORE_MS: i64 = 10 * 60 * 1_000;

/// How soon a failed renewal is tried again.
pub const RETRY_MS: u64 = 60 * 1_000;

/// An installation token, and what it was granted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionToken {
    /// The token itself. Secret.
    pub token: String,
    /// When it stops working, in milliseconds since the epoch.
    pub expires_at_ms: i64,
    /// What it may do, by permission, as GitHub granted it.
    pub permissions: BTreeMap<String, String>,
    /// The repositories it covers, by full name, or none when it covers every
    /// repository of the installation.
    pub repositories: Option<Vec<String>>,
}

/// Mints an installation token for `app`, reading its private key afresh so
/// a rotated key takes effect at the next mint.
pub async fn mint(app: &GithubAppConfig, api: &Api, now_ms: i64) -> Result<SessionToken, String> {
    let pem = std::fs::read_to_string(&app.private_key_path).map_err(|error| {
        format!(
            "the GitHub App's private key at {} could not be read: {error}",
            app.private_key_path
        )
    })?;
    let jwt = app_jwt(app.app_id, &pem, now_ms / 1_000)?;
    let mut body = serde_json::Map::new();
    if !app.repositories.is_empty() {
        body.insert("repositories".to_owned(), json!(app.repositories));
    }
    if !app.permissions.is_empty() {
        body.insert("permissions".to_owned(), json!(app.permissions));
    }
    let reply = api(
        format!("/app/installations/{}/access_tokens", app.installation_id),
        ApiCall {
            method: "POST".to_owned(),
            token: jwt,
            body: Some(Value::Object(body)),
        },
    )
    .await;
    if reply.status != 201 {
        let said = reply
            .body
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("no reason given");
        return Err(format!(
            "GitHub refused to mint a token for installation {} ({}): {said}",
            app.installation_id, reply.status
        ));
    }
    read_token(&reply.body)
}

/// The token GitHub answered a mint with.
fn read_token(body: &Value) -> Result<SessionToken, String> {
    let token = body
        .get("token")
        .and_then(Value::as_str)
        .filter(|token| !token.is_empty())
        .ok_or("GitHub's answer to a mint carried no token")?
        .to_owned();
    let expires_at_ms = body
        .get("expires_at")
        .and_then(Value::as_str)
        .and_then(|at| at.parse::<jiff::Timestamp>().ok())
        .map(jiff::Timestamp::as_millisecond)
        .ok_or("GitHub's answer to a mint did not say when the token expires")?;
    let permissions = body
        .get("permissions")
        .and_then(Value::as_object)
        .map(|granted| {
            granted
                .iter()
                .filter_map(|(name, level)| Some((name.clone(), level.as_str()?.to_owned())))
                .collect()
        })
        .unwrap_or_default();
    let repositories = body
        .get("repositories")
        .and_then(Value::as_array)
        .map(|listed| {
            listed
                .iter()
                .filter_map(|repository| repository.get("full_name")?.as_str().map(str::to_owned))
                .collect()
        });
    Ok(SessionToken {
        token,
        expires_at_ms,
        permissions,
        repositories,
    })
}

/// The JSON Web Token an App proves itself with, signed RS256 by `pem`.
///
/// Issued a minute in the past, as GitHub advises against a clock running
/// ahead of its own, and good for nine minutes, under the ten it allows.
pub fn app_jwt(app_id: u64, pem: &str, now_s: i64) -> Result<String, String> {
    let key = private_key(pem)?;
    let encode = |bytes: &[u8]| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes);
    let header = encode(br#"{"alg":"RS256","typ":"JWT"}"#);
    let claims = encode(
        json!({ "iat": now_s - 60, "exp": now_s + 540, "iss": app_id.to_string() })
            .to_string()
            .as_bytes(),
    );
    let signed = format!("{header}.{claims}");
    let mut signature = vec![0_u8; key.public().modulus_len()];
    key.sign(
        &ring::signature::RSA_PKCS1_SHA256,
        &ring::rand::SystemRandom::new(),
        signed.as_bytes(),
        &mut signature,
    )
    .map_err(|_| "the GitHub App's private key could not sign".to_owned())?;
    Ok(format!("{signed}.{}", encode(&signature)))
}

/// An RSA private key from PEM, as GitHub gives one (PKCS#1) or as it is
/// often converted (PKCS#8).
fn private_key(pem: &str) -> Result<ring::signature::RsaKeyPair, String> {
    let body = |label: &str| {
        let begin = format!("-----BEGIN {label}-----");
        let end = format!("-----END {label}-----");
        let start = pem.find(&begin)? + begin.len();
        let stop = start + pem[start..].find(&end)?;
        let text: String = pem[start..stop]
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect();
        base64::engine::general_purpose::STANDARD.decode(text).ok()
    };
    let unreadable = |why: ring::error::KeyRejected| {
        format!("the GitHub App's private key is not an RSA key ring accepts: {why}")
    };
    if let Some(der) = body("RSA PRIVATE KEY") {
        return ring::signature::RsaKeyPair::from_der(&der).map_err(unreadable);
    }
    if let Some(der) = body("PRIVATE KEY") {
        return ring::signature::RsaKeyPair::from_pkcs8(&der).map_err(unreadable);
    }
    Err("the GitHub App's private key is not a PEM RSA private key".to_owned())
}

/// How long from `now_ms` until a token should be renewed, never less than
/// a retry's wait.
pub fn renew_in_ms(token: &SessionToken, now_ms: i64) -> u64 {
    let due = token
        .expires_at_ms
        .saturating_sub(RENEW_BEFORE_MS)
        .saturating_sub(now_ms);
    u64::try_from(due).unwrap_or(0).max(RETRY_MS)
}

/// What a session's credential can do, in one line for the thread and the
/// status.
pub fn token_line(token: &SessionToken, now_ms: i64) -> String {
    let permissions = if token.permissions.is_empty() {
        "no permissions".to_owned()
    } else {
        token
            .permissions
            .iter()
            .map(|(name, level)| format!("{name}: {level}"))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let repositories = match &token.repositories {
        Some(listed) if !listed.is_empty() => listed.join(", "),
        _ => "every repository of the installation".to_owned(),
    };
    let minutes = (token.expires_at_ms.saturating_sub(now_ms).max(0) + 59_999) / 60_000;
    format!(
        "GitHub: an App token for {repositories} ({permissions}), renewed before it expires in \
         {minutes} minutes"
    )
}

#[cfg(test)]
mod tests;
