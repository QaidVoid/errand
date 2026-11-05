//! Tests for the Incus backend, with the client and the host facts injected.

use std::collections::BTreeMap;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use tempfile::TempDir;

use super::{
    IncusSandbox, Owner, broker_dir, cpu_allowance, env_file, exec_args, idmap, incus_size,
    instance_name, stop_timeout,
};
use crate::config::schema::defaults;
use crate::config::schema::{
    EgressConfig, EgressMode, IncusConfig, NetworkMode, SandboxBackend, SandboxConfig,
};
use crate::log::{LogFields, Logger};
use crate::sandbox::backend::{Denials, SandboxLaunch};
use crate::sandbox::kage_providers::INSTALLED_JSON;
use crate::sandbox::{IncusStop, Run, RunResult};

/// What a scripted command answered.
type Answer = (i32, String);

/// The argument lists a fake runner was handed, in order.
type Calls = Arc<Mutex<Vec<Vec<String>>>>;

fn silent() -> Logger {
    Logger::new(LogFields::new(), Arc::new(|_level, _line| {}))
}

fn config() -> SandboxConfig {
    SandboxConfig {
        backend: SandboxBackend::Incus,
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
        incus: IncusConfig::default(),
        memory: defaults::MEMORY.to_owned(),
        cpus: defaults::CPUS,
        pids: defaults::PIDS,
        file_max: defaults::FILE_MAX.to_owned(),
        tmp_size: defaults::TMP_SIZE.to_owned(),
        shm_size: defaults::SHM_SIZE.to_owned(),
        disk_tmp: defaults::DISK_TMP,
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
        max_provider_retries: defaults::MAX_PROVIDER_RETRIES,
        resume: false,
        grants: None,
    }
}

fn fake_run(answer: impl Fn(&str) -> Answer + Send + Sync + 'static) -> (Run, Calls) {
    let calls: Calls = Arc::new(Mutex::new(Vec::new()));
    let log = Arc::clone(&calls);
    let answer = Arc::new(answer);
    let run = move |args: Vec<String>, _cwd: Option<String>, _stdin: Option<String>| {
        let answer = Arc::clone(&answer);
        let calls = Arc::clone(&log);
        Box::pin(async move {
            calls.lock().unwrap().push(args.clone());
            let joined = args.join(" ");
            let (code, stdout) = answer(&joined);
            Ok(RunResult {
                code,
                stdout,
                stderr: String::new(),
            })
        }) as crate::sandbox::RunFuture<RunResult>
    };
    (Arc::new(run), calls)
}

fn contract(guest_uid: u32, docker_gid: u32) -> String {
    serde_json::json!({
        "version": 1,
        "guestUid": guest_uid,
        "dockerGid": docker_gid,
        "kageProviders": serde_json::from_str::<serde_json::Value>(INSTALLED_JSON)
            .expect("the captured provider list reads"),
    })
    .to_string()
}

fn project_json(restricted: bool, roots: &str) -> String {
    serde_json::json!({
        "config": {
            "restricted": if restricted { "true" } else { "false" },
            "restricted.containers.nesting": "allow",
            "restricted.containers.lowlevel": "allow",
            "restricted.containers.privilege": "unprivileged",
            "restricted.devices.disk": "allow",
            "restricted.devices.disk.paths": roots,
            "restricted.devices.proxy": "block",
            "restricted.idmap.uid": "*",
            "restricted.idmap.gid": "*",
        }
    })
    .to_string()
}

fn image_json(contract: &str) -> String {
    serde_json::json!([{
        "aliases": [{ "name": "errand-agent" }],
        "type": "container",
        "properties": { "user.errand.contract": contract },
    }])
    .to_string()
}

fn host_facts(owner: Owner) -> super::HostFacts {
    super::HostFacts {
        subuid: format!("root:{}:1\n", owner.uid),
        subgid: format!("root:{}:1\n", owner.gid),
        cgroup_controllers: Some("cpuset cpu io memory pids".to_owned()),
    }
}

fn probed(config: SandboxConfig, root: &TempDir, host: super::HostFacts, run: Run) -> IncusSandbox {
    IncusSandbox::new(
        config,
        silent(),
        root.path().join("projects").display().to_string(),
        root.path().join("state").display().to_string(),
        host,
        run,
        BTreeMap::new(),
        None,
    )
}

