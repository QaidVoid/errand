//! errand runs a coding agent from a chat channel, in a sandbox it cannot
//! escape.
//!
//! The command line is what an operator runs: the daemon is one subcommand
//! among several rather than the only thing this program does, because
//! managing what it left on disk is an operator's job and belongs where an
//! operator already is.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use crate::cli::threads::Deps;
use crate::cli::threads::Project;
use crate::cli::threads::run_threads;
use crate::cli::verbosity::take_verbosity;
use crate::config::load::{config_path, file_exists, load_config};
use crate::config::schema::Config;
use crate::config::schema::ConfigError;
use crate::log::now_ms;
use crate::log::{LogLevel, logger};
use crate::log::{LogValue, fields};
use crate::sandbox::policy::POLICY_FILENAME;
use crate::serve::serve;
use crate::session::disk::tree_bytes;
use crate::session::record::record_dir;
use crate::session::registry::ThreadRegistry;

mod admission;
mod agent;
mod chat;
mod cli;
mod config;
mod daemon;
mod issues;
mod lock;
mod log;
mod memory;
mod provider;
mod sandbox;
mod serve;
mod session;
mod web;

#[cfg(test)]
mod docs;
#[cfg(test)]
mod test_util;

/// The environment an operator's shell hands over.
type Vars = BTreeMap<String, String>;

fn usage(env: &Vars) -> String {
    [
        "usage: errand [-v | -vv | -vvv] <command>".to_owned(),
        String::new(),
        "  run                  run the daemon until it is told to stop".to_owned(),
        "  threads [command]    manage remembered threads and their data".to_owned(),
        "  help                 this".to_owned(),
        String::new(),
        "  -v                   also log each decision and why".to_owned(),
        "  -vv                  also log each step between decisions".to_owned(),
        "  -vvv                 also log every line exchanged with the agent".to_owned(),
        String::new(),
        format!(
            "the configuration is read from {}",
            config_path(env, |candidate| std::path::Path::new(candidate).exists())
        ),
    ]
    .join("\n")
}

/// Reads the configuration the way every command does, and the file it came
/// from.
///
/// The environment goes to the reader as well as to the search, so the list a
/// missing file reports names the places actually looked in. Handing it a
/// blank one, as the TypeScript entry point did, produced a report that
/// ignored `ERRAND_CONFIG` and showed paths relative to a `HOME` that was
/// never read: a description of a search that did not happen.
///
/// The path comes back with the configuration, because the daemon watches
/// that file for the rest of its life and must not have to search for it
/// again to find the same place.
fn load(env: &Vars) -> Result<(String, Config), ConfigError> {
    let path = config_path(env, file_exists);
    let config = load_config(
        &path,
        |read| std::fs::read_to_string(read),
        env,
        file_exists,
    )?;
    Ok((path, config))
}

/// Pulls a `name = "value"` pair off the front of a grant body, leaving the
/// rest of it. A body may start with the comma a previous pair left behind.
fn grant_pair<'a>(body: &'a str, name: &str) -> Option<(&'a str, String)> {
    let body = body.trim_start();
    let body = body.strip_prefix(',').unwrap_or(body).trim_start();
    let body = body.strip_prefix(name)?;
    let body = body.trim_start().strip_prefix('=')?.trim_start();
    let body = body.strip_prefix('"')?;
    let end = body.find('"')?;
    Some((&body[end + 1..], body[..end].to_owned()))
}

/// The host path of the grant placed at /workspace, which is how a policy
/// names the project it was written for.
fn workspace_grant(policy: &str) -> Option<String> {
    let mut rest = policy;
    while let Some(open) = rest.find('{') {
        let close = rest[open..].find('}')? + open;
        let body = &rest[open + 1..close];
        let (rest_of_grant, path) = grant_pair(body, "path")?;
        let (rest_of_grant, at) = grant_pair(rest_of_grant, "at")?;
        if at == "/workspace" && rest_of_grant.trim().is_empty() {
            return Some(path);
        }
        rest = &rest[close + 1..];
    }
    None
}

/// The project a session worked in, read back from the policy it was run
/// under.
///
/// The policy names the project as the grant placed at the workspace, which
/// is the one thing on disk that still says where the work was. Nothing is
/// guessed at: a policy that does not say returns nothing, and the caller
/// refuses.
///
/// The policy file sits in the state directory, which the session writes, so
/// what it names is untrusted input. A project outside the configured root is
/// refused, so a rewritten policy cannot revive a session onto a directory
/// the operator never chose, and a root that does not resolve is refused
/// rather than assumed.
fn project_of(state_dir: &str, root: &str) -> Option<Project> {
    let policy =
        std::fs::read_to_string(std::path::Path::new(state_dir).join(POLICY_FILENAME)).ok()?;
    let path = workspace_grant(&policy)?;
    let name = std::path::Path::new(&path)
        .file_name()?
        .to_string_lossy()
        .into_owned();
    let selection = crate::session::projects::ProjectSelection {
        name: name.clone(),
        path,
        prompt: String::new(),
        was_explicit: false,
    };
    crate::session::projects::ensure_project_directory(&selection, root)
        .ok()
        .map(|()| Project {
            name,
            path: selection.path,
        })
}

