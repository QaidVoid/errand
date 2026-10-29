use std::collections::BTreeMap;

use super::{
    REQUEST_FILENAME, SessionLinks, attribution_footer, gh_shim_contents, git_config_contents,
    git_identity_env, review_instructions, thread_link, transcript_link,
};
use crate::config::schema::GithubConfig;
use crate::sandbox::backend::STATE_PATH;

fn github() -> GithubConfig {
    GithubConfig {
        token: "ghp-value".to_owned(),
        user_name: "errand-bot".to_owned(),
        user_email: "bot@example.com".to_owned(),
        trigger: None,
        app: None,
    }
}

#[test]
fn the_git_config_commits_as_the_bot_and_gets_its_token_from_gh() {
    let config = git_config_contents(&github());

    assert!(config.contains("name = errand-bot"));
    assert!(config.contains("email = bot@example.com"));
    assert!(config.contains("helper = !gh auth git-credential"));
}

/// A second copy of the token on disk buys nothing and can be read.
#[test]
fn the_token_is_never_written_into_the_git_config() {
    assert!(!git_config_contents(&github()).contains("ghp-value"));
}

/// The config file alone was not enough: an agent passing its own name at
/// commit time authored as itself, and the environment beats the file.
#[test]
fn the_identity_is_in_the_environment_as_well_as_in_the_file() {
    assert_eq!(
        git_identity_env(&github()),
        BTreeMap::from([
            ("GIT_AUTHOR_NAME".to_owned(), "errand-bot".to_owned()),
            ("GIT_AUTHOR_EMAIL".to_owned(), "bot@example.com".to_owned()),
            ("GIT_COMMITTER_NAME".to_owned(), "errand-bot".to_owned()),
            (
                "GIT_COMMITTER_EMAIL".to_owned(),
                "bot@example.com".to_owned()
            ),
        ])
    );
}

/// A wrapper and a stand-in for the real `gh` that prints what it was given,
/// with the state directory the wrapper reads its token from moved under
/// `root`.
fn wrapper(root: &std::path::Path, reads_token: bool) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;

    let real = root.join("real");
    std::fs::create_dir_all(&real).unwrap();
    std::fs::create_dir_all(root.join("state")).unwrap();
    std::fs::write(
        real.join("gh"),
        "#!/bin/sh\necho \"ran $* with ${GH_TOKEN:-no token}\"\n",
    )
    .unwrap();
    let shim = root.join("gh");
    std::fs::write(
        &shim,
        gh_shim_contents(reads_token)
            .replace(STATE_PATH, &root.join("state").display().to_string()),
    )
    .unwrap();
    for path in [real.join("gh"), shim.clone()] {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    shim
}

/// Runs the wrapper with `args`, as status and what it said.
fn run(root: &std::path::Path, shim: &std::path::Path, args: &[&str]) -> (i32, String) {
    let ran = std::process::Command::new("/bin/sh")
        .arg(shim)
        .args(args)
        .env_clear()
        .env("PATH", root.join("real"))
        .output()
        .unwrap();
    (
        ran.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&ran.stdout).into_owned() + &String::from_utf8_lossy(&ran.stderr),
    )
}

/// Every call the wrapper refuses, and what its refusal says.
const REFUSED: &[(&[&str], &str)] = &[
    (&["pr", "create", "--title", "x"], REQUEST_FILENAME),
    (&["repo", "fork"], REQUEST_FILENAME),
    (&["repo", "delete", "o/r", "--yes"], "gh repo delete"),
    (&["-R", "o/r", "repo", "delete"], "gh repo delete"),
    (&["--repo=o/r", "secret", "set", "X"], "gh secret set"),
    (&["auth", "token"], "not to print"),
    (&["api", "-X", "DELETE", "repos/o/r"], "gh api -X DELETE"),
    (&["api", "--method=patch", "repos/o/r"], "gh api -X patch"),
    (&["api", "-XPUT", "repos/o/r"], "gh api -X PUT"),
    (
        &["api", "--hostname", "evil.example", "user"],
        "gh api for evil.example",
    ),
    (&["-t", "x", "pr", "create", "-b", "y"], REQUEST_FILENAME),
    (
        &["--jq", ".", "api", "-X", "DELETE", "/user"],
        "gh api -X DELETE",
    ),
    (&["api", "-X=DELETE", "/repos/o/r"], "gh api -X DELETE"),
    (&["api", "--method", "put", "/repos/o/r"], "gh api -X put"),
    (&["api", "-iX", "DELETE", "repos/o/r"], "gh api -X DELETE"),
    (&["api", "-iXDELETE", "repos/o/r"], "gh api -X DELETE"),
    (
        &["api", "-X", "POST", "/repos/o/r/transfer"],
        "/repos/o/r/transfer",
    ),
    (
        &["api", "-X", "POST", "repos/o/r/forks?x=1"],
        "repos/o/r/forks",
    ),
    (&["api", "repos/o/r/forks/"], "repos/o/r/forks"),
    (
        &["api", "-f", "new_owner=me", "repos/o/r/transfer?"],
        "repos/o/r/transfer",
    ),
    (&["alias", "set", "rd", "repo delete"], "gh alias set"),
    (&["pr", "-R", "o/r", "create"], REQUEST_FILENAME),
    (&["pr", "new"], REQUEST_FILENAME),
    (&["repo", "--yes", "delete", "o/r"], "gh repo delete"),
    (&["secret", "remove", "X"], "gh secret remove"),
    (&["variable", "remove", "X"], "gh variable remove"),
    (&["auth", "status", "--show-token"], "not to print"),
    (&["auth", "status", "--show-token=true"], "not to print"),
    (&["auth", "status", "-t"], "not to print"),
    (&["auth", "status", "-at"], "not to print"),
    (&["auth", "status", "-ta"], "not to print"),
    (&["auth", "status", "-ti"], "not to print"),
    (&["repo", "transfer", "o/r", "other"], "gh repo transfer"),
    (&["repo", "archive", "o/r"], "gh repo archive"),
    (&["release", "delete", "v1"], "gh release delete"),
    (&["workflow", "disable", "ci"], "gh workflow disable"),
    (&["secret", "delete", "X"], "gh secret delete"),
    (&["variable", "set", "X"], "gh variable set"),
    (&["variable", "delete", "X"], "gh variable delete"),
    (&["alias", "import", "aliases.yml"], "gh alias import"),
];

