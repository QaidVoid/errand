//! Tests for path containment, ported from `paths_test.ts`.

use super::{host_path_under, within};

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
