//! Tests for the bailey backend, ported from `bailey_test.ts`.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use super::{
    BaileyOptions, BaileySandbox, ProviderBrokering, bailey_args, brokered_env,
    egress_proxy_endpoint, egress_proxy_url, parse_doctor, session_environment,
};
use crate::config::schema::NetworkMode;
use crate::config::schema::PolicyExtraConfig;
use crate::config::schema::{EgressConfig, EgressMode, SandboxBackend, SandboxConfig, defaults};
use crate::log::{LogFields, Logger};
use crate::sandbox::Run;
use crate::sandbox::backend::SandboxLaunch;
use tempfile::TempDir;

fn config() -> SandboxConfig {
    SandboxConfig {
        policy_extra: None,
        path_extra: None,
        env: None,
        egress_ports: vec![443],
        ..defaults_only()
    }
}

fn defaults_only() -> SandboxConfig {
    SandboxConfig {
        backend: SandboxBackend::Bailey,
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
        env: BTreeMap::new(),
        system_prompt_path: None,
        provider: "zai-coding-cn".to_owned(),
        model: Some("glm-5.3".to_owned()),
        providers: serde_json::Map::new(),
        extensions: Vec::new(),
        resume: false,
    }
}

const HEALTHY: &str =
    "landlock: yes (abi 5)\nuser namespaces: yes\ncgroup delegation: yes\nseccomp: yes";

/// What a scripted command answers: its exit code, its stdout, its stderr.
type Answer = (Option<i32>, Option<String>, Option<String>);

/// The argument lists a fake runner was handed, in order.
type Calls = Arc<Mutex<Vec<Vec<String>>>>;

/// Answers the tool's commands from a script, and records what was asked.
fn fake_run(answers: BTreeMap<String, Answer>) -> (Run, Calls) {
    let calls: Calls = Arc::new(Mutex::new(Vec::new()));
    let call_log = Arc::clone(&calls);
    let run = move |args: Vec<String>, _cwd: Option<String>| {
        let answers = answers.clone();
        let calls = Arc::clone(&call_log);
        Box::pin(async move {
            calls.lock().unwrap().push(args.clone());
            let key = args.first().map_or("", String::as_str);
            let (code, stdout, stderr) = answers.get(key).cloned().unwrap_or((None, None, None));
            Ok(super::super::RunResult {
                code: code.unwrap_or(0),
                stdout: stdout.unwrap_or_else(|| {
                    if key == "doctor" {
                        HEALTHY.to_owned()
                    } else {
                        String::new()
                    }
                }),
                stderr: stderr.unwrap_or_default(),
            })
        }) as super::super::RunFuture<super::super::RunResult>
    };
    (Arc::new(run), calls)
}

fn silent() -> Logger {
    Logger::new(LogFields::new(), Arc::new(|_level, _line| {}))
}

fn bailey_with(
    config: &SandboxConfig,
    root: &str,
    run: Run,
    options: BaileyOptions,
) -> BaileySandbox {
    BaileySandbox::new(config.clone(), silent(), root.to_owned(), run, options)
}

#[tokio::test]
async fn a_host_missing_landlock_cannot_run_this_backend_at_all() {
    let mut answers = BTreeMap::new();
    answers.insert(
        "doctor".to_owned(),
        (
            Some(0),
            Some("landlock: no\nuser namespaces: yes".to_owned()),
            None,
        ),
    );
    let (run, _calls) = fake_run(answers);
    let sandbox = bailey_with(&config(), "/state", run, BaileyOptions::default());

    let error = sandbox.probe().await.expect_err("refused");
    assert!(error.to_string().contains("does not provide Landlock"));
}

#[tokio::test]
async fn a_host_without_user_namespaces_cannot_run_this_backend_either() {
    let (_, unavailable) = parse_doctor("landlock: yes\nuser namespaces: no");
    assert_eq!(unavailable.len(), 1);
    assert!(unavailable[0].contains("user namespaces"));
}

