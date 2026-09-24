//! Tests for rendering and splitting, ported from `render_test.ts`.

use serde_json::json;

use super::{
    MAX_LISTED_ENTRIES, MESSAGE_LIMIT, THREAD_NAME_LIMIT, Usage, bytes, compaction_line,
    dialog_lines, directory_listing, duration, file_view, split_message, thread_name, tokens,
    tool_line, truncate, turn_summary, turn_timing, usage_summary, usage_table,
    when_relative_plain,
};
use crate::agent::protocol::{DialogMethod, DialogRequest};
use crate::provider::usage::Quota;
use crate::session::files::{Entry, FileContents};

#[test]
fn text_that_fits_is_one_message_and_nothing_is_one_message_of_nothing() {
    assert_eq!(
        split_message("short", MESSAGE_LIMIT),
        vec!["short".to_owned()]
    );
    assert_eq!(split_message("", MESSAGE_LIMIT), Vec::<String>::new());
}

#[test]
fn a_long_text_is_split_at_line_boundaries_and_every_piece_fits() {
    let text: Vec<String> = (0..400).map(|index| format!("line {index}")).collect();
    let text = text.join("\n");

    let pieces = split_message(&text, MESSAGE_LIMIT);

    assert!(pieces.len() > 1);
    for piece in &pieces {
        assert!(piece.chars().count() <= MESSAGE_LIMIT);
    }
    assert_eq!(pieces.join("\n"), text);
}

/// A byte split would cut a character in half and post a broken glyph.
#[test]
fn splitting_counts_code_points_never_bytes() {
    let wide = "\u{1F50C}".repeat(1_500);

    let pieces = split_message(&wide, 100);

    for piece in &pieces {
        assert!(piece.chars().count() <= 100);
        assert!(!piece.contains('\u{FFFD}'));
    }
    assert_eq!(pieces.concat(), wide);
}

/// Each message has to stand on its own, so a split inside a fence closes it
/// and reopens it with the same language.
#[test]
fn a_split_inside_a_code_fence_repairs_the_fence_on_both_sides() {
    let code: Vec<String> = (0..300)
        .map(|index| format!("  const x{index} = {index};"))
        .collect();
    let text = ["before".to_owned(), "```ts".to_owned()]
        .into_iter()
        .chain(code)
        .chain(["```".to_owned(), "after".to_owned()])
        .collect::<Vec<_>>()
        .join("\n");

    let pieces = split_message(&text, MESSAGE_LIMIT);

    assert!(pieces.len() > 1);
    for piece in &pieces {
        let fences = piece.matches("```").count();
        assert_eq!(
            fences % 2,
            0,
            "unbalanced fences in: {}",
            &piece[..40.min(piece.len())]
        );
    }
    assert!(pieces[1].contains("```ts"));
}

#[test]
fn a_single_line_longer_than_the_limit_is_broken_up_before_anything_else() {
    let pieces = split_message(&"x".repeat(5_000), MESSAGE_LIMIT);

    assert!(pieces.len() >= 3);
    for piece in &pieces {
        assert!(piece.chars().count() <= MESSAGE_LIMIT);
    }
}

#[test]
fn a_thread_is_named_after_the_project_and_what_was_asked() {
    let name = thread_name("demo", "fix the failing test\nand explain why");

    assert!(name.contains("demo"));
    assert!(name.contains("fix the failing test"));
    assert!(!name.contains("explain why"));
    assert!(name.chars().count() <= THREAD_NAME_LIMIT);
}

#[test]
fn a_very_long_ask_is_cut_to_the_limit_without_splitting_a_character() {
    let name = thread_name("demo", &"\u{1F50C}".repeat(200));

    assert!(name.chars().count() <= THREAD_NAME_LIMIT);
    assert!(!name.contains('\u{FFFD}'));
}

/// The cut is on the content; the note about it is what makes the cut visible.
#[test]
fn truncating_keeps_the_limit_and_says_what_it_dropped() {
    assert_eq!(truncate("short", 20), "short");

    let cut = truncate(&"x".repeat(50), 10);
    assert!(cut.starts_with(&"x".repeat(10)));
    assert!(cut.contains("40 more characters"));
}

