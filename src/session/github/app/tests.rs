use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use base64::Engine as _;
use serde_json::{Value, json};

use super::{RETRY_MS, SessionToken, app_jwt, mint, renew_in_ms, token_line};
use crate::config::schema::GithubAppConfig;
use crate::session::pr::{Api, ApiReply};

/// A throwaway key made for these tests, never a credential.
const KEY: &str = include_str!("test-key.pem");
/// The same key as PKCS#8, the form a converted key often takes.
const KEY_PKCS8: &str = include_str!("test-key-pkcs8.pem");

/// 2026-01-01T00:00:00Z.
const NOW_MS: i64 = 1_767_225_600_000;

fn decoded(part: &str) -> Value {
    serde_json::from_slice(
        &base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(part)
            .unwrap(),
    )
    .unwrap()
}

/// The token is signed RS256 by the App's key, in either PEM form, names
/// the App as its issuer, and is good for less than the ten minutes GitHub
/// allows.
#[test]
fn the_app_proves_itself_with_a_token_its_key_signed() {
    for pem in [KEY, KEY_PKCS8] {
        let jwt = app_jwt(42, pem, NOW_MS / 1_000).expect("signed");
        let parts: Vec<_> = jwt.split('.').collect();
        assert_eq!(parts.len(), 3);
        assert_eq!(decoded(parts[0])["alg"], "RS256");
        let claims = decoded(parts[1]);
        assert_eq!(claims["iss"], "42");
        let lasts = claims["exp"].as_i64().unwrap() - claims["iat"].as_i64().unwrap();
        assert!(lasts <= 600, "{lasts}");

        let key = super::private_key(KEY).unwrap();
        let signature = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(parts[2])
            .unwrap();
        ring::signature::UnparsedPublicKey::new(
            &ring::signature::RSA_PKCS1_2048_8192_SHA256,
            key.public().as_ref(),
        )
        .verify(format!("{}.{}", parts[0], parts[1]).as_bytes(), &signature)
        .expect("the signature verifies with the App's public key");
    }
    assert!(app_jwt(42, "not a key", 0).is_err());
}

fn app(key_path: &str) -> GithubAppConfig {
    GithubAppConfig {
        app_id: 42,
        installation_id: 7,
        private_key_path: key_path.to_owned(),
        permissions: BTreeMap::from([("contents".to_owned(), "write".to_owned())]),
        repositories: vec!["errand".to_owned()],
    }
}

/// Each call an `Api` was given: its path, method and body.
type Seen = Arc<Mutex<Vec<(String, String, Option<Value>)>>>;

fn answering(reply: ApiReply, seen: Seen) -> Api {
    let reply = Arc::new(Mutex::new(Some(reply)));
    Arc::new(move |path, call| {
        seen.lock()
            .unwrap()
            .push((path, call.method.clone(), call.body.clone()));
        let reply = reply.lock().unwrap().take().expect("one call");
        Box::pin(async move { reply })
    })
}

/// A mint asks the installation for a token narrowed to the configured
/// repositories and permissions, proving itself with the App's token, and
/// reads back what GitHub granted.
#[tokio::test]
async fn a_mint_asks_for_what_is_configured_and_reads_what_was_granted() {
    let root = tempfile::tempdir().unwrap();
    let key = root.path().join("app.pem");
    std::fs::write(&key, KEY).unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let api = answering(
        ApiReply {
            status: 201,
            body: json!({
                "token": "ghs_minted",
                "expires_at": "2026-01-01T01:00:00Z",
                "permissions": { "contents": "write", "metadata": "read" },
                "repositories": [{ "name": "errand", "full_name": "QaidVoid/errand" }],
            }),
        },
        Arc::clone(&seen),
    );

    let token = mint(&app(&key.display().to_string()), &api, NOW_MS)
        .await
        .expect("minted");

    assert_eq!(token.token, "ghs_minted");
    assert_eq!(token.expires_at_ms, NOW_MS + 3_600_000);
    assert_eq!(token.permissions["metadata"], "read");
    assert_eq!(token.repositories, Some(vec!["QaidVoid/errand".to_owned()]));
    let seen = seen.lock().unwrap();
    assert_eq!(seen[0].0, "/app/installations/7/access_tokens");
    assert_eq!(seen[0].1, "POST");
    assert_eq!(
        seen[0].2,
        Some(json!({ "repositories": ["errand"], "permissions": { "contents": "write" } }))
    );
}

/// A refused mint, or a key that cannot be read, is an error that says why,
/// never a token.
#[tokio::test]
async fn a_mint_that_fails_says_why() {
    let root = tempfile::tempdir().unwrap();
    let key = root.path().join("app.pem");
    std::fs::write(&key, KEY).unwrap();
    let api = answering(
        ApiReply {
            status: 404,
            body: json!({ "message": "Not Found" }),
        },
        Arc::new(Mutex::new(Vec::new())),
    );

    let refused = mint(&app(&key.display().to_string()), &api, NOW_MS)
        .await
        .unwrap_err();
    assert!(
        refused.contains("installation 7 (404): Not Found"),
        "{refused}"
    );

    let missing = mint(&app("/nowhere/app.pem"), &api, NOW_MS)
        .await
        .unwrap_err();
    assert!(missing.contains("could not be read"), "{missing}");
}

/// A token is renewed ten minutes before it expires, and a renewal is never
/// sooner than a retry's wait.
#[test]
fn a_token_is_renewed_ahead_of_its_expiry() {
    let token = SessionToken {
        token: "ghs_x".to_owned(),
        expires_at_ms: NOW_MS + 3_600_000,
        permissions: BTreeMap::from([("issues".to_owned(), "write".to_owned())]),
        repositories: None,
    };
    assert_eq!(renew_in_ms(&token, NOW_MS), 50 * 60 * 1_000);
    assert_eq!(renew_in_ms(&token, NOW_MS + 3_590_000), RETRY_MS);
    assert_eq!(
        token_line(&token, NOW_MS),
        "GitHub: an App token for every repository of the installation (issues: write), \
         renewed before it expires in 60 minutes"
    );
}