/// A gap, not a refusal: the daemon still runs, and says what it cannot
/// enforce, rather than pretending the limits are applied.
#[tokio::test]
async fn no_cgroup_delegation_is_a_gap_that_is_reported_not_a_refusal() {
    let (gaps, unavailable) =
        parse_doctor("landlock: yes\nuser namespaces: yes\ncgroup delegation: no");

    assert_eq!(unavailable.len(), 0);
    assert_eq!(gaps.len(), 1);
    assert!(gaps[0].contains("memory, cpu, and process limits are not applied"));
}

#[tokio::test]
async fn a_tool_that_is_not_installed_is_reported_as_unavailable() {
    let run = |_args: Vec<String>, _cwd: Option<String>| {
        Box::pin(async move {
            Err::<super::super::RunResult, _>(std::io::Error::other("no such command"))
        }) as super::super::RunFuture<super::super::RunResult>
    };
    let sandbox = bailey_with(&config(), "/state", Arc::new(run), BaileyOptions::default());

    let error = sandbox.probe().await.expect_err("refused");
    assert!(error.to_string().contains("not installed"));
}

/// A version too old for the generated policy would otherwise fail every
/// launch, rather than once at startup where it can be acted on.
#[tokio::test]
async fn a_tool_that_refuses_the_generated_policy_is_caught_at_startup() {
    let root = TempDir::new().expect("a temporary directory");
    let mut answers = BTreeMap::new();
    answers.insert(
        "run".to_owned(),
        (
            Some(1),
            None,
            Some("unknown key: resources.file_max".to_owned()),
        ),
    );
    let (run, _calls) = fake_run(answers);
    let sandbox = bailey_with(
        &config(),
        &root.path().to_string_lossy(),
        run,
        BaileyOptions::default(),
    );

    let error = sandbox.probe().await.expect_err("refused");
    assert!(error.to_string().contains("does not accept the policy"));
}

#[tokio::test]
async fn the_probe_writes_its_policy_under_the_daemons_state_not_the_project() {
    let root = TempDir::new().expect("a temporary directory");
    let (run, calls) = fake_run(BTreeMap::new());
    let sandbox = bailey_with(
        &config(),
        &root.path().to_string_lossy(),
        run,
        BaileyOptions::default(),
    );

    let _ = sandbox.probe().await;

    let taken = calls.lock().unwrap();
    let trusted = taken
        .iter()
        .find(|call| call[0] == "trust")
        .map(|call| call[1].clone())
        .unwrap_or_default();
    assert!(trusted.contains(&root.path().to_string_lossy().to_string()));
}

#[test]
fn the_agent_args_lead_with_the_run_and_the_config() {
    let args = bailey_args(&config(), &launch(), "/state/s-1/policy.toml", None);

    assert_eq!(
        args[..6],
        [
            "run",
            "--isolate",
            "--config",
            "/state/s-1/policy.toml",
            "--profile",
            "ai-agent"
        ]
    );
    let said = args.join(" ");
    assert!(said.contains("pi --mode rpc --session-dir /state/sessions"));
    assert!(said.contains("--provider zai-coding-cn"));
    assert!(said.contains("--model glm-5.3"));
    assert!(!args.contains(&"--continue".to_owned()));
}

#[test]
fn hiding_the_host_address_asks_the_backend_for_a_private_namespace() {
    let plain = bailey_args(&config(), &launch(), "/p.toml", None);
    assert!(!plain.contains(&"--proxy-net".to_owned()));

    let mut hidden = config();
    hidden.hide_host_address = true;
    let hidden = bailey_args(&hidden, &launch(), "/p.toml", None);
    assert!(hidden.join(" ").contains("--isolate --proxy-net --config"));
}

#[test]
fn a_session_with_no_network_runs_under_the_offline_profile() {
    let mut offline = config();
    offline.network = NetworkMode::None;
    let args = bailey_args(&offline, &launch(), "/p.toml", None);
    assert!(args.join(" ").contains("--profile untrusted"));
}

#[test]
fn a_resumed_session_continues_the_conversation_it_stored() {
    let resumed = SandboxLaunch {
        resume: true,
        ..launch()
    };
    let args = bailey_args(&config(), &resumed, "/p.toml", None);
    assert!(args.contains(&"--continue".to_owned()));
}

