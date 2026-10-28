//! Live checks that a brokered bailey session can reach its broker and no
//! host service directly, run against the real tool.
//!
//! Every test here is ignored, since it needs bailey installed and a kernel
//! with Landlock and user namespaces. Run them by hand with
//! `cargo test sandbox::bailey::live_tests -- --ignored --test-threads=1`.
//!
//! The session is started as a launch starts one: with the arguments
//! [`bailey_args`] builds and a policy [`policy_contents`] writes, under the
//! default proxy mode. Only the command after `--` differs. It is this test
//! binary run again, told where to connect by an extra argument, rather than
//! a variable, since bailey builds the session's environment itself. The
//! test that is that command is `live_session_connect`, which does nothing
//! when run on its own.

use std::io::Read as _;
use std::net::TcpListener;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use super::{bailey_args, egress_proxy_endpoint, parse_doctor};
use crate::config::schema::{
    EgressConfig, EgressMode, NetworkMode, PolicyExtraConfig, SandboxBackend, SandboxConfig,
    defaults,
};
use crate::sandbox::backend::SandboxLaunch;
use crate::sandbox::policy::{PolicyOptions, policy_contents};
use crate::sandbox::runtime::AgentRuntime;

/// The argument that tells this binary, run inside the sandbox, where to
/// connect. The test harness reads it as one more name filter, which matches
/// no test.
const CONNECT_TO: &str = "connect-to=";

/// How long the session waits for a connect before calling it dropped.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);

/// What a session reports for a connect its namespace drops: bailey's
/// firewall in the session's namespace lets out only the broker, and drops
/// the rest without an answer.
const DROPPED: &str = "timed-out";

/// The errno a session gets connecting to a port its policy does not admit,
/// `EACCES`: Landlock refuses it before a packet leaves. Measured on bailey
/// 0.2.3 under Linux 7.2 with Landlock ABI 10.
const NOT_ADMITTED_ERRNO: i32 = 13;

/// The session's side: connects where it is told and prints what came of
/// it. Does nothing when run without the argument, so on its own it passes
/// having checked nothing.
#[test]
#[ignore = "the session half of the live checks; run by them, not alone"]
fn live_session_connect() {
    let Some(target) =
        std::env::args().find_map(|arg| arg.strip_prefix(CONNECT_TO).map(str::to_owned))
    else {
        return;
    };
    let address = target.parse().expect("a socket address");
    let outcome = match std::net::TcpStream::connect_timeout(&address, CONNECT_TIMEOUT) {
        Ok(_) => "connected".to_owned(),
        Err(error) if error.kind() == std::io::ErrorKind::TimedOut => "timed-out".to_owned(),
        Err(error) => format!("errno={}", error.raw_os_error().unwrap_or(-1)),
    };
    println!("LIVE {outcome}");
}

/// Fails loudly, saying why, when this host cannot run bailey, so a missing
/// capability is never read as a passing check.
fn bailey_runs_here() {
    let doctor = std::process::Command::new("bailey")
        .arg("doctor")
        .output()
        .unwrap_or_else(|error| panic!("bailey could not be run here: {error}"));
    let text = format!(
        "{}\n{}",
        String::from_utf8_lossy(&doctor.stdout),
        String::from_utf8_lossy(&doctor.stderr)
    );
    let (_gaps, unavailable) = parse_doctor(&text);
    let namespaces = text
        .lines()
        .any(|line| line.trim() == "user namespaces: yes");
    assert!(
        doctor.status.success() && unavailable.is_empty() && namespaces,
        "this host cannot run bailey: {unavailable:?}\n{text}"
    );
}

/// A listener on `address`, counting the connections it takes.
fn listener(address: &str) -> (u16, Arc<AtomicUsize>) {
    let listener = TcpListener::bind(address).expect("a port to listen on");
    let port = listener.local_addr().unwrap().port();
    let taken = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&taken);
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            counted.fetch_add(1, Ordering::SeqCst);
            let mut stream = stream;
            let _ = stream.read(&mut [0; 1]);
        }
    });
    (port, taken)
}

/// This host's own address on its route out, which a session sharing the
/// host's namespace could reach a service on.
fn host_address() -> String {
    let probe = std::net::UdpSocket::bind("0.0.0.0:0").expect("a UDP socket");
    probe
        .connect("192.0.2.1:9")
        .expect("this host has a route out, whose address is the one to aim at");
    probe.local_addr().unwrap().ip().to_string()
}

