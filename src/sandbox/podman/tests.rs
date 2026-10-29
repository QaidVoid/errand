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
use crate::sandbox::backend::{Denials, SYSTEM_LABEL, SandboxLaunch};
use crate::sandbox::kage_providers::INSTALLED_JSON;

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
        tmp_size: "2g".to_owned(),
        shm_size: "1g".to_owned(),
        disk_tmp: false,
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
        credential_names: BTreeMap::from([("zai-coding-cn".to_owned(), "ZAI_API_KEY".to_owned())]),
        plugins: Vec::new(),
        denied: Denials::default(),
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
                    } else if key == "run" && args.iter().any(|arg| arg == "providers") {
                        INSTALLED_JSON.to_owned()
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
    let args = podman_args(&config(), &launch(), None).args.join(" ");

    for forbidden in FORBIDDEN_ARGS {
        assert!(!args.contains(forbidden), "{forbidden}");
    }
}

#[test]
fn the_container_drops_privileges_it_never_needs() {
    let args = podman_args(&config(), &launch(), None).args;

    assert!(args.contains(&"--cap-drop=ALL".to_owned()));
    assert!(args.contains(&"--read-only".to_owned()));
    assert!(args.contains(&"--userns=keep-id".to_owned()));
    assert!(args.join(" ").contains("--security-opt no-new-privileges"));
}

/// podman's default maps the host, from which a service bound to 0.0.0.0 is
/// reachable from inside the container.
#[test]
fn the_host_is_not_reachable_from_a_session_with_network() {
    let args = podman_args(&config(), &launch(), None).args.join(" ");

    assert!(args.contains(&format!("--network={RESTRICTED_NETWORK}")));
    assert!(args.contains("--map-host-loopback,none"));
    assert!(args.contains("--map-guest-addr,none"));
}

#[test]
fn a_session_with_no_network_is_given_none_at_all() {
    let mut offline = config();
    offline.network = NetworkMode::None;
    let args = podman_args(&offline, &launch(), None).args.join(" ");

    assert!(args.contains("--network=none"));
    assert!(!args.contains("pasta"));
}

#[test]
fn the_project_and_the_state_are_the_only_writable_mounts() {
    let args = podman_args(&config(), &launch(), None).args;
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
    let args = podman_args(&limits, &launch(), None).args.join(" ");

    assert!(args.contains("--memory 2g"));
    assert!(args.contains("--cpus 1"));
    assert!(args.contains("--pids-limit 128"));
}

#[test]
fn scratch_is_a_sized_tmpfs_and_a_sized_shm() {
    let mut sized = config();
    sized.tmp_size = "1.5g".to_owned();
    sized.shm_size = "256m".to_owned();
    let args = podman_args(&sized, &launch(), None).args.join(" ");

    assert!(args.contains(&format!(
        "--tmpfs /tmp:size={},mode=1777",
        parse_size("1.5g").expect("bytes")
    )));
    assert!(args.contains(&format!(
        "--shm-size {}",
        parse_size("256m").expect("bytes")
    )));
}

/// With `diskTmp` the state's tmp directory is bound at `/tmp` in place of
/// the tmpfs, so scratch is on disk and counted by the disk budget.
#[test]
fn disk_tmp_binds_the_state_tmp_directory_in_place_of_the_tmpfs() {
    let mut disk = config();
    disk.disk_tmp = true;
    let args = podman_args(&disk, &launch(), None).args.join(" ");

    assert!(args.contains("--volume /state/s-1/tmp:/tmp:rw,Z"));
    assert!(!args.contains("--tmpfs"));
}

/// The runtime takes bytes, so a size that validated must convert.
#[test]
fn the_file_ceiling_is_passed_as_bytes_not_as_it_was_written() {
    let mut sized = config();
    sized.file_max = "512m".to_owned();
    let args = podman_args(&sized, &launch(), None).args.join(" ");

    assert!(args.contains(&format!(
        "--ulimit fsize={}",
        parse_size("512m").expect("bytes")
    )));
    assert!(!args.contains("fsize=512m"));
}

