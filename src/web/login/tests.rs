//! Tests for Discord login.

use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex};

use axum::http::{HeaderMap, HeaderValue, header};
use serde_json::json;

use super::{
    Authorized, Discord, DiscordCall, LOGIN_TTL_MS, LoginFailure, Logins, MAX_PENDING,
    MEMBERSHIP_TTL_MS, PENDING_TTL_MS, Visitor, cookie,
};
use crate::config::live::LiveConfig;
use crate::config::schema::WebLoginConfig;
use crate::config::validate::validate_config;

const MEMBER: &str = "200000000000000001";
const STRANGER: &str = "200000000000000002";
const BLOCKED: &str = "200000000000000003";

/// The login cookie as an https interface names it.
const HOST_LOGIN: &str = "__Host-errand_login";

/// Discord as a table: who each code belongs to, and who is in the guild.
struct FakeDiscord {
    members: Mutex<Vec<&'static str>>,
    asked: Mutex<Vec<String>>,
    down: AtomicBool,
}

impl Discord for FakeDiscord {
    fn identify(&self, code: String, redirect_uri: String) -> DiscordCall<Visitor> {
        self.asked.lock().unwrap().push(redirect_uri);
        Box::pin(async move {
            if code == "broken" {
                return Err("Discord was down".to_owned());
            }
            Ok(Visitor {
                id: code.clone(),
                name: format!("name-{code}"),
                avatar: None,
            })
        })
    }

    fn is_member(&self, _guild_id: String, user_id: String) -> DiscordCall<bool> {
        let member = self.members.lock().unwrap().contains(&user_id.as_str());
        let down = self.down.load(Ordering::SeqCst);
        Box::pin(async move {
            if down {
                return Err("Discord was down".to_owned());
            }
            Ok(member)
        })
    }
}

struct Fixture {
    logins: Logins,
    clock: Arc<AtomicI64>,
    discord: Arc<FakeDiscord>,
}

fn fixture(allowed: &[&str]) -> Fixture {
    let config = validate_config(&json!({
        "chat": {
            "token": "a.token.value",
            "channelId": "chan",
            "allowedUserIds": allowed,
            "blockedUserIds": [BLOCKED],
        },
        "agent": {
            "provider": "anthropic",
            "providers": {
                "anthropic": { "credentialName": "ANTHROPIC_API_KEY", "credential": "secret" },
            },
        },
        "projectRoot": "/tmp/errand-login-projects",
        "stateDir": "/tmp/errand-login-state",
    }))
    .expect("the test configuration is accepted");
    let clock = Arc::new(AtomicI64::new(1_000));
    let discord = Arc::new(FakeDiscord {
        members: Mutex::new(vec![MEMBER, BLOCKED]),
        asked: Mutex::new(Vec::new()),
        down: AtomicBool::new(false),
    });
    let now = Arc::clone(&clock);
    let logins = Logins::new(
        &WebLoginConfig {
            client_id: "app-1".to_owned(),
            client_secret: "shh".to_owned(),
        },
        "https://errand.example/",
        Arc::clone(&discord) as Arc<dyn Discord>,
        LiveConfig::new(config),
        Arc::new(move || now.load(Ordering::SeqCst)),
    );
    Fixture {
        logins,
        clock,
        discord,
    }
}

fn carrying(name: &str, value: &str) -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(
        header::COOKIE,
        HeaderValue::from_str(&format!("theme=dark; {name}={value}")).unwrap(),
    );
    headers
}

async fn logged_in(fixture: &Fixture, account: &str) -> HeaderMap {
    let (state, _) = fixture.logins.begin().unwrap();
    let (token, _) = fixture
        .logins
        .finish(
            Some(&state),
            Some(&state),
            Some(account),
            None,
            Some("guild-1"),
        )
        .await
        .expect("the login completes");
    carrying(HOST_LOGIN, &token)
}

#[test]
fn a_login_sends_the_browser_to_discord_with_its_state_and_way_back() {
    let fixture = fixture(&["*"]);
    let (state, url) = fixture.logins.begin().unwrap();
    let url = reqwest::Url::parse(&url).unwrap();
    let query: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();

    assert_eq!(url.host_str(), Some("discord.com"));
    assert_eq!(query["state"], state);
    assert_eq!(query["client_id"], "app-1");
    assert_eq!(query["scope"], "identify");
    assert_eq!(
        query["redirect_uri"],
        "https://errand.example/auth/callback"
    );
}

