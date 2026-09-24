//! Tests for validation, ported from `validate_test.ts`.

use serde_json::{Value, json};

use super::validate_config;
use crate::config::schema::EgressMode;
use crate::config::schema::{ConfigError, defaults};

fn valid(overrides: Value) -> Value {
    let mut base = json!({
        "chat": {
            "token": "a.token.value",
            "channelId": "111222333444555666",
            "allowedUserIds": ["777888999000111222"],
        },
        "agent": {
            "provider": "anthropic",
            "providers": {
                "anthropic": { "credentialName": "ANTHROPIC_API_KEY", "credential": "secret-value" },
            },
        },
        "projectRoot": "/tmp/errand/projects",
        "stateDir": "/tmp/errand/state",
    });
    let Value::Object(fields) = &mut base else {
        unreachable!()
    };
    let Value::Object(overrides) = overrides else {
        unreachable!()
    };
    for (key, value) in overrides {
        fields.insert(key, value);
    }
    base
}

fn problems_of(raw: &Value) -> Vec<String> {
    match validate_config(raw) {
        Err(ConfigError { problems }) => problems,
        Ok(_) => panic!("the configuration was expected to be refused"),
    }
}

fn problems_contain(raw: &Value, needle: &str) -> bool {
    problems_of(raw)
        .iter()
        .any(|problem| problem.contains(needle))
}

#[test]
fn a_minimal_file_resolves_with_the_documented_defaults_filled_in() {
    let config = validate_config(&valid(json!({}))).expect("a minimal file resolves");

    assert_eq!(config.chat.channel_id, "111222333444555666");
    assert_eq!(config.agent.model, None);
    assert_eq!(config.sandbox.backend, defaults::BACKEND);
    assert_eq!(
        config.limits.max_concurrent_turns,
        defaults::MAX_CONCURRENT_TURNS
    );
    assert_eq!(config.timeouts.idle_ms, defaults::IDLE_MS);
    assert_eq!(config.chat.blocked_user_ids, Vec::<String>::new());
}

#[test]
fn every_problem_is_reported_not_only_the_first() {
    let problems = problems_of(&json!({ "chat": {}, "agent": {} }));

    assert!(problems.len() > 3);
    let said = problems.join("\n");
    assert!(said.contains("chat.token"));
    assert!(said.contains("agent.provider"));
    assert!(said.contains("projectRoot"));
}

/// A setting that takes no effect is worse than one that is rejected: the
/// daemon then runs without a guarantee somebody believes they configured.
#[test]
fn a_misspelled_setting_is_refused_rather_than_ignored() {
    assert!(problems_contain(
        &valid(json!({ "sandbox": { "requireFullEnforcment": false } })),
        "sandbox.requireFullEnforcment is not a setting"
    ));

    assert!(problems_contain(
        &valid(json!({ "projectRooot": "/tmp/x" })),
        "config.projectRooot is not a setting"
    ));
}

#[test]
fn an_empty_allowlist_refuses_to_start_rather_than_admitting_everyone() {
    assert!(problems_contain(
        &valid(json!({ "chat": { "token": "t", "channelId": "c", "allowedUserIds": [] } })),
        "allow-everyone"
    ));
}

#[test]
fn paths_must_be_absolute_and_neither_may_sit_inside_the_other() {
    assert!(problems_contain(
        &valid(json!({ "projectRoot": "./projects" })),
        "projectRoot must be an absolute path"
    ));
    assert!(problems_contain(
        &valid(json!({ "projectRoot": "/tmp/same", "stateDir": "/tmp/same" })),
        "must be separate directories"
    ));
    // The record directory is `{stateDir}.record`, kept out of the agent's
    // write grant on purpose. State inside the project puts it back in.
    assert!(problems_contain(
        &valid(json!({ "projectRoot": "/tmp/work", "stateDir": "/tmp/work/state" })),
        "must be separate directories"
    ));
    // The other way round is no better: the project would be inside the
    // state directory a session is also given.
    assert!(problems_contain(
        &valid(json!({ "projectRoot": "/tmp/state/projects", "stateDir": "/tmp/state" })),
        "must be separate directories"
    ));
    // A sibling whose name merely starts with the other's is fine.
    assert!(
        validate_config(&valid(
            json!({ "projectRoot": "/tmp/work", "stateDir": "/tmp/workspace" })
        ))
        .is_ok(),
        "a sibling is not a nesting"
    );
}

