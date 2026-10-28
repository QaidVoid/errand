//! CLI thread command tests, ported from `threads_test.ts`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use super::{Deps, Found, Project, find_thread, run_threads};
use crate::log::{LogFields, Logger};
use crate::session::registry::{ThreadRecord, ThreadRegistry};

const NOW: i64 = 1_700_000_000_000;

fn record() -> ThreadRecord {
    record_with("thread-aaaa", "s-1", "/state/s-1")
}

fn record_with(thread_id: &str, session_id: &str, state_dir: &str) -> ThreadRecord {
    ThreadRecord {
        thread_id: thread_id.to_owned(),
        session_id: session_id.to_owned(),
        state_dir: state_dir.to_owned(),
        project_name: "demo".to_owned(),
        project_path: "/projects/demo".to_owned(),
        owner_id: "u-1".to_owned(),
        guests: Vec::new(),
        provider: None,
        model: None,
        updated_at: NOW - 3_600_000,
    }
}

/// A registry in a temporary file, since the commands write to it.
struct Harness {
    deps: Deps,
    registry: Arc<Mutex<ThreadRegistry>>,
    removed: Arc<Mutex<Vec<String>>>,
    written: Arc<Mutex<Vec<String>>>,
    root: tempfile::TempDir,
    /// Whether deletes succeed; off to simulate a refused removal.
    remove_ok: Arc<Mutex<bool>>,
    /// Bytes the commands see for a state directory.
    sizes: Arc<Mutex<HashMap<String, u64>>>,
    /// What `project_of` answers, keyed by state directory.
    projects: Arc<Mutex<HashMap<String, Project>>>,
}

fn harness(records: Vec<ThreadRecord>) -> Harness {
    let root = tempfile::tempdir().expect("a temp directory");
    let registry = Arc::new(Mutex::new(ThreadRegistry::new(
        root.path().join("threads.json").display().to_string(),
        Logger::new(LogFields::new(), Arc::new(|_level, _line| {})),
    )));
    for record in records {
        registry.lock().unwrap().remember(record);
    }

    let written = Arc::new(Mutex::new(Vec::new()));
    let removed = Arc::new(Mutex::new(Vec::new()));
    let remove_ok = Arc::new(Mutex::new(true));
    let sizes = Arc::new(Mutex::new(HashMap::new()));
    let projects = Arc::new(Mutex::new(HashMap::new()));
    let deps = Deps {
        registry: Arc::clone(&registry),
        size_of: {
            let sizes = Arc::clone(&sizes);
            Arc::new(move |state_dir: String| {
                let size = sizes.lock().unwrap().get(&state_dir).copied();
                Box::pin(async move { size })
            })
        },
        remove: {
            let removed = Arc::clone(&removed);
            let remove_ok = Arc::clone(&remove_ok);
            Arc::new(move |state_dir: String| {
                let removed = Arc::clone(&removed);
                let allowed = *remove_ok.lock().unwrap();
                Box::pin(async move {
                    if allowed {
                        removed.lock().unwrap().push(state_dir);
                        Ok(())
                    } else {
                        Err("permission denied".to_owned())
                    }
                })
            })
        },
        state_root: "/state".to_owned(),
        project_of: {
            let projects = Arc::clone(&projects);
            Arc::new(move |state_dir: String| {
                let project = projects.lock().unwrap().get(&state_dir).cloned();
                Box::pin(async move { project })
            })
        },
        write: {
            let written = Arc::clone(&written);
            Arc::new(move |line: &str| written.lock().unwrap().push(line.to_owned()))
        },
        now: Arc::new(|| NOW),
        receipts_of: Arc::new(|_| Ok(Vec::new())),
        project_root: "/projects".to_owned(),
        remove_project: Arc::new(|_| Ok(())),
        older_than_ms: 30 * 24 * 60 * 60 * 1_000,
        hold_lock: Arc::new(|| Ok(Box::new(|| {}))),
        forget_receipts: Arc::new(|_| Ok(0)),
    };
    Harness {
        deps,
        registry,
        removed,
        written,
        root,
        remove_ok,
        sizes,
        projects,
    }
}