fn populated() -> TempDir {
    let root = tempfile::tempdir().expect("a temporary directory");
    std::fs::create_dir_all(root.path().join("projects")).expect("a project root");
    std::fs::create_dir_all(root.path().join("state")).expect("a state root");
    root
}

fn project_roots(root: &TempDir) -> String {
    format!(
        "{},{}",
        root.path().join("projects").display(),
        root.path().join("state").display()
    )
}

fn probe_answers(
    project: String,
    image: String,
) -> impl Fn(&str) -> Answer + Send + Sync + 'static {
    move |joined| {
        if joined.contains("query /1.0/projects/") {
            (0, project.clone())
        } else if joined.contains("image list") {
            (0, image.clone())
        } else if joined.contains("query /1.0/profiles/default") {
            (
                0,
                serde_json::json!({"devices": {"root": {"pool": "default"}}}).to_string(),
            )
        } else if joined.contains("query /1.0/storage-pools/") {
            (0, serde_json::json!({"driver": "btrfs"}).to_string())
        } else if joined.contains("query /1.0") {
            (0, "{}".to_owned())
        } else {
            (0, String::new())
        }
    }
}

#[tokio::test]
async fn the_probe_accepts_a_configured_host() {
    let root = populated();
    let owner = super::owner_of(&root.path().join("projects").display().to_string());
    let (run, _) = fake_run(probe_answers(
        project_json(true, &project_roots(&root)),
        image_json(&contract(1000, 1000)),
    ));
    let mut config = config();
    config.egress.mode = EgressMode::Open;
    let sandbox = probed(config, &root, host_facts(owner), run);

    let report = sandbox.probe().await.expect("the host is configured");
    assert!(report.gaps.is_empty(), "{:?}", report.gaps);
    assert_eq!(report.backend, SandboxBackend::Incus);
    assert!(report.kage_providers.get("anthropic").is_some());
}

#[tokio::test]
async fn a_project_that_is_not_restricted_is_reported() {
    let root = populated();
    let owner = super::owner_of(&root.path().join("projects").display().to_string());
    let (run, _) = fake_run(probe_answers(
        project_json(false, &project_roots(&root)),
        image_json(&contract(1000, 1000)),
    ));
    let mut config = config();
    config.egress.mode = EgressMode::Open;
    let sandbox = probed(config, &root, host_facts(owner), run);

    let report = sandbox.probe().await.expect("the host is reachable");
    assert!(
        report.gaps.iter().any(|gap| gap.contains("not restricted")),
        "{:?}",
        report.gaps
    );
}

#[tokio::test]
async fn an_image_contract_with_another_uid_is_reported() {
    let root = populated();
    let owner = super::owner_of(&root.path().join("projects").display().to_string());
    let (run, _) = fake_run(probe_answers(
        project_json(true, &project_roots(&root)),
        image_json(&contract(999, 1000)),
    ));
    let mut config = config();
    config.egress.mode = EgressMode::Open;
    let sandbox = probed(config, &root, host_facts(owner), run);

    let report = sandbox.probe().await.expect("the host is reachable");
    assert!(
        report
            .gaps
            .iter()
            .any(|gap| gap.contains("runs the agent as uid 999")),
        "{:?}",
        report.gaps
    );
}

#[tokio::test]
async fn a_missing_subuid_allocation_is_reported() {
    let root = populated();
    let (run, _) = fake_run(probe_answers(
        project_json(true, &project_roots(&root)),
        image_json(&contract(1000, 1000)),
    ));
    let mut config = config();
    config.egress.mode = EgressMode::Open;
    let host = super::HostFacts {
        subuid: String::new(),
        subgid: String::new(),
        cgroup_controllers: Some("memory cpu pids".to_owned()),
    };
    let sandbox = probed(config, &root, host, run);

    let report = sandbox.probe().await.expect("the host is reachable");
    assert!(
        report
            .gaps
            .iter()
            .any(|gap| gap.contains("/etc/subuid has no allocation")),
        "{:?}",
        report.gaps
    );
}