#[test]
fn the_container_is_labelled_so_a_leftover_can_be_found_later() {
    let args = podman_args(&config(), &launch(), None).args.join(" ");

    assert!(args.contains(&format!("{SYSTEM_LABEL}=true")));
    assert!(args.contains("errand.session=s-1"));
    assert!(args.contains("--name errand-s-1"));
}

#[test]
fn the_agent_is_started_with_its_provider_and_model_inside_the_image() {
    let args = podman_args(&config(), &launch(), None).args;
    let image = args
        .iter()
        .position(|arg| *arg == config().image)
        .expect("the image leads the command");

    assert!(image > 0);
    assert!(args[image..].join(" ").contains("kage rpc"));
    assert!(args.join(" ").contains("-m zai-coding-cn/glm-5.3"));
}

/// A secret is named on the command line and its value handed to podman's
/// environment, so no key sits where any local user can read the process
/// list. Anything else is written out, since a name given bare is read by
/// podman itself too, and a session's XDG directories would move its storage.
#[test]
fn a_secret_crosses_by_name_and_the_rest_is_written_out() {
    let mut config = config();
    config.env = Some(BTreeMap::from([(
        "CARGO_HOME".to_owned(),
        "/var/cache/cargo".to_owned(),
    )]));
    let mut launch = launch();
    launch
        .env
        .insert("XDG_DATA_HOME".to_owned(), "/state/kage".to_owned());
    launch
        .env
        .insert("GH_TOKEN".to_owned(), "gh-secret".to_owned());
    let command = podman_args(&config, &launch, None);
    let args = command.args.join(" ");

    assert!(args.contains("--env ZAI_API_KEY "));
    assert!(args.contains("--env GH_TOKEN "));
    assert!(!args.contains("provider-secret"));
    assert!(!args.contains("gh-secret"));
    assert!(args.contains("--env CARGO_HOME=/var/cache/cargo"));
    assert!(args.contains("--env XDG_DATA_HOME=/state/kage"));
    assert!(args.contains("--env HOME=/state/home"));
    assert_eq!(command.env["ZAI_API_KEY"], "provider-secret");
    assert_eq!(command.env["GH_TOKEN"], "gh-secret");
    assert_eq!(
        command.env.keys().collect::<Vec<_>>(),
        ["GH_TOKEN", "ZAI_API_KEY"]
    );
}

/// Under a broker, the container is handed the session's stand-in for each
/// provider key, never the key, and its proxy URL, all off the command line.
#[test]
fn a_brokered_launch_carries_stand_ins_not_keys() {
    let mut broker = session_broker(8443);
    broker.nonces = BTreeMap::from([("zai-coding-cn".to_owned(), "n-zai".to_owned())]);
    broker.credential_names = launch().credential_names.clone();
    let command = super::session_command(&config(), &launch(), Some(&broker));

    assert_eq!(command.env["ZAI_API_KEY"], "n-zai");
    assert!(command.env.contains_key("HTTPS_PROXY"));
    assert!(!command.env.keys().any(|name| name.starts_with("XDG_")));
    let args = command.args.join(" ");
    assert!(!args.contains("provider-secret"));
    assert!(!args.contains("n-zai"));
    assert!(!args.contains("run-secret"));
}

/// Every name the command line carries has its value, since podman skips a
/// bare `--env NAME` it cannot resolve without a word, and the session would
/// start with the variable missing.
#[test]
fn every_named_variable_has_a_value() {
    let mut config = config();
    config.env = Some(BTreeMap::from([("A".to_owned(), "1".to_owned())]));
    for broker in [None, Some(session_broker(8443))] {
        let command = podman_args(&config, &launch(), broker.as_ref());
        let named: Vec<&String> = command
            .args
            .windows(2)
            .filter(|pair| pair[0] == "--env" && !pair[1].contains('='))
            .map(|pair| &pair[1])
            .collect();
        assert!(!named.is_empty());
        for name in named {
            assert!(command.env.contains_key(name), "{name} has no value");
        }
    }
}

/// The credential is plumbing, so a file cannot decide what the agent uses.
#[test]
fn a_variable_the_daemon_sets_keeps_the_daemons_value() {
    let mut config = config();
    config.env = Some(BTreeMap::from([(
        "ZAI_API_KEY".to_owned(),
        "not-the-real-one".to_owned(),
    )]));
    let command = podman_args(&config, &launch(), None);

    assert_eq!(command.env["ZAI_API_KEY"], "provider-secret");
    assert_eq!(
        command
            .args
            .iter()
            .filter(|arg| *arg == "ZAI_API_KEY")
            .count(),
        1
    );
}

