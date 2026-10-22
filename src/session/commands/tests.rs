//! Tests for the command table, ported from `commands_test.ts`.

use super::{
    COMMANDS, CommandAccess, Standing, answer_without_session, argument_of, asks_for_pull_request,
    help_text, image_args, is_addressed_to_bot, is_aside, is_command, may_run, parse_user_id,
    pull_request_args,
};
use crate::chat::render::MESSAGE_LIMIT;

const OWNER: Standing = Standing {
    is_owner: true,
    is_guest: false,
};
const GUEST: Standing = Standing {
    is_owner: false,
    is_guest: true,
};
const STRANGER: Standing = Standing {
    is_owner: false,
    is_guest: false,
};

#[test]
fn a_command_is_named_by_its_whole_first_word() {
    assert!(is_command("!status"));
    assert!(is_command("  !ls src  "));
    assert!(!is_command("!statusreport"));
    assert!(!is_command("please !status"));
    assert!(!is_command("write a !status handler"));
}

/// An unknown `!` word belongs to another bot, and is not worth a model call.
#[test]
fn anything_starting_with_a_bang_is_addressed_to_a_bot_known_or_not() {
    assert!(is_addressed_to_bot("!somebodyelses thing"));
    assert!(is_addressed_to_bot("  !status"));
    assert!(!is_addressed_to_bot("what does ! mean"));
}

/// The argument a command was given, or nothing when it was given none. The
/// name must be followed by nothing or by whitespace, so a longer word that
/// begins with it carries no argument.
#[test]
fn an_argument_is_what_follows_the_command_name_and_nothing_else() {
    assert_eq!(argument_of("!usage refresh", "!usage"), "refresh");
    assert_eq!(argument_of("  !usage   refresh  ", "!usage"), "refresh");
    assert_eq!(argument_of("!usage", "!usage"), "");
    assert_eq!(argument_of("!usage   ", "!usage"), "");
    assert_eq!(argument_of("!usagereport", "!usage"), "");
    assert_eq!(argument_of("!usage refresh now", "!usage"), "refresh now");
}

#[test]
fn an_aside_is_marked_three_times_so_it_cannot_be_typed_by_accident() {
    assert!(is_aside("!!! ignore this"));
    assert!(!is_aside("!! nearly"));
    assert!(!is_aside("!status"));
}

/// An aside must not be answered as a command, whatever follows the marks.
#[test]
fn an_aside_is_never_a_command() {
    assert!(!is_command("!!! !stop"));
}

#[test]
fn only_the_owner_may_steer_or_end_the_session() {
    for standing in [GUEST, STRANGER] {
        assert!(!may_run(CommandAccess::Owner, standing));
    }
    assert!(may_run(CommandAccess::Owner, OWNER));
}

#[test]
fn an_invited_guest_may_read_the_project_a_stranger_may_not() {
    assert!(may_run(CommandAccess::Guest, GUEST));
    assert!(may_run(CommandAccess::Guest, OWNER));
    assert!(!may_run(CommandAccess::Guest, STRANGER));
}

#[test]
fn what_is_harmless_is_open_to_anyone_permitted_to_be_here() {
    assert!(may_run(CommandAccess::Anyone, STRANGER));
}

/// Powering off the machine is not a session's to grant, not even the owner's.
#[test]
fn no_session_role_turns_the_host_off() {
    for standing in [OWNER, GUEST, STRANGER] {
        assert!(!may_run(CommandAccess::Host, standing));
    }
}

/// A command added later must state who may run it, not inherit "anyone".
#[test]
fn every_command_declares_its_access_and_its_group() {
    for (name, meta) in COMMANDS {
        assert!(name.starts_with('!'), "{name}");
        assert!(!meta.summary.is_empty(), "{name}");
        let _ = meta.access;
    }
}

#[test]
fn an_account_is_named_by_a_mention_or_by_its_bare_id() {
    assert_eq!(parse_user_id("<@123456789>"), Some("123456789".to_owned()));
    assert_eq!(
        parse_user_id(" <@!123456789> "),
        Some("123456789".to_owned())
    );
    assert_eq!(parse_user_id("123456789"), Some("123456789".to_owned()));
    assert_eq!(
        parse_user_id("<@github:QaidVoid>"),
        Some("github:qaidvoid".to_owned())
    );
    assert_eq!(parse_user_id("<@github:not a login>"), None);
}

/// Guessing at an id means `!deny` withdrawing somebody who was never named.
#[test]
fn anything_that_is_not_an_id_is_refused_rather_than_guessed_at() {
    for text in ["", "amelia", "<@abc>", "12", "<@123456789> and friends"] {
        assert_eq!(parse_user_id(text), None, "{text}");
    }
}

#[test]
fn asking_for_a_pull_request_is_recognised_however_it_is_spelled() {
    for text in [
        "open a PR",
        "raise a pull request",
        "send pull-requests",
        "a merge request",
    ] {
        assert!(asks_for_pull_request(text), "{text}");
    }
}

/// The gate only has to catch the ask; it must not fire on ordinary work.
#[test]
fn ordinary_work_is_not_read_as_asking_to_publish() {
    for text in ["prepare the branch", "review the diff", "push to my fork"] {
        assert!(!asks_for_pull_request(text), "{text}");
    }
}

#[test]
fn help_is_answered_without_starting_a_session_and_nothing_else_is() {
    let answer = answer_without_session("!help");
    assert!(answer.unwrap_or_default().contains("!stop"));
    assert!(answer_without_session("!help").is_some_and(|text| text.contains("!ls")));
    assert_eq!(answer_without_session("!ls src"), None);
    assert_eq!(answer_without_session("write me a parser"), None);
}