#[test]
fn sizes_and_counts_are_checked_so_a_typo_cannot_become_a_limit() {
    let problems = problems_of(&valid(json!({
        "sandbox": { "memory": "four gigs", "cpus": 0 },
        "limits": { "maxQueueLength": -3 },
    })));

    let said = problems.join("\n");
    assert!(said.contains("sandbox.memory must be a size"));
    assert!(said.contains("sandbox.cpus must be a number greater than zero"));
    assert!(said.contains("limits.maxQueueLength"));
}

#[test]
fn a_backend_that_does_not_exist_is_named_with_the_ones_that_do() {
    assert!(problems_contain(
        &valid(json!({ "sandbox": { "backend": "docker" } })),
        "sandbox.backend must be one of podman, bailey"
    ));
}

#[test]
fn anything_that_is_not_an_object_is_refused_with_one_clear_reason() {
    assert_eq!(
        problems_of(&json!([1, 2, 3])),
        vec!["the configuration file must contain a JSON object"]
    );
    assert_eq!(
        problems_of(&json!("nope")),
        vec!["the configuration file must contain a JSON object"]
    );
}

/// Inert under bailey, required under podman, and validated the same either way.
#[test]
fn the_container_image_defaults_and_is_refused_when_it_is_not_a_name() {
    let config = validate_config(&valid(json!({}))).expect("resolves");
    assert_eq!(config.sandbox.image, defaults::IMAGE);

    let config = validate_config(&valid(
        json!({ "sandbox": { "image": "localhost/mine:v2" } }),
    ))
    .expect("resolves");
    assert_eq!(config.sandbox.image, "localhost/mine:v2");

    assert!(problems_contain(
        &valid(json!({ "sandbox": { "image": 7 } })),
        "sandbox.image must be a non-empty string"
    ));
}

#[test]
fn a_daemon_with_no_github_identity_is_configured_not_broken() {
    let config = validate_config(&valid(json!({}))).expect("resolves");
    assert_eq!(config.github, None);
}

#[test]
fn a_github_identity_is_taken_whole() {
    let github = validate_config(&valid(json!({
        "github": { "token": "ghp-value", "userName": "errand-bot", "userEmail": "bot@example.com" },
    })))
    .expect("resolves")
    .github;

    assert_eq!(
        github.as_ref().map(|github| github.user_name.as_str()),
        Some("errand-bot")
    );
    assert_eq!(
        github.as_ref().map(|github| github.token.as_str()),
        Some("ghp-value")
    );
}

/// Pushing as half an identity is worse than not being able to push.
#[test]
fn a_github_section_missing_a_field_is_refused_not_half_filled() {
    let problems = problems_of(&valid(json!({ "github": { "token": "ghp-value" } }))).join("\n");

    assert!(problems.contains("github.userName is required"));
    assert!(problems.contains("github.userEmail is required"));
}

#[test]
fn a_misspelled_github_setting_is_refused_like_any_other() {
    assert!(problems_contain(
        &valid(json!({
            "github": { "token": "t", "userName": "n", "userEmail": "e", "userNmae": "typo" },
        })),
        "github.userNmae is not a setting"
    ));
}

#[test]
fn what_reaches_a_thread_has_documented_defaults() {
    let config = validate_config(&valid(json!({}))).expect("resolves");

    assert_eq!(
        config.output.forward_tool_output,
        defaults::FORWARD_TOOL_OUTPUT
    );
    assert_eq!(
        config.output.max_tool_output_chars,
        defaults::MAX_TOOL_OUTPUT_CHARS
    );
    assert_eq!(
        config.output.max_attachment_bytes,
        defaults::MAX_ATTACHMENT_BYTES
    );
    assert_eq!(
        config.output.max_attachments_per_message,
        defaults::MAX_ATTACHMENTS_PER_MESSAGE
    );
    assert_eq!(config.output.post_diffs, defaults::POST_DIFFS);
}