#[tokio::test]
async fn a_member_on_the_allowlist_may_act_as_themselves() {
    let fixture = fixture(&["*"]);
    let headers = logged_in(&fixture, MEMBER).await;
    let signed = fixture.logins.signed(&headers).expect("a login");

    assert_eq!(signed.visitor.id, MEMBER);
    assert!(signed.may_act);
    assert_eq!(
        fixture.discord.asked.lock().unwrap().as_slice(),
        ["https://errand.example/auth/callback"]
    );
}

#[tokio::test]
async fn a_wildcard_allowlist_does_not_admit_someone_outside_the_server() {
    let fixture = fixture(&["*"]);
    let headers = logged_in(&fixture, STRANGER).await;
    let signed = fixture.logins.signed(&headers).expect("a login");

    assert!(!signed.may_act);
}

#[tokio::test]
async fn a_blocked_or_unlisted_member_is_logged_in_but_may_not_act() {
    let fixture = fixture(&[MEMBER, BLOCKED]);
    let blocked = logged_in(&fixture, BLOCKED).await;
    assert!(!fixture.logins.signed(&blocked).unwrap().may_act);

    let fixture = self::fixture(&[STRANGER]);
    let unlisted = logged_in(&fixture, MEMBER).await;
    assert!(!fixture.logins.signed(&unlisted).unwrap().may_act);
}

#[tokio::test]
async fn without_a_known_server_nobody_may_act() {
    let fixture = fixture(&["*"]);
    let (state, _) = fixture.logins.begin().unwrap();
    let (token, _) = fixture
        .logins
        .finish(Some(&state), Some(&state), Some(MEMBER), None, None)
        .await
        .unwrap();
    let signed = fixture
        .logins
        .signed(&carrying(HOST_LOGIN, &token))
        .unwrap();

    assert!(!signed.may_act);
}

#[tokio::test]
async fn a_callback_for_another_browsers_login_is_refused() {
    let fixture = fixture(&["*"]);
    let (state, _) = fixture.logins.begin().unwrap();
    let (other, _) = fixture.logins.begin().unwrap();

    let refused = fixture
        .logins
        .finish(Some(&state), Some(&other), Some(MEMBER), None, Some("g"))
        .await;
    assert_eq!(refused.unwrap_err(), LoginFailure::Expired);

    let missing = fixture
        .logins
        .finish(Some(&other), None, Some(MEMBER), None, Some("g"))
        .await;
    assert_eq!(missing.unwrap_err(), LoginFailure::Expired);
}

#[tokio::test]
async fn a_state_is_good_for_one_callback_and_only_in_time() {
    let fixture = fixture(&["*"]);
    let (state, _) = fixture.logins.begin().unwrap();
    fixture
        .logins
        .finish(Some(&state), Some(&state), Some(MEMBER), None, Some("g"))
        .await
        .unwrap();
    let replayed = fixture
        .logins
        .finish(Some(&state), Some(&state), Some(MEMBER), None, Some("g"))
        .await;
    assert_eq!(replayed.unwrap_err(), LoginFailure::Expired);

    let (late, _) = fixture.logins.begin().unwrap();
    fixture.clock.fetch_add(PENDING_TTL_MS, Ordering::SeqCst);
    let too_late = fixture
        .logins
        .finish(Some(&late), Some(&late), Some(MEMBER), None, Some("g"))
        .await;
    assert_eq!(too_late.unwrap_err(), LoginFailure::Expired);
}

#[tokio::test]
async fn declining_on_discord_and_discord_failing_are_told_apart() {
    let fixture = fixture(&["*"]);
    let (state, _) = fixture.logins.begin().unwrap();
    let declined = fixture
        .logins
        .finish(
            Some(&state),
            Some(&state),
            None,
            Some("access_denied"),
            Some("g"),
        )
        .await;
    assert_eq!(declined.unwrap_err(), LoginFailure::Denied);

    let (state, _) = fixture.logins.begin().unwrap();
    let failed = fixture
        .logins
        .finish(Some(&state), Some(&state), Some("broken"), None, Some("g"))
        .await;
    assert!(matches!(failed.unwrap_err(), LoginFailure::Failed(_)));
}

