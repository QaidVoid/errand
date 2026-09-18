//! Tests for the podman backend, ported from `podman_test.ts`.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use super::{PodmanSandbox, RESTRICTED_NETWORK, podman_args};
use tempfile::TempDir;

use crate::config::schema::{
    EgressConfig, EgressMode, NetworkMode, SandboxBackend, SandboxConfig, defaults,
};
use crate::config::size::parse_size;
use crate::log::{LogFields, Logger};
use crate::sandbox::Run;
use crate::sandbox::backend::{SYSTEM_LABEL, SandboxLaunch};

/// Flags that would undo the isolation this backend exists to provide.
const FORBIDDEN_ARGS: [&str; 8] = [
    "--privileged",
    "--pid=host",
    "--ipc=host",
    "--network=host",
    "--userns=host",
    "--cap-add",
    "docker.sock",
    "podman.sock",
];

fn config() -> SandboxConfig {
    SandboxConfig {
        backend: SandboxBackend::Podman,
        policy_extra: None,
        path_extra: None,
        env: None,
        egress_ports: vec![443],
        ..defaults_config()
    }
}

fn defaults_config() -> SandboxConfig {
    SandboxConfig {
        backend: SandboxBackend::Podman,
        require_full_enforcement: defaults::REQUIRE_FULL_ENFORCEMENT,
        network: NetworkMode::Restricted,
        egress_ports: vec![443],
        egress: EgressConfig {
            mode: EgressMode::Proxy,
            allow: vec!["*".to_owned()],
            allow_internal: defaults::EGRESS_ALLOW_INTERNAL,
        },
        hide_host_address: defaults::HIDE_HOST_ADDRESS,
        image: defaults::IMAGE.to_owned(),
        memory: defaults::MEMORY.to_owned(),
        cpus: defaults::CPUS,
        pids: defaults::PIDS,
        file_max: defaults::FILE_MAX.to_owned(),
        disk: defaults::DISK.to_owned(),
        disk_check_ms: defaults::DISK_CHECK_MS,
        grace_period_ms: defaults::GRACE_PERIOD_MS,
        policy_extra: None,
        path_extra: None,
        env: None,
    }
}

fn launch() -> SandboxLaunch {
    SandboxLaunch {
        session_id: "s-1".to_owned(),
        project_path: "/projects/demo".to_owned(),
        state_dir: "/state/s-1".to_owned(),
        env: BTreeMap::from([("ZAI_API_KEY".to_owned(), "provider-secret".to_owned())]),
        system_prompt_path: None,
        provider: "zai-coding-cn".to_owned(),
        model: Some("glm-5.3".to_owned()),
        providers: serde_json::Map::new(),
        resume: false,
    }
}

/// What a scripted command answers: its exit code and its stdout.
type Answer = (Option<i32>, Option<String>);

/// The argument lists a fake runner was handed, in order.
type Calls = Arc<Mutex<Vec<Vec<String>>>>;

/// Answers podman's commands from a script, and records what was asked.
fn fake_run(answers: BTreeMap<String, Answer>) -> (Run, Calls) {
    let calls: Calls = Arc::new(Mutex::new(Vec::new()));
    let call_log = Arc::clone(&calls);
    let run = move |args: Vec<String>, _cwd: Option<String>| {
        let answers = answers.clone();
        let calls = Arc::clone(&call_log);
        Box::pin(async move {
            calls.lock().unwrap().push(args.clone());
            let key = args.first().map_or("", String::as_str);
            let (code, stdout) = answers.get(key).cloned().unwrap_or((None, None));
            Ok(super::super::RunResult {
                code: code.unwrap_or(0),
                stdout: stdout.unwrap_or_else(|| {
                    if key == "info" {
                        "true".to_owned()
                    } else {
                        String::new()
                    }
                }),
                stderr: String::new(),
            })
        }) as super::super::RunFuture<super::super::RunResult>
    };
    (Arc::new(run), calls)
}