#[test]
fn what_reaches_a_thread_can_be_turned_up_or_down() {
    let config = validate_config(&valid(json!({
        "output": { "forwardToolOutput": true, "postDiffs": false, "maxToolOutputChars": 4000 },
    })))
    .expect("resolves");

    assert!(config.output.forward_tool_output);
    assert!(!config.output.post_diffs);
    assert_eq!(config.output.max_tool_output_chars, 4000);
    assert_eq!(
        config.output.max_attachments_per_message,
        defaults::MAX_ATTACHMENTS_PER_MESSAGE
    );
}

#[test]
fn an_output_limit_that_is_not_a_number_is_refused() {
    let problems = problems_of(&valid(json!({
        "output": { "maxAttachmentBytes": "5mb", "postDiffs": "yes" },
    })))
    .join("\n");

    assert!(problems.contains("output.maxAttachmentBytes must be a number"));
    assert!(problems.contains("output.postDiffs must be true or false"));
}

/// Silence about a command that turns the machine off means nobody.
#[test]
fn nobody_may_power_off_the_host_unless_somebody_is_named() {
    let config = validate_config(&valid(json!({}))).expect("resolves");
    assert_eq!(config.shutdown.allowed_user_ids, Vec::<String>::new());

    let config = validate_config(&valid(json!({ "shutdown": { "allowedUserIds": ["777"] } })))
        .expect("resolves");
    assert_eq!(config.shutdown.allowed_user_ids, vec!["777"]);
}

#[test]
fn a_shutdown_list_that_is_not_a_list_of_accounts_is_refused() {
    assert!(problems_contain(
        &valid(json!({ "shutdown": { "allowedUserIds": "everyone" } })),
        "shutdown.allowedUserIds must be a list of account ids"
    ));
}

#[test]
fn a_daemon_with_no_interface_configured_serves_none() {
    let config = validate_config(&valid(json!({}))).expect("resolves");
    assert_eq!(config.web, None);
}

#[test]
fn an_interface_takes_its_address_port_and_role() {
    let web = validate_config(&valid(json!({
        "web": { "host": "100.64.0.2", "port": 9000, "observer": true,
                 "publicUrl": "https://errand.example" },
    })))
    .expect("resolves")
    .web;

    assert_eq!(
        web.as_ref().map(|web| web.host.as_str()),
        Some("100.64.0.2")
    );
    assert_eq!(web.as_ref().map(|web| web.port), Some(9000));
    assert_eq!(web.as_ref().map(|web| web.observer), Some(true));
    assert_eq!(
        web.and_then(|web| web.public_url),
        Some("https://errand.example".to_owned())
    );
}

#[test]
fn an_interface_that_says_only_that_it_exists_gets_the_defaults() {
    let web = validate_config(&valid(json!({ "web": {} })))
        .expect("resolves")
        .web;

    assert_eq!(
        web.as_ref().map(|web| web.host.as_str()),
        Some(defaults::WEB_HOST)
    );
    assert_eq!(web.as_ref().map(|web| web.port), Some(defaults::WEB_PORT));
    assert_eq!(web.as_ref().map(|web| web.observer), Some(false));
    assert_eq!(web.and_then(|web| web.public_url), None);
}

#[test]
fn an_interface_port_that_is_not_a_port_is_refused() {
    assert!(problems_contain(
        &valid(json!({ "web": { "port": "8080" } })),
        "web.port must be a number greater than zero"
    ));
}

#[test]
fn no_extra_grant_is_the_same_as_no_policy_extra_section() {
    let config = validate_config(&valid(json!({}))).expect("resolves");
    assert_eq!(config.sandbox.policy_extra, None);
}

