use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};

use super::{ASK_COOLDOWN_MS, GrantStore, Lasting, MAX_ASKS, parse_grant};
use crate::log::{LogFields, Logger};

struct Held {
    store: GrantStore,
    clock: Arc<AtomicI64>,
    asked: Arc<Mutex<Vec<String>>>,
    path: std::path::PathBuf,
    _root: tempfile::TempDir,
}

fn held() -> Held {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("s-1.record").join("grants.json");
    let (store, clock, asked) = opened(&path);
    Held {
        store,
        clock,
        asked,
        path,
        _root: root,
    }
}

fn opened(path: &std::path::Path) -> (GrantStore, Arc<AtomicI64>, Arc<Mutex<Vec<String>>>) {
    let clock = Arc::new(AtomicI64::new(1_000_000));
    let asked = Arc::new(Mutex::new(Vec::new()));
    let (store, problem) = GrantStore::open(
        path,
        {
            let clock = Arc::clone(&clock);
            Arc::new(move || clock.load(Ordering::SeqCst))
        },
        {
            let asked = Arc::clone(&asked);
            Arc::new(move |host: &str| asked.lock().unwrap().push(host.to_owned()))
        },
        Logger::new(LogFields::new(), Arc::new(|_level, _line| {})),
    );
    assert_eq!(problem, None);
    (store, clock, asked)
}

/// A grant with no length lets the next connection through and no more.
#[test]
fn a_grant_for_once_is_used_up_by_the_next_connection() {
    let held = held();
    assert!(!held.store.spend("api.example.com"));

    held.store.grant("api.example.com", Lasting::Once, "u-1");

    assert!(held.store.holds("api.example.com"));
    assert!(held.store.holds("api.example.com"), "looking used it up");
    assert!(held.store.spend("api.example.com"));
    assert!(!held.store.spend("api.example.com"));
    assert!(held.store.list().is_empty());
}

/// A timed grant admits until its time is up, by the injected clock, and a
/// grant for always outlasts it.
#[test]
fn a_timed_grant_runs_out_and_an_always_one_does_not() {
    let held = held();
    held.store
        .grant("api.example.com", Lasting::For(600_000), "u-1");
    held.store.grant("*.other.example", Lasting::Always, "u-1");

    held.clock.fetch_add(599_999, Ordering::SeqCst);
    assert!(held.store.spend("api.example.com"));
    held.clock.fetch_add(1, Ordering::SeqCst);
    assert!(!held.store.spend("api.example.com"));

    held.clock.fetch_add(365 * 24 * 3_600_000, Ordering::SeqCst);
    assert!(held.store.spend("deep.sub.other.example"));
    assert!(!held.store.spend("other.example"));
    assert_eq!(held.store.list().len(), 1);
}

/// Revoking drops every grant for the host at once, and nothing else.
#[test]
fn revoking_drops_every_grant_for_the_host() {
    let held = held();
    held.store.grant("api.example.com", Lasting::Always, "u-1");
    held.store
        .grant("api.example.com", Lasting::For(60_000), "u-1");
    held.store.grant("kept.example.com", Lasting::Always, "u-1");

    assert_eq!(held.store.revoke("API.example.com").0, 2);

    assert!(!held.store.spend("api.example.com"));
    assert!(held.store.spend("kept.example.com"));
}

/// What is granted is on disk, so a store opened again over the same file, as
/// a resumed session's is, admits the same hosts. The file holds a host,
/// times and an account id, and nothing else.
#[test]
fn grants_survive_a_resume_and_the_file_holds_no_more_than_the_grant() {
    let held = held();
    held.store
        .grant("api.example.com", Lasting::For(600_000), "u-1");
    held.store.grant("once.example.com", Lasting::Once, "u-1");
    assert!(held.store.spend("once.example.com"));

    let (resumed, _, _) = opened(&held.path);

    assert!(resumed.spend("api.example.com"));
    assert!(!resumed.spend("once.example.com"));
    let written: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&held.path).unwrap()).unwrap();
    let mut keys: Vec<_> = written[0]
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        ["createdAt", "expiresAt", "grantedBy", "host", "id", "once"]
    );
}

/// A file that is not a list of grants grants nothing, and says why.
#[test]
fn a_grant_file_that_does_not_parse_grants_nothing_and_says_so() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("grants.json");
    std::fs::write(&path, "[{\"host\": \"*\"").unwrap();

    let (store, problem) = GrantStore::open(
        &path,
        Arc::new(|| 0),
        Arc::new(|_: &str| {}),
        Logger::new(LogFields::new(), Arc::new(|_level, _line| {})),
    );

    assert!(problem.unwrap().contains("is not a list of grants"));
    assert!(!store.spend("anything.example"));
}

/// An agent that retries a refused host in a loop asks the thread once per
/// cooldown, not once per attempt, and one that reaches for many names asks
/// about only a few of them. A name no grant could be written for is never
/// put in front of the owner.
#[test]
fn a_refused_host_is_asked_about_once_per_cooldown_and_a_few_at_a_time() {
    let held = held();

    for _ in 0..5 {
        held.store.refused("API.example.com");
    }
    held.store.refused("*.com");
    held.store.refused("under_score.internal");
    for n in 0..10 {
        held.store.refused(&format!("host{n}.example.com"));
    }
    assert_eq!(held.asked.lock().unwrap().len(), MAX_ASKS);
    assert_eq!(held.asked.lock().unwrap()[0], "api.example.com");

    held.clock.fetch_add(ASK_COOLDOWN_MS, Ordering::SeqCst);
    held.store.refused("api.example.com");
    assert_eq!(held.asked.lock().unwrap().len(), MAX_ASKS + 1);
}

/// `!grant` takes a host, or every name beneath a domain, and how long.
#[test]
fn a_grant_is_read_as_a_host_and_how_long() {
    assert_eq!(
        parse_grant("API.example.com"),
        Ok(("api.example.com".to_owned(), Lasting::Once))
    );
    assert_eq!(
        parse_grant("*.example.com 10m"),
        Ok(("*.example.com".to_owned(), Lasting::For(600_000)))
    );
    assert_eq!(
        parse_grant("example.com always"),
        Ok(("example.com".to_owned(), Lasting::Always))
    );
    for refused in [
        "",
        "*",
        "*.",
        "example.com:443",
        "https://example.com",
        "a..b",
        "example.com 0m",
    ] {
        assert!(parse_grant(refused).is_err(), "{refused:?}");
    }
    assert!(
        parse_grant("example.com soon")
            .unwrap_err()
            .contains("how long")
    );
}

/// A connection a standing grant admits leaves a grant for once to the host
/// unspent, and an id is never issued twice, even after its grant ran out.
#[test]
fn a_standing_grant_is_used_before_one_for_once_and_ids_are_not_reused() {
    let held = held();
    let first = held
        .store
        .grant("api.example.com", Lasting::For(1_000), "u-1")
        .0;
    held.clock.fetch_add(1_000, Ordering::SeqCst);
    held.store.grant("api.example.com", Lasting::Once, "u-1");
    let standing = held.store.grant("*.example.com", Lasting::Always, "u-1").0;

    assert!(held.store.spend("api.example.com"));

    let ids: Vec<_> = held
        .store
        .list()
        .into_iter()
        .map(|grant| grant.id)
        .collect();
    assert_eq!(ids, ["g2", "g3"]);
    assert_eq!(first.id, "g1");
    assert_eq!(standing.id, "g3");
    assert_eq!(
        held.store.covering("api.example.com").map(|grant| grant.id),
        Some("g2".to_owned())
    );
}
