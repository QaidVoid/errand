use std::io::Write;
use std::sync::{Arc, Mutex};

use super::{OPENING_SCAN_BYTES, Transcript};
use crate::log::{LogLevel, Logger};
use crate::session::event::{NoticeLevel, SessionEvent};

/// A temp directory with a transcript in it, and the lines its log caught.
struct Keep {
    root: tempfile::TempDir,
    path: std::path::PathBuf,
    lines: Arc<Mutex<Vec<(LogLevel, String)>>>,
}

fn keep() -> Keep {
    let root = tempfile::tempdir().expect("a temp directory");
    let path = root.path().join("transcript.jsonl");
    Keep {
        root,
        path,
        lines: Arc::new(Mutex::new(Vec::new())),
    }
}

impl Keep {
    fn transcript(&self) -> Transcript {
        let lines = Arc::clone(&self.lines);
        let sink: Arc<dyn Fn(LogLevel, &str) + Send + Sync> =
            Arc::new(move |level, line| lines.lock().unwrap().push((level, line.to_owned())));
        Transcript::new(&self.path, Some(Logger::new(Default::default(), sink)))
    }

    fn logged(&self) -> Vec<(LogLevel, String)> {
        self.lines.lock().unwrap().clone()
    }
}

fn post(text: &str) -> SessionEvent {
    SessionEvent::Post {
        text: text.to_owned(),
    }
}

#[test]
fn what_is_appended_comes_back_in_order_with_its_turn() {
    let keep = keep();
    let transcript = keep.transcript();
    transcript.append_at(&post("first"), Some(1), 1_000);
    transcript.append_at(
        &SessionEvent::Activity {
            line: "ran something".to_owned(),
            tool: None,
        },
        Some(2),
        2_000,
    );

    let stored = transcript.read();

    assert_eq!(stored.entries.len(), 2);
    assert_eq!(stored.entries[0].turn, Some(1));
    assert_eq!(stored.entries[0].at, 1_000);
    assert!(matches!(
        &stored.entries[1].entry,
        SessionEvent::Activity { .. }
    ));
    assert_eq!(stored.dropped, 0);
}

#[test]
fn a_session_with_no_transcript_reads_empty_rather_than_failing() {
    let keep = keep();
    let missing = Transcript::new(keep.path.with_extension("nowhere"), None);

    let stored = missing.read();

    assert_eq!(stored.entries.len(), 0);
    assert_eq!(stored.dropped, 0);
    assert_eq!(missing.opening(), None);
}