#[test]
fn extra_grants_are_taken_as_absolute_paths() {
    let extra = validate_config(&valid(json!({
        "sandbox": {
            "policyExtra": {
                "read": ["/opt/toolchains"],
                "write": ["/srv/output"],
                "execute": ["/opt/toolchains/bin"],
            },
        },
    })))
    .expect("resolves")
    .sandbox
    .policy_extra;

    assert_eq!(
        extra.as_ref().map(|extra| extra.read.as_slice()),
        Some(["/opt/toolchains".to_owned()].as_slice())
    );
    assert_eq!(
        extra.as_ref().map(|extra| extra.write.as_slice()),
        Some(["/srv/output".to_owned()].as_slice())
    );
    assert_eq!(
        extra.as_ref().map(|extra| extra.execute.as_slice()),
        Some(["/opt/toolchains/bin".to_owned()].as_slice())
    );
}

/// There is no working directory to resolve one against after the pivot.
#[test]
fn a_relative_path_in_a_grant_is_refused_not_resolved() {
    assert!(problems_contain(
        &valid(json!({ "sandbox": { "policyExtra": { "read": ["./shared", "/fine"] } } })),
        "must be an absolute path"
    ));
}

/// A section that grants nothing is a mistake worth naming.
#[test]
fn an_empty_grant_is_refused_rather_than_silently_doing_nothing() {
    assert!(problems_contain(
        &valid(json!({ "sandbox": { "policyExtra": {} } })),
        "grants nothing"
    ));
}

#[test]
fn a_misspelled_grant_list_is_refused_like_any_other_setting() {
    assert!(problems_contain(
        &valid(json!({ "sandbox": { "policyExtra": { "reed": ["/opt"] } } })),
        "sandbox.policyExtra.reed is not a setting"
    ));
}

#[test]
fn no_environment_section_is_the_same_as_no_variables() {
    let config = validate_config(&valid(json!({}))).expect("resolves");
    assert_eq!(config.sandbox.env, None);
}

#[test]
fn variables_named_in_configuration_are_read() {
    let env = validate_config(&valid(json!({
        "sandbox": { "env": { "CARGO_HOME": "/var/cache/cargo", "RUSTUP_HOME": "/opt/rustup" } },
    })))
    .expect("resolves")
    .sandbox
    .env;

    let env = env.expect("an environment was configured");
    assert_eq!(
        env.get("CARGO_HOME").map(String::as_str),
        Some("/var/cache/cargo")
    );
    assert_eq!(
        env.get("RUSTUP_HOME").map(String::as_str),
        Some("/opt/rustup")
    );
    assert_eq!(env.len(), 2);
}

/// The policy sets both, to paths it places.
#[test]
fn a_variable_the_policy_sets_itself_is_refused() {
    assert!(problems_contain(
        &valid(json!({ "sandbox": { "env": { "HOME": "/somewhere" } } })),
        "must not set HOME"
    ));
}

/// Shadowing it would authenticate the agent with whatever was set here.
#[test]
fn the_variable_carrying_the_credential_is_refused() {
    assert!(problems_contain(
        &valid(json!({ "sandbox": { "env": { "ANTHROPIC_API_KEY": "not-the-real-one" } } })),
        "carries a provider credential"
    ));
}

#[test]
fn a_name_no_shell_would_accept_is_refused() {
    assert!(problems_contain(
        &valid(json!({ "sandbox": { "env": { "CARGO HOME": "/var/cache" } } })),
        "is not a variable name"
    ));
}

#[test]
fn a_value_that_is_not_a_string_is_refused() {
    assert!(problems_contain(
        &valid(json!({ "sandbox": { "env": { "CARGO_HOME": 7 } } })),
        "must be a string"
    ));
}

#[test]
fn an_empty_environment_is_refused_rather_than_silently_doing_nothing() {
    assert!(problems_contain(
        &valid(json!({ "sandbox": { "env": {} } })),
        "names nothing"
    ));
}

