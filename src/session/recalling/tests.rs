//! Tests for the recall watcher: a query on disk becomes matches on disk.

use std::sync::Arc;

use super::{Recalling, render};
use crate::agent::requests::RECALL_DIR;
use crate::log::Logger;
use crate::memory::store::{Fact, MemoryStore, Scope};

fn silent() -> Logger {
    Logger::new(crate::log::LogFields::new(), Arc::new(|_, _| {}))
}

fn seeded(dir: &std::path::Path) -> Arc<MemoryStore> {
    let memory = MemoryStore::open(dir.join("memory.db")).expect("opened");
    for fact in [
        "the deploy target is cloudflare pages",
        "cloudflare needs the vitepress output directory set",
    ] {
        memory
            .remember(Scope::Project, "demo", fact, "s1", 0)
            .expect("stored");
    }
    memory
        .remember(Scope::User, "owner", "prefers jj over git", "s1", 0)
        .expect("stored");
    Arc::new(memory)
}

fn write_request(dir: &std::path::Path, id: &str, query: &str) {
    let recalls = dir.join(RECALL_DIR);
    std::fs::create_dir_all(&recalls).expect("made the recall dir");
    std::fs::write(
        recalls.join(format!("{id}.request")),
        serde_json::json!({ "query": query }).to_string(),
    )
    .expect("wrote the request");
}

/// A query on disk is answered with the matches on disk, the request removed.
#[test]
fn a_query_is_answered_from_the_store() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let memory = seeded(dir.path());
    write_request(dir.path(), "r1", "cloudflare");

    let recalling = Recalling::new(
        dir.path(),
        memory,
        "owner".to_owned(),
        "demo".to_owned(),
        silent(),
    );
    recalling.sweep();

    let answer =
        std::fs::read_to_string(dir.path().join(RECALL_DIR).join("r1.answer")).expect("answered");
    assert!(answer.contains("cloudflare pages"));
    assert!(answer.contains("vitepress"));
    // The request is consumed, so a later sweep does not answer it again.
    assert!(!dir.path().join(RECALL_DIR).join("r1.request").exists());
}

/// A query that matches nothing says so, rather than leaving a blank the agent
/// would read as a failure.
#[test]
fn a_query_that_matches_nothing_says_so() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let memory = seeded(dir.path());
    write_request(dir.path(), "r2", "kubernetes");

    Recalling::new(
        dir.path(),
        memory,
        "owner".to_owned(),
        "demo".to_owned(),
        silent(),
    )
    .sweep();

    let answer =
        std::fs::read_to_string(dir.path().join(RECALL_DIR).join("r2.answer")).expect("answered");
    assert!(answer.contains("Nothing recorded"), "got {answer}");
}

/// The owner's own facts are reachable too, not only the project's.
#[test]
fn a_recall_reaches_the_owners_facts() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let memory = seeded(dir.path());
    write_request(dir.path(), "r3", "jj");

    Recalling::new(
        dir.path(),
        memory,
        "owner".to_owned(),
        "demo".to_owned(),
        silent(),
    )
    .sweep();

    let answer =
        std::fs::read_to_string(dir.path().join(RECALL_DIR).join("r3.answer")).expect("answered");
    assert!(answer.contains("prefers jj"), "got {answer}");
}

/// Rendering states the query so the agent attributes the answer to it.
#[test]
fn the_answer_names_what_was_asked() {
    let found = vec![Fact {
        id: 1,
        fact: "the build runs with bun".to_owned(),
        created_at: 0,
    }];
    let rendered = render("build tool", &found);
    assert!(rendered.contains("`build tool`"));
    assert!(rendered.contains("- the build runs with bun"));
}
