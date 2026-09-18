//! Tests for the memory store, ported from `store_test.ts`.

use super::{
    DEFAULT_MEMORY_BUDGET, MAX_FACT_LENGTH, MAX_PROJECT_FACTS, MemoryStore, Scope,
    memory_instructions, parse_notes,
};
use tempfile::TempDir;

fn store() -> MemoryStore {
    MemoryStore::open(":memory:").expect("an in-memory store opens")
}

#[test]
fn a_fact_told_once_is_remembered_and_read_back() {
    let memory = store();
    assert!(
        memory
            .remember(Scope::User, "u1", "prefers jj over git", "s1", 0)
            .expect("stored")
    );

    let facts = memory.facts_for(Scope::User, "u1", 100).expect("read back");
    assert_eq!(
        facts
            .iter()
            .map(|held| held.fact.as_str())
            .collect::<Vec<_>>(),
        ["prefers jj over git"]
    );
    memory.close().expect("closes");
}

/// An agent writing the same fact every session must not grow the block.
#[test]
fn the_same_fact_twice_is_stored_once_and_reported_as_nothing_new() {
    let memory = store();
    memory
        .remember(Scope::User, "u1", "prefers jj", "s1", 0)
        .expect("stored");

    assert!(
        !memory
            .remember(Scope::User, "u1", "  prefers   jj  ", "s2", 0)
            .expect("stored")
    );
    assert_eq!(
        memory
            .facts_for(Scope::User, "u1", 100)
            .expect("read back")
            .len(),
        1
    );
    memory.close().expect("closes");
}

#[test]
fn nothing_worth_storing_is_refused_rather_than_stored_empty() {
    let memory = store();

    assert!(
        !memory
            .remember(Scope::User, "u1", "   ", "s1", 0)
            .expect("judged")
    );
    assert!(
        !memory
            .remember(Scope::User, "", "a real fact", "s1", 0)
            .expect("judged")
    );
    memory.close().expect("closes");
}

#[test]
fn a_fact_longer_than_a_fact_is_cut_to_one() {
    let memory = store();
    memory
        .remember(Scope::User, "u1", &"x".repeat(500), "s1", 0)
        .expect("stored");

    let held = memory.facts_for(Scope::User, "u1", 100).expect("read back");
    assert_eq!(held[0].fact.chars().count(), MAX_FACT_LENGTH);
    memory.close().expect("closes");
}

/// The two scopes are separate memories that happen to share a table.
#[test]
fn what_a_project_knows_is_not_what_a_person_knows() {
    let memory = store();
    memory
        .remember(Scope::User, "u1", "likes short commits", "s1", 0)
        .expect("stored");
    memory
        .remember(
            Scope::Project,
            "demo",
            "the build is deno task check",
            "s1",
            0,
        )
        .expect("stored");

    assert_eq!(
        memory
            .facts_for(Scope::User, "demo", 100)
            .expect("read back")
            .len(),
        0
    );
    assert_eq!(
        memory
            .facts_for(Scope::Project, "demo", 100)
            .expect("read back")
            .len(),
        1
    );
    memory.close().expect("closes");
}

#[test]
fn facts_come_back_newest_first_since_the_newest_is_the_correction() {
    let memory = store();
    memory
        .remember(Scope::User, "u1", "first", "s1", 0)
        .expect("stored");
    memory
        .remember(Scope::User, "u1", "second", "s1", 0)
        .expect("stored");

    let facts = memory.facts_for(Scope::User, "u1", 100).expect("read back");
    assert_eq!(
        facts
            .iter()
            .map(|held| held.fact.as_str())
            .collect::<Vec<_>>(),
        ["second", "first"]
    );
    memory.close().expect("closes");
}

/// A project collects facts from everyone who works on it, so it is pruned.
#[test]
fn a_project_keeps_the_newest_facts_and_drops_the_oldest() {
    let memory = store();
    for index in 0..MAX_PROJECT_FACTS + 10 {
        memory
            .remember(Scope::Project, "demo", &format!("fact {index}"), "s1", 0)
            .expect("stored");
    }

    let kept = memory
        .facts_for(Scope::Project, "demo", i64::MAX)
        .expect("read back");
    assert_eq!(
        i64::try_from(kept.len()).expect("a count fits"),
        MAX_PROJECT_FACTS
    );
    assert_eq!(kept[0].fact, format!("fact {}", MAX_PROJECT_FACTS + 9));
    memory.close().expect("closes");
}