#[test]
fn a_tool_call_reads_as_the_tool_and_what_it_acted_on() {
    let line = tool_line("bash", Some("ls -la"));
    assert!(line.contains("`bash`"));
    assert!(line.contains("`ls -la`"));
    assert!(tool_line("read", None).contains("`read`"));
    assert!(tool_line("read", Some("  ")).contains("`read`"));
}

/// A backtick in the target would end the code span and spill markup.
#[test]
fn a_backtick_in_what_a_tool_acted_on_cannot_break_the_line() {
    let line = tool_line("bash", Some("echo `whoami`"));

    assert!(!line.contains("`whoami`"));
    assert_eq!(line.matches('`').count() % 2, 0);
}

#[test]
fn a_target_spanning_lines_is_flattened_onto_one() {
    assert!(!tool_line("bash", Some("one\n  two")).contains('\n'));
}

#[test]
fn counts_are_shown_with_the_magnitude_a_reader_can_compare() {
    assert_eq!(tokens(0.0), "0");
    assert_eq!(tokens(999.0), "999");
    assert_eq!(tokens(1_500.0), "1.5k");
    assert_eq!(tokens(1_500_000.0), "1.5M");
    assert_eq!(tokens(1_500_000_000.0), "1.5B");
    assert_eq!(tokens(123_400.0), "123k");
}

/// Rounding up must not report a value in the magnitude below its own.
#[test]
fn a_count_that_rounds_past_its_magnitude_carries_up() {
    assert_eq!(tokens(999_999.0), "1.0M");
    assert_eq!(tokens(999_999_999.0), "1.0B");
}

#[test]
fn usage_says_what_a_session_cost_in_terms_a_reader_can_act_on() {
    let line = usage_summary(&Usage {
        input: 240_000.0,
        cache_read: 214_000.0,
        total_tokens: 264_000.0,
        cost: 0.41,
        context_tokens: 118_000.0,
        context_window: 1_000_000.0,
    });

    assert!(line.contains("264k tokens"));
    assert!(line.contains("47% cached"));
    assert!(line.contains("$0.41"));
}

/// Without the share, a number of tokens says nothing about how much is left.
#[test]
fn context_is_reported_as_a_share_of_what_the_model_holds() {
    let line = usage_summary(&Usage {
        input: 1.0,
        cache_read: 0.0,
        total_tokens: 1.0,
        cost: 0.0,
        context_tokens: 500_000.0,
        context_window: 1_000_000.0,
    });

    assert!(line.contains("50%"));
}

#[test]
fn a_byte_count_is_shown_in_the_units_a_disk_quota_uses() {
    assert_eq!(bytes(0.0), "0 B");
    assert_eq!(bytes(999.0), "999 B");
    assert_eq!(bytes(1_500.0), "1.5 kB");
    assert_eq!(bytes(2_400_000.0), "2.4 MB");
    assert_eq!(bytes(3_000_000_000.0), "3.0 GB");
    assert_eq!(bytes(5e15), "5000.0 TB");
}

#[test]
fn a_listing_puts_the_sizes_in_one_column_and_marks_directories() {
    let listing = directory_listing(
        &[
            Entry {
                name: "src".to_owned(),
                path: "src".to_owned(),
                directory: true,
                size: 0,
            },
            Entry {
                name: "readme.md".to_owned(),
                path: "readme.md".to_owned(),
                directory: false,
                size: 1_200,
            },
        ],
        "demo",
    );

    let rows: Vec<&str> = listing
        .split("```")
        .nth(1)
        .unwrap_or("")
        .split('\n')
        .filter(|row| !row.is_empty())
        .collect();
    assert!(rows[0].ends_with("src/"));
    assert!(rows[1].contains("1.2 kB"));
    assert_eq!(rows[0].find("src/"), rows[1].find("readme.md"));
}

#[test]
fn an_empty_directory_says_so_rather_than_showing_an_empty_block() {
    let listing = directory_listing(&[], "demo/src");

    assert!(!listing.contains("```"));
    assert!(listing.contains("is empty"));
}