/// The agent reads it where the state is placed, not where the host keeps it.
#[test]
fn the_system_prompt_is_named_at_the_path_the_agent_will_see() {
    let with_prompt = SandboxLaunch {
        system_prompt_path: Some("/home/operator/.local/state/errand/s-1/memory.md".to_owned()),
        ..launch()
    };
    let args = bailey_args(&config(), &with_prompt, "/p.toml", None);

    assert!(
        args.join(" ")
            .contains("--append-system-prompt /state/memory.md")
    );
    assert!(!args.join(" ").contains("/home/operator"));
}

/// The daemon's own environment holds the chat token.
#[test]
fn the_environment_is_rebuilt_from_a_named_list_not_inherited() {
    let source = BTreeMap::from([
        ("PATH".to_owned(), "/usr/bin".to_owned()),
        ("LANG".to_owned(), "en_GB.UTF-8".to_owned()),
        ("CHAT_TOKEN".to_owned(), "the-bot-token".to_owned()),
        ("HOME".to_owned(), "/home/operator".to_owned()),
    ]);
    let launch_env = BTreeMap::from([("ZAI_API_KEY".to_owned(), "provider-secret".to_owned())]);
    let env = session_environment(&launch_env, &source, "/state/s-1/home");

    assert_eq!(
        env.get("ZAI_API_KEY").map(String::as_str),
        Some("provider-secret")
    );
    assert_eq!(env.get("PATH").map(String::as_str), Some("/usr/bin"));
    assert_eq!(env.get("LANG").map(String::as_str), Some("en_GB.UTF-8"));
    assert_eq!(env.get("CHAT_TOKEN"), None);
    assert_eq!(env.get("HOME").map(String::as_str), Some("/state/s-1/home"));
}

/// Per-session limits are applied only when the tool has a cgroup it may
/// create children in, and it is told about one through this. Without it
/// crossing, an operator can set it on the service and watch it do nothing.
#[test]
fn the_cgroup_the_tool_may_use_is_passed_through() {
    let source = BTreeMap::from([
        (
            "BAILEY_CGROUP_ROOT".to_owned(),
            "/sys/fs/cgroup/system.slice/errand.service".to_owned(),
        ),
        ("CHAT_TOKEN".to_owned(), "secret".to_owned()),
    ]);
    let env = session_environment(&BTreeMap::new(), &source, "/state/s-1/home");

    assert_eq!(
        env.get("BAILEY_CGROUP_ROOT").map(String::as_str),
        Some("/sys/fs/cgroup/system.slice/errand.service")
    );
    assert_eq!(env.get("CHAT_TOKEN"), None);
}

/// Stands in for an installed agent, so a probe does not depend on the host.
/// CI has no pi on PATH, and these tests are about what the report says.
fn fake_agent(name: &str) -> Option<String> {
    (name == "pi").then(|| "/usr/local/bin/pi".to_owned())
}

/// The report must never describe a tighter boundary than the one applied.
#[tokio::test]
async fn extra_grants_are_named_in_what_the_backend_reports() {
    let root = TempDir::new().expect("a temporary directory");
    let (run, _calls) = fake_run(BTreeMap::new());
    let mut config = config();
    config.policy_extra = Some(PolicyExtraConfig {
        read: vec!["/opt/toolchains".to_owned()],
        write: vec!["/srv/output".to_owned()],
        execute: Vec::new(),
    });
    let sandbox = bailey_with(
        &config,
        &root.path().to_string_lossy(),
        run,
        BaileyOptions {
            lookup: Some(Arc::new(fake_agent)),
            ..BaileyOptions::default()
        },
    );

    let report = sandbox.probe().await.expect("a report");
    let said = report.notes.join("\n");

    assert!(said.contains("grants 2 path(s) beyond the generated policy"));
    assert!(said.contains("1 of them writable"));
    assert!(said.contains("/srv/output"));
}