/// How many connections `taken` counted, given a moment for the listener
/// thread to count one that has just been made.
fn counted(taken: &AtomicUsize, expected: usize) -> usize {
    for _ in 0..50 {
        if taken.load(Ordering::SeqCst) >= expected {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    taken.load(Ordering::SeqCst)
}

/// The configuration a daemon launches with by default, with `also` admitted
/// as an outbound port beside 443.
fn default_config(also: u16, directory: &str) -> SandboxConfig {
    SandboxConfig {
        backend: SandboxBackend::Bailey,
        require_full_enforcement: defaults::REQUIRE_FULL_ENFORCEMENT,
        network: NetworkMode::Restricted,
        egress_ports: vec![443, also],
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
        tmp_size: "64m".to_owned(),
        shm_size: "64m".to_owned(),
        disk_tmp: false,
        disk: defaults::DISK.to_owned(),
        disk_check_ms: defaults::DISK_CHECK_MS,
        grace_period_ms: defaults::GRACE_PERIOD_MS,
        policy_extra: Some(PolicyExtraConfig {
            read: vec![directory.to_owned()],
            write: Vec::new(),
            execute: vec![directory.to_owned()],
        }),
        path_extra: None,
        env: None,
    }
}

/// Runs this binary as a session launched with the default configuration,
/// its broker on `broker_port` and `also` admitted outbound, has it connect
/// to `target`, and returns what it reported.
fn session_connects(broker_port: u16, also: u16, target: &str) -> String {
    let exe = std::env::current_exe().expect("this test binary");
    let directory = exe.parent().expect("its directory").display().to_string();
    let root = tempfile::tempdir().expect("a session's directories");
    let project = root.path().join("project");
    let state = root.path().join("state");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::create_dir_all(state.join("home")).unwrap();
    let resolv = root.path().join("resolv.conf");
    std::fs::write(&resolv, "nameserver 127.0.0.1\n").unwrap();
    let config = default_config(also, &directory);
    let launch = SandboxLaunch {
        session_id: "live".to_owned(),
        project_path: project.display().to_string(),
        state_dir: state.display().to_string(),
        provider: "live".to_owned(),
        ..SandboxLaunch::default()
    };
    let policy = state.join("policy.toml");
    std::fs::write(
        &policy,
        policy_contents(&PolicyOptions {
            launch: &launch,
            network: config.network,
            egress_ports: Some(&config.egress_ports),
            runtime: &AgentRuntime::default(),
            file_max: &config.file_max,
            tmp_size: &config.tmp_size,
            shm_size: &config.shm_size,
            disk_tmp: config.disk_tmp,
            resolv_conf: &resolv.display().to_string(),
            extra: config.policy_extra.as_ref(),
            env: None,
            path_extra: None,
        }),
    )
    .unwrap();

    let mut args = bailey_args(
        &config,
        &launch,
        &policy.display().to_string(),
        Some(broker_port),
    );
    let command = args
        .iter()
        .position(|arg| arg == "--")
        .expect("the command follows --");
    args.truncate(command + 1);
    args.push(exe.display().to_string());
    args.extend([
        "sandbox::bailey::live_tests::live_session_connect".to_owned(),
        format!("{CONNECT_TO}{target}"),
        "--ignored".to_owned(),
        "--nocapture".to_owned(),
        "--test-threads=1".to_owned(),
    ]);
    let output = std::process::Command::new("bailey")
        .current_dir(&project)
        .args(&args)
        .output()
        .expect("bailey runs");
    let stdout = String::from_utf8_lossy(&output.stdout);
    stdout
        .lines()
        .find_map(|line| line.split_once("LIVE ").map(|(_, outcome)| outcome.trim()))
        .unwrap_or_else(|| {
            panic!(
                "the session reported nothing: {stdout}\n{}",
                String::from_utf8_lossy(&output.stderr)
            )
        })
        .to_owned()
}

#[test]
#[ignore = "needs bailey and user namespaces; run with --ignored"]
fn this_host_can_run_bailey() {
    bailey_runs_here();
}

/// The control: a session reaches its broker, so a refusal below is the
/// sandbox and not a session that cannot connect at all.
#[test]
#[ignore = "needs bailey and user namespaces; run with --ignored"]
fn a_brokered_session_reaches_its_broker() {
    bailey_runs_here();
    let (broker, taken) = listener("127.0.0.1:0");

    let reported = session_connects(broker, 9, &egress_proxy_endpoint(broker));

    assert_eq!(reported, "connected");
    assert_eq!(counted(&taken, 1), 1);
}

/// A port the policy admits is still not a way out: a host service on that
/// port is never reached, at the host's loopback, at its own address, or at
/// the address the session reaches its broker at, which maps to the host's
/// loopback. Landlock admits the port, so the drop is the namespace and its
/// firewall, which let out only the broker's port.
#[test]
#[ignore = "needs bailey and user namespaces; run with --ignored"]
fn an_admitted_port_reaches_no_host_service_directly() {
    bailey_runs_here();
    let (broker, _) = listener("127.0.0.1:0");
    let (service, taken) = listener("0.0.0.0:0");

    for target in [
        format!("127.0.0.1:{service}"),
        format!("{}:{service}", host_address()),
        egress_proxy_endpoint(service),
    ] {
        let reported = session_connects(broker, service, &target);
        assert_eq!(reported, DROPPED, "{target}");
    }
    assert_eq!(counted(&taken, 0), 0);
}

/// A port the policy does not admit is refused at once by Landlock, with one
/// errno.
#[test]
#[ignore = "needs bailey and user namespaces; run with --ignored"]
fn a_port_not_admitted_is_refused_at_once() {
    bailey_runs_here();
    let (broker, _) = listener("127.0.0.1:0");

    let reported = session_connects(broker, 9, "1.1.1.1:8443");

    assert_eq!(reported, format!("errno={NOT_ADMITTED_ERRNO}"));
}

/// The broker's own port, aimed at the host directly rather than at the
/// broker's address, reaches nothing.
#[test]
#[ignore = "needs bailey and user namespaces; run with --ignored"]
fn the_broker_port_reaches_nothing_but_the_broker() {
    bailey_runs_here();
    let (broker, taken) = listener("127.0.0.1:0");

    let reported = session_connects(broker, 9, &format!("127.0.0.1:{broker}"));

    assert_eq!(reported, DROPPED);
    assert_eq!(taken.load(Ordering::SeqCst), 0);
}