#[tokio::test]
async fn orphans_are_the_instances_carrying_the_system_label() {
    let root = populated();
    let (run, _) = fake_run(|joined| {
        if joined.contains(" list ") {
            (
                0,
                serde_json::json!([
                    {"name": "errand-a", "config": {"user.errand.system": "true"}},
                    {"name": "errand-golden", "config": {}},
                ])
                .to_string(),
            )
        } else {
            (0, String::new())
        }
    });
    let sandbox = probed(config(), &root, super::HostFacts::default(), run);
    assert_eq!(sandbox.list_orphans().await.expect("listed"), ["errand-a"]);
}

#[tokio::test]
async fn a_listing_failure_is_an_error_not_an_empty_list() {
    let root = populated();
    let (run, _) = fake_run(|joined| {
        if joined.contains(" list ") {
            (1, String::new())
        } else {
            (0, String::new())
        }
    });
    let sandbox = probed(config(), &root, super::HostFacts::default(), run);
    assert!(sandbox.list_orphans().await.is_err());
}

#[tokio::test]
async fn removing_orphans_counts_what_it_removed() {
    let root = populated();
    let (run, calls) = fake_run(|_| (0, String::new()));
    let sandbox = probed(config(), &root, super::HostFacts::default(), run);
    assert_eq!(
        sandbox
            .remove_orphans(&["errand-a".to_owned(), "errand-b".to_owned()])
            .await,
        2
    );
    let calls = calls.lock().unwrap();
    assert!(
        calls
            .iter()
            .any(|call| call.join(" ").contains("delete --force"))
    );
}

fn stop_handle(run: Run, grace_ms: u64) -> crate::sandbox::SandboxHandle {
    let env = BTreeMap::from([("PATH".to_owned(), "/bin".to_owned())]);
    let spawned = crate::sandbox::spawn::spawn_agent("sleep", &["30".to_owned()], Some(&env), None)
        .expect("a stand-in for the launcher");
    crate::sandbox::SandboxHandle::Incus(Box::new(IncusStop {
        session_id: "s-1".to_owned(),
        name: "errand-s-1".to_owned(),
        project: "errand".to_owned(),
        project_path: "/projects/demo".to_owned(),
        broker_dir: None,
        spawned: Arc::new(spawned),
        run,
        grace_ms,
        log: silent(),
        broker: Mutex::new(None),
        stopped: AtomicBool::new(false),
    }))
}

#[tokio::test]
async fn a_graceful_stop_deletes_the_instance() {
    let (run, calls) = fake_run(|_| (0, String::new()));
    let handle = stop_handle(run, 10_000);
    assert!(!handle.stop().await, "no kill was needed");

    let calls = calls.lock().unwrap();
    let joined: Vec<String> = calls.iter().map(|call| call.join(" ")).collect();
    assert!(
        joined
            .iter()
            .any(|call| call.contains("stop errand-s-1 --timeout 10")),
        "{joined:?}"
    );
    assert!(
        joined
            .iter()
            .any(|call| call.contains("delete errand-s-1") && !call.contains("delete --force")),
        "{joined:?}"
    );
}

#[tokio::test]
async fn a_failed_stop_escalates_to_a_forced_delete() {
    let (run, calls) = fake_run(|joined| {
        if joined.contains(" stop ") {
            (1, String::new())
        } else {
            (0, String::new())
        }
    });
    let handle = stop_handle(run, 1_500);
    assert!(handle.stop().await, "the instance had to be forced");

    let calls = calls.lock().unwrap();
    let joined: Vec<String> = calls.iter().map(|call| call.join(" ")).collect();
    assert!(
        joined
            .iter()
            .any(|call| call.contains("stop errand-s-1 --timeout 2")),
        "{joined:?}"
    );
    assert!(
        joined
            .iter()
            .any(|call| call.contains("delete --force errand-s-1")),
        "{joined:?}"
    );
}

#[tokio::test]
async fn a_zero_grace_still_waits_one_second() {
    let (run, calls) = fake_run(|_| (0, String::new()));
    let handle = stop_handle(run, 0);
    let _ = handle.stop().await;
    let calls = calls.lock().unwrap();
    assert!(
        calls
            .iter()
            .any(|call| call.join(" ").contains("--timeout 1")),
        "{calls:?}"
    );
}

