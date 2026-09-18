//! Tests for the backend contract, ported from the agent-command and naming
//! assertions in `bailey_test.ts` and `podman_test.ts`.

use super::{AgentCommand, SANDBOX_NAME_PREFIX, agent_command, placed_prompt_path, sandbox_name};

#[test]
fn the_agent_is_started_with_provider_model_and_session_directory() {
    let command = agent_command(&AgentCommand {
        session_dir: "/state/sessions".to_owned(),
        provider: "zai-coding-cn".to_owned(),
        model: Some("glm-5.3".to_owned()),
        system_prompt_path: None,
        resume: false,
    });
    let said = command.join(" ");

    assert_eq!(command[..3], ["pi", "--mode", "rpc"]);
    assert!(said.contains("--session-dir /state/sessions"));
    assert!(said.contains("--provider zai-coding-cn"));
    assert!(said.contains("--model glm-5.3"));
    assert!(!command.contains(&"--continue".to_owned()));
}

#[test]
fn a_system_prompt_is_named_where_the_agent_will_read_it() {
    let command = agent_command(&AgentCommand {
        session_dir: "/state/sessions".to_owned(),
        provider: "zai-coding-cn".to_owned(),
        model: None,
        system_prompt_path: Some("/state/memory.md".to_owned()),
        resume: false,
    });
    let said = command.join(" ");

    assert!(said.contains("--append-system-prompt /state/memory.md"));
    // Without a model the flag is absent, so the provider default stands.
    assert!(!said.contains("--model"));
}

#[test]
fn the_sandbox_is_named_after_the_session_it_belongs_to() {
    assert_eq!(sandbox_name("s-1"), format!("{SANDBOX_NAME_PREFIX}s-1"));
}

/// A host path is replaced by the placed one, so the agent never sees one.
#[test]
fn the_prompt_path_is_placed_and_ends_with_the_same_file() {
    let host_path = Some("/home/operator/.local/state/errand/s-1/memory.md".to_owned());
    assert_eq!(
        placed_prompt_path(host_path.as_ref()),
        Some("/state/memory.md".to_owned())
    );
    assert_eq!(placed_prompt_path(None), None);
}