#[test]
fn directories_added_to_the_path_are_taken_as_absolute_paths() {
    let path_extra = validate_config(&valid(json!({
        "sandbox": { "pathExtra": ["/opt/toolchains/bin"] },
    })))
    .expect("resolves")
    .sandbox
    .path_extra;

    assert_eq!(path_extra, Some(vec!["/opt/toolchains/bin".to_owned()]));
}

#[test]
fn a_relative_directory_on_the_path_is_refused() {
    assert!(problems_contain(
        &valid(json!({ "sandbox": { "pathExtra": ["bin"] } })),
        "must be an absolute path"
    ));
}

#[test]
fn the_egress_ports_default_to_https_alone() {
    let config = validate_config(&valid(json!({}))).expect("resolves");
    assert_eq!(config.sandbox.egress_ports, vec![443]);
}

#[test]
fn configured_egress_ports_are_read_in_order() {
    let ports = validate_config(&valid(json!({ "sandbox": { "egressPorts": [80, 443] } })))
        .expect("resolves")
        .sandbox
        .egress_ports;

    assert_eq!(ports, vec![80, 443]);
}

#[test]
fn a_port_outside_the_socket_range_is_refused() {
    assert!(problems_contain(
        &valid(json!({ "sandbox": { "egressPorts": [80, 70000] } })),
        "not a port between 1 and 65535"
    ));
}

#[test]
fn an_empty_egress_list_is_refused_rather_than_silencing_the_network() {
    assert!(problems_contain(
        &valid(json!({ "sandbox": { "egressPorts": [] } })),
        "names no port"
    ));
}

#[test]
fn the_host_address_is_shown_to_a_session_by_default() {
    let config = validate_config(&valid(json!({}))).expect("resolves");
    assert!(!config.sandbox.hide_host_address);
}

#[test]
fn hiding_the_host_address_is_read_as_a_flag() {
    let hidden = validate_config(&valid(json!({ "sandbox": { "hideHostAddress": true } })))
        .expect("resolves")
        .sandbox
        .hide_host_address;

    assert!(hidden);
}

#[test]
fn an_absolute_rules_path_is_kept_and_absent_stays_absent() {
    let with_rules = validate_config(&valid(json!({
        "agent": {
            "provider": "anthropic",
            "providers": {
                "anthropic": { "credentialName": "ANTHROPIC_API_KEY", "credential": "secret-value" },
            },
            "rulesPath": "/etc/errand/AGENTS.md",
        },
    })))
    .expect("resolves");
    assert_eq!(
        with_rules.agent.rules_path,
        Some("/etc/errand/AGENTS.md".to_owned())
    );

    let without = validate_config(&valid(json!({}))).expect("resolves");
    assert_eq!(without.agent.rules_path, None);
}

#[test]
fn a_relative_rules_path_is_refused_rather_than_resolved_against_the_daemons_cwd() {
    // Resolving it would make the same configuration name a different file
    // depending on where the daemon happened to be started from.
    assert!(problems_contain(
        &valid(json!({
            "agent": {
                "provider": "anthropic",
                "providers": {
                    "anthropic": { "credentialName": "ANTHROPIC_API_KEY", "credential": "secret-value" },
                },
                "rulesPath": "AGENTS.md",
            },
        })),
        "agent.rulesPath must be an absolute path, got AGENTS.md"
    ));
}

#[test]
fn a_rules_path_that_is_not_a_path_at_all_is_refused() {
    assert!(problems_contain(
        &valid(json!({
            "agent": {
                "provider": "anthropic",
                "providers": {
                    "anthropic": { "credentialName": "ANTHROPIC_API_KEY", "credential": "secret-value" },
                },
                "rulesPath": "",
            },
        })),
        "agent.rulesPath must be an absolute path"
    ));
}

