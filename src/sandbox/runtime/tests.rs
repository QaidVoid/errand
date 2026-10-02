//! Tests for runtime discovery.

use std::os::unix::fs::PermissionsExt;
use std::sync::Arc;

use super::{Lookup, agent_runtime};
use tempfile::TempDir;

fn lookup_found(launcher: String) -> Lookup {
    Arc::new(move |name: &str| (name == "kage").then(|| launcher.clone()))
}

fn executable(path: &std::path::Path) {
    std::fs::write(path, "#!/bin/sh\n").expect("written");
    let mut permissions = std::fs::metadata(path).expect("stated").permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(path, permissions).expect("chmod");
}

/// The agent is one binary: its own directory is granted for the exec and
/// the PATH entry, and a link is followed to the directory holding the real
/// file.
#[test]
fn the_binaries_directory_is_granted_and_links_followed() {
    let root = TempDir::with_prefix("errand-runtime-").expect("a temporary directory");
    let bin = root.path().join("bin");
    let real_dir = root.path().join("real");
    std::fs::create_dir_all(&bin).expect("created");
    std::fs::create_dir_all(&real_dir).expect("created");
    let real = real_dir.join("kage");
    executable(&real);
    let launcher = bin.join("kage");
    std::os::unix::fs::symlink(&real, &launcher).expect("linked");

    let runtime = agent_runtime(&lookup_found(launcher.to_string_lossy().into_owned()))
        .expect("the agent is found");

    assert!(
        runtime
            .path_entries
            .contains(&bin.to_string_lossy().into_owned()),
        "{:?}",
        runtime.path_entries
    );
    let read_paths = runtime.read_paths.join("\n");
    assert!(
        read_paths.contains(&bin.to_string_lossy().to_string()),
        "{read_paths}"
    );
    assert!(
        read_paths.contains(&real_dir.to_string_lossy().to_string()),
        "{read_paths}"
    );
}

#[test]
fn no_agent_means_no_runtime_to_grant() {
    let lookup: Lookup = Arc::new(|_: &str| None);

    assert_eq!(agent_runtime(&lookup), None);
}
