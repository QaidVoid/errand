//! Tests for opening a chat thread for work asked for on GitHub.

use std::sync::{Arc, Mutex};

use serde_json::json;

use super::ByThread;
use crate::issues::view::IssueThreads;
use crate::log::{LogFields, Logger};
use crate::session::event::SessionEvent;
use crate::session::manager::{CreatedThread, FoundView, MadeThread, ThreadFactory};
use crate::session::pr::{Api, ApiReply};
use crate::session::session::IncomingMessage;
use crate::session::views::{SessionView, ViewError};

fn silent() -> Logger {
    Logger::new(LogFields::new(), Arc::new(|_level, _line| {}))
}

/// A chat view that counts what it was told.
struct Counted(Mutex<usize>);

impl SessionView for Counted {
    fn observe<'a>(
        &'a self,
        _event: &'a SessionEvent,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), ViewError>> + Send + 'a>>
    {
        *self.0.lock().unwrap() += 1;
        Box::pin(async { Ok(()) })
    }
}

/// A chat service that opens thread `t1`, records what opened it, and finds
/// any thread asked for.
struct Chat {
    opened: Mutex<Vec<(String, String)>>,
    view: Arc<Counted>,
}

impl ThreadFactory for Chat {
    fn create(self: Arc<Self>, message: IncomingMessage, _name: String) -> MadeThread {
        Box::pin(async move {
            Ok(CreatedThread {
                id: format!("under-{}", message.id),
                view: Arc::clone(&self.view) as Arc<dyn SessionView>,
            })
        })
    }

    fn open(self: Arc<Self>, name: String, opener: String) -> MadeThread {
        self.opened.lock().unwrap().push((name, opener));
        Box::pin(async move {
            Ok(CreatedThread {
                id: "t1".to_owned(),
                view: Arc::clone(&self.view) as Arc<dyn SessionView>,
            })
        })
    }

    fn port_for(self: Arc<Self>, _thread_id: String) -> FoundView {
        Box::pin(async move { Some(Arc::clone(&self.view) as Arc<dyn SessionView>) })
    }
}

fn routed(path: std::path::PathBuf) -> (Arc<ByThread>, Arc<Chat>, Arc<Mutex<Vec<String>>>) {
    routed_at(path, "GitHub", "https://github.com")
}

fn routed_at(
    path: std::path::PathBuf,
    name: &'static str,
    site: &str,
) -> (Arc<ByThread>, Arc<Chat>, Arc<Mutex<Vec<String>>>) {
    let posted = Arc::new(Mutex::new(Vec::new()));
    let seen = Arc::clone(&posted);
    let api: Api = Arc::new(move |path, _init| {
        seen.lock().unwrap().push(path);
        Box::pin(async {
            ApiReply {
                status: 201,
                body: json!({}),
            }
        })
    });
    let chat = Arc::new(Chat {
        opened: Mutex::new(Vec::new()),
        view: Arc::new(Counted(Mutex::new(0))),
    });
    let issues = Arc::new(IssueThreads {
        kind: if name == "GitHub" {
            crate::config::schema::ForgeKind::Github
        } else {
            crate::config::schema::ForgeKind::Forgejo
        },
        api,
        token: "ghp".to_owned(),
        log: silent(),
    });
    let router = Arc::new(ByThread::new(
        Arc::clone(&chat) as Arc<dyn ThreadFactory>,
        issues,
        name,
        site,
        path,
        silent(),
    ));
    (router, chat, posted)
}

fn said(channel_id: &str) -> IncomingMessage {
    IncomingMessage {
        id: "github-comment:5".to_owned(),
        author_id: "github:qaidvoid".to_owned(),
        channel_id: channel_id.to_owned(),
        author_name: Some("QaidVoid".to_owned()),
        content: String::new(),
        attachments: Vec::new(),
        from_interface: false,
    }
}