#[test]
fn names_fit_the_incus_rules() {
    assert_eq!(instance_name("s-1"), "errand-s-1");
    assert_eq!(instance_name("s_1"), "errand-s1");
    assert_eq!(
        instance_name(&"a".repeat(80)),
        format!("errand-{}", "a".repeat(56))
    );
    assert!(!instance_name(&format!("{}-", "b".repeat(60))).ends_with('-'));
    assert_eq!(instance_name(""), "errand");
}

#[test]
fn sizes_use_the_units_incus_reads() {
    assert_eq!(incus_size(4 * 1024 * 1024 * 1024), "4GiB");
    assert_eq!(incus_size(512 * 1024 * 1024), "512MiB");
    assert_eq!(incus_size(1536 * 1024), "1536KiB");
    assert_eq!(incus_size(1), "1KiB");
}

#[test]
fn cpus_become_a_time_quota() {
    assert_eq!(cpu_allowance(2.0), "200ms/100ms");
    assert_eq!(cpu_allowance(1.5), "150ms/100ms");
    assert_eq!(cpu_allowance(0.5), "50ms/100ms");
}

#[test]
fn stop_time_is_whole_seconds_rounded_up() {
    assert_eq!(stop_timeout(10_000), 10);
    assert_eq!(stop_timeout(1_500), 2);
    assert_eq!(stop_timeout(0), 1);
}

#[test]
fn the_idmap_maps_the_workspace_owner_to_the_agent() {
    assert_eq!(idmap(999, 998, 1000), "uid 999 1000\ngid 998 1000");
}

#[test]
fn the_command_plan_puts_every_guarantee_before_start() {
    let plan = IncusSandbox::command_plan(&config(), &launch(), Owner { uid: 999, gid: 998 });

    assert_eq!(
        plan.create,
        [
            "--force-local",
            "--project",
            "errand",
            "init",
            "errand-agent",
            "errand-s-1",
        ]
    );
    assert_eq!(
        plan.start,
        [
            "--force-local",
            "--project",
            "errand",
            "start",
            "errand-s-1",
        ]
    );

    let keys: BTreeMap<&str, &str> = plan
        .config
        .iter()
        .map(|(key, value)| (key.as_str(), value.as_str()))
        .collect();
    assert_eq!(keys["security.privileged"], "false");
    assert_eq!(keys["security.nesting"], "true");
    assert_eq!(keys["security.guestapi"], "false");
    assert_eq!(keys["boot.autostart"], "false");
    assert_eq!(keys["boot.autorestart"], "false");
    assert_eq!(keys["limits.memory"], "4GiB");
    assert_eq!(keys["limits.memory.swap"], "false");
    assert_eq!(keys["limits.processes"], "512");
    assert_eq!(keys["limits.cpu.allowance"], "200ms/100ms");
    assert_eq!(keys["limits.kernel.fsize"], "1073741824");
    assert_eq!(keys["raw.idmap"], "uid 999 1000\ngid 998 1000");
    assert!(!keys.contains_key("limits.cpu"));

    assert_eq!(plan.root_size, "20GiB");
    let devices = plan
        .devices
        .iter()
        .map(|device| device.join(" "))
        .collect::<Vec<_>>()
        .join(" ");
    assert!(devices.contains("add errand-s-1 eth0 none"), "{devices}");
    assert!(
        devices.contains("source=/projects/demo path=/workspace"),
        "{devices}"
    );
    assert!(
        devices.contains("source=/state/s-1 path=/state"),
        "{devices}"
    );
    assert!(
        devices.contains("source=/state/s-1.broker path=/mnt/errand-broker"),
        "{devices}"
    );
}

#[test]
fn open_mode_keeps_the_profile_nic_and_drops_the_broker() {
    let mut config = config();
    config.egress.mode = EgressMode::Open;
    let plan = IncusSandbox::command_plan(
        &config,
        &launch(),
        Owner {
            uid: 1000,
            gid: 1000,
        },
    );
    let devices = plan
        .devices
        .iter()
        .map(|device| device.join(" "))
        .collect::<Vec<_>>()
        .join(" ");
    assert!(!devices.contains("eth0 none"), "{devices}");
    assert!(!devices.contains("errand-broker"), "{devices}");
}