impl Harness {
    /// Says a state directory exists and holds this much.
    fn hold(&self, state_dir: &str, size: u64) {
        self.sizes
            .lock()
            .unwrap()
            .insert(state_dir.to_owned(), size);
    }

    /// Says the project a state directory worked in.
    fn worked_in(&self, state_dir: &str, name: &str, path: &str) {
        self.projects.lock().unwrap().insert(
            state_dir.to_owned(),
            Project {
                name: name.to_owned(),
                path: path.to_owned(),
            },
        );
    }
}

impl Harness {
    fn out(&self) -> String {
        self.written.lock().unwrap().join("\n")
    }
}

#[tokio::test]
async fn an_empty_index_says_so_rather_than_printing_a_header() {
    let harness = harness(Vec::new());

    assert_eq!(run_threads(&["list".to_owned()], &harness.deps).await, 0);

    assert_eq!(harness.out(), "no threads are remembered");
}

#[tokio::test]
async fn listing_shows_each_thread_its_project_and_what_it_holds() {
    let harness = harness(vec![record()]);

    run_threads(&["list".to_owned()], &harness.deps).await;

    assert!(harness.out().contains("thread-aaaa"));
    assert!(harness.out().contains("demo"));
    assert!(harness.out().contains("1h"));
}

#[tokio::test]
async fn a_thread_whose_data_is_gone_is_listed_as_gone_not_as_empty() {
    let harness = harness(vec![record()]);

    run_threads(&["list".to_owned()], &harness.deps).await;

    assert!(harness.out().contains("gone"));
}

#[test]
fn a_thread_can_be_named_by_a_part_of_its_id_when_only_one_matches() {
    let records = vec![record(), record_with("thread-bbbb", "s-2", "/state/s-2")];

    assert!(matches!(
        find_thread(&records, "thread-aa"),
        Some(Found::One(found)) if found.thread_id == "thread-aaaa"
    ));
    assert!(find_thread(&records, "nothing").is_none());
    assert!(matches!(
        find_thread(&records, "thread-"),
        Some(Found::Ambiguous(_))
    ));
}

/// Chat ids are snowflakes: nineteen digits differing only near the end, so a
/// prefix matches everything and the tail is what anybody copies.
#[test]
fn the_tail_of_a_snowflake_identifies_it_where_the_head_cannot() {
    let records = vec![
        record_with("1546185772997157045", "s-1", "/state/s-1"),
        record_with("1546185772997199999", "s-2", "/state/s-2"),
    ];

    assert!(matches!(
        find_thread(&records, "157045"),
        Some(Found::One(found)) if found.thread_id == "1546185772997157045"
    ));
    assert!(matches!(
        find_thread(&records, "199999"),
        Some(Found::One(found)) if found.thread_id == "1546185772997199999"
    ));
    assert!(matches!(
        find_thread(&records, "15461857729971"),
        Some(Found::Ambiguous(_))
    ));
}

#[tokio::test]
async fn showing_a_thread_prints_what_it_is_and_where_it_lives() {
    let harness = harness(vec![record()]);

    assert_eq!(
        run_threads(&["show".to_owned(), "thread-aa".to_owned()], &harness.deps).await,
        0
    );

    assert!(harness.out().contains("session   s-1"));
    assert!(harness.out().contains("/projects/demo"));
    assert!(harness.out().contains("guests    none"));
}

