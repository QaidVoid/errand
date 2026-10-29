//! Tests for the bailey backend, ported from `bailey_test.ts`.

use crate::config::route::RoutePolicy;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use super::{
    BaileyOptions, BaileySandbox, bailey_args, egress_proxy_endpoint, parse_doctor,
    session_environment,
};
use crate::config::schema::NetworkMode;
use crate::config::schema::PolicyExtraConfig;
use crate::config::schema::{EgressConfig, EgressMode, SandboxBackend, SandboxConfig, defaults};
use crate::log::{LogFields, Logger};
use crate::sandbox::Run;
use crate::sandbox::backend::{Denials, SandboxLaunch};
use crate::sandbox::broker::{Broker, BrokerTemplate, ProviderUpstream, SessionBroker};
use crate::sandbox::egress::{brokered_env, egress_proxy_url};
use crate::sandbox::kage_providers::INSTALLED_JSON;
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
        credential_names: BTreeMap::new(),
        plugins: Vec::new(),
        denied: Denials::default(),
        resume: false,
    }
}

const HEALTHY: &str = "landlock: ABI 5\nuser namespaces: yes\ncgroup delegation: yes\nseccomp: yes";

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
            Some("landlock: unavailable\nuser namespaces: yes".to_owned()),
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
    let (_, unavailable) = parse_doctor("landlock: ABI 4\nuser namespaces: no");
    assert_eq!(unavailable.len(), 1);
    assert!(unavailable[0].contains("user namespaces"));
}

/// The one report the tool makes when the kernel has no Landlock at all. A
/// check that matches a line the tool never prints is how a Landlock-less
/// host would come to be described as fully confined.
#[test]
fn a_host_reporting_landlock_unavailable_is_refused() {
    let (_, unavailable) = parse_doctor("landlock: unavailable\nuser namespaces: yes");
    assert_eq!(unavailable.len(), 1);
    assert!(unavailable[0].contains("Landlock"));

    let (_, unavailable) = parse_doctor("user namespaces: yes");
    assert_eq!(unavailable.len(), 1);
    assert!(unavailable[0].contains("Landlock"));
}