/// The verbs a session is not to run are refused with what to do instead,
/// however the global flags are placed, and everything else reaches the real
/// `gh`.
#[test]
fn the_gh_wrapper_refuses_what_a_session_is_not_to_do_and_passes_the_rest() {
    let root = tempfile::tempdir().unwrap();
    let shim = wrapper(root.path(), false);

    for &(args, says) in REFUSED {
        let (status, said) = run(root.path(), &shim, args);
        assert_eq!(status, 1, "{args:?}: {said}");
        assert!(said.contains(says), "{args:?}: {said}");
        assert!(!said.contains("ran "), "{args:?} reached gh: {said}");
    }

    for args in [
        &["pr", "view", "1"][..],
        &["-R", "o/r", "issue", "list"][..],
        &["api", "repos/o/r/issues", "-f", "title=x"][..],
        &["repo", "view"][..],
        &[
            "issue",
            "create",
            "--title",
            "repo delete",
            "--body",
            "pr create",
        ][..],
        &["auth", "git-credential", "get"][..],
        &["auth", "status"][..],
        &["pr", "create-not", "-t", "x"][..],
        &["issue", "create", "-t", "title"][..],
    ] {
        let (status, said) = run(root.path(), &shim, args);
        assert_eq!(status, 0, "{args:?}: {said}");
        assert!(
            said.starts_with(&format!("ran {}", args.join(" "))),
            "{said}"
        );
    }
}

/// With a token the daemon renews, the wrapper hands `gh` whatever the file
/// holds at the time of the call, and nothing when there is no file.
#[test]
fn the_wrapper_presents_the_token_the_file_holds_now() {
    let root = tempfile::tempdir().unwrap();
    let shim = wrapper(root.path(), true);
    let token = root.path().join("state").join(super::app::TOKEN_FILENAME);

    assert!(
        run(root.path(), &shim, &["pr", "view"])
            .1
            .contains("no token")
    );
    std::fs::write(&token, "ghs_first\n").unwrap();
    assert!(
        run(root.path(), &shim, &["pr", "view"])
            .1
            .contains("with ghs_first")
    );
    std::fs::write(&token, "ghs_renewed").unwrap();
    assert!(
        run(root.path(), &shim, &["pr", "view"])
            .1
            .contains("with ghs_renewed")
    );
}

/// The account that matches a chat name on GitHub belongs to somebody who
/// asked for nothing. This has notified the wrong person once already.
#[test]
fn the_footer_names_who_asked_and_mentions_nobody() {
    let footer = attribution_footer(
        "amelia",
        &SessionLinks {
            thread: Some("https://discord.com/channels/1/2".to_owned()),
            transcript: Some("https://errand.example/?session=abc".to_owned()),
        },
    );

    assert!(footer.contains("Requested by amelia via errand."));
    assert!(footer.contains("https://discord.com/channels/1/2"));
    assert!(!footer.contains('@'));
}

#[test]
fn a_footer_with_nowhere_to_point_at_is_still_an_attribution() {
    assert_eq!(
        attribution_footer("amelia", &SessionLinks::default()),
        "Requested by amelia via errand."
    );
}

#[test]
fn the_links_are_the_addresses_a_reader_can_open() {
    assert_eq!(
        thread_link("111", "222"),
        "https://discord.com/channels/111/222"
    );
    assert_eq!(
        transcript_link("https://errand.example/", "s 1"),
        "https://errand.example/?session=s%201"
    );
}

#[test]
fn the_instructions_carry_the_exact_footer_for_the_agent_to_copy() {
    let instructions = review_instructions(
        &github(),
        "amelia",
        &SessionLinks {
            thread: Some("https://t/1".to_owned()),
            transcript: None,
        },
    );

    assert!(instructions.contains(&attribution_footer(
        "amelia",
        &SessionLinks {
            thread: Some("https://t/1".to_owned()),
            transcript: None,
        }
    )));
    assert!(instructions.contains("Open nothing unless you were asked to"));
    assert!(instructions.contains("errand-bot <bot@example.com>"));
}

/// Announcing a pull request before the daemon opens it has happened.
#[test]
fn the_instructions_say_the_request_is_not_the_result() {
    let instructions = review_instructions(&github(), "amelia", &SessionLinks::default());

    assert!(instructions.contains("a request, not the result"));
    assert!(instructions.contains("do not say a pull request"));
    assert!(instructions.contains("Never run `gh pr create`"));
}

/// A host named through the environment is refused as one named by flag is.
#[test]
fn the_wrapper_refuses_another_host_named_in_the_environment() {
    let root = tempfile::tempdir().unwrap();
    let shim = wrapper(root.path(), false);
    let ran = std::process::Command::new("/bin/sh")
        .arg(&shim)
        .args(["api", "/user"])
        .env_clear()
        .env("PATH", root.path().join("real"))
        .env("GH_HOST", "evil.example")
        .output()
        .unwrap();

    assert_eq!(ran.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&ran.stderr).contains("gh for evil.example"));
}