/// What a thread's session spent its provider credentials on is shown under
/// the rest: how the newest calls came out, then one line each.
#[tokio::test]
async fn showing_a_thread_lists_the_provider_calls_it_made() {
    use crate::sandbox::broker::receipts::{Outcome, Receipt};

    let call = |path: &str, outcome: Outcome, status: Option<u16>| Receipt {
        at: NOW - 60_000,
        session_id: "s-1".to_owned(),
        provider: "/provider/zai".to_owned(),
        credential_name: Some("ZAI_KEY".to_owned()),
        upstream_host: Some("api.example.com".to_owned()),
        method: "POST".to_owned(),
        path: path.to_owned(),
        query_bytes: None,
        status,
        outcome,
        elapsed_ms: 42,
        bytes_out: 100,
        bytes_in: 2_000,
    };
    let mut harness = harness(vec![record()]);
    let asked = Arc::new(Mutex::new(Vec::new()));
    let noted = Arc::clone(&asked);
    harness.deps.receipts_of = Arc::new(move |session: String| {
        noted.lock().unwrap().push(session);
        Ok(vec![
            call("/chat", Outcome::Completed, Some(200)),
            call("/chat", Outcome::Denied, Some(401)),
            call("/models", Outcome::Completed, Some(200)),
        ])
    });

    assert_eq!(
        run_threads(&["show".to_owned(), "thread-aa".to_owned()], &harness.deps).await,
        0
    );

    assert_eq!(*asked.lock().unwrap(), ["s-1"]);
    let out = harness.out();
    assert!(
        out.contains("calls     the newest 3: 2 completed, 1 denied"),
        "{out}"
    );
    assert!(
        out.contains("/provider/zai POST /chat  401  denied  42 ms  100 B out  2000 B in"),
        "{out}"
    );
}

/// Calls that cannot be read are said, and the command fails, so a script
/// reading it does not take a broken store for an empty one.
#[tokio::test]
async fn unreadable_provider_calls_fail_the_command() {
    let mut harness = harness(vec![record()]);
    harness.deps.receipts_of = Arc::new(|_| Err("database disk image is malformed".to_owned()));

    assert_eq!(
        run_threads(&["show".to_owned(), "thread-aa".to_owned()], &harness.deps).await,
        1
    );
    assert!(
        harness
            .out()
            .contains("calls     could not be read: database disk image")
    );
}

/// A session that made no provider call says so rather than printing an
/// empty list.
#[tokio::test]
async fn a_thread_with_no_provider_calls_says_none() {
    let harness = harness(vec![record()]);

    run_threads(&["show".to_owned(), "thread-aa".to_owned()], &harness.deps).await;

    assert!(harness.out().contains("calls     none recorded"));
}

#[tokio::test]
async fn an_ambiguous_name_is_refused_rather_than_guessed_at() {
    let harness = harness(vec![
        record(),
        record_with("thread-bbbb", "s-2", "/state/s-2"),
    ]);

    assert_eq!(
        run_threads(&["show".to_owned(), "thread-".to_owned()], &harness.deps).await,
        1
    );
    assert!(harness.out().contains("matches 2 threads"));
}

#[tokio::test]
async fn forgetting_stops_a_thread_resuming_and_keeps_its_data() {
    let harness = harness(vec![record()]);

    assert_eq!(
        run_threads(
            &["forget".to_owned(), "thread-aaaa".to_owned()],
            &harness.deps
        )
        .await,
        0
    );

    assert!(
        harness
            .registry
            .lock()
            .unwrap()
            .get("thread-aaaa")
            .is_none()
    );
    assert!(harness.removed.lock().unwrap().is_empty());
    assert!(harness.out().contains("its data is still at /state/s-1"));
}

/// It deletes the agent's history, and nothing else keeps a copy.
#[tokio::test]
async fn removing_asks_first_and_does_nothing_until_it_is_confirmed() {
    let harness = harness(vec![record()]);
    harness.hold("/state/s-1", 1024);

    assert_eq!(
        run_threads(
            &["remove".to_owned(), "thread-aaaa".to_owned()],
            &harness.deps
        )
        .await,
        1
    );

    assert!(harness.out().contains("this deletes /state/s-1"));
    assert!(harness.out().contains("--yes"));
    assert!(harness.removed.lock().unwrap().is_empty());
    assert!(
        harness
            .registry
            .lock()
            .unwrap()
            .get("thread-aaaa")
            .is_some()
    );
}

#[tokio::test]
async fn removing_with_consent_deletes_the_data_and_forgets_the_thread() {
    let harness = harness(vec![record()]);
    harness.hold("/state/s-1", 1024);

    assert_eq!(
        run_threads(
            &[
                "remove".to_owned(),
                "thread-aaaa".to_owned(),
                "--yes".to_owned()
            ],
            &harness.deps
        )
        .await,
        0
    );

    assert_eq!(*harness.removed.lock().unwrap(), ["/state/s-1".to_owned()]);
    assert!(
        harness
            .registry
            .lock()
            .unwrap()
            .get("thread-aaaa")
            .is_none()
    );
}