fn silent() -> Logger {
    Logger::new(LogFields::new(), Arc::new(|_level, _line| {}))
}

/// Each of these would undo the isolation this backend exists to provide.
#[test]
fn nothing_that_would_open_the_container_is_ever_passed() {
    let args = podman_args(&config(), &launch()).join(" ");

    for forbidden in FORBIDDEN_ARGS {
        assert!(!args.contains(forbidden), "{forbidden}");
    }
}

#[test]
fn the_container_drops_privileges_it_never_needs() {
    let args = podman_args(&config(), &launch());

    assert!(args.contains(&"--cap-drop=ALL".to_owned()));
    assert!(args.contains(&"--read-only".to_owned()));
    assert!(args.contains(&"--userns=keep-id".to_owned()));
    assert!(args.join(" ").contains("--security-opt no-new-privileges"));
}

/// podman's default maps the host, from which a service bound to 0.0.0.0 is
/// reachable from inside the container.
#[test]
fn the_host_is_not_reachable_from_a_session_with_network() {
    let args = podman_args(&config(), &launch()).join(" ");

    assert!(args.contains(&format!("--network={RESTRICTED_NETWORK}")));
    assert!(args.contains("--map-host-loopback,none"));
    assert!(args.contains("--map-guest-addr,none"));
}

#[test]
fn a_session_with_no_network_is_given_none_at_all() {
    let mut offline = config();
    offline.network = NetworkMode::None;
    let args = podman_args(&offline, &launch()).join(" ");

    assert!(args.contains("--network=none"));
    assert!(!args.contains("pasta"));
}

#[test]
fn the_project_and_the_state_are_the_only_writable_mounts() {
    let args = podman_args(&config(), &launch());
    let volumes: Vec<String> = args
        .iter()
        .enumerate()
        .filter(|(index, _)| *index > 0 && args[index - 1] == "--volume")
        .map(|(_, arg)| arg.clone())
        .collect();

    assert_eq!(
        volumes,
        vec![
            "/projects/demo:/workspace:rw,Z".to_owned(),
            "/state/s-1:/state:rw,Z".to_owned(),
        ]
    );
}

#[test]
fn the_configured_limits_reach_the_container() {
    let mut limits = config();
    limits.memory = "2g".to_owned();
    limits.cpus = 1.0;
    limits.pids = 128;
    let args = podman_args(&limits, &launch()).join(" ");

    assert!(args.contains("--memory 2g"));
    assert!(args.contains("--cpus 1"));
    assert!(args.contains("--pids-limit 128"));
}

/// The runtime takes bytes, so a size that validated must convert.
#[test]
fn the_file_ceiling_is_passed_as_bytes_not_as_it_was_written() {
    let mut sized = config();
    sized.file_max = "512m".to_owned();
    let args = podman_args(&sized, &launch()).join(" ");

    assert!(args.contains(&format!(
        "--ulimit fsize={}",
        parse_size("512m").expect("bytes")
    )));
    assert!(!args.contains("fsize=512m"));
}

#[test]
fn the_container_is_labelled_so_a_leftover_can_be_found_later() {
    let args = podman_args(&config(), &launch()).join(" ");

    assert!(args.contains(&format!("{SYSTEM_LABEL}=true")));
    assert!(args.contains("errand.session=s-1"));
    assert!(args.contains("--name errand-s-1"));
}

#[test]
fn the_agent_is_started_with_its_provider_and_model_inside_the_image() {
    let args = podman_args(&config(), &launch());
    let image = args
        .iter()
        .position(|arg| *arg == config().image)
        .expect("the image leads the command");

    assert!(image > 0);
    assert!(
        args[image..]
            .join(" ")
            .contains("pi --mode rpc --session-dir /state/sessions")
    );
    assert!(args.join(" ").contains("--provider zai-coding-cn"));
}