#[tokio::test]
async fn a_login_lapses_and_a_logout_ends_it() {
    let fixture = fixture(&["*"]);
    let headers = logged_in(&fixture, MEMBER).await;
    fixture.clock.fetch_add(LOGIN_TTL_MS, Ordering::SeqCst);
    assert!(fixture.logins.signed(&headers).is_none());

    let headers = logged_in(&fixture, MEMBER).await;
    fixture.logins.end(&headers);
    assert!(fixture.logins.signed(&headers).is_none());
}

#[test]
fn cookies_are_found_by_exact_name_and_marked_for_https() {
    let headers = carrying("errand_login", "abc");
    assert_eq!(cookie(&headers, "errand_login").as_deref(), Some("abc"));
    assert_eq!(cookie(&headers, "errand"), None);

    let fixture = fixture(&["*"]);
    let set = fixture.logins.login_cookie("abc");
    assert!(set.starts_with("__Host-errand_login=abc; Path=/;"), "{set}");
    assert!(set.contains("HttpOnly"));
    assert!(set.contains("SameSite=Lax"));
    assert!(set.ends_with("; Secure"));
    assert!(
        fixture
            .logins
            .state_cookie("s")
            .starts_with("__Host-errand_login_state=s; Path=/;")
    );
}

/// A site on a sibling subdomain can set a cookie without the prefix, and only
/// without it, so one it sets is never read as a login.
#[tokio::test]
async fn a_cookie_without_the_prefix_is_not_a_login_over_https() {
    let fixture = fixture(&["*"]);
    let held = logged_in(&fixture, MEMBER).await;
    let token = cookie(&held, HOST_LOGIN).unwrap();

    assert!(
        fixture
            .logins
            .signed(&carrying("errand_login", &token))
            .is_none()
    );
    assert!(
        fixture
            .logins
            .state_of(&carrying("errand_login_state", "s"))
            .is_none()
    );
}

/// Starting a login costs a stranger one request, so how many may be waiting
/// is bounded, and the bound frees itself as they lapse.
#[test]
fn logins_in_progress_are_bounded_until_they_lapse() {
    let fixture = fixture(&["*"]);
    for _ in 0..MAX_PENDING {
        assert!(fixture.logins.begin().is_some());
    }
    assert!(fixture.logins.begin().is_none());

    fixture.clock.fetch_add(PENDING_TTL_MS, Ordering::SeqCst);
    assert!(fixture.logins.begin().is_some());
}

fn may_act(authorized: &Authorized) -> bool {
    matches!(authorized, Authorized::Signed(signed) if signed.may_act)
}

/// Somebody who leaves the server stops being able to act once the answer
/// that let them in is old, rather than for as long as their login lasts.
#[tokio::test]
async fn leaving_the_server_is_noticed_when_the_answer_is_stale() {
    let fixture = fixture(&["*"]);
    let headers = logged_in(&fixture, MEMBER).await;
    fixture.discord.members.lock().unwrap().clear();

    let fresh = fixture.logins.authorize(&headers, Some("g")).await;
    assert!(may_act(&fresh), "trusted while the answer is fresh");

    fixture.clock.fetch_add(MEMBERSHIP_TTL_MS, Ordering::SeqCst);
    let stale = fixture.logins.authorize(&headers, Some("g")).await;
    assert!(!may_act(&stale));
    assert!(!fixture.logins.signed(&headers).unwrap().may_act);
}

/// Not knowing is not the same as yes: nothing is done on their behalf until
/// Discord can be asked, and they are told why.
#[tokio::test]
async fn an_unanswered_membership_check_acts_for_nobody() {
    let fixture = fixture(&["*"]);
    let headers = logged_in(&fixture, MEMBER).await;
    fixture.discord.down.store(true, Ordering::SeqCst);
    fixture.clock.fetch_add(MEMBERSHIP_TTL_MS, Ordering::SeqCst);

    let answer = fixture.logins.authorize(&headers, Some("g")).await;
    assert!(matches!(answer, Authorized::Unconfirmed(_)), "{answer:?}");

    fixture.discord.down.store(false, Ordering::SeqCst);
    assert!(may_act(
        &fixture.logins.authorize(&headers, Some("g")).await
    ));
}
