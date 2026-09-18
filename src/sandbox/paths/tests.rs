//! Tests for path containment, ported from `paths_test.ts`.

use super::{host_path_under, read_beneath, truncate_beneath, within};

const ROOT: &str = "/projects/demo";

#[test]
fn a_path_inside_the_root_resolves_to_an_absolute_path() {
    assert_eq!(
        within(ROOT, "src/main.ts"),
        Some("/projects/demo/src/main.ts".to_owned())
    );
    assert_eq!(
        within(ROOT, "./notes.md"),
        Some("/projects/demo/notes.md".to_owned())
    );
    assert_eq!(
        within(ROOT, "a/../b.txt"),
        Some("/projects/demo/b.txt".to_owned())
    );
}

#[test]
fn the_root_itself_is_inside_it() {
    assert_eq!(within(ROOT, "."), Some(ROOT.to_owned()));
    assert_eq!(within(ROOT, ROOT), Some(ROOT.to_owned()));
}

#[test]
fn a_path_that_climbs_out_is_refused() {
    assert_eq!(within(ROOT, "../other/secret"), None);
    assert_eq!(within(ROOT, "src/../../escaped"), None);
    assert_eq!(within(ROOT, "/etc/passwd"), None);
}

/// A sibling sharing a prefix is not inside, however similar the string is.
#[test]
fn a_sibling_with_the_same_prefix_is_not_inside() {
    assert_eq!(within(ROOT, "/projects/demo-other/file"), None);
    assert_eq!(within("/projects/demo", "/projects/demoted"), None);
}

#[test]
fn deep_traversal_is_refused_however_it_is_spelled() {
    for path in ["../..", "a/b/../../../out", "./../out", "a/./../../out"] {
        assert_eq!(within(ROOT, path), None, "{path}");
    }
}

#[test]
fn a_path_the_agent_sees_becomes_a_path_on_the_host() {
    assert_eq!(
        host_path_under("/workspace", ROOT, "/workspace/src/a.ts"),
        Some("/projects/demo/src/a.ts".to_owned())
    );
    assert_eq!(
        host_path_under("/workspace", ROOT, "src/a.ts"),
        Some("/projects/demo/src/a.ts".to_owned())
    );
    assert_eq!(
        host_path_under("/workspace", ROOT, "/workspace"),
        Some(ROOT.to_owned())
    );
}

/// A leading separator is not the host's root, or a tool call could read it.
#[test]
fn an_absolute_path_outside_the_workspace_is_read_as_project_relative() {
    assert_eq!(
        host_path_under("/workspace", ROOT, "/etc/passwd"),
        Some("/projects/demo/etc/passwd".to_owned())
    );
}

#[test]
fn a_path_that_climbs_out_of_the_project_has_no_host_path() {
    assert_eq!(
        host_path_under("/workspace", ROOT, "/workspace/../../secrets"),
        None
    );
    assert_eq!(host_path_under("/workspace", ROOT, "../secrets"), None);
    assert_eq!(host_path_under("/workspace", ROOT, "   "), None);
}

/// A sibling whose name merely starts with the root's is not inside it.
#[test]
fn a_root_is_matched_by_component_not_by_prefix() {
    assert_eq!(within("/srv/project", "."), Some("/srv/project".to_owned()));
    // `/srv/project-old` shares the root's spelling but not its path.
    assert_eq!(within("/srv/project", "/srv/project-old/secret"), None);
    assert_eq!(within("/srv/project", "../project-old/secret"), None);
}

/// The daemon runs outside the sandbox and a session can write inside it, so
/// a link planted in a session's own tree must not be a way to reach a file
/// the session could never open itself.
#[test]
fn a_planted_link_reads_nothing_outside_the_root() {
    let root = tempfile::tempdir().expect("a temporary directory");
    let outside = tempfile::tempdir().expect("a temporary directory");
    std::fs::write(outside.path().join("secret"), "the host's own").expect("written");
    std::fs::write(root.path().join("ordinary"), "the session's own").expect("written");
    std::os::unix::fs::symlink(outside.path().join("secret"), root.path().join("escape"))
        .expect("linked");
    let inside = root.path().display().to_string();

    assert_eq!(
        read_beneath(&inside, "ordinary").expect("an ordinary file reads"),
        "the session's own"
    );
    assert!(read_beneath(&inside, "escape").is_err(), "a link out");
    assert!(read_beneath(&inside, "../secret").is_err(), "a climb out");
    assert!(
        read_beneath(
            &inside,
            &outside.path().join("secret").display().to_string()
        )
        .is_err(),
        "an absolute path out"
    );
}

/// A link that stays inside is refused too. A session with something to say
/// about a file can say it in the file.
#[test]
fn even_a_link_that_points_back_inside_is_refused() {
    let root = tempfile::tempdir().expect("a temporary directory");
    std::fs::write(root.path().join("real"), "inside").expect("written");
    std::os::unix::fs::symlink("real", root.path().join("alias")).expect("linked");
    let inside = root.path().display().to_string();

    assert_eq!(read_beneath(&inside, "real").expect("the file"), "inside");
    assert!(read_beneath(&inside, "alias").is_err());
}

/// A directory on the way is as good a place to plant a link as the file.
#[test]
fn a_link_part_way_along_the_path_is_refused() {
    let root = tempfile::tempdir().expect("a temporary directory");
    let outside = tempfile::tempdir().expect("a temporary directory");
    std::fs::create_dir(outside.path().join("elsewhere")).expect("made");
    std::fs::write(outside.path().join("elsewhere").join("secret"), "out").expect("written");
    std::os::unix::fs::symlink(outside.path().join("elsewhere"), root.path().join("hop"))
        .expect("linked");

    assert!(read_beneath(&root.path().display().to_string(), "hop/secret").is_err());
}

/// Emptying a notes file must empty that file, not whatever it points at.
#[test]
fn truncating_through_a_link_leaves_the_target_alone() {
    let root = tempfile::tempdir().expect("a temporary directory");
    let outside = tempfile::tempdir().expect("a temporary directory");
    let target = outside.path().join("keep");
    std::fs::write(&target, "must survive").expect("written");
    std::os::unix::fs::symlink(&target, root.path().join("notes.md")).expect("linked");
    let inside = root.path().display().to_string();

    assert!(truncate_beneath(&inside, "notes.md").is_err());
    assert_eq!(
        std::fs::read_to_string(&target).expect("read back"),
        "must survive"
    );

    // An ordinary file in the same place is emptied as it always was.
    std::fs::write(root.path().join("plain.md"), "spent").expect("written");
    truncate_beneath(&inside, "plain.md").expect("emptied");
    assert_eq!(
        std::fs::read_to_string(root.path().join("plain.md")).expect("read back"),
        ""
    );
}
