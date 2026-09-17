use super::{MIN_CHECK_MS, next_check_ms, tree_bytes, verdict};

#[test]
fn everything_under_a_directory_is_counted_at_any_depth() {
    let root = tempfile::tempdir().expect("a temp directory");
    std::fs::write(root.path().join("a.txt"), "x".repeat(100)).unwrap();
    std::fs::create_dir_all(root.path().join("deep").join("deeper")).unwrap();
    std::fs::write(root.path().join("deep").join("b.txt"), "y".repeat(50)).unwrap();
    std::fs::write(
        root.path().join("deep").join("deeper").join("c.txt"),
        "z".repeat(25),
    )
    .unwrap();

    assert_eq!(
        tree_bytes(root.path().to_str().unwrap()),
        Some(100 + 50 + 25)
    );
}

/// Following one would let a session look enormous, or hide what it wrote.
#[test]
fn a_symlink_counts_as_the_link_not_as_what_it_points_at() {
    let root = tempfile::tempdir().expect("a temp directory");
    std::fs::write(root.path().join("real.txt"), "x".repeat(1000)).unwrap();
    std::fs::create_dir(root.path().join("inside")).unwrap();
    std::os::unix::fs::symlink(
        root.path().join("real.txt"),
        root.path().join("inside/link.txt"),
    )
    .unwrap();

    let total = tree_bytes(root.path().join("inside").to_str().unwrap()).unwrap_or(0);

    assert!(total < 1000, "the link is not counted as its target");
}

#[test]
fn a_directory_that_is_not_there_is_not_zero() {
    assert_eq!(tree_bytes("/no/such/place/at/all"), None);
}

#[test]
fn an_empty_directory_holds_nothing() {
    let root = tempfile::tempdir().expect("a temp directory");

    assert_eq!(tree_bytes(root.path().to_str().unwrap()), Some(0));
}

#[test]
fn a_session_under_its_budget_is_left_alone_and_over_it_is_not() {
    assert_eq!(verdict(0, 1_000).as_str(), "under");
    assert_eq!(verdict(700, 1_000).as_str(), "under");
    assert_eq!(verdict(800, 1_000).as_str(), "close");
    assert_eq!(verdict(1_000, 1_000).as_str(), "over");
    assert_eq!(verdict(5_000, 1_000).as_str(), "over");
}

/// No budget is not a budget of zero, which everything would be over.
#[test]
fn a_session_with_no_budget_is_always_under_it() {
    assert_eq!(verdict(9_999, 0).as_str(), "under");
}

#[test]
fn an_idle_session_settles_back_to_the_configured_interval() {
    assert_eq!(next_check_ms(500, 500, 10_000, 1_000, 30_000), 30_000);
    assert_eq!(next_check_ms(400, 500, 10_000, 1_000, 30_000), 30_000);
}

/// A fixed interval decides the overshoot: at 30 second checks, a session
/// writing a gigabyte a second is 20 GB past a 5 GB budget before anything
/// notices. That is not hypothetical, it happened.
#[test]
fn a_fast_writer_is_measured_again_long_before_it_reaches_its_budget() {
    let budget = 5_000_000_000;
    let written = 1_000_000_000;

    let next = next_check_ms(written, 0, budget, 1_000, 30_000);

    assert_eq!(next, 2_000);
    assert!(next < 30_000);
}

#[test]
fn the_check_never_runs_faster_than_its_floor() {
    assert_eq!(next_check_ms(999, 0, 1_000, 1_000, 30_000), MIN_CHECK_MS);
}

/// A session removing its own work while it is being measured is ordinary,
/// and once threw out of the measurement rather than being counted as gone.
#[tokio::test]
async fn a_directory_that_goes_mid_walk_does_not_fail_the_measurement() {
    let root = tempfile::tempdir().expect("a temp directory");
    for name in ["a", "b", "c"] {
        std::fs::create_dir_all(root.path().join(name)).unwrap();
        std::fs::write(root.path().join(name).join("file"), "x".repeat(10)).unwrap();
    }

    let measuring = {
        let path = root.path().to_path_buf();
        tokio::task::spawn_blocking(move || tree_bytes(path.to_str().unwrap()))
    };
    std::fs::remove_dir_all(root.path().join("b")).unwrap();

    let total = measuring.await.unwrap();
    assert!(total.is_some(), "the measurement returns a number");
    assert!(total.unwrap_or(0) <= 30);
}
