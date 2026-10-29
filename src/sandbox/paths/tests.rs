//! Tests for path containment, ported from `paths_test.ts`.

use super::{
    MAX_READ_BYTES, OpenOptions, copy_tree_beneath, host_path_under, open_beneath, read_beneath,
    truncate_beneath, within,
};

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
        read_beneath(&inside, "ordinary", MAX_READ_BYTES).expect("an ordinary file reads"),
        "the session's own"
    );
    assert!(
        read_beneath(&inside, "escape", MAX_READ_BYTES).is_err(),
        "a link out"
    );
    assert!(
        read_beneath(&inside, "../secret", MAX_READ_BYTES).is_err(),
        "a climb out"
    );
    assert!(
        read_beneath(
            &inside,
            &outside.path().join("secret").display().to_string(),
            MAX_READ_BYTES,
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

    assert_eq!(
        read_beneath(&inside, "real", MAX_READ_BYTES).expect("the file"),
        "inside"
    );
    assert!(read_beneath(&inside, "alias", MAX_READ_BYTES).is_err());
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

    assert!(
        read_beneath(
            &root.path().display().to_string(),
            "hop/secret",
            MAX_READ_BYTES
        )
        .is_err()
    );
}

/// The agent can make a pipe where a file should be. Opening it to read must
/// not wait for a writer that never comes, so the caller gets to see that it
/// is not a file and refuse it.
#[test]
fn a_planted_pipe_opens_at_once_and_is_not_a_file() {
    let root = tempfile::tempdir().expect("a temporary directory");
    nix::unistd::mkfifo(
        &root.path().join("pipe"),
        nix::sys::stat::Mode::S_IRUSR | nix::sys::stat::Mode::S_IWUSR,
    )
    .expect("a pipe");
    let inside = root.path().display().to_string();

    let opened = open_beneath(&inside, "pipe", &OpenOptions::read()).expect("opened at once");
    assert!(!opened.metadata().expect("its metadata").is_file());
    assert_eq!(
        read_beneath(&inside, "pipe", MAX_READ_BYTES).expect("nothing to read"),
        ""
    );
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

/// A file past the limit is refused with a kind of its own, read through the
/// bound rather than measured, and one at the limit reads whole.
#[test]
fn a_read_past_the_limit_is_refused_and_one_at_it_reads() {
    let root = tempfile::tempdir().unwrap();
    let inside = root.path().display().to_string();
    std::fs::write(root.path().join("at"), "x".repeat(16)).unwrap();
    std::fs::write(root.path().join("past"), "x".repeat(17)).unwrap();

    assert_eq!(read_beneath(&inside, "at", 16).unwrap(), "x".repeat(16));
    let refused = read_beneath(&inside, "past", 16).expect_err("past the limit");
    assert_eq!(refused.kind(), std::io::ErrorKind::FileTooLarge);

    // A character cut by the bound is still refused for its length.
    std::fs::write(root.path().join("cut"), format!("{}\u{e9}", "x".repeat(16))).unwrap();
    let cut = read_beneath(&inside, "cut", 16).expect_err("past the limit");
    assert_eq!(cut.kind(), std::io::ErrorKind::FileTooLarge);

    // Text that is not UTF-8 is a different failure, not this one.
    std::fs::write(root.path().join("binary"), [0xff, 0xfe]).unwrap();
    let binary = read_beneath(&inside, "binary", 16).expect_err("not text");
    assert_eq!(binary.kind(), std::io::ErrorKind::InvalidData);
}

/// Emptying a name the agent turned into a pipe does not wait for a reader:
/// it fails at once.
#[test]
fn emptying_a_planted_pipe_fails_rather_than_waits() {
    let root = tempfile::tempdir().unwrap();
    let pipe = root.path().join("notes");
    nix::unistd::mkfifo(&pipe, nix::sys::stat::Mode::from_bits_truncate(0o600)).unwrap();

    assert!(truncate_beneath(&root.path().display().to_string(), "notes").is_err());
}

/// A copy of a tree the session can change takes its files and directories
/// and leaves out what would have to be followed or read as something else:
/// a link, wherever it is, and a pipe. A link in place of the tree itself is
/// refused, and so is a tree nested past the depth.
#[test]
fn a_copy_takes_files_and_leaves_links_and_pipes_out() {
    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("secret"), "secret").unwrap();
    let tree = root.path().join("tree");
    std::fs::create_dir_all(tree.join("nested/deeper")).unwrap();
    std::fs::write(tree.join("nested/deeper/file"), "kept").unwrap();
    std::os::unix::fs::symlink(outside.path(), tree.join("nested/linked")).unwrap();
    std::os::unix::fs::symlink(outside.path().join("secret"), tree.join("file-link")).unwrap();
    nix::unistd::mkfifo(
        &tree.join("pipe"),
        nix::sys::stat::Mode::from_bits_truncate(0o600),
    )
    .unwrap();
    let inside = root.path().display().to_string();
    let copy = tempfile::tempdir().unwrap();

    copy_tree_beneath(&inside, "tree", &copy.path().join("out"), 8, 1024).expect("copied");

    let out = copy.path().join("out");
    assert_eq!(
        std::fs::read_to_string(out.join("nested/deeper/file")).unwrap(),
        "kept"
    );
    for left_out in ["nested/linked", "file-link", "pipe", "socket"] {
        assert!(
            std::fs::symlink_metadata(out.join(left_out)).is_err(),
            "{left_out}"
        );
    }

    std::os::unix::fs::symlink(&tree, root.path().join("alias")).unwrap();
    assert!(copy_tree_beneath(&inside, "alias", &copy.path().join("aliased"), 8, 1024).is_err());
    assert!(copy_tree_beneath(&inside, "tree", &copy.path().join("shallow"), 1, 1024).is_err());
    let over = copy_tree_beneath(&inside, "tree", &copy.path().join("small"), 8, 3)
        .expect_err("past the budget");
    assert_eq!(over.kind(), std::io::ErrorKind::FileTooLarge);
}
