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
        api,
        token: "ghp".to_owned(),
        log: silent(),
    });
    let router = Arc::new(ByThread::new(
        Arc::clone(&chat) as Arc<dyn ThreadFactory>,
        issues,
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
        .create(said("github:o/r#4"), "r-4: fix it".to_owned())
        .await
        .expect("made");
    assert_eq!(made.id, "t1");
    assert_eq!(
        *chat.opened.lock().unwrap(),
        [(
            "r-4: fix it".to_owned(),
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
    found
        .observe(&SessionEvent::Reply {
            text: "ok".to_owned(),
            command: "!model".to_owned(),
        })
        .await
        .unwrap();
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
