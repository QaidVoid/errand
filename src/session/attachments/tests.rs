use super::{ATTACHMENTS_DIR, Limits, RawAttachment, is_image, receive};

const LIMITS: Limits = Limits {
    max_bytes: 1_000,
    max_count: 3,
};

fn sent(name: &str) -> RawAttachment {
    sent_as(name, 4, None)
}

fn sent_as(name: &str, size: u64, content_type: Option<&str>) -> RawAttachment {
    RawAttachment {
        id: "1".to_owned(),
        name: name.to_owned(),
        url: format!("https://files.example/{name}"),
        size,
        content_type: content_type.map(str::to_owned),
    }
}

const CONTENT: &[u8] = b"hello";

async fn serve(_url: String) -> Result<Vec<u8>, Box<dyn std::error::Error + Send + Sync>> {
    Ok(CONTENT.to_vec())
}

fn project() -> tempfile::TempDir {
    tempfile::tempdir().expect("a temp directory")
}

#[tokio::test]
async fn a_file_is_written_under_the_project_in_a_directory_of_its_own() {
    let root = project();

    let outcome = receive(
        &[sent("notes.txt")],
        root.path().to_str().unwrap(),
        LIMITS,
        serve,
    )
    .await;

    assert!(outcome.refused.is_empty());
    assert_eq!(outcome.taken[0].path, "attachments/notes.txt");
    assert_eq!(
        std::fs::read_to_string(root.path().join(ATTACHMENTS_DIR).join("notes.txt")).unwrap(),
        "hello"
    );
}

/// The name is a label. Anything that could make it a location is removed.
#[tokio::test]
async fn a_name_aiming_out_of_the_project_lands_inside_it_anyway() {
    let root = project();

    let outcome = receive(
        &[sent("../../etc/passwd")],
        root.path().to_str().unwrap(),
        LIMITS,
        serve,
    )
    .await;

    assert_eq!(outcome.taken[0].path, "attachments/_.._etc_passwd");
    assert_eq!(
        std::fs::read_dir(root.path().join(ATTACHMENTS_DIR))
            .unwrap()
            .count(),
        1
    );
}

#[tokio::test]
async fn a_name_that_is_nothing_but_punctuation_still_gets_a_file() {
    let root = project();

    let outcome = receive(&[sent("...")], root.path().to_str().unwrap(), LIMITS, serve).await;

    assert_eq!(outcome.taken[0].path, "attachments/attachment");
}

/// Being helpful must never overwrite the work.
#[tokio::test]
async fn a_second_file_of_the_same_name_is_numbered_not_written_over() {
    let root = project();

    let outcome = receive(
        &[sent("notes.txt"), sent("notes.txt"), sent("notes.txt")],
        root.path().to_str().unwrap(),
        LIMITS,
        serve,
    )
    .await;

    let paths: Vec<String> = outcome.taken.into_iter().map(|file| file.path).collect();
    assert_eq!(
        paths,
        [
            "attachments/notes.txt",
            "attachments/notes-2.txt",
            "attachments/notes-3.txt"
        ]
    );
}

#[tokio::test]
async fn what_is_too_large_is_refused_by_its_claimed_size_before_it_is_fetched() {
    let root = project();
    let fetched = std::cell::Cell::new(0);
    let counting = |url: String| {
        fetched.set(fetched.get() + 1);
        async move { serve(url).await }
    };

    let outcome = receive(
        &[sent_as("huge.bin", 9_999, None)],
        root.path().to_str().unwrap(),
        LIMITS,
        counting,
    )
    .await;

    assert_eq!(fetched.get(), 0);
    assert!(outcome.refused[0].reason.contains("1000 byte limit"));
}

/// The size is a claim until the bytes are in hand, so it is checked twice.
#[tokio::test]
async fn a_file_that_arrives_larger_than_it_claimed_is_still_refused() {
    let root = project();
    let oversized = |url: String| async move {
        let _ = url;
        Ok::<Vec<u8>, Box<dyn std::error::Error + Send + Sync>>(vec![b'x'; 5_000])
    };

    let outcome = receive(
        &[sent("small.bin")],
        root.path().to_str().unwrap(),
        LIMITS,
        oversized,
    )
    .await;

    assert!(outcome.taken.is_empty());
    assert!(outcome.refused[0].reason.contains("byte limit"));
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
}

#[tokio::test]
async fn more_files_than_allowed_leaves_the_ones_that_fit() {
    let root = project();
    let many: Vec<RawAttachment> = ["a.txt", "b.txt", "c.txt", "d.txt"]
        .iter()
        .map(|name| sent(name))
        .collect();

    let outcome = receive(&many, root.path().to_str().unwrap(), LIMITS, serve).await;

    assert_eq!(outcome.taken.len(), 3);
    assert!(outcome.refused[0].reason.contains("more than 3 file(s)"));
}

/// One file failing must not lose the message, or the files beside it.
#[tokio::test]
async fn a_file_that_cannot_be_fetched_is_reported_and_the_rest_are_kept() {
    let root = project();
    let flaky = |url: String| async move {
        if url.contains("gone") {
            Err::<Vec<u8>, Box<dyn std::error::Error + Send + Sync>>("404".into())
        } else {
            Ok(CONTENT.to_vec())
        }
    };

    let outcome = receive(
        &[sent("gone.txt"), sent("here.txt")],
        root.path().to_str().unwrap(),
        LIMITS,
        flaky,
    )
    .await;

    let paths: Vec<String> = outcome.taken.into_iter().map(|file| file.path).collect();
    assert_eq!(paths, ["attachments/here.txt"]);
    assert!(outcome.refused[0].reason.contains("could not be fetched"));
}

#[test]
fn an_image_is_recognised_by_what_it_says_it_is_or_by_its_name() {
    assert!(is_image(Some("image/png"), "screenshot"));
    assert!(is_image(None, "screenshot.JPEG"));
    assert!(!is_image(None, "notes.txt"));
    assert!(is_image(Some("application/octet-stream"), "logo.webp"));
}

/// The agent writes the attachments directory, so a link planted at the name
/// an attachment is about to take must not redirect the write.
#[tokio::test]
async fn a_link_at_the_target_name_does_not_redirect_the_write() {
    let root = project();
    let outside = project();
    let target = outside.path().join("id_ed25519");
    std::fs::write(&target, "the host's own key").expect("written");

    let directory = root.path().join(ATTACHMENTS_DIR);
    std::fs::create_dir_all(&directory).expect("made");
    std::os::unix::fs::symlink(&target, directory.join("notes.txt")).expect("linked");

    let outcome = receive(
        &[sent("notes.txt")],
        root.path().to_str().unwrap(),
        LIMITS,
        serve,
    )
    .await;

    // The link is a name already taken, so the next one is used instead.
    assert_eq!(outcome.taken.len(), 1);
    assert_ne!(outcome.taken[0].path, "attachments/notes.txt");
    assert_eq!(
        std::fs::read_to_string(&target).expect("read back"),
        "the host's own key",
        "the host's file must be untouched"
    );
}
