//! Tests for the restart wait and the note a restart leaves.

use super::{RestartNote, Turn, leave_note, take_note, wait_of};

#[test]
fn the_wait_is_the_configured_one_unless_the_command_names_another() {
    assert_eq!(wait_of("", 600_000), Ok(600_000));
    assert_eq!(wait_of("now", 600_000), Ok(0));
    assert_eq!(wait_of(" 30m ", 600_000), Ok(1_800_000));
    assert_eq!(wait_of("1h 30m", 600_000), Ok(5_400_000));
    assert!(wait_of("soon", 600_000).unwrap_err().contains("`soon`"));
}

/// The note is read once: a second start must not continue the same turns
/// again, and a start with no note is an ordinary one.
#[test]
fn a_note_is_read_back_once_and_its_absence_is_no_error() {
    let dir = tempfile::tempdir().unwrap();
    let state_dir = dir.path().to_str().unwrap();
    assert_eq!(take_note(state_dir), Ok(None));

    let note = RestartNote {
        asked_in: "111".to_owned(),
        interrupted: vec![Turn {
            thread_id: "222".to_owned(),
            owner_id: "333".to_owned(),
        }],
        dropped: vec!["444".to_owned()],
    };
    leave_note(state_dir, &note).unwrap();

    assert_eq!(take_note(state_dir), Ok(Some(note)));
    assert_eq!(take_note(state_dir), Ok(None));
}

#[test]
fn a_note_that_does_not_read_is_reported_and_not_left_to_trip_every_start() {
    let dir = tempfile::tempdir().unwrap();
    let state_dir = dir.path().to_str().unwrap();
    std::fs::write(dir.path().join(super::NOTE_FILENAME), "{ not json").unwrap();

    assert!(take_note(state_dir).unwrap_err().contains("does not read"));
    assert_eq!(take_note(state_dir), Ok(None));
}