/// Work asked for on an issue gets a chat thread of its own, saying where it
/// was asked; the thread answers the issue too, and the link outlives the
/// daemon.
#[tokio::test]
async fn an_issue_gets_a_chat_thread_that_answers_it_across_a_restart() {
    let state = tempfile::tempdir().unwrap();
    let links = state.path().join("issue-threads.json");
    let (router, chat, posted) = routed(links.clone());

    let made = Arc::clone(&router)
        .create(said("github:o/r#4"), "fix it".to_owned())
        .await
        .expect("made");
    assert_eq!(made.id, "t1");
    assert_eq!(
        *chat.opened.lock().unwrap(),
        [(
            "fix it".to_owned(),
            "QaidVoid asked on GitHub, on o/r#4: https://github.com/o/r/issues/4".to_owned()
        )]
    );
    assert_eq!(router.thread_for("github:o/r#4").as_deref(), Some("t1"));

    made.view
        .observe(&SessionEvent::Post {
            text: "done".to_owned(),
        })
        .await
        .unwrap();
    made.view
        .observe(&SessionEvent::Busy { busy: false })
        .await
        .unwrap();
    assert_eq!(
        *chat.view.0.lock().unwrap(),
        2,
        "the thread is told everything"
    );
    assert_eq!(*posted.lock().unwrap(), ["/repos/o/r/issues/4/comments"]);

    let (restarted, _, posted_again) = routed(links);
    assert_eq!(restarted.thread_for("github:o/r#4").as_deref(), Some("t1"));
    let found = Arc::clone(&restarted)
        .port_for("t1".to_owned())
        .await
        .expect("found");
    for event in [
        SessionEvent::Post {
            text: "ok".to_owned(),
        },
        SessionEvent::Busy { busy: false },
    ] {
        found.observe(&event).await.unwrap();
    }
    assert_eq!(
        *posted_again.lock().unwrap(),
        ["/repos/o/r/issues/4/comments"]
    );
}