#[test]
fn being_forgotten_removes_the_facts_and_the_name_and_says_how_many() {
    let memory = store();
    memory.remember_user("u1", "amelia", 0).expect("kept");
    memory
        .remember(Scope::User, "u1", "one", "s1", 0)
        .expect("stored");
    memory
        .remember(Scope::User, "u1", "two", "s1", 0)
        .expect("stored");

    assert_eq!(memory.forget(Scope::User, "u1").expect("forgotten"), 2);
    assert_eq!(
        memory.facts_for(Scope::User, "u1", 100).expect("read back"),
        vec![]
    );
    assert_eq!(memory.display_name("u1").expect("read"), None);
    assert_eq!(
        memory
            .render("u1", DEFAULT_MEMORY_BUDGET)
            .expect("rendered"),
        ""
    );
    memory.close().expect("closes");
}

#[test]
fn a_name_is_kept_and_updated_when_it_changes() {
    let memory = store();
    memory.remember_user("u1", "amelia", 0).expect("kept");
    memory
        .remember_user("u1", "amelia (away)", 0)
        .expect("kept");

    assert_eq!(
        memory.display_name("u1").expect("read"),
        Some("amelia (away)".to_owned())
    );
    memory.close().expect("closes");
}

/// Somebody new must cost no context at all.
#[test]
fn nothing_known_about_somebody_renders_nothing() {
    let memory = store();

    assert_eq!(
        memory
            .render("nobody", DEFAULT_MEMORY_BUDGET)
            .expect("rendered"),
        ""
    );
    assert_eq!(
        memory
            .render_for_speaker("nobody", DEFAULT_MEMORY_BUDGET)
            .expect("rendered"),
        ""
    );
    assert_eq!(
        memory
            .render_project("untouched", DEFAULT_MEMORY_BUDGET)
            .expect("rendered"),
        ""
    );
    memory.close().expect("closes");
}

#[test]
fn what_is_known_is_rendered_as_the_agent_will_read_it() {
    let memory = store();
    memory.remember_user("u1", "amelia", 0).expect("kept");
    memory
        .remember(Scope::User, "u1", "prefers jj over git", "s1", 0)
        .expect("stored");

    let block = memory
        .render("u1", DEFAULT_MEMORY_BUDGET)
        .expect("rendered");

    assert!(block.contains("You are talking to amelia."));
    assert!(block.contains("- prefers jj over git"));
    memory.close().expect("closes");
}

#[test]
fn a_name_nobody_recorded_still_gets_the_facts() {
    let memory = store();
    memory
        .remember(Scope::User, "u1", "prefers jj", "s1", 0)
        .expect("stored");

    assert!(
        memory
            .render("u1", DEFAULT_MEMORY_BUDGET)
            .expect("rendered")
            .contains("- prefers jj")
    );
    memory.close().expect("closes");
}

/// Every line is paid for in context on every session that person starts.
#[test]
fn the_block_stops_at_its_budget_keeping_the_newest() {
    let memory = store();
    for index in 0..200 {
        memory
            .remember(
                Scope::User,
                "u1",
                &format!("fact number {index} with some words after it"),
                "s1",
                0,
            )
            .expect("stored");
    }

    let block = memory
        .render("u1", DEFAULT_MEMORY_BUDGET)
        .expect("rendered");

    assert!(block.len() <= DEFAULT_MEMORY_BUDGET);
    assert!(block.contains("fact number 199"));
    assert!(!block.contains("fact number 0 "));
    memory.close().expect("closes");
}