/// A gap, not a refusal: the daemon still runs, and says what it cannot
/// enforce, rather than pretending the limits are applied.
#[tokio::test]
async fn no_cgroup_delegation_is_a_gap_that_is_reported_not_a_refusal() {
    let (gaps, unavailable) =
        parse_doctor("landlock: ABI 5\nuser namespaces: yes\ncgroup delegation: no");

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
    assert!(said.contains("kage rpc"));
    assert!(said.contains("-m zai-coding-cn/glm-5.3"));
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
fn a_resumed_session_takes_no_flag_history_continues_over_the_protocol() {
    let resumed = SandboxLaunch {
        resume: true,
        ..launch()
    };
    let fresh = bailey_args(&config(), &launch(), "/p.toml", None);
    let resumed = bailey_args(&config(), &resumed, "/p.toml", None);
    assert_eq!(fresh, resumed);
}

/// The agent is told where its memory is, not what is in it. The block is
/// the session's own state, and a command line is readable by every user on
/// the host; naming the file also keeps a host path out of the agent's view.
#[test]
fn the_memory_is_named_where_no_host_path_leaks() {
    let root = tempfile::tempdir().expect("a state directory");
    let memory = root.path().join("memory.md");
    std::fs::write(&memory, "remember this").expect("written");
    let with_prompt = SandboxLaunch {
        system_prompt_path: Some(memory.display().to_string()),
        ..launch()
    };
    let args = bailey_args(&config(), &with_prompt, "/p.toml", None);

    let said = args.join(" ");
    assert!(said.contains("--system-file /state/memory.md"), "{said}");
    assert!(!said.contains("remember this"), "{said}");
    assert!(!said.contains(&root.path().display().to_string()), "{said}");
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

/// bailey reads its global config, profiles, and trust store from the XDG
/// directories, falling back to HOME, which here is the session's private
/// home. A planted file there would run hooks as the daemon user, so the
/// tool is pointed at the operator's own directories instead.
#[test]
fn the_tool_reads_user_config_and_data_from_the_operator_dirs() {
    let source = BTreeMap::from([("HOME".to_owned(), "/home/operator".to_owned())]);
    let env = session_environment(&BTreeMap::new(), &source, "/state/s-1/home");

    assert_eq!(
        env.get("XDG_CONFIG_HOME").map(String::as_str),
        Some("/home/operator/.config")
    );
    assert_eq!(
        env.get("XDG_DATA_HOME").map(String::as_str),
        Some("/home/operator/.local/share")
    );
    assert_eq!(env.get("HOME").map(String::as_str), Some("/state/s-1/home"));
}

/// The daemon's own XDG directories win when they name absolute paths, so an
/// operator override keeps working.
#[test]
fn an_absolute_operator_xdg_directory_is_kept() {
    let source = BTreeMap::from([
        ("HOME".to_owned(), "/home/operator".to_owned()),
        (
            "XDG_CONFIG_HOME".to_owned(),
            "/etc/operator-config".to_owned(),
        ),
        (
            "XDG_DATA_HOME".to_owned(),
            "/var/lib/operator-data".to_owned(),
        ),
    ]);
    let env = session_environment(&BTreeMap::new(), &source, "/state/s-1/home");

    assert_eq!(
        env.get("XDG_CONFIG_HOME").map(String::as_str),
        Some("/etc/operator-config")
    );
    assert_eq!(
        env.get("XDG_DATA_HOME").map(String::as_str),
        Some("/var/lib/operator-data")
    );
}

/// A relative XDG directory would resolve against the tool's working
/// directory, which is the project the agent writes, so it falls back to the
/// operator's home like an unset one.
#[test]
fn a_relative_operator_xdg_directory_falls_back_to_the_home() {
    let source = BTreeMap::from([
        ("HOME".to_owned(), "/home/operator".to_owned()),
        ("XDG_CONFIG_HOME".to_owned(), "relative/config".to_owned()),
        ("XDG_DATA_HOME".to_owned(), String::new()),
    ]);
    let env = session_environment(&BTreeMap::new(), &source, "/state/s-1/home");

    assert_eq!(
        env.get("XDG_CONFIG_HOME").map(String::as_str),
        Some("/home/operator/.config")
    );
    assert_eq!(
        env.get("XDG_DATA_HOME").map(String::as_str),
        Some("/home/operator/.local/share")
    );
}

/// Stands in for an installed agent, so a probe does not depend on the host.
/// CI has no kage on PATH, and these tests are about what the report says.
fn fake_agent(name: &str) -> Option<String> {
    (name == "kage").then(|| "/usr/local/bin/kage".to_owned())
}

/// Stands in for the agent's own answer to being asked which providers it
/// knows, and records what it was run with.
fn fake_kage(code: i32, stdout: &'static str) -> (Run, Calls) {
    let calls: Calls = Arc::new(Mutex::new(Vec::new()));
    let call_log = Arc::clone(&calls);
    let run = move |args: Vec<String>, _cwd: Option<String>| {
        call_log.lock().unwrap().push(args);
        Box::pin(async move {
            Ok(super::super::RunResult {
                code,
                stdout: stdout.to_owned(),
                stderr: "error: unrecognized subcommand 'providers'".to_owned(),
            })
        }) as super::super::RunFuture<super::super::RunResult>
    };
    (Arc::new(run), calls)
}

/// The kage asked is the one on PATH that sessions run, and the probe keeps
/// its answer for every launch.
#[tokio::test]
async fn the_hosts_own_kage_is_asked_which_providers_it_knows() {
    let root = TempDir::new().expect("a temporary directory");
    let (run, _calls) = fake_run(BTreeMap::new());
    let (kage, asked) = fake_kage(0, INSTALLED_JSON);
    let sandbox = bailey_with(
        &config(),
        &root.path().to_string_lossy(),
        run,
        BaileyOptions {
            lookup: Some(Arc::new(fake_agent)),
            kage: Some(kage),
            ..BaileyOptions::default()
        },
    );

    sandbox.probe().await.expect("a report");

    assert_eq!(
        *asked.lock().unwrap(),
        [["/usr/local/bin/kage", "providers", "--json"]]
    );
    assert!(
        sandbox
            .kage_providers
            .get()
            .expect("kept")
            .overrides("openai")
    );
}

/// A kage too old to say which providers it knows would have its sessions
/// written by a guess, so the backend refuses to start instead.
#[tokio::test]
async fn a_kage_that_cannot_list_its_providers_is_refused_at_startup() {
    let root = TempDir::new().expect("a temporary directory");
    let (run, _calls) = fake_run(BTreeMap::new());
    let sandbox = bailey_with(
        &config(),
        &root.path().to_string_lossy(),
        run,
        BaileyOptions {
            lookup: Some(Arc::new(fake_agent)),
            kage: Some(fake_kage(2, "").0),
            ..BaileyOptions::default()
        },
    );

    let error = sandbox.probe().await.expect_err("refused").to_string();
    assert!(error.contains("unrecognized subcommand"), "{error}");
    assert!(error.contains("update kage"), "{error}");
    assert!(sandbox.kage_providers.get().is_err());
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
            kage: Some(fake_kage(0, INSTALLED_JSON).0),
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
            kage: Some(fake_kage(0, INSTALLED_JSON).0),
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
    let secret = "run-secret";
    assert_eq!(
        egress_proxy_url("169.254.169.1", 8443, secret),
        "http://session:run-secret@169.254.169.1:8443"
    );
    assert!(!egress_proxy_endpoint(8443).contains(secret));
}

#[test]
fn egress_env_carries_the_credential_in_all_four_proxy_variables() {
    let mut config = config();
    config.env = Some(BTreeMap::from([(
        "CARGO_HOME".to_owned(),
        "/cache".to_owned(),
    )]));
    let (run, _) = fake_run(BTreeMap::new());
    let sandbox = bailey_with(&config, "/state", run, BaileyOptions::default());
    let broker = session_broker(8443, "run-secret");
    let env = sandbox.egress_env(Some(&broker)).unwrap();
    for name in ["HTTPS_PROXY", "https_proxy", "HTTP_PROXY", "http_proxy"] {
        assert_eq!(env[name], "http://session:run-secret@169.254.169.1:8443");
    }
    for name in ["NO_PROXY", "no_proxy"] {
        assert_eq!(env[name], "169.254.169.1");
    }
    assert_eq!(env["NODE_USE_ENV_PROXY"], "1");
    assert_eq!(env["CARGO_HOME"], "/cache");

    let args = bailey_args(&config, &launch(), "/p.toml", Some(8443));
    let proxy_flag = args.iter().position(|arg| arg == "--egress-proxy").unwrap();
    assert_eq!(args[proxy_flag + 1], "169.254.169.1:8443");
    assert!(!args.iter().any(|arg| arg.contains("run-secret")));
}

#[test]
fn egress_env_names_no_proxy_without_a_broker_or_outside_proxy_mode() {
    for broker in [None, Some(session_broker(8443, "run-secret"))] {
        for env in [
            None,
            Some(BTreeMap::from([
                ("CARGO_HOME".to_owned(), "/cache".to_owned()),
                ("HTTP_PROXY".to_owned(), "http://operator:8080".to_owned()),
                ("NO_PROXY".to_owned(), "operator.example".to_owned()),
            ])),
        ] {
            let mut config = config();
            config.env = env.clone();
            // A broker only matters under proxy mode, and none at all leaves
            // the operator env as it was in any mode.
            if broker.is_some() {
                config.egress.mode = EgressMode::Open;
            }
            let (run, _) = fake_run(BTreeMap::new());
            let sandbox = bailey_with(&config, "/state", run, BaileyOptions::default());
            assert_eq!(sandbox.egress_env(broker.as_ref()), env);
        }
    }
}

/// A session started on a provider other than the default still carries no
/// real key under a broker: its variable holds that provider's nonce, and a
/// provider the broker cannot route loses its variable.
#[test]
fn no_real_key_crosses_under_a_broker_whichever_provider_starts() {
    let credential_names = BTreeMap::from([
        ("default".to_owned(), "DEFAULT_KEY".to_owned()),
        ("other".to_owned(), "OTHER_KEY".to_owned()),
        ("unrouted".to_owned(), "UNROUTED_KEY".to_owned()),
    ]);
    let nonces = BTreeMap::from([
        ("default".to_owned(), "n-default".to_owned()),
        ("other".to_owned(), "n-other".to_owned()),
    ]);
    let env = BTreeMap::from([
        ("OTHER_KEY".to_owned(), "real-other".to_owned()),
        ("UNROUTED_KEY".to_owned(), "real-unrouted".to_owned()),
        ("GH_TOKEN".to_owned(), "gh".to_owned()),
    ]);

    assert_eq!(
        brokered_env(&env, &credential_names, &nonces),
        BTreeMap::from([
            ("DEFAULT_KEY".to_owned(), "n-default".to_owned()),
            ("OTHER_KEY".to_owned(), "n-other".to_owned()),
            ("GH_TOKEN".to_owned(), "gh".to_owned()),
        ])
    );
}

/// A session broker that never listened, for what a launch derives from one.
fn session_broker(port: u16, secret: &str) -> SessionBroker {
    SessionBroker {
        broker: Broker::new(
            Vec::new(),
            vec![443],
            silent(),
            Vec::new(),
            false,
            String::new(),
            None,
        ),
        port,
        proxy_secret: secret.to_owned(),
        nonces: BTreeMap::new(),
        credential_names: BTreeMap::new(),
    }
}

fn template() -> BrokerTemplate {
    BrokerTemplate {
        allow: vec!["api.example.com".to_owned()],
        ports: vec![443],
        allow_internal: false,
        log: silent(),
        providers: vec![ProviderUpstream {
            name: "zai-coding-cn".to_owned(),
            prefix: "/provider/zai-coding-cn".to_owned(),
            upstream: "https://api.example.com/v4".to_owned(),
            credential: "real-key".to_owned(),
            policy: RoutePolicy::default(),
        }],
        credential_names: BTreeMap::from([("zai-coding-cn".to_owned(), "ZAI_KEY".to_owned())]),
        receipts: None,
        now: Arc::new(|| 0),
    }
}

/// Whether nothing accepts on a loopback port any more. The accept loop
/// learns of a close on the runtime, so the port is given a moment to go.
async fn stops_listening(port: u16) -> bool {
    for _ in 0..100 {
        if tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .is_err()
        {
            return true;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    false
}

/// The proxy secret and broker port a launch wrote into its policy.
fn proxy_in_policy(state_dir: &std::path::Path) -> (String, u16) {
    let policy = std::fs::read_to_string(state_dir.join("policy.toml")).expect("a policy");
    let url = policy
        .split_once("HTTPS_PROXY = \"")
        .and_then(|(_, rest)| rest.split_once('"'))
        .map(|(url, _)| url)
        .expect("the proxy variable");
    let (secret, port) = url
        .strip_prefix("http://session:")
        .and_then(|rest| rest.split_once("@169.254.169.1:"))
        .expect("a proxy URL with a credential");
    (secret.to_owned(), port.parse().expect("a port"))
}

/// Each launch starts a broker of its own, and one that fails after starting
/// it leaves nothing listening: a session that never ran must not leave a
/// port behind that still honours what it was handed.
#[tokio::test]
async fn each_launch_has_its_own_broker_and_a_failed_one_leaves_none() {
    let root = TempDir::new().expect("a temporary directory");
    let mut answers = BTreeMap::new();
    answers.insert(
        "trust".to_owned(),
        (Some(1), None, Some("refused".to_owned())),
    );
    let (run, _calls) = fake_run(answers);
    let sandbox = Arc::new(bailey_with(
        &config(),
        &root.path().to_string_lossy(),
        run,
        BaileyOptions {
            broker: Some(template()),
            lookup: Some(Arc::new(fake_agent)),
            kage: Some(fake_kage(0, INSTALLED_JSON).0),
            ..BaileyOptions::default()
        },
    ));
    sandbox.probe().await.expect("a report");

    let mut seen = Vec::new();
    for session in ["s-1", "s-2"] {
        let state_dir = root.path().join(session);
        let launch = SandboxLaunch {
            session_id: session.to_owned(),
            state_dir: state_dir.to_string_lossy().into_owned(),
            project_path: root.path().to_string_lossy().into_owned(),
            ..launch()
        };
        let Err(error) = sandbox.launch(&launch).await else {
            panic!("the launch was meant to fail at trust");
        };
        assert!(error.0.contains("could not trust"), "{}", error.0);
        let (secret, port) = proxy_in_policy(&state_dir);
        assert!(
            stops_listening(port).await,
            "{session} left port {port} open"
        );
        seen.push((secret, port));
    }
    assert_ne!(seen[0].0, seen[1].0, "two sessions shared a proxy secret");
}

/// Stopping a session closes its broker, so nothing it was handed is
/// honoured once it is asked to stop.
#[tokio::test]
async fn stopping_a_session_closes_its_broker() {
    let started = template().start("s-1").await.expect("a broker");
    let port = started.port;
    let env = BTreeMap::from([("PATH".to_owned(), std::env::var("PATH").unwrap_or_default())]);
    let spawned = crate::sandbox::spawn::spawn_agent("sleep", &["30".to_owned()], Some(&env), None)
        .expect("a stand-in for the launcher");
    let (run, _calls) = fake_run(BTreeMap::new());
    let handle = crate::sandbox::SandboxHandle::Bailey(Box::new(crate::sandbox::BaileyStop {
        session_id: "s-1".to_owned(),
        name: "errand-s-1".to_owned(),
        project_path: "/projects/demo".to_owned(),
        spawned: Arc::new(spawned),
        policy: "/state/s-1/policy.toml".to_owned(),
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

    assert!(
        stops_listening(port).await,
        "the broker outlived its session"
    );
}