/// A thread cannot show thousands of entries, and nobody reads them there.
#[test]
fn a_very_long_listing_is_cut_and_says_how_much_it_left_out() {
    let many: Vec<Entry> = (0..MAX_LISTED_ENTRIES + 20)
        .map(|index| Entry {
            name: format!("file-{index}"),
            path: format!("file-{index}"),
            directory: false,
            size: 1,
        })
        .collect();

    let listing = directory_listing(&many, "demo");

    assert!(listing.contains("220 entries"));
    assert!(listing.contains("... 20 more"));
}

#[test]
fn a_file_is_shown_fenced_in_its_own_language() {
    let view = file_view(&FileContents {
        path: "src/main.ts".to_owned(),
        size: 13,
        binary: false,
        truncated: false,
        text: "const x = 1;\n".to_owned(),
        language: "ts".to_owned(),
    });

    assert!(view.contains("```ts"));
    assert!(view.contains("const x = 1;"));
    assert!(!view.contains("cut at"));
}

#[test]
fn a_file_that_was_cut_says_so_and_says_where_to_get_the_rest() {
    let view = file_view(&FileContents {
        path: "big.txt".to_owned(),
        size: 5_000_000,
        binary: false,
        truncated: true,
        text: "x".to_owned(),
        language: String::new(),
    });

    assert!(view.contains("cut at"));
    assert!(view.contains("5.0 MB"));
    assert!(view.contains("!file"));
}

#[test]
fn a_binary_file_is_named_and_offered_not_pasted() {
    let view = file_view(&FileContents {
        path: "logo.png".to_owned(),
        size: 40_000,
        binary: true,
        truncated: false,
        text: String::new(),
        language: String::new(),
    });

    assert!(!view.contains("```"));
    assert!(view.contains("40.0 kB"));
    assert!(view.contains("!file"));
}

/// One that freed nothing looks like one that freed half the window.
#[test]
fn a_compaction_says_how_much_context_it_actually_freed() {
    let line = compaction_line(&json!({
        "success": true,
        "data": { "tokensBefore": 180_000, "estimatedTokensAfter": 42_000 },
    }));

    assert!(line.contains("180k"));
    assert!(line.contains("42.0k"));
}

#[test]
fn a_compaction_that_says_nothing_is_still_reported_as_having_run() {
    assert!(
        compaction_line(&json!({ "success": true, "data": {} }))
            .contains("compacted the conversation")
    );
}

#[test]
fn a_compaction_the_agent_refused_says_so_with_its_reason() {
    let line = compaction_line(&json!({ "success": false, "error": "nothing to compact" }));

    assert!(line.contains("did not run"));
    assert!(line.contains("nothing to compact"));
}

/// A thread has no buttons, so an answer has to be spelled out.
#[test]
fn a_choice_is_numbered_so_a_reply_can_name_one() {
    let lines = dialog_lines(&DialogRequest {
        id: "1".to_owned(),
        method: DialogMethod::Select,
        title: "Which branch?".to_owned(),
        message: None,
        options: Some(vec!["main".to_owned(), "develop".to_owned()]),
        placeholder: None,
        prefill: None,
    });

    assert!(lines.contains("1. main"));
    assert!(lines.contains("2. develop"));
    assert!(lines.contains("reply with a number"));
}

#[test]
fn a_confirmation_says_what_answers_it_takes() {
    let lines = dialog_lines(&DialogRequest {
        id: "1".to_owned(),
        method: DialogMethod::Confirm,
        title: "Delete it?".to_owned(),
        message: None,
        options: None,
        placeholder: None,
        prefill: None,
    });

    assert!(lines.contains("Delete it?"));
    assert!(lines.contains("reply yes or no"));
}

#[test]
fn a_question_with_no_options_still_asks_for_an_answer() {
    let lines = dialog_lines(&DialogRequest {
        id: "1".to_owned(),
        method: DialogMethod::Input,
        title: "Name it".to_owned(),
        message: Some("any name will do".to_owned()),
        options: None,
        placeholder: None,
        prefill: None,
    });

    assert!(lines.contains("any name will do"));
    assert!(lines.contains("reply with your answer"));
}

#[test]
fn a_plain_countdown_carries_minutes_then_hours_and_never_a_negative() {
    let now = 1_000_000_000_000;
    assert_eq!(when_relative_plain(now + 14 * 60_000, now), "in 14m");
    assert_eq!(when_relative_plain(now + 60 * 60_000, now), "in 1h");
    assert_eq!(when_relative_plain(now + 125 * 60_000, now), "in 2h 5m");
    // A window that rolled over while the status was on screen reads as
    // ready, not as a negative wait.
    assert_eq!(when_relative_plain(now - 60_000, now), "now");
    assert_eq!(when_relative_plain(now, now), "now");
}