#[test]
fn help_groups_the_commands_and_says_which_are_not_open_to_all() {
    let text = help_text();

    assert!(text.contains("THE SESSION"));
    assert!(text.contains("WHO TAKES PART"));
    assert!(text.contains("!steer <instruction>"));
    assert!(text.contains("(owner)"));
    assert_eq!(text.matches("```").count(), 2);
}

/// `!help` reaches a slash command as a single interaction response, and the
/// service refuses one over its message limit rather than truncating it. A
/// command added to the table must not push the list past that.
///
/// The list sat at 1993 of 2000 characters when `!gen-image` last changed, so
/// the next command or argument added here needs something else shortened to
/// make room. This test is what says so.
#[test]
fn the_help_list_fits_one_message() {
    let text = help_text();

    assert!(
        text.chars().count() <= MESSAGE_LIMIT,
        "the help list is {} characters, over the {MESSAGE_LIMIT} a message allows",
        text.chars().count()
    );
}

/// A summary that ran past its column would break the alignment for every row.
#[test]
fn the_command_column_is_one_width_for_every_line() {
    let text = help_text();
    let body = text.split("```").nth(1).unwrap_or("");
    let summary_at: Vec<Option<usize>> = body
        .split('\n')
        .filter(|line| line.starts_with("  !"))
        .map(|line| {
            // Where the command's own summary starts, found by its text, since
            // an argument may hold spaces of its own.
            COMMANDS
                .iter()
                .filter(|(name, _)| line[2..].starts_with(&format!("{name} ")))
                .find_map(|(_, meta)| line.find(meta.summary))
        })
        .collect();

    let distinct: std::collections::HashSet<_> = summary_at.iter().collect();
    assert_eq!(distinct.len(), 1, "{summary_at:?}");
}

/// `--repo` names the clone by its directory; without it the whole of what
/// follows is the title, a slash in its first word included.
#[test]
fn a_pull_request_names_its_repository_only_by_flag() {
    assert_eq!(
        pull_request_args(" --repo edu/playground/sub/inner  Add the proof file "),
        (Some("edu/playground/sub/inner"), "Add the proof file")
    );
    assert_eq!(
        pull_request_args("fix/parser: handle empty input"),
        (None, "fix/parser: handle empty input")
    );
    assert_eq!(
        pull_request_args("--repo tools/parser"),
        (Some("tools/parser"), "")
    );
    assert_eq!(
        pull_request_args("--repository x"),
        (None, "--repository x")
    );
}

/// A prompt alone asks for nothing but the picture.
#[test]
fn an_image_prompt_alone_names_no_model_and_no_effort() {
    let asked = image_args("a lighthouse in the fog").expect("read");

    assert_eq!(asked.model, None);
    assert_eq!(asked.effort, None);
    assert_eq!(asked.prompt, "a lighthouse in the fog");
}

/// The effort may be a flag, which is how a slash command says it.
#[test]
fn an_effort_flag_is_read_and_kept_off_the_prompt() {
    let asked = image_args("--effort max a lighthouse").expect("read");

    assert_eq!(asked.effort.as_deref(), Some("max"));
    assert_eq!(asked.prompt, "a lighthouse");
    assert_eq!(asked.model, None);
}

/// The effort may also be a level on the model, which is how `!model` says it.
#[test]
fn a_model_with_a_level_names_both_the_model_and_the_effort() {
    let asked = image_args("image:xhigh a lighthouse").expect("read");

    assert_eq!(asked.model, Some("image:xhigh"));
    assert_eq!(asked.effort.as_deref(), Some("xhigh"));
    assert_eq!(asked.prompt, "a lighthouse");
}

/// A flag beats a level when both are given, since it is the more explicit.
#[test]
fn a_flag_beats_a_level_on_the_model() {
    let asked = image_args("--effort low image:max a lighthouse").expect("read");

    assert_eq!(asked.model, Some("image:max"));
    assert_eq!(asked.effort.as_deref(), Some("low"));
    assert_eq!(asked.prompt, "a lighthouse");
}

/// A prompt that happens to begin with a word is the prompt, not a model: only
/// a word whose end is a level this host knows names one.
#[test]
fn a_first_word_that_is_not_a_level_stays_in_the_prompt() {
    let asked = image_args("draw a lighthouse").expect("read");

    assert_eq!(asked.model, None);
    assert_eq!(asked.prompt, "draw a lighthouse");

    // A colon that introduces no level is part of the word.
    let asked = image_args("note: this is prose").expect("read");
    assert_eq!(asked.model, None);
    assert_eq!(asked.prompt, "note: this is prose");
}

/// An effort nothing would understand is refused here rather than passed on.
#[test]
fn an_unknown_effort_is_refused_naming_the_ones_that_work() {
    let refused = image_args("--effort extreme a lighthouse").expect_err("not a level");

    assert!(refused.contains("extreme"), "{refused}");
    assert!(refused.contains("xhigh"), "{refused}");
}

/// A flag with nothing after it says what to type instead of drawing nothing.
#[test]
fn an_effort_flag_with_no_level_says_what_to_type() {
    let refused = image_args("--effort").expect_err("no level");

    assert!(refused.contains("--effort"), "{refused}");
}

/// The effort is taken in any case, as a level on a model is.
#[test]
fn an_effort_is_read_case_insensitively() {
    let asked = image_args("--effort MAX a lighthouse").expect("read");

    assert_eq!(asked.effort.as_deref(), Some("max"));
}