/// Sent with a message rather than in a prompt that was fixed long ago.
#[test]
fn a_speaker_joining_later_is_introduced_on_one_line() {
    let memory = store();
    memory.remember_user("u2", "brendan", 0).expect("kept");
    memory
        .remember(Scope::User, "u2", "works on the parser", "s1", 0)
        .expect("stored");

    let line = memory
        .render_for_speaker("u2", DEFAULT_MEMORY_BUDGET)
        .expect("rendered");

    assert!(line.starts_with("[context: "));
    assert!(!line.contains('\n'));
    assert!(line.contains("brendan"));
    memory.close().expect("closes");
}

#[test]
fn a_projects_memory_names_the_project_it_is_about() {
    let memory = store();
    memory
        .remember(
            Scope::Project,
            "demo",
            "the build is deno task check",
            "s1",
            0,
        )
        .expect("stored");

    let rendered = memory
        .render_project("demo", DEFAULT_MEMORY_BUDGET)
        .expect("rendered");
    assert!(rendered.contains("about the demo project"));
    assert!(rendered.contains("- the build is deno task check"));
    memory.close().expect("closes");
}

#[test]
fn what_the_agent_writes_by_hand_is_read_however_it_wrote_it() {
    assert_eq!(
        parse_notes("# heading\n\n- first fact\n* second fact\n  third fact  \n\n"),
        vec![
            "first fact".to_owned(),
            "second fact".to_owned(),
            "third fact".to_owned()
        ]
    );
}

#[test]
fn the_instructions_name_both_files_and_refuse_secrets() {
    let instructions = memory_instructions("/state/remember.md", "/state/project-notes.md");

    assert!(instructions.contains("/state/remember.md"));
    assert!(instructions.contains("/state/project-notes.md"));
    assert!(instructions.contains("Never record secrets"));
    // The whole point of the rewrite: the agent must not read the notes files
    // back to recall, and must know its memory is already in the prompt.
    assert!(instructions.contains("write-only"));
    assert!(instructions.contains("already above"));
}

/// Memory outlives the process, so it has to survive being reopened.
#[test]
fn what_was_remembered_is_still_there_after_a_restart() {
    let directory = TempDir::with_prefix("errand-memory-").expect("a temporary directory");
    let path = directory.path().join("memory.db");

    let first = MemoryStore::open(&path).expect("opened");
    first.remember_user("u1", "amelia", 0).expect("kept");
    first
        .remember(Scope::User, "u1", "prefers jj", "s1", 0)
        .expect("stored");
    first.close().expect("closed");

    let second = MemoryStore::open(&path).expect("reopened");
    assert_eq!(
        second.display_name("u1").expect("read"),
        Some("amelia".to_owned())
    );
    assert_eq!(
        second
            .facts_for(Scope::User, "u1", 100)
            .expect("read back")
            .len(),
        1
    );
    second.close().expect("closed");
}

/// The rendered block carries only the newest facts that fit the budget;
/// search reaches the rest by matching every word of a query against the
/// fact text, newest first.
#[test]
fn search_finds_facts_beyond_the_block_by_every_word() {
    let memory = store();
    for fact in [
        "the build runs with bun, not deno",
        "the deploy target is cloudflare pages",
        "cloudflare needs the vitepress output directory set",
        "prefers jj over git for everything",
    ] {
        memory
            .remember(Scope::Project, "demo", fact, "s1", 0)
            .expect("stored");
    }

    let hits: Vec<String> = memory
        .search(Scope::Project, "demo", "cloudflare", 10)
        .expect("searched")
        .into_iter()
        .map(|fact| fact.fact)
        .collect();
    assert_eq!(hits.len(), 2, "both cloudflare facts, {hits:?}");
    // Newest first.
    assert!(hits[0].contains("vitepress"));

    // Every word must appear, so a second word narrows.
    let narrowed = memory
        .search(Scope::Project, "demo", "cloudflare vitepress", 10)
        .expect("searched");
    assert_eq!(narrowed.len(), 1);

    // A blank query matches nothing: recalling everything is what the block is.
    assert!(
        memory
            .search(Scope::Project, "demo", "   ", 10)
            .expect("searched")
            .is_empty()
    );

    // A wildcard in the query is a literal, not a match-all.
    assert!(
        memory
            .search(Scope::Project, "demo", "%%%%", 10)
            .expect("searched")
            .is_empty()
    );
    memory.close().expect("closes");
}