/// A session broker that never listened, for what a launch derives from one.
fn session_broker(port: u16) -> crate::sandbox::broker::SessionBroker {
    crate::sandbox::broker::SessionBroker {
        broker: crate::sandbox::broker::Broker::new(
            Vec::new(),
            vec![443],
            silent(),
            Vec::new(),
            false,
            String::new(),
            None,
        ),
        port,
        proxy_secret: "run-secret".to_owned(),
        nonces: BTreeMap::new(),
        credential_names: BTreeMap::new(),
    }
}

/// A brokered session's network is the broker's one port, forwarded from
/// the container's loopback, and nothing else; its tools are pointed there.
/// An unbrokered one keeps the plain restricted network, and one with no
/// network has none and no proxy.
#[test]
fn a_brokered_session_reaches_only_its_broker() {
    let broker = session_broker(8443);
    let brokered = podman_args(&config(), &launch(), Some(&broker));
    let network = brokered
        .args
        .iter()
        .find(|arg| arg.starts_with("--network="))
        .unwrap();
    assert_eq!(network, "--network=pasta:--splice-only,-T,8443");
    assert_eq!(
        brokered.env["HTTPS_PROXY"],
        "http://session:run-secret@127.0.0.1:8443"
    );
    assert!(brokered.args.contains(&"NO_PROXY=127.0.0.1".to_owned()));
    assert!(!brokered.args.join(" ").contains("run-secret"));

    let plain = podman_args(&config(), &launch(), None);
    assert!(
        plain
            .args
            .contains(&format!("--network={RESTRICTED_NETWORK}"))
    );
    assert!(!plain.env.contains_key("HTTPS_PROXY"));

    let mut offline = config();
    offline.network = NetworkMode::None;
    let none = podman_args(&offline, &launch(), Some(&broker));
    assert!(none.args.contains(&"--network=none".to_owned()));
    assert!(!none.env.contains_key("HTTPS_PROXY"));
}

#[tokio::test]
async fn podman_that_is_not_rootless_cannot_run_this_backend() {
    let mut answers = BTreeMap::new();
    answers.insert("info".to_owned(), (Some(0), Some("false".to_owned())));
    let (run, _calls) = fake_run(answers);
    let sandbox = PodmanSandbox::new(config(), silent(), run, BTreeMap::new(), None);

    let error = sandbox.probe().await.expect_err("refused");
    assert!(error.to_string().contains("not running rootless"));
}

#[tokio::test]
async fn a_missing_image_is_reported_at_startup_not_at_the_first_session() {
    let mut answers = BTreeMap::new();
    answers.insert("image".to_owned(), (Some(1), None));
    let (run, _calls) = fake_run(answers);
    let sandbox = PodmanSandbox::new(config(), silent(), run, BTreeMap::new(), None);

    let error = sandbox.probe().await.expect_err("refused");
    assert!(error.to_string().contains("is not present"));
}

/// Proxy mode promises a broker, so a backend that would run without one
/// says so, and the default refuses to start on it. With one wired, or under
/// open mode, there is no gap.
#[tokio::test]
async fn an_unbrokered_proxy_mode_reports_the_missing_broker() {
    let template = crate::sandbox::broker::BrokerTemplate {
        allow: vec!["*".to_owned()],
        ports: vec![443],
        allow_internal: false,
        log: silent(),
        providers: Vec::new(),
        credential_names: BTreeMap::new(),
        receipts: None,
        now: Arc::new(|| 0),
    };
    let mut open = config();
    open.egress.mode = EgressMode::Open;
    for (config, broker, gaps) in [
        (config(), None, 1),
        (config(), Some(template), 0),
        (open, None, 0),
    ] {
        let (run, _calls) = fake_run(BTreeMap::new());
        let sandbox = PodmanSandbox::new(config, silent(), run, BTreeMap::new(), broker);
        let report = sandbox.probe().await.expect("a report");
        assert_eq!(report.gaps.len(), gaps, "{:?}", report.gaps);
        if gaps == 1 {
            assert!(report.gaps[0].contains("no broker"), "{:?}", report.gaps);
        }
    }
}

