use std::path::PathBuf;

use super::{
    prepare_record_dir, record_dir, transcript_path, withdraw_from_agent_session,
    withdraw_from_record,
};
use crate::session::transcript::TRANSCRIPT_FILENAME;

/// The state directory is handed to the session as `/state`, writable. What
/// the daemon records about a session must therefore not be inside it.
#[test]
fn the_record_sits_beside_the_state_directory_never_inside_it() {
    let state = "/var/lib/errand/abc";

    assert!(!record_dir(state).starts_with(&format!("{state}/")));
    assert_eq!(record_dir(state), "/var/lib/errand/abc.record");
}

#[test]
fn a_transcript_left_in_the_older_place_is_moved_out_of_reach() {
    let root = tempfile::tempdir().expect("a temp directory");
    let state = root.path().join("session");
    std::fs::create_dir_all(&state).expect("the state directory is made");
    let legacy = state.join(TRANSCRIPT_FILENAME);
    std::fs::write(&legacy, "{\"turn\":1}\n").expect("the old transcript is written");

    // Read before the move still finds the history.
    assert_eq!(transcript_path(state.to_str().expect("a path")), legacy);

    prepare_record_dir(state.to_str().expect("a path"));

    let placed =
        PathBuf::from(record_dir(state.to_str().expect("a path"))).join(TRANSCRIPT_FILENAME);
    assert_eq!(transcript_path(state.to_str().expect("a path")), placed);
    assert_eq!(
        std::fs::read_to_string(&placed).expect("the moved transcript reads"),
        "{\"turn\":1}\n"
    );
    // Gone from the directory the session can write.
    assert!(!transcript_path(state.to_str().expect("a path")).starts_with(&state));
}

/// A session's two directories, with a prompt worth taking back in each.
struct WithSession {
    _root: tempfile::TempDir,
    state: PathBuf,
    record: PathBuf,
    said: String,
}

fn with_session() -> WithSession {
    let root = tempfile::tempdir().expect("a temp directory");
    let state = root.path().join("session");
    let record = PathBuf::from(format!("{}.record", state.display()));
    let said = "my token is ghp_TOPSECRET123";
    std::fs::create_dir_all(&record).expect("the record directory is made");
    std::fs::create_dir_all(state.join("sessions")).expect("the sessions directory is made");
    std::fs::write(
        record.join(TRANSCRIPT_FILENAME),
        [
            serde_json::json!({"at": 1, "entry": {"call": "prompt", "author": "a", "text": "hello", "id": "m-1"}}).to_string(),
            serde_json::json!({"at": 2, "entry": {"call": "prompt", "author": "a", "text": said, "id": "m-2"}}).to_string(),
            serde_json::json!({"at": 3, "entry": {"call": "post", "text": "sure, noted"}}).to_string(),
        ]
        .join("\n"),
    )
    .expect("the transcript is written");
    std::fs::write(
        state.join("sessions").join("s.jsonl"),
        [
            serde_json::json!({"type": "session", "version": 3}).to_string(),
            serde_json::json!({
                "type": "message",
                "id": "a1",
                "parentId": "root",
                "message": {"role": "user", "content": [{"type": "text", "text": format!("<context>\n{said}")}]},
            })
            .to_string(),
            serde_json::json!({
                "type": "message",
                "id": "a2",
                "parentId": "a1",
                "message": {"role": "assistant", "content": [{"type": "text", "text": "sure, noted"}]},
            })
            .to_string(),
        ]
        .join("\n"),
    )
    .expect("the stored conversation is written");

    WithSession {
        _root: root,
        state,
        record,
        said: said.to_owned(),
    }
}

/// The words are the whole point: what a person deletes soonest is what they
/// should not have sent, and it lives in two files after the chat forgets it.
#[test]
fn a_withdrawn_message_leaves_both_copies() {
    let session = with_session();
    let state = session.state.to_str().expect("a path");

    let removed = withdraw_from_record(state, "m-2").expect("the record is rewritten");
    assert_eq!(removed.as_deref(), Some(session.said.as_str()));
    assert_eq!(
        withdraw_from_agent_session(state, &removed.expect("the text is reported"))
            .expect("the stored conversation is rewritten"),
        true
    );

    let transcript =
        std::fs::read_to_string(session.record.join(TRANSCRIPT_FILENAME)).expect("it reads");
    assert!(!transcript.contains("ghp_TOPSECRET123"));
    assert!(transcript.contains(r#""withdrawn":true"#));
    // A transcript that quietly drops a turn reads as one that never had it.
    assert!(transcript.contains("hello"));
    assert!(transcript.contains("sure, noted"));

    let stored =
        std::fs::read_to_string(session.state.join("sessions").join("s.jsonl")).expect("it reads");
    assert!(!stored.contains("ghp_TOPSECRET123"));
    // The file is a chain by parentId; stranding the rest would be worse than
    // leaving the text.
    assert!(stored.contains(r#""parentId":"a1""#));
    for line in stored.split('\n') {
        if line.trim().is_empty() {
            continue;
        }
        serde_json::from_str::<serde_json::Value>(line).expect("every line parses");
    }
}

#[test]
fn a_message_nobody_sent_here_withdraws_nothing() {
    let session = with_session();
    let state = session.state.to_str().expect("a path");

    assert_eq!(
        withdraw_from_record(state, "not-a-message").expect("it reads"),
        None
    );
    assert_eq!(
        withdraw_from_agent_session(state, "words never said").expect("it reads"),
        false
    );
}

/// Repairing the agent's file is not a withdrawal's business.
#[test]
fn a_line_that_cannot_be_parsed_is_carried_across_untouched() {
    let session = with_session();
    let state = session.state.to_str().expect("a path");
    let path = session.state.join("sessions").join("s.jsonl");
    let mut text = std::fs::read_to_string(&path).expect("it reads");
    text.push_str("\n{ this is not json");
    std::fs::write(&path, text).expect("the torn line is added");

    assert_eq!(
        withdraw_from_agent_session(state, &session.said).expect("it rewrites"),
        true
    );
    let stored = std::fs::read_to_string(&path).expect("it reads");
    assert!(stored.contains("{ this is not json"));
    assert!(!stored.contains("ghp_TOPSECRET123"));
}