/// A crash mid-write tears the last line; the rest of the session survives.
#[test]
fn a_torn_line_is_skipped_and_everything_around_it_is_kept() {
    let keep = keep();
    let transcript = keep.transcript();
    transcript.append_at(&post("before"), Some(1), 1_000);
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(&keep.path)
        .expect("the transcript opens");
    file.write_all(r#"{"at":2000,"turn":1,"entry":{"call":"po"#.as_bytes())
        .expect("the tear is written");
    file.write_all(b"\n").expect("the tear ends");
    transcript.append_at(&post("after"), Some(1), 3_000);

    let stored = transcript.read();

    assert_eq!(
        stored
            .entries
            .iter()
            .map(|held| held.entry.clone())
            .collect::<Vec<_>>(),
        [post("before"), post("after")]
    );
}

#[test]
fn anything_that_is_not_an_entry_is_ignored() {
    let keep = keep();
    let transcript = keep.transcript();
    std::fs::write(&keep.path, "\"a bare string\"\n{\"no\":\"entry\"}\n[]\n\n")
        .expect("the noise is written");
    transcript.append_at(&post("real"), Some(1), 1_000);

    assert_eq!(transcript.read().entries.len(), 1);
}

#[test]
fn only_the_most_recent_are_read_back_and_the_gap_is_admitted() {
    let keep = keep();
    let transcript = keep.transcript();
    for index in 0..10 {
        transcript.append_at(&post(&format!("line {index}")), Some(1), index);
    }

    let stored = transcript.read_up_to(4);

    assert_eq!(stored.entries.len(), 4);
    assert_eq!(stored.dropped, 6);
    assert_eq!(stored.entries[0].entry, post("line 6"));
}

/// A transcript written before turns were kept still reads.
#[test]
fn an_entry_with_no_turn_is_still_an_entry() {
    let keep = keep();
    let transcript = keep.transcript();
    std::fs::write(
        &keep.path,
        "{\"at\":1,\"entry\":{\"call\":\"post\",\"text\":\"old\"}}\n",
    )
    .expect("the old line is written");

    let stored = transcript.read();

    assert_eq!(stored.entries[0].turn, None);
    assert_eq!(stored.entries[0].entry, post("old"));
}

#[test]
fn what_a_session_was_first_asked_is_read_from_its_transcript() {
    let keep = keep();
    let transcript = keep.transcript();
    transcript.append_at(
        &SessionEvent::Notice {
            text: "ready".to_owned(),
            level: NoticeLevel::Started,
        },
        Some(0),
        1_000,
    );
    transcript.append_at(
        &SessionEvent::Prompt {
            author: "amelia".to_owned(),
            text: "  cache the secret  ".to_owned(),
            id: None,
            withdrawn: false,
        },
        Some(1),
        2_000,
    );
    transcript.append_at(
        &SessionEvent::Prompt {
            author: "amelia".to_owned(),
            text: "and then this".to_owned(),
            id: None,
            withdrawn: false,
        },
        Some(2),
        3_000,
    );

    assert_eq!(transcript.opening().as_deref(), Some("cache the secret"));
}

#[test]
fn a_session_that_was_never_asked_anything_has_no_opening() {
    let keep = keep();
    let transcript = keep.transcript();
    transcript.append_at(
        &SessionEvent::Notice {
            text: "started".to_owned(),
            level: NoticeLevel::Started,
        },
        Some(0),
        1_000,
    );
    transcript.append_at(
        &SessionEvent::Notice {
            text: "it failed".to_owned(),
            level: NoticeLevel::Ended,
        },
        Some(0),
        2_000,
    );

    assert_eq!(transcript.opening(), None);
}

/// Proof that the scan is bounded: a prompt past the head is not found. It is
/// read once per stopped session whenever they are listed, so reading the
/// whole file would cost the length of every session.
///
/// A read landing mid-line must also not hand a half-entry to the parser.
#[test]
fn only_the_head_is_scanned_cut_back_to_the_last_whole_line() {
    let keep = keep();
    let transcript = keep.transcript();
    let filler = "y".repeat(OPENING_SCAN_BYTES);
    transcript.append_at(&post(&filler), Some(0), 1_000);
    transcript.append_at(
        &SessionEvent::Prompt {
            author: "amelia".to_owned(),
            text: "after the big one".to_owned(),
            id: None,
            withdrawn: false,
        },
        Some(1),
        2_000,
    );

    // The prompt is past the scanned head, so it is not found, and nothing
    // half-read is mistaken for an entry.
    assert_eq!(transcript.opening(), None);
}

#[test]
fn a_transcript_that_cannot_be_written_says_so_and_does_not_throw() {
    let keep = keep();
    let blocked = Transcript::new(keep.path.join("nested").join("transcript.jsonl"), {
        let lines = Arc::clone(&keep.lines);
        let sink: Arc<dyn Fn(LogLevel, &str) + Send + Sync> =
            Arc::new(move |level, line| lines.lock().unwrap().push((level, line.to_owned())));
        Some(Logger::new(Default::default(), sink))
    });

    blocked.append_at(&post("nowhere to go"), Some(1), 1_000);

    let logged = keep.logged();
    assert_eq!(logged.len(), 1);
    assert_eq!(logged[0].0, LogLevel::Warn);
}
