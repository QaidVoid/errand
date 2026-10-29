//! Tests for the project a session works in.

use super::{ProjectEscapeError, ensure_project_directory, own_project};
use tempfile::TempDir;

/// Every session works in a directory named after it, and the whole message
/// is its prompt, a leading word and colon included.
#[test]
fn a_session_works_in_a_directory_of_its_own() {
    let chosen = own_project("  errand: fix the failing test  ", "/projects", "s7k2");

    assert_eq!(chosen.name, "s7k2");
    assert_eq!(chosen.path, "/projects/s7k2");
    assert_eq!(chosen.prompt, "errand: fix the failing test");
}

#[test]
fn the_project_directory_is_created_under_the_root() {
    let root = TempDir::with_prefix("errand-projects-").expect("a temporary directory");
    let root_path = root.path().to_string_lossy().into_owned();

    let chosen = own_project("go", &root_path, "s1");
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
    let chosen = own_project("go", &root_path, "escapee");

    let error = ensure_project_directory(&chosen, &root_path).expect_err("an escape is refused");
    assert!(matches!(error, ProjectEscapeError { .. }));
}