/// A message in the chat is made a thread by the chat, as it always was.
#[tokio::test]
async fn a_chat_message_is_threaded_by_the_chat_alone() {
    let state = tempfile::tempdir().unwrap();
    let (router, chat, posted) = routed(state.path().join("issue-threads.json"));

    let made = Arc::clone(&router)
        .create(said("1524281087214223390"), "demo".to_owned())
        .await
        .expect("made");
    made.view
        .observe(&SessionEvent::Busy { busy: false })
        .await
        .unwrap();

    assert_eq!(made.id, "under-github-comment:5");
    assert!(chat.opened.lock().unwrap().is_empty());
    assert!(posted.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_legacy_github_link_never_posts_to_forgejo() {
    let state = tempfile::tempdir().unwrap();
    let links = state.path().join("issue-threads.json");
    std::fs::write(&links, r#"{"github:o/r#4":"old-thread"}"#).unwrap();
    let (router, _, posted) = routed_at(links.clone(), "Forgejo", "https://forge.example");
    assert_eq!(router.thread_for("github:o/r#4"), None);
    let view = Arc::clone(&router)
        .port_for("old-thread".to_owned())
        .await
        .unwrap();
    finish_turn(&view).await;
    assert!(posted.lock().unwrap().is_empty());

    let (github, _, posted) = routed(links);
    assert_eq!(
        github.thread_for("github:o/r#4").as_deref(),
        Some("old-thread")
    );
    let view = Arc::clone(&github)
        .port_for("old-thread".to_owned())
        .await
        .unwrap();
    finish_turn(&view).await;
    assert_eq!(*posted.lock().unwrap(), ["/repos/o/r/issues/4/comments"]);
}

#[tokio::test]
async fn scoped_github_link_wins_over_legacy_and_migration_is_written_on_next_link() {
    let state = tempfile::tempdir().unwrap();
    let links = state.path().join("issue-threads.json");
    let original = r#"{"github:o/r#4":"old-thread","https://github.com|github:o/r#4":"scoped-thread","forgejo:o/r#9":"detached-thread"}"#;
    std::fs::write(&links, original).unwrap();
    let (router, _, posted) = routed(links.clone());
    assert_eq!(
        router.thread_for("github:o/r#4").as_deref(),
        Some("scoped-thread")
    );
    assert_eq!(std::fs::read_to_string(&links).unwrap(), original);
    let old = Arc::clone(&router)
        .port_for("old-thread".to_owned())
        .await
        .unwrap();
    finish_turn(&old).await;
    assert!(posted.lock().unwrap().is_empty());
    Arc::clone(&router)
        .create(said("github:o/r#5"), "new".to_owned())
        .await
        .unwrap();
    let saved: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&links).unwrap()).unwrap();
    assert!(saved.get("github:o/r#4").is_none());
    assert_eq!(saved["https://github.com|github:o/r#4"], "scoped-thread");
    assert_eq!(saved["forgejo:o/r#9"], "detached-thread");
}

#[tokio::test]
async fn a_saved_link_belongs_to_one_forgejo_instance() {
    let state = tempfile::tempdir().unwrap();
    let links = state.path().join("issue-threads.json");
    let (first, _, _) = routed_at(links.clone(), "Forgejo", "https://forge.example/first");
    Arc::clone(&first)
        .create(said("forgejo:o/r#4"), "first".to_owned())
        .await
        .unwrap();

    for site in [
        "https://other.example/first",
        "https://forge.example/second",
    ] {
        let (other, _, posted) = routed_at(links.clone(), "Forgejo", site);
        assert_eq!(other.thread_for("forgejo:o/r#4"), None);
        let view = Arc::clone(&other).port_for("t1".to_owned()).await.unwrap();
        finish_turn(&view).await;
        assert!(posted.lock().unwrap().is_empty());
    }

    let (restarted, _, posted) = routed_at(links, "Forgejo", "https://forge.example/first");
    assert_eq!(restarted.thread_for("forgejo:o/r#4").as_deref(), Some("t1"));
    let view = Arc::clone(&restarted)
        .port_for("t1".to_owned())
        .await
        .unwrap();
    finish_turn(&view).await;
    assert_eq!(*posted.lock().unwrap(), ["/repos/o/r/issues/4/comments"]);
}

#[tokio::test]
async fn an_unscoped_legacy_forgejo_link_has_no_safe_destination() {
    let state = tempfile::tempdir().unwrap();
    let links = state.path().join("issue-threads.json");
    std::fs::write(&links, r#"{"forgejo:o/r#4":"old-thread"}"#).unwrap();
    let (router, _, posted) = routed_at(links, "Forgejo", "https://forge.example");
    assert_eq!(router.thread_for("forgejo:o/r#4"), None);
    let view = Arc::clone(&router)
        .port_for("old-thread".to_owned())
        .await
        .unwrap();
    finish_turn(&view).await;
    assert!(posted.lock().unwrap().is_empty());
}

async fn finish_turn(view: &Arc<dyn SessionView>) {
    for event in [
        SessionEvent::Post {
            text: "done".to_owned(),
        },
        SessionEvent::Busy { busy: false },
    ] {
        view.observe(&event).await.unwrap();
    }
}

#[tokio::test]
async fn writing_a_new_link_preserves_other_instances_and_legacy_github_links() {
    let state = tempfile::tempdir().unwrap();
    let links = state.path().join("issue-threads.json");
    std::fs::write(
        &links,
        r#"{"github:o/r#4":"github-thread","https://first.example|forgejo:o/r#4":"first-thread"}"#,
    )
    .unwrap();
    let (second, _, _) = routed_at(links.clone(), "Forgejo", "https://second.example");
    Arc::clone(&second)
        .create(said("forgejo:o/r#4"), "second".to_owned())
        .await
        .unwrap();

    let (github, _, _) = routed(links.clone());
    assert_eq!(
        github.thread_for("github:o/r#4").as_deref(),
        Some("github-thread")
    );
    let (first, _, _) = routed_at(links.clone(), "Forgejo", "https://first.example");
    assert_eq!(
        first.thread_for("forgejo:o/r#4").as_deref(),
        Some("first-thread")
    );
    let (second, _, _) = routed_at(links, "Forgejo", "https://second.example");
    assert_eq!(second.thread_for("forgejo:o/r#4").as_deref(), Some("t1"));
}

#[tokio::test]
async fn a_new_issue_from_another_forge_is_refused_before_opening_a_thread() {
    let state = tempfile::tempdir().unwrap();
    let (router, chat, posted) = routed(state.path().join("issue-threads.json"));
    assert!(
        Arc::clone(&router)
            .create(said("forgejo:o/r#4"), "wrong forge".to_owned())
            .await
            .is_err()
    );
    assert!(chat.opened.lock().unwrap().is_empty());
    assert!(posted.lock().unwrap().is_empty());
}

#[tokio::test]
async fn forgejo_issue_opener_uses_configured_site_and_avoids_double_slash() {
    let state = tempfile::tempdir().unwrap();
    let (router, chat, _) = routed_at(
        state.path().join("issue-threads.json"),
        "Forgejo",
        "https://forge.example/forge/",
    );
    Arc::clone(&router)
        .create(said("forgejo:o/r#4"), "fix it".to_owned())
        .await
        .expect("open Forgejo thread");
    assert_eq!(
        *chat.opened.lock().unwrap(),
        [(
            "fix it".to_owned(),
            "QaidVoid asked on Forgejo, on o/r#4: https://forge.example/forge/o/r/issues/4"
                .to_owned()
        )]
    );
}