async fn threads(args: &[String], env: &Vars, level: LogLevel) -> Result<i32, ConfigError> {
    let (_path, config) = load(env)?;
    let log = logger(level);
    let registry = Arc::new(Mutex::new(ThreadRegistry::new(
        ThreadRegistry::path_for(&config.state_dir),
        log,
    )));
    registry.lock().expect("the registry lock").load();

    Ok(run_threads(
        args,
        &Deps {
            registry,
            size_of: Arc::new(|state_dir| Box::pin(async move { tree_bytes(&state_dir) })),
            remove: Arc::new(|state_dir| {
                Box::pin(async move {
                    // The record sits beside the state directory, so removing
                    // a thread has to take it too or the transcript outlives
                    // what it describes.
                    tokio::fs::remove_dir_all(&state_dir)
                        .await
                        .map_err(|error| error.to_string())?;
                    let _ = tokio::fs::remove_dir_all(record_dir(&state_dir)).await;
                    Ok(())
                })
            }),
            state_root: config.state_dir,
            project_of: {
                let root = config.project_root;
                Arc::new(move |state_dir: String| {
                    let root = root.clone();
                    Box::pin(async move { project_of(&state_dir, &root) })
                })
            },
            write: Arc::new(|line| println!("{line}")),
            now: Arc::new(now_ms),
        },
    )
    .await)
}

/// Runs the daemon, turning the failures an operator can act on into an exit
/// code and one line rather than a stack trace.
///
/// A configuration that cannot be read at all is fatal here and only here: a
/// daemon with nothing to run on cannot serve. The same file failing to
/// resolve later is not, and the watcher carries on.
async fn run(env: &Vars, level: LogLevel) -> i32 {
    let log = logger(level);
    let (path, config) = match load(env) {
        Ok(loaded) => loaded,
        Err(error) => {
            log.error(
                "the daemon failed to start",
                &fields([("detail", LogValue::from(format!("ConfigError: {error}")))]),
            );
            return 1;
        }
    };
    serve(config, path, env.clone(), log).await.code()
}

fn main() -> std::process::ExitCode {
    let env: Vars = std::env::vars_os()
        .map(|(key, value)| {
            (
                key.to_string_lossy().into_owned(),
                value.to_string_lossy().into_owned(),
            )
        })
        .collect();
    let args: Vec<String> = std::env::args().skip(1).collect();

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("a tokio runtime is always buildable");

    match runtime.block_on(dispatch(args, env)) {
        Ok(code) => std::process::ExitCode::from(u8::try_from(code).unwrap_or(1)),
        Err(error) => {
            // A configuration problem is the operator's to fix, so it is
            // printed as itself rather than as a stack trace.
            eprintln!("ConfigError: {error}");
            std::process::ExitCode::FAILURE
        }
    }
}

async fn dispatch(mut args: Vec<String>, env: Vars) -> Result<i32, ConfigError> {
    let level = take_verbosity(&mut args);
    let Some(command) = args.first().cloned() else {
        println!("{}", usage(&env));
        return Ok(2);
    };
    match command.as_str() {
        "run" => Ok(run(&env, level).await),
        "threads" => threads(&args[1..], &env, level).await,
        "help" | "--help" => {
            println!("{}", usage(&env));
            Ok(0)
        }
        other => {
            eprintln!("there is no command called {other}");
            eprintln!("{}", usage(&env));
            Ok(2)
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::grant_pair;
    use super::workspace_grant;
    use crate::config::schema::NetworkMode;
    use crate::sandbox::backend::SandboxLaunch;
    use crate::sandbox::policy::{PolicyOptions, policy_contents};
    use crate::sandbox::runtime::AgentRuntime;

    /// The revive reader scans inline tables for the grant placed at the
    /// workspace. The generated policy grows new kinds of entries over time
    /// (`[[device]]` tables, network ports); a block it cannot read must
    /// never make the whole scan give up before the workspace grant is
    /// found, or every revive refuses.
    #[test]
    fn the_policy_reader_finds_the_workspace_grant_in_a_policy_with_devices() {
        let launch = SandboxLaunch {
            session_id: "s-1".to_owned(),
            project_path: "/home/operator/code/demo".to_owned(),
            state_dir: "/home/operator/.local/state/errand/s-1".to_owned(),
            env: BTreeMap::new(),
            system_prompt_path: None,
            provider: "zai-coding-cn".to_owned(),
            model: Some("glm-5.3".to_owned()),
            providers: serde_json::Map::new(),
            credential_names: BTreeMap::new(),
            extensions: Vec::new(),
            resume: false,
        };
        let runtime = AgentRuntime {
            read_paths: vec!["/opt/agent/bin".to_owned()],
            path_entries: vec!["/opt/agent/bin".to_owned()],
        };
        for network in [NetworkMode::Restricted, NetworkMode::None] {
            let options = PolicyOptions {
                launch: &launch,
                network,
                egress_ports: Some(&[443]),
                runtime: &runtime,
                file_max: "1g",
                tmp_size: "512m",
                shm_size: "256m",
                disk_tmp: false,
                resolv_conf: "/var/lib/errand/resolv.conf",
                extra: None,
                env: None,
                path_extra: None,
            };
            let policy = policy_contents(&options);
            assert_eq!(
                workspace_grant(&policy).as_deref(),
                Some("/home/operator/code/demo"),
                "the workspace grant is lost in a {network:?} policy"
            );
        }
    }

    /// The pairs after the first arrive with a comma in front, and the
    /// reader has to step over it rather than give up on the whole scan.
    #[test]
    fn a_grant_pair_reads_the_pair_a_comma_precedes() {
        let body = " path = \"/a\", at = \"/workspace\" ";
        let (rest, path) = grant_pair(body, "path").expect("the path pair is read");
        assert_eq!(path, "/a");
        let (rest, at) = grant_pair(rest, "at").expect("the at pair is read");
        assert_eq!(at, "/workspace");
        assert!(rest.trim().is_empty());
    }
}