#[tokio::test]
async fn a_healthy_host_reports_what_it_enforces() {
    let (run, _calls) = fake_run(BTreeMap::new());
    let sandbox = PodmanSandbox::new(config(), silent(), run, BTreeMap::new(), None);

    let report = sandbox.probe().await.expect("a report");

    assert!(report.notes.join("\n").contains("rootless containers"));
    assert!(
        report
            .notes
            .join("\n")
            .contains("measured rather than enforced")
    );
}

/// The list comes from the image's kage, which is the one sessions run, and
/// the probe keeps it for every launch.
#[tokio::test]
async fn the_images_own_kage_is_asked_which_providers_it_knows() {
    let (run, calls) = fake_run(BTreeMap::new());
    let sandbox = PodmanSandbox::new(config(), silent(), run, BTreeMap::new(), None);

    sandbox.probe().await.expect("a report");

    let taken = calls.lock().unwrap();
    let asked = taken
        .iter()
        .find(|call| call.first().map(String::as_str) == Some("run"))
        .expect("kage was asked");
    let image = &config().image;
    assert!(
        asked.ends_with(&[
            image.clone(),
            "kage".to_owned(),
            "providers".to_owned(),
            "--json".to_owned()
        ]),
        "{asked:?}"
    );
    assert!(asked.contains(&"--network=none".to_owned()), "{asked:?}");
    assert!(
        sandbox
            .kage_providers
            .get()
            .expect("kept")
            .overrides("openai")
    );
}

/// An image whose kage predates the command cannot be written for, so the
/// backend refuses to start and names the image.
#[tokio::test]
async fn an_image_whose_kage_cannot_list_its_providers_is_refused() {
    let mut answers = BTreeMap::new();
    answers.insert("run".to_owned(), (Some(2), Some(String::new())));
    let (run, _calls) = fake_run(answers);
    let sandbox = PodmanSandbox::new(config(), silent(), run, BTreeMap::new(), None);

    let error = sandbox.probe().await.expect_err("refused").to_string();
    assert!(error.contains(&config().image), "{error}");
    assert!(error.contains("update kage"), "{error}");
}

#[tokio::test]
async fn leftover_containers_are_found_by_label_and_removed() {
    let mut answers = BTreeMap::new();
    answers.insert(
        "ps".to_owned(),
        (Some(0), Some("errand-s-1\nerrand-s-2\n".to_owned())),
    );
    let (run, calls) = fake_run(answers);
    let sandbox = PodmanSandbox::new(config(), silent(), run, BTreeMap::new(), None);

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

/// Stopping a container closes its session's broker, so nothing it was
/// handed is honoured once it is asked to stop.
#[tokio::test]
async fn stopping_a_container_closes_its_broker() {
    let template = crate::sandbox::broker::BrokerTemplate {
        allow: vec!["*".to_owned()],
        ports: vec![443],
        allow_internal: false,
        log: silent(),
        providers: Vec::new(),
        credential_names: BTreeMap::new(),
        receipts: None,
        now: Arc::new(|| 0),
    };
    let started = template.start("s-1").await.expect("a broker");
    let port = started.port;
    let env = BTreeMap::from([("PATH".to_owned(), std::env::var("PATH").unwrap_or_default())]);
    let spawned = crate::sandbox::spawn::spawn_agent("sleep", &["30".to_owned()], Some(&env), None)
        .expect("a stand-in for the launcher");
    let (run, _calls) = fake_run(BTreeMap::new());
    let handle = crate::sandbox::SandboxHandle::Podman(Box::new(crate::sandbox::PodmanStop {
        session_id: "s-1".to_owned(),
        name: "errand-s-1".to_owned(),
        project_path: "/projects/demo".to_owned(),
        spawned: Arc::new(spawned),
        run,
        grace_ms: 1_000,
        log: silent(),
        broker: Mutex::new(Some(started.broker)),
        stopped: std::sync::atomic::AtomicBool::new(false),
    }));
    assert!(
        tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .is_ok()
    );

    handle.stop().await;

    for _ in 0..100 {
        if tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .is_err()
        {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!("the broker outlived its container");
}
