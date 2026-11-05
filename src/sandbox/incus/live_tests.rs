//! Live checks that a session is created, configured, entered and stopped on a
//! real Incus host.
//!
//! Ignored, since it needs an Incus host, the `errand` project with the
//! documented exceptions, and a guest image carrying the contract property.
//! Run them by hand with:
//!
//! `cargo test sandbox::incus::live_tests -- --ignored --test-threads=1`
//!
//! `ERRAND_INCUS_TEST_IMAGE` names the image alias, `ERRAND_INCUS_TEST_PROJECT`
//! the project, and `ERRAND_INCUS_TEST_PROJECT_ROOT` and
//! `ERRAND_INCUS_TEST_STATE_ROOT` the two host roots. They default to
//! `errand-agent`, `errand`, `/srv/errand/projects` and `/var/lib/errand`.

use std::collections::BTreeMap;
use std::os::unix::fs::MetadataExt;
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use super::{HostFacts, IncusSandbox, instance_name, run_incus_arc};
use crate::config::schema::defaults;
use crate::config::schema::{
    EgressConfig, EgressMode, IncusConfig, NetworkMode, SandboxBackend, SandboxConfig,
};
use crate::log::{LogFields, Logger};
use crate::sandbox::backend::{Denials, SandboxLaunch};

fn config(project: String, image: String) -> SandboxConfig {
    SandboxConfig {
        backend: SandboxBackend::Incus,
        require_full_enforcement: true,
        network: NetworkMode::None,
        egress_ports: vec![443],
        egress: EgressConfig {
            mode: EgressMode::Open,
            allow: Vec::new(),
            allow_internal: false,
        },
        hide_host_address: false,
        image: defaults::IMAGE.to_owned(),
        incus: IncusConfig {
            project,
            image,
            golden: std::env::var("ERRAND_INCUS_TEST_GOLDEN")
                .ok()
                .filter(|golden| !golden.is_empty()),
            ..IncusConfig::default()
        },
        memory: "1g".to_owned(),
        cpus: 1.0,
        pids: 128,
        file_max: "64m".to_owned(),
        tmp_size: "64m".to_owned(),
        shm_size: "32m".to_owned(),
        disk_tmp: false,
        disk: defaults::DISK.to_owned(),
        disk_check_ms: defaults::DISK_CHECK_MS,
        grace_period_ms: 30_000,
        policy_extra: None,
        path_extra: None,
        env: None,
    }
}

fn launching(session: &str, project: &str, state: &str) -> SandboxLaunch {
    SandboxLaunch {
        session_id: session.to_owned(),
        project_path: format!("{project}/{session}"),
        state_dir: format!("{state}/{session}"),
        env: BTreeMap::from([("ZAI_API_KEY".to_owned(), "test-secret".to_owned())]),
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

fn incus(project: &str, args: &[&str]) -> std::process::Output {
    Command::new("incus")
        .args(["--force-local", "--project", project])
        .args(args)
        .output()
        .expect("incus runs")
}

#[tokio::test]
#[ignore = "needs a live Incus host and a guest image carrying the contract"]
async fn a_session_is_created_configured_entered_and_stopped() {
    let project =
        std::env::var("ERRAND_INCUS_TEST_PROJECT").unwrap_or_else(|_| "errand".to_owned());
    let image =
        std::env::var("ERRAND_INCUS_TEST_IMAGE").unwrap_or_else(|_| "errand-agent".to_owned());
    let project_root = std::env::var("ERRAND_INCUS_TEST_PROJECT_ROOT")
        .unwrap_or_else(|_| "/srv/errand/projects".to_owned());
    let state_root = std::env::var("ERRAND_INCUS_TEST_STATE_ROOT")
        .unwrap_or_else(|_| "/var/lib/errand".to_owned());

    let session = format!("live-{}", std::process::id());
    let launch = launching(&session, &project_root, &state_root);
    std::fs::create_dir_all(&launch.project_path).expect("the project directory");
    std::fs::create_dir_all(std::path::Path::new(&launch.state_dir).join("home"))
        .expect("the state home");
    let name = instance_name(&session);

    let log = Logger::new(
        LogFields::new(),
        Arc::new(|level, line| eprintln!("[{level:?}] {line}")),
    );
    let sandbox = IncusSandbox::new(
        config(project.clone(), image),
        log,
        project_root.clone(),
        state_root.clone(),
        HostFacts::read(),
        run_incus_arc(),
        BTreeMap::new(),
        None,
    );

    let report = sandbox.probe().await.expect("the host and image are ready");
    assert!(report.gaps.is_empty(), "{:?}", report.gaps);

    let handle = sandbox.launch(&launch).await.expect("the session launches");
    tokio::time::sleep(Duration::from_secs(2)).await;

    let inspected = incus(
        &project,
        &[
            "exec",
            &name,
            "-T",
            "--",
            "sh",
            "-c",
            "cat /sys/fs/cgroup/memory.max; cat /sys/fs/cgroup/pids.max; \
             df -h /tmp /dev/shm | tail -2; test ! -e /run/errand/env && echo env-gone; \
             ip -brief addr | grep -c eth0 || true",
        ],
    );
    let seen = String::from_utf8_lossy(&inspected.stdout);
    assert!(
        inspected.status.success(),
        "{}",
        String::from_utf8_lossy(&inspected.stderr)
    );
    assert!(seen.contains("1073741824"), "memory limit: {seen}");
    assert!(seen.contains("128"), "pids limit: {seen}");
    assert!(seen.contains("64M"), "tmp size: {seen}");
    assert!(seen.contains("32M"), "shm size: {seen}");
    assert!(
        seen.contains("env-gone"),
        "the environment file stayed: {seen}"
    );
    assert!(seen.trim_end().ends_with('0'), "an eth0 appeared: {seen}");

    let written = incus(
        &project,
        &[
            "exec",
            &name,
            "--cwd",
            "/workspace",
            "--user",
            "1000",
            "--group",
            "1000",
            "-T",
            "--",
            "touch",
            "written-by-the-agent",
        ],
    );
    assert!(
        written.status.success(),
        "{}",
        String::from_utf8_lossy(&written.stderr)
    );
    let owner =
        std::fs::metadata(std::path::Path::new(&launch.project_path).join("written-by-the-agent"))
            .expect("the file the agent wrote")
            .uid();
    assert_eq!(
        owner,
        std::fs::metadata(&launch.project_path)
            .expect("the project")
            .uid()
    );

    assert!(!handle.stop().await, "the instance stopped without force");

    let listed = incus(&project, &["list", "--format", "csv"]);
    let names = String::from_utf8_lossy(&listed.stdout);
    assert!(!names.contains(&name), "{name} was left behind: {names}");

    let _ = std::fs::remove_dir_all(&launch.project_path);
    let _ = std::fs::remove_dir_all(&launch.state_dir);
}