/// The ask-first message says what would be deleted and how big it is.
#[tokio::test]
async fn removing_mentions_the_size_of_what_it_would_delete() {
    let harness = harness(vec![record()]);
    harness.hold("/state/s-1", 1024);

    assert_eq!(
        run_threads(
            &["remove".to_owned(), "thread-aaaa".to_owned()],
            &harness.deps
        )
        .await,
        1
    );

    assert!(
        harness
            .out()
            .contains("this deletes /state/s-1 and its 1.0K")
    );
}

/// Forgetting a thread whose data survived would strand the data.
#[tokio::test]
async fn a_failed_delete_leaves_the_thread_remembered() {
    let harness = harness(vec![record()]);
    *harness.remove_ok.lock().unwrap() = false;
    harness.hold("/state/s-1", 1024);

    assert_eq!(
        run_threads(
            &[
                "remove".to_owned(),
                "thread-aaaa".to_owned(),
                "--yes".to_owned()
            ],
            &harness.deps
        )
        .await,
        1
    );

    assert!(harness.out().contains("could not delete"));
    assert!(
        harness
            .registry
            .lock()
            .unwrap()
            .get("thread-aaaa")
            .is_some()
    );
}

#[tokio::test]
async fn pruning_forgets_only_the_threads_whose_data_is_already_gone() {
    let harness = harness(vec![
        record(),
        record_with("thread-bbbb", "s-2", "/state/s-2"),
    ]);
    harness.hold("/state/s-2", 512);

    assert_eq!(run_threads(&["prune".to_owned()], &harness.deps).await, 0);

    assert!(
        harness
            .registry
            .lock()
            .unwrap()
            .get("thread-aaaa")
            .is_none()
    );
    assert!(
        harness
            .registry
            .lock()
            .unwrap()
            .get("thread-bbbb")
            .is_some()
    );
    assert!(harness.out().contains("forgot 1"));
}

#[tokio::test]
async fn a_command_nobody_has_heard_of_prints_what_there_is() {
    let harness = harness(Vec::new());

    assert_eq!(
        run_threads(&["destroy-everything".to_owned()], &harness.deps).await,
        2
    );

    assert!(
        harness
            .out()
            .contains("no threads command called destroy-everything")
    );
    assert!(harness.out().contains("usage: errand threads"));
}

#[tokio::test]
async fn a_command_that_needs_a_thread_says_so_instead_of_guessing() {
    let harness = harness(vec![record()]);

    assert_eq!(run_threads(&["show".to_owned()], &harness.deps).await, 2);
    assert_eq!(run_threads(&["remove".to_owned()], &harness.deps).await, 2);
    assert!(harness.out().contains("say which thread"));
}

#[tokio::test]
async fn reviving_puts_a_forgotten_thread_back_so_it_can_be_resumed() {
    let harness = harness(Vec::new());
    harness.hold("/state/s-9", 4096);
    harness.worked_in("/state/s-9", "demo", "/projects/demo");

    let code = run_threads(
        &[
            "revive".to_owned(),
            "s-9".to_owned(),
            "--thread".to_owned(),
            "thread-zzzz".to_owned(),
            "--owner".to_owned(),
            "u-7".to_owned(),
        ],
        &harness.deps,
    )
    .await;

    assert_eq!(code, 0);
    let put = harness.registry.lock().unwrap().get("thread-zzzz").cloned();
    assert_eq!(put.expect("the thread is remembered").session_id, "s-9");
    assert!(harness.out().contains("post in the thread to resume it"));
}