/// Brokered by default: it is what keeps the provider credential out of a
/// sandbox, and a lone `*` restricts no host, so what it costs is the audit
/// and the port, not reach.
#[test]
fn egress_defaults_to_a_brokered_pass_through() {
    let config = validate_config(&valid(json!({}))).expect("resolves");
    assert_eq!(config.sandbox.egress.mode, EgressMode::Proxy);
    assert_eq!(config.sandbox.egress.allow, vec!["*"]);
}

#[test]
fn proxy_mode_with_a_hostname_allowlist_is_accepted_and_lower_cased() {
    let egress = validate_config(&valid(json!({
        "sandbox": {
            "egress": {
                "mode": "proxy",
                "allow": ["GitHub.com", "*.githubusercontent.com"],
                "allowInternal": false,
            },
        },
    })))
    .expect("resolves")
    .sandbox
    .egress;

    assert_eq!(
        egress.allow,
        vec![
            "github.com".to_owned(),
            "*.githubusercontent.com".to_owned()
        ]
    );
}

#[test]
fn a_lone_star_catch_all_is_accepted_as_an_allowlist_entry() {
    let egress = validate_config(&valid(json!({
        "sandbox": { "egress": { "mode": "proxy", "allow": ["*"], "allowInternal": false } },
    })))
    .expect("resolves")
    .sandbox
    .egress;

    assert_eq!(egress.allow, vec!["*".to_owned()]);
}

#[test]
fn an_unknown_egress_mode_is_refused() {
    assert!(problems_contain(
        &valid(json!({ "sandbox": { "egress": { "mode": "wideopen" } } })),
        "sandbox.egress.mode must be one of open, proxy"
    ));
}

#[test]
fn a_malformed_allowlist_entry_is_refused_rather_than_handed_to_the_broker() {
    // A broker told to permit "http://x" or "x/y" either permits nothing or
    // more than was meant, so a non-hostname is a configuration error.
    assert!(problems_contain(
        &valid(json!({ "sandbox": { "egress": { "allow": ["http://evil.com/x"] } } })),
        "is not a hostname"
    ));
}

#[test]
fn an_unknown_key_under_egress_is_refused() {
    assert!(problems_contain(
        &valid(json!({ "sandbox": { "egress": { "mode": "proxy", "allowlist": [] } } })),
        "sandbox.egress"
    ));
}

#[test]
fn provider_definitions_are_passed_through_and_their_shape_is_checked() {
    let config = validate_config(&valid(json!({
        "agent": {
            "provider": "meta",
            "providers": {
                "meta": {
                    "credentialName": "META_API_KEY",
                    "credential": "k",
                    "baseUrl": "https://api.meta.example/v1",
                    "api": "openai-completions",
                    "models": [{ "id": "muse-spark-1.3-contributor", "reasoning": true }],
                },
            },
        },
    })))
    .expect("resolves");

    // Handed to the agent as written, so a field this does not know is kept.
    let meta = config
        .agent
        .providers
        .get("meta")
        .expect("the meta provider");
    assert_eq!(
        meta.get("api").and_then(Value::as_str),
        Some("openai-completions")
    );
    assert_eq!(
        meta.get("models").and_then(Value::as_array).map(Vec::len),
        Some(1)
    );
}

#[test]
fn a_provider_definition_that_is_not_an_object_is_refused() {
    let problems = problems_of(&valid(json!({
        "agent": {
            "provider": "p",
            "providers": {
                "p": { "credentialName": "K", "credential": "k" },
                "meta": "https://api.meta.example/v1",
            },
        },
    })))
    .join("\n");

    assert!(problems.contains("agent.providers.meta"));
}

/// The credential moved into the provider it belongs to. An operator meeting
/// this has a working configuration in front of them, so the refusal names
/// the block to write rather than calling the key unknown.
#[test]
fn a_credential_at_the_old_place_is_refused_with_the_new_one() {
    let problems = problems_of(&valid(json!({
        "agent": {
            "provider": "anthropic",
            "credentialName": "ANTHROPIC_API_KEY",
            "credential": "secret",
            "providers": { "anthropic": { "credential": "secret" } },
        },
    })));

    let said = problems.join("\n");
    assert!(
        said.contains("agent.providers.anthropic.credential instead"),
        "{said}"
    );
    assert!(
        said.contains("agent.providers.anthropic.credentialName instead"),
        "{said}"
    );
}