#[test]
fn the_credential_crosses_as_an_environment_variable() {
    let args = podman_args(&config(), &launch()).join(" ");
    assert!(args.contains("--env ZAI_API_KEY=provider-secret"));
    assert!(args.contains("--env HOME=/state/home"));
}

#[test]
fn variables_set_by_configuration_cross_into_the_container() {
    let mut config = config();
    config.env = Some(BTreeMap::from([(
        "CARGO_HOME".to_owned(),
        "/var/cache/cargo".to_owned(),
    )]));
    let args = podman_args(&config, &launch()).join(" ");
    assert!(args.contains("--env CARGO_HOME=/var/cache/cargo"));
}

/// The credential is plumbing, so a file cannot decide what the agent uses.
#[test]
fn a_variable_the_daemon_sets_keeps_the_daemons_value() {
    let mut config = config();
    config.env = Some(BTreeMap::from([(
        "ZAI_API_KEY".to_owned(),
        "not-the-real-one".to_owned(),
    )]));
    let args = podman_args(&config, &launch());

    assert!(
        args.iter()
            .rposition(|arg| arg == "ZAI_API_KEY=provider-secret")
            > args
                .iter()
                .rposition(|arg| arg == "ZAI_API_KEY=not-the-real-one")
    );
}

#[tokio::test]
async fn podman_that_is_not_rootless_cannot_run_this_backend() {
    let mut answers = BTreeMap::new();
    answers.insert("info".to_owned(), (Some(0), Some("false".to_owned())));
    let (run, _calls) = fake_run(answers);
    let sandbox = PodmanSandbox::new(config(), silent(), run);

    let error = sandbox.probe().await.expect_err("refused");
    assert!(error.to_string().contains("not running rootless"));
}

#[tokio::test]
async fn a_missing_image_is_reported_at_startup_not_at_the_first_session() {
    let mut answers = BTreeMap::new();
    answers.insert("image".to_owned(), (Some(1), None));
    let (run, _calls) = fake_run(answers);
    let sandbox = PodmanSandbox::new(config(), silent(), run);

    let error = sandbox.probe().await.expect_err("refused");
    assert!(error.to_string().contains("is not present"));
}

#[tokio::test]
async fn a_healthy_host_reports_what_it_enforces_and_no_gaps() {
    let (run, _calls) = fake_run(BTreeMap::new());
    let sandbox = PodmanSandbox::new(config(), silent(), run);

    let report = sandbox.probe().await.expect("a report");

    assert_eq!(report.gaps.len(), 0);
    assert!(report.notes.join("\n").contains("rootless containers"));
    assert!(
        report
            .notes
            .join("\n")
            .contains("measured rather than enforced")
    );
}

#[tokio::test]
async fn leftover_containers_are_found_by_label_and_removed() {
    let mut answers = BTreeMap::new();
    answers.insert(
        "ps".to_owned(),
        (Some(0), Some("errand-s-1\nerrand-s-2\n".to_owned())),
    );
    let (run, calls) = fake_run(answers);
    let sandbox = PodmanSandbox::new(config(), silent(), run);

    assert_eq!(
        sandbox.list_orphans().await.expect("listed"),
        vec!["errand-s-1".to_owned(), "errand-s-2".to_owned()]
    );
    assert_eq!(
        sandbox
            .remove_orphans(&["errand-s-1".to_owned(), "errand-s-2".to_owned()])
            .await,
        2
    );

    let taken = calls.lock().unwrap();
    let filter = taken
        .iter()
        .find(|call| call[0] == "ps")
        .map(|call| call.join(" "))
        .unwrap_or_default();
    assert!(filter.contains(&format!("label={SYSTEM_LABEL}=true")));
}

/// A temporary directory keeps the shape of the fixture set; unused here.
#[allow(dead_code)]
fn _unused(temporary: TempDir) -> TempDir {
    temporary
}
