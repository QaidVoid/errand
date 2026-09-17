//! Tests for project selection, ported from `projects_test.ts`.

use super::{ProjectEscapeError, ensure_project_directory, is_valid_project_name, select_project};
use tempfile::TempDir;

#[test]
fn a_leading_name_picks_the_project_and_leaves_the_rest_as_the_prompt() {
    let chosen = select_project("errand: fix the failing test", "/projects", "loose");

    assert_eq!(chosen.name, "errand");
    assert_eq!(chosen.path, "/projects/errand");
    assert_eq!(chosen.prompt, "fix the failing test");
    assert!(chosen.was_explicit);
}

#[test]
fn a_message_with_no_prefix_works_in_the_fallback_project() {
    let chosen = select_project("  just do the thing  ", "/projects", "session-7");

    assert_eq!(chosen.name, "session-7");
    assert_eq!(chosen.prompt, "just do the thing");
    assert!(!chosen.was_explicit);
}

/// A name that does not validate has to stay prompt text. Treating it as a
/// selection anyway is how a malformed name redirects a session instead of
/// merely failing to choose one.
#[test]
fn a_prefix_that_is_not_a_valid_name_is_ordinary_text() {
    for text in ["..: escape", ".hidden: go", "a b: spaced"] {
        let chosen = select_project(text, "/projects", "loose");
        assert!(!chosen.was_explicit, "{text}");
        assert_eq!(chosen.name, "loose", "{text}");
        assert_eq!(chosen.prompt, text.trim(), "{text}");
    }
}

/// A URL is the everyday case of a colon that names nothing.
#[test]
fn a_colon_inside_a_sentence_does_not_select_a_project() {
    let chosen = select_project(
        "read https://example.com/x and say what it does",
        "/p",
        "loose",
    );

    assert!(!chosen.was_explicit);
    assert_eq!(
        chosen.prompt,
        "read https://example.com/x and say what it does"
    );
}

#[test]
fn a_name_is_one_path_segment_and_never_reaches_for_another() {
    assert!(is_valid_project_name("errand"));
    assert!(is_valid_project_name("errand.v2_final-1"));
    for bad in [
        "",
        ".",
        "..",
        ".git",
        "a/b",
        "a\\b",
        "-lead",
        &"x".repeat(65),
    ] {
        assert!(!is_valid_project_name(bad), "{bad}");
    }
}

#[test]
fn the_project_directory_is_created_under_the_root() {
    let root = TempDir::with_prefix("errand-projects-").expect("a temporary directory");
    let root_path = root.path().to_string_lossy().into_owned();

    let chosen = select_project("demo: go", &root_path, "loose");
    ensure_project_directory(&chosen, &root_path).expect("created");

    assert!(
        std::fs::metadata(&chosen.path).expect("it exists").is_dir(),
        "{}",
        chosen.path
    );
    ensure_project_directory(&chosen, &root_path).expect("created again");
}

/// The name is a clean segment and the check still has to fail: only the
/// resolved path shows that the directory is a symlink out of the root.
#[test]
fn a_project_that_is_a_symlink_out_of_the_root_is_refused() {
    let root = TempDir::with_prefix("errand-projects-").expect("a temporary directory");
    let elsewhere = TempDir::with_prefix("errand-elsewhere-").expect("a temporary directory");
    #[cfg(unix)]
    std::os::unix::fs::symlink(elsewhere.path(), root.path().join("escapee")).expect("symlinked");

    let root_path = root.path().to_string_lossy().into_owned();
    let chosen = select_project("escapee: go", &root_path, "loose");

    let error = ensure_project_directory(&chosen, &root_path).expect_err("an escape is refused");
    assert!(matches!(error, ProjectEscapeError { .. }));
}

#[test]
fn a_message_that_opens_with_a_link_is_a_prompt_not_a_project_called_https() {
    // Reported: the first such message made a shared `https` project with a
    // live session, and every later link-first message was refused because
    // that project was busy.
    let selection = select_project(
        "https://github.com/QaidVoid/errand look at this",
        "/srv/projects",
        "s-1",
    );

    assert!(!selection.was_explicit);
    assert_eq!(selection.name, "s-1");
    assert_eq!(
        selection.prompt,
        "https://github.com/QaidVoid/errand look at this"
    );
}

#[test]
fn every_scheme_is_left_alone_not_just_https() {
    for url in [
        "http://x.dev",
        "ssh://git@x.dev/r",
        "ftp://x.dev",
        "file:///tmp/x",
    ] {
        let selection = select_project(url, "/srv/projects", "fallback");
        assert!(!selection.was_explicit, "{url}");
        assert_eq!(selection.prompt, url);
    }
}

#[test]
fn a_name_is_still_a_name_when_slashes_are_not_what_follows_the_colon() {
    // The fix keys on `://`, so a prompt whose text merely begins with slashes
    // after a space still selects its project.
    let selection = select_project("notes: //TODO tidy this up", "/srv/projects", "fallback");

    assert!(selection.was_explicit);
    assert_eq!(selection.name, "notes");
    assert_eq!(selection.prompt, "//TODO tidy this up");
}

#[test]
fn an_ordinary_project_prefix_is_unaffected() {
    let selection = select_project("errand: add a test", "/srv/projects", "fallback");

    assert!(selection.was_explicit);
    assert_eq!(selection.name, "errand");
    assert_eq!(selection.prompt, "add a test");
}
