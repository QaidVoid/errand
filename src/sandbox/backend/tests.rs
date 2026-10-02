//! Tests for the backend contract, ported from the agent-command and naming
//! assertions in `bailey_test.ts` and `podman_test.ts`.

use super::{
    AgentCommand, KAGE_ROLE, SANDBOX_NAME_PREFIX, agent_command, disk_tmp_dir, fresh_disk_tmp,
    sandbox_name,
};

#[test]
fn the_agent_is_started_with_provider_model_and_memory() {
    let command = agent_command(&AgentCommand {
        provider: "zai-coding-cn".to_owned(),
        model: Some("glm-5.3".to_owned()),
        system_text: Some("remember this".to_owned()),
    });
    let said = command.join(" ");

    assert_eq!(command[..2], ["kage", "rpc"]);
    assert!(said.contains("-m zai-coding-cn/glm-5.3"));
    assert!(said.contains("--system"));
    assert!(said.contains("remember this"));
    assert!(said.contains(KAGE_ROLE));
}

#[test]
fn a_level_is_split_off_the_model_and_memory_may_be_absent() {
    let command = agent_command(&AgentCommand {
        provider: "zai-coding-cn".to_owned(),
        model: Some("glm-5.3-flash:max".to_owned()),
        system_text: None,
    });
    let said = command.join(" ");

    assert!(said.contains("-m zai-coding-cn/glm-5.3-flash"));
    assert!(!said.contains("--system"));
    assert!(!said.contains("--model"));
}

#[test]
fn without_a_model_the_flag_is_absent_so_the_provider_default_stands() {
    let command = agent_command(&AgentCommand {
        provider: "zai-coding-cn".to_owned(),
        model: None,
        system_text: None,
    });

    assert_eq!(command, ["kage", "rpc"]);
}

#[test]
fn the_sandbox_is_named_after_the_session_it_belongs_to() {
    assert_eq!(sandbox_name("s-1"), format!("{SANDBOX_NAME_PREFIX}s-1"));
}

/// A launch after the scratch filled starts on an empty `/tmp`, and a link
/// the session planted there is removed without touching what it points at.
#[tokio::test]
async fn a_disk_tmp_starts_every_launch_empty() {
    let state = tempfile::tempdir().expect("a state directory");
    let outside = tempfile::tempdir().expect("a directory outside");
    let state_dir = state.path().display().to_string();
    let kept = outside.path().join("kept");
    std::fs::write(&kept, "kept").expect("written");

    fresh_disk_tmp(&state_dir).await.expect("made when missing");
    let tmp = std::path::PathBuf::from(disk_tmp_dir(&state_dir));
    std::fs::create_dir(tmp.join("build")).expect("a nested directory");
    std::fs::write(tmp.join("build").join("blob"), "spent").expect("written");
    std::os::unix::fs::symlink(outside.path(), tmp.join("hop")).expect("linked");

    fresh_disk_tmp(&state_dir).await.expect("emptied");
    assert_eq!(std::fs::read_dir(&tmp).expect("listed").count(), 0);
    assert_eq!(std::fs::read_to_string(&kept).expect("still there"), "kept");
}
