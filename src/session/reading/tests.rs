//! Tests for the read commands' answers.

use super::{Project, ReadAnswer, answer};

/// A project holding one small file, for a read to look at.
fn project_with_a_file() -> tempfile::TempDir {
    let root = tempfile::tempdir().expect("a temp directory");
    std::fs::write(root.path().join("notes.txt"), "kept on disk").expect("a file is written");
    root
}

fn say(answer: Option<ReadAnswer>) -> String {
    match answer {
        Some(ReadAnswer::Say(text)) => text,
        other => panic!("expected words, got {other:?}"),
    }
}

/// A path that climbs out of the project is refused by name, for every read.
#[test]
fn a_path_outside_the_project_is_refused() {
    let root = project_with_a_file();
    let path = root.path().display().to_string();
    let project = Project {
        name: "demo",
        path: &path,
    };
    for word in ["!ls", "!cat", "!file"] {
        let text = say(answer(word, "../../etc/passwd", &project, 1024));
        assert!(
            text.contains("is not inside this session's project"),
            "{word}: {text}"
        );
    }
}

/// A file over the upload limit is refused with its size, and one under it
/// goes up whole with its name.
#[test]
fn an_upload_is_held_to_the_limit() {
    let root = project_with_a_file();
    let path = root.path().display().to_string();
    let project = Project {
        name: "demo",
        path: &path,
    };
    let text = say(answer("!file", "notes.txt", &project, 4));
    assert!(
        text.contains("larger than the 4 byte upload limit"),
        "{text}"
    );

    assert_eq!(
        answer("!file", "notes.txt", &project, 1024),
        Some(ReadAnswer::Upload {
            name: "notes.txt".to_owned(),
            bytes: b"kept on disk".to_vec(),
            caption: "`notes.txt` 12 bytes".to_owned(),
        })
    );
    assert!(say(answer("!file", "", &project, 1024)).contains("say which file"));
}

/// Only the read commands are answered here.
#[test]
fn any_other_command_is_left_alone() {
    let project = Project {
        name: "demo",
        path: "/nowhere",
    };
    assert_eq!(answer("!status", "", &project, 1024), None);
}
