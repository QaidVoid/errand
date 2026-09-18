//! Tests for finding and reading the configuration, ported from
//! `load_test.ts`.

use std::io::{Error, ErrorKind};

use serde_json::{Value, json};

use super::{CONFIG_VARIABLE, Environment, config_candidates, config_path, load_config};

fn environment(entries: &[(&str, &str)]) -> Environment {
    entries
        .iter()
        .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
        .collect()
}

fn valid_text() -> String {
    json!({
        "chat": { "token": "t", "channelId": "c", "allowedUserIds": ["u"] },
        "agent": {
            "provider": "anthropic",
            "providers": {
                "anthropic": { "credentialName": "ANTHROPIC_API_KEY", "credential": "k" },
            },
        },
        "projectRoot": "/tmp/errand/projects",
        "stateDir": "/tmp/errand/state",
    })
    .to_string()
}

/// Naming the file outright wins, so nothing has to be guessed at.
#[test]
fn the_environment_names_the_file_and_nothing_else_is_consulted() {
    let named = environment(&[(CONFIG_VARIABLE, "/etc/errand.json")]);
    assert_eq!(config_path(&named, |_| false), "/etc/errand.json");

    let elsewhere = environment(&[(CONFIG_VARIABLE, "/somewhere.json")]);
    assert_eq!(
        config_candidates(&elsewhere),
        vec!["/somewhere.json".to_owned()]
    );
}

#[test]
fn a_blank_variable_is_not_a_path_so_the_search_runs() {
    let looked = config_candidates(&environment(&[
        (CONFIG_VARIABLE, "  "),
        ("HOME", "/home/amelia"),
    ]));

    assert_eq!(looked[0], "/home/amelia/.config/errand/config.json");
}

/// A person's own configuration comes first, so running the daemon by hand on
/// a host that also serves one does not pick up the service's token.
#[test]
fn it_looks_in_the_accounts_config_directory_then_the_systems() {
    assert_eq!(
        config_candidates(&environment(&[("HOME", "/home/amelia")])),
        vec![
            "/home/amelia/.config/errand/config.json",
            "/home/amelia/.config/errand/config.jsonc",
            "/etc/errand/config.json",
            "/etc/errand/config.jsonc",
            "config.json",
            "config.jsonc",
        ]
    );
}

#[test]
fn a_chosen_config_root_is_honoured() {
    let candidates = config_candidates(&environment(&[
        ("HOME", "/home/amelia"),
        ("XDG_CONFIG_HOME", "/home/amelia/cfg"),
    ]));

    assert_eq!(candidates[0], "/home/amelia/cfg/errand/config.json");
}

#[test]
fn the_first_one_that_is_actually_there_is_the_one_used() {
    let env = environment(&[("HOME", "/home/amelia")]);

    assert_eq!(
        config_path(&env, |path| path == "/etc/errand/config.json"),
        "/etc/errand/config.json"
    );
    assert_eq!(
        config_path(&env, |path| path == "config.json"),
        "config.json"
    );
    // With none of them there, the failure names the place most likely meant.
    assert_eq!(
        config_path(&env, |_| false),
        "/home/amelia/.config/errand/config.json"
    );
}

#[test]
fn a_valid_file_loads() {
    let config = load_config(
        "/anywhere",
        |_| Ok(valid_text()),
        &Environment::new(),
        |_| false,
    )
    .expect("a valid file loads");
    assert_eq!(config.chat.channel_id, "c");
}

/// Three different failures, so three different things to do about them.
#[test]
fn a_missing_file_says_where_it_looked_and_what_to_do() {
    let error = load_config(
        "/home/amelia/.config/errand/config.json",
        |_| Err(Error::new(ErrorKind::NotFound, "nope")),
        &environment(&[("HOME", "/home/amelia")]),
        |_| false,
    )
    .expect_err("a missing file is refused");

    let said = error.problems.join("\n");
    assert!(said.contains("/home/amelia/.config/errand/config.json"));
    assert!(said.contains("/etc/errand/config.json"));
    assert!(said.contains("ERRAND_CONFIG"));
}

#[test]
fn a_file_that_is_not_json_is_not_reported_as_a_field_problem() {
    let error = load_config(
        "/c.json",
        |_| Ok("{ nope".to_owned()),
        &Environment::new(),
        |_| false,
    )
    .expect_err("a broken file is refused");

    assert!(error.to_string().contains("not valid JSON"));
}