#[test]
fn no_network_keeps_the_nic_disabled_and_drops_the_broker() {
    let mut config = config();
    config.network = NetworkMode::None;
    let plan = IncusSandbox::command_plan(
        &config,
        &launch(),
        Owner {
            uid: 1000,
            gid: 1000,
        },
    );
    let devices = plan
        .devices
        .iter()
        .map(|device| device.join(" "))
        .collect::<Vec<_>>()
        .join(" ");
    assert!(devices.contains("eth0 none"), "{devices}");
    assert!(!devices.contains("errand-broker"), "{devices}");
}

#[test]
fn a_golden_is_copied_instead_of_created_from_the_image() {
    let mut config = config();
    config.incus.golden = Some("errand-golden".to_owned());
    let plan = IncusSandbox::command_plan(
        &config,
        &launch(),
        Owner {
            uid: 1000,
            gid: 1000,
        },
    );
    assert_eq!(
        plan.create,
        [
            "--force-local",
            "--project",
            "errand",
            "copy",
            "errand-golden",
            "errand-s-1",
        ]
    );
}

#[test]
fn disk_tmp_mounts_a_disk_at_tmp() {
    let mut config = config();
    config.disk_tmp = true;
    let plan = IncusSandbox::command_plan(
        &config,
        &launch(),
        Owner {
            uid: 1000,
            gid: 1000,
        },
    );
    let devices = plan
        .devices
        .iter()
        .map(|device| device.join(" "))
        .collect::<Vec<_>>()
        .join(" ");
    assert!(
        devices.contains("source=/state/s-1/tmp path=/tmp"),
        "{devices}"
    );
}

#[test]
fn the_exec_argv_names_the_wrapper_and_carries_no_environment() {
    let agent = vec!["kage".to_owned(), "rpc".to_owned(), "--yolo".to_owned()];
    let args = exec_args("errand", "errand-s-1", 1000, &agent);
    assert_eq!(
        args,
        [
            "--force-local",
            "--project",
            "errand",
            "exec",
            "errand-s-1",
            "--cwd",
            "/workspace",
            "--user",
            "1000",
            "--group",
            "1000",
            "-T",
            "--",
            "/usr/local/bin/errand-exec",
            "kage",
            "rpc",
            "--yolo",
        ]
    );
    assert!(!args.iter().any(|arg| arg.contains("provider-secret")));
}

#[test]
fn the_environment_file_is_one_line_per_variable() {
    let env = BTreeMap::from([
        ("HOME".to_owned(), "/state/home".to_owned()),
        ("A".to_owned(), "a=b c".to_owned()),
    ]);
    let contents = env_file(&env).expect("written");
    assert_eq!(contents, "A=a=b c\nHOME=/state/home\n");

    let bad = BTreeMap::from([("A".to_owned(), "two\nlines".to_owned())]);
    assert!(env_file(&bad).is_err());
    let bad = BTreeMap::from([("not a name".to_owned(), "x".to_owned())]);
    assert!(env_file(&bad).is_err());
}

#[test]
fn tmpfs_mounts_are_fresh_and_sized() {
    let command = super::tmpfs_mounts(&config());
    assert!(command.starts_with("mkdir -p /tmp /dev/shm"), "{command}");
    assert!(
        command.contains("size=536870912,mode=1777,nosuid,nodev tmpfs /tmp"),
        "{command}"
    );
    assert!(
        command.contains("size=268435456,mode=1777,nosuid,nodev tmpfs /dev/shm"),
        "{command}"
    );

    let mut with_disk = config();
    with_disk.disk_tmp = true;
    let command = super::tmpfs_mounts(&with_disk);
    assert!(
        command.starts_with("find /tmp -mindepth 1 -ignore_readdir_race -delete"),
        "{command}"
    );
    assert!(!command.contains("tmpfs /tmp"), "{command}");
    assert!(command.contains("tmpfs /dev/shm"), "{command}");
}

#[test]
fn the_broker_directory_is_a_sibling_of_the_state() {
    assert_eq!(broker_dir("/state/s-1"), "/state/s-1.broker");
}
