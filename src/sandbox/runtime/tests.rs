//! Tests for runtime discovery, ported from `runtime_test.ts`.

use std::path::Path;
use std::sync::Arc;

use super::{Lookup, agent_runtime};
use tempfile::TempDir;

fn lookup_found(launcher: String) -> Lookup {
    Arc::new(move |name: &str| (name == "pi").then(|| launcher.clone()))
}

/// The agent's dependencies are packages of their own, beside it rather than
/// inside it. Granting only its own package leaves an import of a sibling
/// unresolvable, which surfaces as the agent exiting at startup.
#[test]
fn the_node_modules_the_agent_was_installed_into_is_granted() {
    let root = TempDir::with_prefix("errand-runtime-").expect("a temporary directory");
    let modules = root.path().join("node_modules");
    let own = modules.join("@scope").join("agent");
    let sibling = modules.join("@scope").join("helper");
    std::fs::create_dir_all(own.join("bin")).expect("created");
    std::fs::create_dir(&sibling).expect("created");
    std::fs::write(own.join("package.json"), "{}").expect("written");
    std::fs::write(sibling.join("package.json"), "{}").expect("written");
    let launcher = own.join("bin").join("pi");
    std::fs::write(&launcher, "#!/usr/bin/env node\n").expect("written");

    let runtime = agent_runtime(&lookup_found(launcher.to_string_lossy().into_owned()))
        .expect("the agent is found");

    // The package itself, and the tree its siblings resolve from.
    let read_paths = runtime.read_paths.join("\n");
    assert!(
        read_paths.contains(&Path::new(&own).to_string_lossy().to_string()),
        "{read_paths}"
    );
    assert!(
        read_paths.contains(&modules.to_string_lossy().to_string()),
        "{read_paths}"
    );
}

#[test]
fn an_agent_outside_any_node_modules_grants_only_what_it_has() {
    let root = TempDir::with_prefix("errand-runtime-").expect("a temporary directory");
    let own = root.path().join("opt").join("agent");
    std::fs::create_dir_all(own.join("bin")).expect("created");
    std::fs::write(own.join("package.json"), "{}").expect("written");
    let launcher = own.join("bin").join("pi");
    std::fs::write(&launcher, "#!/usr/bin/env node\n").expect("written");

    let runtime = agent_runtime(&lookup_found(launcher.to_string_lossy().into_owned()))
        .expect("the agent is found");

    assert!(
        runtime
            .read_paths
            .contains(&own.to_string_lossy().into_owned()),
        "{:?}",
        runtime.read_paths
    );
    // Nothing invented: there is no install tree to grant.
    assert!(
        !runtime
            .read_paths
            .iter()
            .any(|path| path.ends_with("node_modules"))
    );
}