/// Names only: a value is the operator's own and may be anything.
#[tokio::test]
async fn variables_set_by_configuration_are_named_in_what_the_backend_reports() {
    let root = TempDir::new().expect("a temporary directory");
    let (run, _calls) = fake_run(BTreeMap::new());
    let mut config = config();
    config.env = Some(BTreeMap::from([(
        "CARGO_HOME".to_owned(),
        "/var/cache/cargo".to_owned(),
    )]));
    let sandbox = bailey_with(
        &config,
        &root.path().to_string_lossy(),
        run,
        BaileyOptions {
            lookup: Some(Arc::new(fake_agent)),
            ..BaileyOptions::default()
        },
    );

    let said = sandbox.probe().await.expect("a report").notes.join("\n");

    assert!(said.contains("sessions are given CARGO_HOME from configuration"));
    assert!(!said.contains("/var/cache/cargo"));
}

#[test]
fn proxy_egress_passes_the_proxy_flag_and_drops_the_plain_one() {
    let mut config = config();
    config.hide_host_address = true; // would add --proxy-net on its own
    config.egress = EgressConfig {
        mode: EgressMode::Proxy,
        allow: vec!["github.com".to_owned()],
        allow_internal: false,
    };
    let args = bailey_args(&config, &launch(), "/p.toml", Some(54_321));
    assert!(
        args.join(" ")
            .contains(&format!("--egress-proxy {}", egress_proxy_endpoint(54_321)))
    );
    // --egress-proxy implies --proxy-net, so the plain flag is not added on top.
    assert!(!args.contains(&"--proxy-net".to_owned()));
}

#[test]
fn proxy_egress_without_a_running_broker_adds_no_flag() {
    // The port is none when no broker was started; the run stays as it was
    // rather than naming a proxy that is not there.
    let mut config = config();
    config.egress = EgressConfig {
        mode: EgressMode::Proxy,
        allow: Vec::new(),
        allow_internal: false,
    };
    let args = bailey_args(&config, &launch(), "/p.toml", None);
    assert!(!args.contains(&"--egress-proxy".to_owned()));
}

#[test]
fn open_egress_never_names_the_proxy_even_given_a_port() {
    let mut open = config();
    open.egress = EgressConfig {
        mode: EgressMode::Open,
        allow: Vec::new(),
        allow_internal: false,
    };
    let args = bailey_args(&open, &launch(), "/p.toml", Some(54_321));
    assert!(!args.contains(&"--egress-proxy".to_owned()));
}

#[test]
fn the_proxy_endpoint_and_url_are_built_from_the_map_address_and_port() {
    assert_eq!(egress_proxy_endpoint(8443), "169.254.169.1:8443");
    assert_eq!(egress_proxy_url(8443), "http://169.254.169.1:8443");
}

/// A session started on a provider other than the default still carries no
/// real key under a broker: its variable holds that provider's nonce, and a
/// provider the broker cannot route loses its variable.
#[test]
fn no_real_key_crosses_under_a_broker_whichever_provider_starts() {
    let brokering = ProviderBrokering {
        credential_names: BTreeMap::from([
            ("default".to_owned(), "DEFAULT_KEY".to_owned()),
            ("other".to_owned(), "OTHER_KEY".to_owned()),
            ("unrouted".to_owned(), "UNROUTED_KEY".to_owned()),
        ]),
        nonces: BTreeMap::from([
            ("default".to_owned(), "n-default".to_owned()),
            ("other".to_owned(), "n-other".to_owned()),
        ]),
    };
    let env = BTreeMap::from([
        ("OTHER_KEY".to_owned(), "real-other".to_owned()),
        ("UNROUTED_KEY".to_owned(), "real-unrouted".to_owned()),
        ("GH_TOKEN".to_owned(), "gh".to_owned()),
    ]);

    assert_eq!(
        brokered_env(&env, &brokering),
        BTreeMap::from([
            ("DEFAULT_KEY".to_owned(), "n-default".to_owned()),
            ("OTHER_KEY".to_owned(), "n-other".to_owned()),
            ("GH_TOKEN".to_owned(), "gh".to_owned()),
        ])
    );
}

/// What the brokering type carries; kept where the daemon wiring reads it.
#[allow(dead_code)]
fn brokering_shape() -> Option<ProviderBrokering> {
    None
}