#[test]
fn a_countdown_rounds_up_so_it_never_claims_less_wait_than_there_is() {
    let now = 1_000_000_000_000;
    assert_eq!(when_relative_plain(now + 61_000, now), "in 2m");
    assert_eq!(when_relative_plain(now + 1, now), "in 1m");
}

/// The wait before the first word is what a person in a thread feels, and it
/// is not the same number as how long the whole turn took.
#[test]
fn a_turn_says_what_it_waited_and_what_it_took() {
    assert_eq!(
        turn_timing(Some(820), 47_300),
        "47.3s (820ms to first word)"
    );
    // A turn that produced nothing has no first word to report.
    assert_eq!(turn_timing(None, 1_500), "1.5s");
}

/// Sub-second is where an interesting first-token wait lives and hours is
/// where a long agent turn lives, so neither end is rounded away.
#[test]
fn a_span_is_shown_in_the_unit_that_still_says_something() {
    assert_eq!(duration(0), "0ms");
    assert_eq!(duration(999), "999ms");
    assert_eq!(duration(1_000), "1.0s");
    assert_eq!(duration(59_940), "59.9s");
    assert_eq!(duration(60_000), "1m00s");
    assert_eq!(duration(3_599_000), "59m59s");
    assert_eq!(duration(3_600_000), "1h00m");
    assert_eq!(duration(7_830_000), "2h10m");
    // A clock that went backwards is not a negative span.
    assert_eq!(duration(-5), "0ms");
}

/// Three questions, not one comma-separated run: how long it took, what it
/// spent in tokens, and what it cost. A reader should not have to count
/// commas to find the one they came for.
#[test]
fn a_turn_summary_groups_what_it_answers() {
    let usage = Usage {
        input: 7_000.0,
        cache_read: 3_000.0,
        total_tokens: 10_900.0,
        cost: 0.000_3,
        context_tokens: 3_300.0,
        context_window: 1_000_000.0,
    };

    assert_eq!(
        turn_summary(
            Some("musecringe:max"),
            "2.4s (1.6s to first word)",
            Some(&usage)
        ),
        "`musecringe:max` | 2.4s (1.6s to first word) | \
         10.9k tokens, 30% cached, 3.3k/1.0M context (0%), $0.0003"
    );
    // A turn with no usage yet is still worth timing, and a session that was
    // never told which model answered says nothing about one.
    assert_eq!(turn_summary(None, "2.4s", None), "2.4s");
    assert_eq!(turn_summary(Some(""), "2.4s", None), "2.4s");
}

/// `!usage` lines up as a table in a code block: what is left, when it resets
/// as a span, and the moment in UTC, with a dash for what a provider did not
/// say.
#[test]
fn usage_is_a_table_with_the_reset_in_utc() {
    let now = 1_790_000_000_000;
    let table = usage_table(
        &[
            (
                "ajamxhacker".to_owned(),
                Some(Quota {
                    percentage: 42.4,
                    resets_at: Some(now + 2 * 3_600_000 + 13 * 60_000),
                }),
            ),
            (
                "zai-coding-cn".to_owned(),
                Some(Quota {
                    percentage: 100.0,
                    resets_at: Some(now + 93 * 3_600_000),
                }),
            ),
            (
                "fresh".to_owned(),
                Some(Quota {
                    percentage: 0.0,
                    resets_at: None,
                }),
            ),
            ("quiet".to_owned(), None),
        ],
        now,
    );

    assert_eq!(
        table,
        "```\n\
| provider      | left    | resets in | at (UTC)         |\n\
|---------------|---------|-----------|------------------|\n\
| ajamxhacker   | 58%     | 2h 13m    | 2026-09-21 16:26 |\n\
| zai-coding-cn | spent   | 3d 21h    | 2026-09-25 11:13 |\n\
| fresh         | 100%    | -         | -                |\n\
| quiet         | unknown | -         | -                |\n\
```"
    );
}