#[test]
fn comments_and_trailing_commas_are_read_so_a_config_can_be_annotated() {
    let jsonc = r#"{
        // who drives the bot
        "chat": { "token": "t", "channelId": "c", "allowedUserIds": ["u"] },
        "agent": {
            "provider": "anthropic",
            "providers": { "anthropic": { "credentialName": "K", "credential": "k" } },
        },
        "projectRoot": "/tmp/errand/projects",
        "stateDir": "/tmp/errand/state", // note the trailing comma
    }"#;
    let config = load_config(
        "/c.jsonc",
        |_| Ok(jsonc.to_owned()),
        &Environment::new(),
        |_| false,
    )
    .expect("a JSONC file loads");
    assert_eq!(config.chat.channel_id, "c");
}

#[test]
fn a_file_that_parses_but_says_something_impossible_lists_every_reason() {
    let error = load_config(
        "/c.json",
        |_| Ok("{}".to_owned()),
        &Environment::new(),
        |_| false,
    )
    .expect_err("an empty file is refused");

    assert!(error.problems.len() > 3);
}

#[test]
fn house_rules_beside_the_configuration_are_picked_up_without_being_named() {
    let config = load_config(
        "/etc/errand/config.json",
        |_| Ok(valid_text()),
        &Environment::new(),
        |path| path == "/etc/errand/AGENTS.md",
    )
    .expect("resolves");

    assert_eq!(
        config.agent.rules_path,
        Some("/etc/errand/AGENTS.md".to_owned())
    );
}

#[test]
fn the_default_follows_the_configuration_that_was_loaded_not_a_fixed_path() {
    // Somebody with a file in their home directory and another in /etc gets
    // the one belonging to the configuration in force.
    let config = load_config(
        "/home/a/.config/errand/config.json",
        |_| Ok(valid_text()),
        &Environment::new(),
        |path| path.starts_with("/home/a/"),
    )
    .expect("resolves");

    assert_eq!(
        config.agent.rules_path,
        Some("/home/a/.config/errand/AGENTS.md".to_owned())
    );
}

#[test]
fn a_named_rules_path_wins_over_the_file_beside_the_configuration() {
    let mut named: Value = serde_json::from_str(&valid_text()).expect("valid JSON");
    if let Value::Object(fields) = &mut named {
        fields.insert(
            "agent".to_owned(),
            json!({
                "provider": "anthropic",
                "providers": {
                    "anthropic": { "credentialName": "ANTHROPIC_API_KEY", "credential": "k" },
                },
                "rulesPath": "/somewhere/else/RULES.md",
            }),
        );
    }

    let config = load_config(
        "/etc/errand/config.json",
        |_| Ok(named.to_string()),
        &Environment::new(),
        |_| true,
    )
    .expect("resolves");

    assert_eq!(
        config.agent.rules_path,
        Some("/somewhere/else/RULES.md".to_owned())
    );
}

#[test]
fn no_file_beside_the_configuration_means_no_rules_and_is_not_a_refusal() {
    // A default nobody asked for must not be able to stop the daemon.
    let config = load_config(
        "/etc/errand/config.json",
        |_| Ok(valid_text()),
        &Environment::new(),
        |_| false,
    )
    .expect("resolves");

    assert_eq!(config.agent.rules_path, None);
}

/// The report has to name the places actually searched. Computing it from a
/// blank environment describes a search that did not happen: it ignores
/// `ERRAND_CONFIG` and shows paths relative to a `HOME` that was never read.
#[test]
fn the_places_it_looked_are_the_places_it_looked() {
    let env = Environment::from([("HOME".to_owned(), "/home/somebody".to_owned())]);

    let refused = load_config(
        "/home/somebody/.config/errand/config.json",
        |_| Err(std::io::Error::new(std::io::ErrorKind::NotFound, "nothing")),
        &env,
        |_| false,
    )
    .expect_err("a missing file is refused");

    let said = refused.problems.join("\n");
    assert!(
        said.contains("/home/somebody/.config/errand/config.json"),
        "{said}"
    );
    assert!(
        !said.contains(" .config/errand/config.json"),
        "a path relative to a HOME nobody read: {said}"
    );
}

/// A named file is the whole search, and the report says so.
#[test]
fn a_named_file_is_reported_as_the_only_place_looked() {
    let env = Environment::from([("ERRAND_CONFIG".to_owned(), "/named/errand.json".to_owned())]);

    let refused = load_config(
        "/named/errand.json",
        |_| Err(std::io::Error::new(std::io::ErrorKind::NotFound, "nothing")),
        &env,
        |_| false,
    )
    .expect_err("a missing file is refused");

    let said = refused.problems.join("\n");
    assert!(said.contains("/named/errand.json"), "{said}");
    assert!(
        !said.contains("/etc/errand"),
        "the search was one file: {said}"
    );
}
