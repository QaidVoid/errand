//! Tests for the policy renderer, ported from `policy_test.ts`, plus the
//! golden output that pins the bytes the TypeScript produces.

use std::collections::BTreeMap;

use super::{PolicyOptions, RESOLV_CONF, policy_contents, policy_path};
use crate::config::schema::{NetworkMode, PolicyExtraConfig};
use crate::sandbox::backend::SandboxLaunch;
use crate::sandbox::runtime::AgentRuntime;

fn runtime() -> AgentRuntime {
    AgentRuntime {
        read_paths: vec!["/opt/agent/bin".to_owned(), "/opt/agent/lib/pi".to_owned()],
        path_entries: vec!["/opt/agent/bin".to_owned()],
    }
}

fn launch() -> SandboxLaunch {
    SandboxLaunch {
        session_id: "s-1".to_owned(),
        project_path: "/home/operator/code/demo".to_owned(),
        state_dir: "/home/operator/.local/state/errand/s-1".to_owned(),
        env: BTreeMap::from([("ZAI_API_KEY".to_owned(), "secret-value".to_owned())]),
        system_prompt_path: None,
        provider: "zai-coding-cn".to_owned(),
        model: Some("glm-5.3".to_owned()),
        providers: serde_json::Map::new(),
        extensions: Vec::new(),
        resume: false,
    }
}

/// What a test overrides, as borrowed pieces of the caller's own frame.
#[derive(Default)]
struct Overrides<'a> {
    launch: Option<&'a SandboxLaunch>,
    network: Option<NetworkMode>,
    egress_ports: Option<&'a [u16]>,
    runtime: Option<&'a AgentRuntime>,
    file_max: Option<&'a str>,
    tmp_size: Option<&'a str>,
    shm_size: Option<&'a str>,
    resolv_conf: Option<&'a str>,
    extra: Option<&'a PolicyExtraConfig>,
    env: Option<&'a BTreeMap<String, String>>,
    path_extra: Option<&'a [String]>,
}

fn policy_bytes_with<'a>(launch: &'a SandboxLaunch, overrides: &Overrides<'a>) -> String {
    let runtime = runtime();
    let options = PolicyOptions {
        launch: overrides.launch.unwrap_or(launch),
        network: overrides.network.unwrap_or(NetworkMode::Restricted),
        egress_ports: overrides.egress_ports.or(Some(&[443])),
        runtime: overrides.runtime.unwrap_or(&runtime),
        file_max: overrides.file_max.unwrap_or("1g"),
        tmp_size: overrides.tmp_size.unwrap_or("512m"),
        shm_size: overrides.shm_size.unwrap_or("256m"),
        resolv_conf: overrides
            .resolv_conf
            .unwrap_or("/var/lib/errand/resolv.conf"),
        extra: overrides.extra,
        env: overrides.env,
        path_extra: overrides.path_extra,
    };
    policy_contents(&options)
}

fn policy_bytes(overrides: &Overrides) -> String {
    let held = launch();
    policy_bytes_with(&held, overrides)
}

fn overrides() -> Overrides<'static> {
    Overrides::default()
}

#[test]
fn the_policy_clears_the_profiles_grants_before_listing_its_own() {
    assert!(policy_bytes(&overrides()).contains("reset = true"));
}