/// Neither is written down in the session, so neither is guessed at.
#[tokio::test]
async fn reviving_without_a_thread_or_an_owner_is_refused() {
    let harness = harness(Vec::new());
    harness.hold("/state/s-9", 10);
    harness.worked_in("/state/s-9", "demo", "/projects/demo");

    assert_eq!(
        run_threads(
            &[
                "revive".to_owned(),
                "s-9".to_owned(),
                "--thread".to_owned(),
                "thread-zzzz".to_owned()
            ],
            &harness.deps
        )
        .await,
        2
    );
    assert!(
        harness
            .out()
            .contains("both --thread and --owner are needed")
    );
}

#[tokio::test]
async fn reviving_a_session_with_nothing_on_disk_is_refused() {
    let harness = harness(Vec::new());

    let code = run_threads(
        &[
            "revive".to_owned(),
            "gone".to_owned(),
            "--thread".to_owned(),
            "t".to_owned(),
            "--owner".to_owned(),
            "u".to_owned(),
        ],
        &harness.deps,
    )
    .await;

    assert_eq!(code, 1);
    assert!(harness.out().contains("nothing on disk"));
}

#[tokio::test]
async fn reviving_onto_a_thread_that_is_already_remembered_changes_nothing() {
    let harness = harness(vec![record()]);
    harness.hold("/state/s-9", 10);
    harness.worked_in("/state/s-9", "demo", "/projects/demo");

    let code = run_threads(
        &[
            "revive".to_owned(),
            "s-9".to_owned(),
            "--thread".to_owned(),
            "thread-aaaa".to_owned(),
            "--owner".to_owned(),
            "u".to_owned(),
        ],
        &harness.deps,
    )
    .await;

    assert_eq!(code, 1);
    assert!(harness.out().contains("already remembered"));
}

/// The project a session worked in is read out of the policy it kept.
#[tokio::test]
async fn reviving_reads_the_project_from_what_the_session_left_on_disk() {
    let root = tempfile::tempdir().unwrap();
    let registry = Arc::new(Mutex::new(ThreadRegistry::new(
        root.path().join("threads.json").display().to_string(),
        Logger::new(LogFields::new(), Arc::new(|_level, _line| {})),
    )));
    let written = Arc::new(Mutex::new(Vec::new()));
    let deps = Deps {
        registry: Arc::clone(&registry),
        size_of: Arc::new(|state_dir: String| {
            Box::pin(async move { std::fs::metadata(&state_dir).is_ok().then_some(10) })
        }),
        remove: Arc::new(|_state_dir: String| Box::pin(async { Ok(()) })),
        state_root: root.path().display().to_string(),
        project_of: Arc::new(|state_dir: String| {
            Box::pin(async move {
                let policy = std::path::Path::new(&state_dir).join("policy.toml");
                let text = std::fs::read_to_string(policy).ok()?;
                let at = text.find("path = \"")?;
                let rest = &text[at + "path = \"".len()..];
                let end = rest.find('"')?;
                let path = rest[..end].to_owned();
                Some(Project {
                    name: path.rsplit('/').next().unwrap_or_default().to_owned(),
                    path,
                })
            })
        }),
        write: {
            let written = Arc::clone(&written);
            Arc::new(move |line: &str| written.lock().unwrap().push(line.to_owned()))
        },
        now: Arc::new(|| NOW),
        receipts_of: Arc::new(|_| Ok(Vec::new())),
        project_root: "/projects".to_owned(),
        remove_project: Arc::new(|_| Ok(())),
        older_than_ms: 30 * 24 * 60 * 60 * 1_000,
        hold_lock: Arc::new(|| Ok(Box::new(|| {}))),
        forget_receipts: Arc::new(|_| Ok(0)),
    };

    let session_dir = root.path().join("s-9");
    std::fs::create_dir_all(&session_dir).unwrap();
    std::fs::write(
        session_dir.join("policy.toml"),
        "grants = [\n  { path = \"/srv/errand/projects/demo\", at = \"/workspace\" },\n]\n",
    )
    .unwrap();

    assert_eq!(
        run_threads(
            &[
                "revive".to_owned(),
                "s-9".to_owned(),
                "--thread".to_owned(),
                "thread-zzzz".to_owned(),
                "--owner".to_owned(),
                "u-7".to_owned(),
            ],
            &deps
        )
        .await,
        0
    );

    let put = registry.lock().unwrap().get("thread-zzzz").cloned();
    let put = put.expect("the thread is remembered");
    assert_eq!(put.project_path, "/srv/errand/projects/demo");
    assert_eq!(put.project_name, "demo");
    drop(written);
}