/// A session starting on a provider nothing describes cannot reach a model,
/// which is worth saying before a sandbox is built rather than after.
#[test]
fn a_starting_provider_nothing_describes_is_refused() {
    assert!(problems_contain(
        &valid(json!({
            "agent": {
                "provider": "somewhere-else",
                "providers": { "anthropic": { "credential": "secret" } },
            },
        })),
        "agent.provider is somewhere-else but agent.providers describes only anthropic"
    ));
}

/// Every provider's variable is checked, not just the one a session starts on.
#[test]
fn a_shadowed_credential_variable_is_refused_for_any_provider() {
    assert!(problems_contain(
        &valid(json!({
            "agent": {
                "provider": "anthropic",
                "providers": {
                    "anthropic": { "credentialName": "ANTHROPIC_API_KEY", "credential": "a" },
                    "meta": { "credentialName": "META_API_KEY", "credential": "b" },
                },
            },
            "sandbox": { "env": { "META_API_KEY": "not-the-real-one" } },
        })),
        "sandbox.env must not set META_API_KEY"
    ));
}

/// A provider a pi extension registers needs no credential: it is anonymous or
/// carries its own, and errand does not reach it. So a session may start on
/// one without the credential an ordinary starting provider requires.
#[test]
fn an_extension_provider_starts_without_a_credential() {
    let resolved = validate_config(&valid(json!({
        "agent": {
            "provider": "free-models",
            "model": "free-fast",
            "extensions": ["/home/somebody/.pi/extensions/free-models"],
            "providers": {
                "free-models": {
                    "extension": true,
                    "models": [{ "id": "free-fast" }],
                },
            },
        },
    })));

    let config = resolved.expect("an extension provider needs no credential");
    assert_eq!(config.agent.provider, "free-models");
    assert!(config.agent.is_extension_provider("free-models"));
    assert_eq!(
        config.agent.extensions,
        ["/home/somebody/.pi/extensions/free-models"]
    );
}

/// An ordinary starting provider still must carry a credential; the relaxation
/// is only for the extension case.
#[test]
fn an_ordinary_starting_provider_still_needs_its_credential() {
    assert!(problems_contain(
        &valid(json!({
            "agent": {
                "provider": "bare",
                "providers": { "bare": { "baseUrl": "https://api.example/v1" } },
            },
        })),
        "agent.providers.bare.credential is required"
    ));
}

/// Asking a provider needs somewhere to ask and a key to ask with, and a
/// misshapen `discover` or `usage` is refused rather than quietly skipped.
#[test]
fn asking_a_provider_needs_a_base_url_and_a_key() {
    let raw = valid(json!({
        "agent": {
            "provider": "anthropic",
            "providers": {
                "anthropic": { "credential": "secret-value" },
                "keyless": { "baseUrl": "https://keyless.example/v1", "usage": "gateway" },
                "nowhere": { "credential": "k", "usage": "gateway", "discover": true },
                "zai": { "credential": "k", "usage": "zai" },
                "odd": { "baseUrl": "https://odd.example/v1", "credential": "k", "discover": 1 },
            },
        },
    }));
    let problems = problems_of(&raw);

    assert!(
        problems
            .contains(&"agent.providers.keyless.usage needs a credential to ask with".to_owned())
    );
    assert!(
        problems.contains(&"agent.providers.nowhere.usage needs a baseUrl to ask under".to_owned())
    );
    assert!(
        problems
            .contains(&"agent.providers.nowhere.discover needs a baseUrl to ask under".to_owned())
    );
    assert!(
        problems
            .iter()
            .any(|problem| problem.starts_with("agent.providers.odd.discover"))
    );
    assert!(
        !problems
            .iter()
            .any(|problem| problem.contains("providers.zai"))
    );
}