/// A host path names the operator and the shape of their machine.
#[test]
fn the_project_and_the_state_are_placed_never_shown_as_host_paths() {
    let written = policy_bytes(&overrides());

    assert!(written.contains(r#"{ path = "/home/operator/code/demo", at = "/workspace" }"#));
    assert!(
        written.contains(r#"{ path = "/home/operator/.local/state/errand/s-1", at = "/state" }"#)
    );
}

#[test]
fn the_state_directory_is_readable_as_well_as_writable() {
    let written = policy_bytes(&overrides());
    let read = written
        .split('\n')
        .find(|line| line.starts_with("read = "))
        .unwrap_or("");
    let write = written
        .split('\n')
        .find(|line| line.starts_with("write = "))
        .unwrap_or("");

    assert!(read.contains(r#"at = "/state""#));
    assert!(write.contains(r#"at = "/state""#));
}

/// Only the project and the session's own state may be written.
#[test]
fn nothing_outside_the_session_is_writable() {
    let write = policy_bytes(&overrides())
        .split('\n')
        .find(|line| line.starts_with("write = "))
        .unwrap_or("")
        .to_owned();

    assert!(!write.contains("/usr"));
    assert!(!write.contains("/etc"));
    assert!(!write.contains("/proc"));
    assert_eq!(write.matches("path = ").count(), 2);
}

#[test]
fn the_hosts_own_resolver_is_never_granted() {
    let written = policy_bytes(&overrides());

    assert!(
        written.contains(r#"{ path = "/var/lib/errand/resolv.conf", at = "/etc/resolv.conf" }"#)
    );
    assert!(!written.contains("\"/etc/resolv.conf\"]"));
    assert!(!written.contains("\"/etc/resolv.conf\","));
}

#[test]
fn the_resolver_that_is_handed_over_names_a_public_one_not_the_hosts() {
    assert!(RESOLV_CONF.contains("nameserver 1.1.1.1"));
    assert!(!RESOLV_CONF.contains("192.168."));
}

/// The daemon's environment holds the chat token. Naming what crosses is the
/// boundary that keeps it out of a session.
#[test]
fn only_the_named_variables_cross_and_no_value_is_written() {
    let held = SandboxLaunch {
        env: BTreeMap::from([
            ("ZAI_API_KEY".to_owned(), "secret-value".to_owned()),
            ("GH_TOKEN".to_owned(), "another-secret".to_owned()),
        ]),
        ..launch()
    };
    let written = policy_bytes_with(&held, &overrides());

    assert!(written.contains(r#"pass = ["GH_TOKEN", "ZAI_API_KEY"]"#));
    assert!(!written.contains("secret-value"));
    assert!(!written.contains("another-secret"));
}

#[test]
fn the_agents_own_directories_are_readable_or_it_cannot_start() {
    let written = policy_bytes(&overrides());

    assert!(written.contains("\"/opt/agent/lib/pi\""));
    assert!(written.contains("/opt/agent/bin"));
}

#[test]
fn the_wrapper_directory_leads_the_path_and_is_executable() {
    let written = policy_bytes(&overrides());
    let path = written
        .split('\n')
        .find(|line| line.starts_with("set = "))
        .unwrap_or("");
    let execute = written
        .split('\n')
        .find(|line| line.starts_with("execute = "))
        .unwrap_or("");

    assert!(
        path.contains(r#"PATH = "/state/home/bin:/opt/agent/bin:/usr/local/bin:/usr/bin:/bin""#)
    );
    assert!(path.contains(r#"HOME = "/state/home""#));
    assert!(execute.contains("\"/state/home/bin\""));
}

#[test]
fn a_file_size_ceiling_is_set_as_a_resource_limit() {
    assert!(
        policy_bytes(&Overrides {
            file_max: Some("512m"),
            ..overrides()
        })
        .contains(r#"file_max = "512m""#)
    );
}

/// The private /tmp and /dev/shm sizes reach the backend as resource limits,
/// so a build that unpacks or compiles under /tmp is not held to the tiny
/// default the backend would otherwise use.
#[test]
fn the_scratch_sizes_are_set_as_resource_limits() {
    let policy = policy_bytes(&Overrides {
        tmp_size: Some("512m"),
        shm_size: Some("256m"),
        ..overrides()
    });
    assert!(policy.contains(r#"tmp_size = "512m""#), "{policy}");
    assert!(policy.contains(r#"shm_size = "256m""#), "{policy}");
}

#[test]
fn outbound_https_is_allowed_and_no_network_means_no_egress_at_all() {
    assert!(policy_bytes(&overrides()).contains(r#"egress_allow = [{ host = "*", port = 443 }]"#));

    let offline = policy_bytes(&Overrides {
        network: Some(NetworkMode::None),
        ..overrides()
    });
    assert!(!offline.contains("[network]"));
    assert!(!offline.contains("egress_allow"));
}

#[test]
fn configured_ports_become_the_egress_allowlist_in_order() {
    let configured = policy_bytes(&Overrides {
        egress_ports: Some(&[80, 443]),
        ..overrides()
    });
    assert!(
        configured
            .contains(r#"egress_allow = [{ host = "*", port = 80 }, { host = "*", port = 443 }]"#)
    );
}

/// A session with no network opens nothing, whatever ports were named.
#[test]
fn ports_do_not_grant_egress_to_a_session_that_has_no_network() {
    let offline = policy_bytes(&Overrides {
        network: Some(NetworkMode::None),
        egress_ports: Some(&[80, 443]),
        ..overrides()
    });
    assert!(!offline.contains("egress_allow"));
}

#[test]
fn the_policy_lives_in_the_state_directory_never_in_the_project() {
    let written = policy_path(&launch());

    assert!(written.contains("/home/operator/.local/state/errand/s-1/"));
    assert!(!written.starts_with("/home/operator/code/demo"));
}

fn extra() -> PolicyExtraConfig {
    PolicyExtraConfig {
        read: vec!["/opt/toolchains".to_owned(), "/var/cache/shared".to_owned()],
        write: vec!["/srv/output".to_owned()],
        execute: vec!["/opt/toolchains/bin".to_owned()],
    }
}

#[test]
fn paths_granted_by_configuration_reach_the_policy() {
    let granted = extra();
    let policy = policy_bytes(&Overrides {
        extra: Some(&granted),
        ..overrides()
    });

    assert!(policy.contains("\"/opt/toolchains\""));
    assert!(policy.contains("\"/var/cache/shared\""));
    assert!(policy.contains("\"/srv/output\""));
    assert!(policy.contains("\"/opt/toolchains/bin\""));
}

/// Additive only: what the daemon grants is the floor, not a suggestion.
#[test]
fn an_extra_grant_takes_nothing_away() {
    let plain = policy_bytes(&Overrides {
        resolv_conf: Some("/state/resolv.conf"),
        ..overrides()
    });
    let granted = extra();
    let widened = policy_bytes(&Overrides {
        resolv_conf: Some("/state/resolv.conf"),
        extra: Some(&granted),
        ..overrides()
    });

    // Every path the generated policy names is still named in the widened one.
    let mut quoted = String::new();
    let mut characters = plain.chars().peekable();
    while let Some(character) = characters.next() {
        if character == '"' {
            quoted.push('"');
            for inner in characters.by_ref() {
                quoted.push(inner);
                if inner == '"' {
                    break;
                }
            }
        }
    }
    for name in quoted.split('"').filter(|name| !name.is_empty()) {
        let named = format!("\"{name}\"");
        assert!(widened.contains(&named), "{named}");
    }
    assert!(widened.contains("reset = true"));
}

/// A grant that names nothing must not silently widen anything.
#[test]
fn granting_nothing_changes_nothing() {
    let plain = policy_bytes(&Overrides {
        resolv_conf: Some("/state/resolv.conf"),
        ..overrides()
    });
    let empty = PolicyExtraConfig {
        read: Vec::new(),
        write: Vec::new(),
        execute: Vec::new(),
    };
    let widened = policy_bytes(&Overrides {
        resolv_conf: Some("/state/resolv.conf"),
        extra: Some(&empty),
        ..overrides()
    });

    assert_eq!(widened, plain);
}

#[test]
fn variables_set_by_configuration_reach_the_policy() {
    let env = BTreeMap::from([("CARGO_HOME".to_owned(), "/var/cache/cargo".to_owned())]);
    let policy = policy_bytes(&Overrides {
        resolv_conf: Some("/state/resolv.conf"),
        env: Some(&env),
        ..overrides()
    });

    assert!(policy.contains(r#"CARGO_HOME = "/var/cache/cargo""#));
    assert!(policy.contains(r#"HOME = "/state/home""#));
}

/// The credential and the GitHub token are plumbing, not settings.
#[test]
fn a_variable_the_daemon_passes_cannot_be_shadowed_by_configuration() {
    let env = BTreeMap::from([("ZAI_API_KEY".to_owned(), "not-the-real-one".to_owned())]);
    let policy = policy_bytes(&Overrides {
        resolv_conf: Some("/state/resolv.conf"),
        env: Some(&env),
        ..overrides()
    });

    assert!(!policy.contains("not-the-real-one"));
}

/// A toolchain named on purpose is the one a session should find.
#[test]
fn directories_added_by_configuration_lead_the_system_path() {
    let path_extra = vec!["/opt/toolchains/bin".to_owned()];
    let policy = policy_bytes(&Overrides {
        resolv_conf: Some("/state/resolv.conf"),
        path_extra: Some(&path_extra),
        ..overrides()
    });

    let named = policy
        .split('\n')
        .find(|line| line.starts_with("set = "))
        .unwrap_or("");
    let path_start = named
        .find("PATH = \"")
        .map_or(0, |at| at + "PATH = \"".len());
    let path_end = named[path_start..]
        .find('"')
        .map_or(path_start, |at| path_start + at);
    let path: Vec<&str> = named[path_start..path_end].split(':').collect();

    assert!(
        path.iter()
            .position(|entry| *entry == "/opt/toolchains/bin")
            < path.iter().position(|entry| *entry == "/usr/bin")
    );
    assert_eq!(path[0], "/state/home/bin");
}

#[test]
fn every_familys_certificate_bundle_is_reachable_not_just_debians() {
    // Measured targets of the canonical bundle path: arch /etc/ca-certificates,
    // fedora /etc/pki, tumbleweed /var/lib/ca-certificates.
    let written = policy_bytes(&overrides());

    for path in [
        "/etc/ssl",
        "/etc/ca-certificates",
        "/var/lib/ca-certificates",
        "/etc/pki",
    ] {
        assert!(written.contains(&format!("\"{path}\"")));
    }
}

/// A read grant used to carry execute with it, and bailey separated the two.
/// The runtime's directories hold the agent and the interpreter that runs it,
/// so without execute on them the agent's own `execve` is refused and the
/// session dies before it starts.
#[test]
fn the_runtimes_directories_are_granted_execute_not_only_read() {
    let runtime = AgentRuntime {
        read_paths: vec![
            "/opt/agent/node_modules".to_owned(),
            "/opt/agent/pkg".to_owned(),
        ],
        path_entries: vec!["/opt/agent/bin".to_owned(), "/opt/node/bin".to_owned()],
    };
    let text = policy_bytes_with(
        &launch(),
        &Overrides {
            runtime: Some(&runtime),
            ..overrides()
        },
    );
    let execute = text
        .split('\n')
        .find(|line| line.starts_with("execute ="))
        .unwrap_or("")
        .to_owned();
    for path in ["/opt/agent/bin", "/opt/node/bin", "/opt/agent/node_modules"] {
        assert!(execute.contains(path), "{path}");
    }
    // The rest of the read list is not widened by this.
    assert!(!execute.contains("/etc/ssl"));
}

/// The rendered policy is byte-identical to what the TypeScript produces for
/// the same configuration, which is the port's proof about the boundary.
#[test]
fn the_rendered_policy_matches_the_typescript_byte_for_byte() {
    let golden = include_str!("tests/golden.toml");
    let written = policy_bytes(&overrides());
    assert_eq!(written, golden);
}