/// The roots a sweep looks in, made real, with one cold unnamed session in
/// them and the harness pointed there.
fn cold_harness() -> (Harness, tempfile::TempDir, std::path::PathBuf) {
    let roots = tempfile::tempdir().unwrap();
    let state = roots.path().join("state");
    let projects = roots.path().join("projects");
    for directory in [
        state.join("cold1"),
        state.join("cold1.record"),
        projects.join("cold1"),
    ] {
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(directory.join("held"), "x").unwrap();
    }
    let mut cold = record_with(
        "thread-cold",
        "cold1",
        &state.join("cold1").display().to_string(),
    );
    cold.project_name = "cold1".to_owned();
    cold.updated_at = NOW - 40 * 24 * 60 * 60 * 1_000;
    let mut harness = harness(vec![cold, record()]);
    harness.deps.state_root = state.display().to_string();
    harness.deps.project_root = projects.display().to_string();
    (harness, roots, state)
}

/// A dry run says what would go and deletes nothing, and needs no lock.
#[tokio::test]
async fn a_dry_run_names_what_would_be_collected_and_deletes_nothing() {
    let (mut harness, roots, state) = cold_harness();
    harness.deps.hold_lock = Arc::new(|| Err("the daemon is running".to_owned()));

    assert_eq!(
        run_threads(&["gc".to_owned(), "--dry-run".to_owned()], &harness.deps).await,
        0
    );

    assert!(
        harness.out().contains("would collect thread-cold"),
        "{}",
        harness.out()
    );
    assert!(!harness.out().contains("thread-aaaa"));
    assert!(state.join("cold1").exists());
    assert!(roots.path().join("projects/cold1").exists());
    assert_eq!(harness.registry.lock().unwrap().size(), 2);
}

/// A sweep that deletes refuses while the daemon holds its lock, since the
/// daemon owns the index and alone knows which sessions are live.
#[tokio::test]
async fn a_sweep_refuses_while_the_daemon_runs() {
    let (mut harness, _roots, state) = cold_harness();
    harness.deps.hold_lock = Arc::new(|| Err("the daemon is running".to_owned()));

    assert_eq!(run_threads(&["gc".to_owned()], &harness.deps).await, 1);

    assert!(
        harness.out().contains("stop the daemon first"),
        "{}",
        harness.out()
    );
    assert!(state.join("cold1").exists());
}

/// A sweep collects the cold session whole, its project, state, record,
/// index entry and receipts, and leaves the warm one alone.
#[tokio::test]
async fn a_sweep_collects_a_cold_session_whole() {
    let (mut harness, roots, state) = cold_harness();
    let forgot = Arc::new(Mutex::new(Vec::new()));
    let noted = Arc::clone(&forgot);
    harness.deps.forget_receipts = Arc::new(move |ids| {
        noted.lock().unwrap().extend(ids);
        Ok(1)
    });

    assert_eq!(run_threads(&["gc".to_owned()], &harness.deps).await, 0);

    for gone in [
        state.join("cold1"),
        state.join("cold1.record"),
        roots.path().join("projects/cold1"),
    ] {
        assert!(!gone.exists(), "{}", gone.display());
    }
    let left: Vec<String> = harness
        .registry
        .lock()
        .unwrap()
        .all()
        .into_iter()
        .map(|record| record.thread_id)
        .collect();
    assert_eq!(left, ["thread-aaaa"]);
    assert_eq!(*forgot.lock().unwrap(), ["cold1"]);
}

/// A window given on the command line is read as written, and one that is not
/// a length of time is refused.
#[tokio::test]
async fn the_window_can_be_given_and_is_checked() {
    let (harness, _roots, state) = cold_harness();

    assert_eq!(
        run_threads(
            &[
                "gc".to_owned(),
                "--older-than".to_owned(),
                "soon".to_owned()
            ],
            &harness.deps
        )
        .await,
        2
    );
    assert_eq!(
        run_threads(
            &["gc".to_owned(), "--older-than".to_owned(), "1y".to_owned()],
            &harness.deps
        )
        .await,
        0
    );
    assert!(state.join("cold1").exists(), "a year has not passed");
}

/// Removing an unnamed session takes its own project with it, and says so;
/// a named project is the operator's and stays.
#[tokio::test]
async fn removing_an_unnamed_session_takes_its_project_and_a_named_one_keeps_it() {
    let mut unnamed = record_with("thread-un", "un1", "/state/un1");
    unnamed.project_name = "un1".to_owned();
    unnamed.project_path = "/projects/un1".to_owned();
    let mut harness = harness(vec![unnamed, record()]);
    let projects_removed = Arc::new(Mutex::new(Vec::new()));
    let noted = Arc::clone(&projects_removed);
    harness.deps.remove_project = Arc::new(move |id| {
        noted.lock().unwrap().push(id);
        Ok(())
    });

    run_threads(
        &["remove".to_owned(), "thread-un".to_owned()],
        &harness.deps,
    )
    .await;
    assert!(
        harness
            .out()
            .contains("the session's own project at /projects/un1")
    );
    run_threads(
        &[
            "remove".to_owned(),
            "thread-un".to_owned(),
            "--yes".to_owned(),
        ],
        &harness.deps,
    )
    .await;
    run_threads(
        &[
            "remove".to_owned(),
            "thread-aaaa".to_owned(),
            "--yes".to_owned(),
        ],
        &harness.deps,
    )
    .await;

    assert_eq!(*projects_removed.lock().unwrap(), ["un1"]);
    assert!(harness.out().contains("and /projects/un1"));
}

/// An argument `gc` does not know is refused rather than ignored, so a
/// misspelt `--dry-run` never runs a sweep that deletes.
#[tokio::test]
async fn gc_refuses_an_argument_it_does_not_know() {
    let (harness, _roots, state) = cold_harness();

    assert_eq!(
        run_threads(&["gc".to_owned(), "--dryrun".to_owned()], &harness.deps).await,
        2
    );

    assert!(harness.out().contains("gc does not take --dryrun"));
    assert!(state.join("cold1").exists());
}

/// Removing a thread deletes nothing while the daemon runs, since the daemon
/// may be running its session.
#[tokio::test]
async fn removing_refuses_while_the_daemon_runs() {
    let mut harness = harness(vec![record()]);
    harness.deps.hold_lock = Arc::new(|| Err("the daemon is running".to_owned()));

    assert_eq!(
        run_threads(
            &[
                "remove".to_owned(),
                "thread-aaaa".to_owned(),
                "--yes".to_owned()
            ],
            &harness.deps
        )
        .await,
        1
    );

    assert!(harness.removed.lock().unwrap().is_empty());
    assert_eq!(harness.registry.lock().unwrap().size(), 1);
}

/// What the daemon wrote before the lock was taken is what removal judges:
/// a thread it forgot meanwhile is not deleted, and the lock is let go.
#[tokio::test]
async fn removing_reads_the_index_again_under_the_lock() {
    let mut harness = harness(vec![record()]);
    let path = harness.root.path().join("threads.json");
    let released = Arc::new(Mutex::new(false));
    harness.deps.hold_lock = {
        let released = Arc::clone(&released);
        Arc::new(move || {
            let mut daemon = ThreadRegistry::new(
                path.display().to_string(),
                Logger::new(LogFields::new(), Arc::new(|_level, _line| {})),
            );
            daemon.load();
            daemon.forget("thread-aaaa");
            let released = Arc::clone(&released);
            Ok(Box::new(move || *released.lock().unwrap() = true))
        })
    };

    let code = run_threads(
        &[
            "remove".to_owned(),
            "thread-aaaa".to_owned(),
            "--yes".to_owned(),
        ],
        &harness.deps,
    )
    .await;

    assert_eq!(code, 1);
    assert!(harness.out().contains("no longer remembered"));
    assert!(harness.removed.lock().unwrap().is_empty());
    assert!(*released.lock().unwrap());
}
