//! Tests for rendering and splitting, ported from `render_test.ts`.

use serde_json::json;

use super::usage_cells;
use super::{
    MAX_LISTED_ENTRIES, MESSAGE_LIMIT, THREAD_NAME_LIMIT, Usage, bytes, compaction_line,
    dialog_lines, directory_listing, duration, file_view, model_pages, output_rate, split_message,
    split_tables, table_card, thread_name, tokens, tool_line, truncate, turn_card, turn_summary,
    turn_timing, usage_card, usage_summary, when_relative_plain,
};
use crate::agent::protocol::{DialogMethod, DialogRequest};
use crate::provider::usage::Quota;
use crate::session::event::CardColor;
use crate::session::event::CardField;
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
fn splitting_counts_what_the_limit_counts_never_bytes() {
    let wide = "\u{1F50C}".repeat(1_500);

    let pieces = split_message(&wide, 100);

    for piece in &pieces {
        assert!(piece.encode_utf16().count() <= 100);
        assert!(!piece.contains('\u{FFFD}'));
    }
    assert_eq!(pieces.concat(), wide);
}

/// The service counts UTF-16 units, so an astral character costs two: 1500
/// of them is 1500 code points but 3000 units, and has to split.
#[test]
fn the_limit_counts_utf16_units_so_an_astral_character_costs_two() {
    let wide = "\u{1F50C}".repeat(1_500);

    let pieces = split_message(&wide, MESSAGE_LIMIT);

    assert!(pieces.len() > 1, "1500 astral characters exceed the limit");
    for piece in &pieces {
        assert!(piece.encode_utf16().count() <= MESSAGE_LIMIT);
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
fn a_thread_is_named_after_what_was_asked() {
    let name = thread_name("fix the failing test\nand explain why");

    assert_eq!(name, "fix the failing test");
    assert!(!name.contains("explain why"));
    assert!(name.chars().count() <= THREAD_NAME_LIMIT);
}

#[test]
fn a_very_long_ask_is_cut_to_the_limit_without_splitting_a_character() {
    let name = thread_name(&"\u{1F50C}".repeat(200));

    assert!(name.encode_utf16().count() <= THREAD_NAME_LIMIT);
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
        ..Usage::default()
    });

    assert!(line.contains("264k tokens"));
    assert!(line.contains("47% cached"));
    assert!(line.contains("$0.41"));
}

/// An agent that does not split what it charged has not said that nothing
/// was cached, and a zero would read as though it had.
#[test]
fn counts_the_agent_did_not_report_are_left_out() {
    let line = usage_summary(&Usage {
        context_tokens: 500.0,
        ..Usage::default()
    });

    assert_eq!(line, "500 context");
}

/// Cache writes are prompt tokens too, so they count toward what was sent.
#[test]
fn the_cached_share_is_of_every_prompt_token_and_speed_comes_last() {
    let line = usage_summary(&Usage {
        input: 100.0,
        cache_read: 600.0,
        cache_write: 300.0,
        total_tokens: 1_000.0,
        output_rate: Some(41.6),
        ..Usage::default()
    });

    assert_eq!(line, "1.0k tokens, 60% cached, 42 tok/s");
}

#[test]
fn speed_needs_both_tokens_and_time() {
    assert_eq!(output_rate(500, 10_000), Some(50.0));
    assert_eq!(output_rate(0, 10_000), None);
    assert_eq!(output_rate(500, 0), None);
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
        ..Usage::default()
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
        ..Usage::default()
    };

    assert_eq!(
        turn_summary(
            Some("muse-lite:max"),
            "2.4s (1.6s to first word)",
            Some(&usage)
        ),
        "`muse-lite:max` | 2.4s (1.6s to first word) | \
         10.9k tokens, 30% cached, 3.3k/1.0M context (0%), $0.0003"
    );
    // A turn with no usage yet is still worth timing, and a session that was
    // never told which model answered says nothing about one.
    assert_eq!(turn_summary(None, "2.4s", None), "2.4s");
    assert_eq!(turn_summary(Some(""), "2.4s", None), "2.4s");
}

/// The card carries the verdict in its color: amber when a provider has
/// spent its window, green otherwise. A provider that did not answer is not
/// a warning.
#[test]
fn the_usage_card_is_amber_only_when_a_window_is_spent() {
    let now = 1_790_000_000_000;
    let healthy = usage_card(&[(
        "acme".to_owned(),
        Some(Quota {
            percentage: 42.4,
            resets_at: Some(now),
        }),
    )]);
    assert_eq!(healthy.color, CardColor::Ready);

    let spent = usage_card(&[
        (
            "acme".to_owned(),
            Some(Quota {
                percentage: 42.4,
                resets_at: Some(now),
            }),
        ),
        (
            "zai-coding-cn".to_owned(),
            Some(Quota {
                percentage: 100.0,
                resets_at: Some(now),
            }),
        ),
    ]);
    assert_eq!(spent.color, CardColor::Warning);

    let silent = usage_card(&[("quiet".to_owned(), None)]);
    assert_eq!(silent.color, CardColor::Ready);
}

/// A two-column table is a name and its value, so it becomes a card of
/// labeled cells: nothing to misalign, and the same layout the usage card
/// draws. Alignment colons in the rule are part of the syntax, not a
/// reason to keep the fence.
#[test]
fn a_two_column_table_is_drawn_as_labeled_cells() {
    let card = table_card(
        "find",
        "| name | size |\n|:-----|-----:|\n| a.rs | 1.2k |\n| b/ | - |",
    )
    .expect("a plain table transcribes");
    assert_eq!(card.title, "find");
    assert_eq!(card.color, CardColor::Ready);
    assert_eq!(card.description, "**name \u{b7} size**");
    assert_eq!(
        card.fields,
        vec![
            CardField {
                name: "a.rs".to_owned(),
                value: "1.2k".to_owned(),
                inline: false,
            },
            CardField {
                name: "b/".to_owned(),
                value: "-".to_owned(),
                inline: false,
            },
        ]
    );
}

/// A wider table has columns a reader maps values back to, which stacked
/// cells lose, so it is drawn whole as an aligned block: the fixed-width
/// font keeps every column lined up, which is what the raw pipes in a
/// message could not do.
#[test]
fn a_wide_table_is_drawn_as_an_aligned_block() {
    let card = table_card(
        "find",
        "| name | size | mtime |\n|---|---|---|\n| a.rs | 1.2k | today |\n| b | - | yesterday |",
    )
    .expect("a plain table transcribes");
    assert_eq!(card.title, "find");
    assert_eq!(card.fields, Vec::new());
    assert_eq!(
        card.description,
        "```\n\
| name | size | mtime     |\n\
|------|------|-----------|\n\
| a.rs | 1.2k | today     |\n\
| b    | -    | yesterday |\n\
```"
    );
}

/// Prose around a table belongs with the table in the fence it is already
/// shown in: a card that dropped the words would drop half the output.
#[test]
fn prose_around_a_table_keeps_the_output_fenced() {
    let table = "| a | b |\n|---|---|\n| 1 | 2 |";
    assert_eq!(table_card("ls", &format!("here it is:\n{table}")), None);
    assert_eq!(table_card("ls", &format!("{table}\nthat is all")), None);
    assert_eq!(table_card("ls", ""), None);
}

/// A table longer than a page goes onto several rather than being cut
/// short or left fenced, and every page carries the header, so each reads on
/// its own.
#[test]
fn a_long_table_is_paged_with_its_header_on_every_page() {
    let header = "| id | note |\n|----|------|\n";
    let rows = (1..=26)
        .map(|n| format!("| {n} | x |"))
        .collect::<Vec<_>>()
        .join("\n");
    let card = table_card("ls", &format!("{header}{rows}\n")).expect("drawn");
    assert_eq!(card.page_count(), 3);
    let last = card.page(2);
    assert_eq!(last.fields.len(), 6);
    assert_eq!(last.fields[0].name, "21");
    assert!(last.description.contains("id"));
    assert!(last.footer.unwrap().contains("page 3 of 3: rows 21 to 26"));

    let wide = "| id | name | note |\n|----|------|------|\n".to_owned()
        + &(1..=200)
            .map(|n| format!("| {n} | someone | {} |", "y".repeat(20)))
            .collect::<Vec<_>>()
            .join("\n");
    let card = table_card("ls", &wide).expect("drawn");
    assert!(card.page_count() > 1);
    for index in 0..card.page_count() {
        let page = card.page(index);
        assert!(
            page.description.starts_with("```\n| id"),
            "{}",
            page.description
        );
    }
}

/// A table whose rows do not all agree with the header, or whose row has
/// nothing to head its field with, is not transcribed: the fence shows it
/// exactly as the tool wrote it.
#[test]
fn a_ragged_or_unheaded_table_keeps_the_output_fenced() {
    assert_eq!(
        table_card("ls", "| a | b |\n|---|---|\n| 1 | 2 |\n| 3 |"),
        None
    );
    assert_eq!(table_card("ls", "| a | b |\n|---|---|\n| | 2 |"), None);
    assert_eq!(table_card("ls", "| a | b |\n|x|y|\n| 1 | 2 |"), None);
    assert_eq!(table_card("ls", "| a |\n|---|\n| 1 |"), None);
}

/// A row too heavy for a page of its own is not drawn at all: the service
/// refuses an over-heavy embed, which would lose the words beside it too.
#[test]
fn a_row_too_heavy_for_a_page_keeps_the_output_fenced() {
    let cell = "x".repeat(7_000);
    let table = format!("| id | note |\n|----|------|\n| 1 | {cell} |");
    assert_eq!(table_card("ls", &table), None);
}

/// An answer that ends with a table is split: the words before it travel
/// with the card as its content, untouched, so a thread can draw one under
/// the other without the answer itself ever being rewritten.
#[test]
fn an_answer_ending_with_a_table_is_split_from_its_words() {
    let text = "here is what I found:\n\n| name | size |\n|------|------|\n| a.rs | 1.2k |";
    let pieces = split_tables(text);
    assert_eq!(pieces.len(), 1);
    assert_eq!(pieces[0].0, "here is what I found:");
    let card = pieces[0].1.as_ref().expect("the table is a card");
    assert_eq!(card.title, "");
    assert_eq!(card.description, "**name \u{b7} size**");
    assert_eq!(
        card.fields,
        vec![CardField {
            name: "a.rs".to_owned(),
            value: "1.2k".to_owned(),
            inline: false,
        }]
    );
}

/// A table-only answer splits with no words before it.
#[test]
fn a_table_only_answer_splits_with_no_prose() {
    let pieces = split_tables("| a | b |\n|---|---|\n| 1 | 2 |");
    assert_eq!(pieces.len(), 1);
    assert_eq!(pieces[0].0, "");
    assert!(pieces[0].1.is_some());
}

/// Words after a table follow it as their own message: the card always
/// draws under the words, so lifting a table that is followed would turn
/// the answer around. Every table in an answer is drawn, wherever it
/// sits.
#[test]
fn words_after_a_table_follow_it_as_their_own_message() {
    let table = "| a | b |\n|---|---|\n| 1 | 2 |";
    let pieces = split_tables(&format!("{table}\nthat is all"));
    assert_eq!(pieces.len(), 2);
    assert_eq!(pieces[0].0, "");
    assert!(pieces[0].1.is_some());
    assert_eq!(pieces[1], ("that is all".to_owned(), None));
}

/// Every table in an answer is drawn: two tables each become a card, and
/// the words between them travel with the table they introduce.
#[test]
fn an_answer_with_two_tables_draws_both() {
    let table = "| a | b |\n|---|---|\n| 1 | 2 |";
    let pieces = split_tables(&format!("{table}\n\nand more:\n\n{table}"));
    assert_eq!(pieces.len(), 2);
    assert_eq!(pieces[0].0, "");
    assert!(pieces[0].1.is_some());
    assert_eq!(pieces[1].0, "and more:");
    assert!(pieces[1].1.is_some());
}

/// Words too long for one message travel on their own, where the ordinary
/// split handles their length, and the table after them goes out alone.
#[test]
fn an_answer_whose_prose_exceeds_one_message_travels_apart() {
    let prose = "word ".repeat(500);
    let text = format!("{prose}\n| a | b |\n|---|---|\n| 1 | 2 |");
    let pieces = split_tables(&text);
    assert_eq!(pieces.len(), 2);
    assert_eq!(pieces[0].1, None);
    assert!(pieces[0].0.starts_with("word word"));
    assert_eq!(pieces[1].0, "");
    assert!(pieces[1].1.is_some());
}

/// An answer with no table comes back whole and untouched, so a thread's
/// posting of it is exactly what it would have been without any of this.
#[test]
fn an_answer_with_no_table_comes_back_untouched() {
    let text = "just words, no table";
    assert_eq!(split_tables(text), vec![(text.to_owned(), None)]);
}

/// A table inside a code fence is text the agent chose to show verbatim,
/// not a layout to redraw, so it stays in the words untouched.
#[test]
fn a_table_inside_a_code_fence_stays_words() {
    let text = "```markdown\n| a | b |\n|---|---|\n| 1 | 2 |\n```";
    assert_eq!(split_tables(text), vec![(text.to_owned(), None)]);
}

/// A server's text lands in a code span, so it must stay on one line and
/// hold no backtick that would close the span early.
#[test]
fn server_text_is_one_inert_line_cut_short() {
    assert_eq!(
        super::server_text("a `b`\n\n[x](https://evil.example) <@1>\u{2713}"),
        "a b [x](https://evil.example) 1>"
    );
    assert_eq!(super::server_text(" \n\t"), "");
    let long = "word ".repeat(100);
    let cut = super::server_text(&long);
    assert!(cut.ends_with("..."), "{cut}");
    assert!(cut.len() <= super::MAX_SERVER_TEXT_CHARS + 3, "{cut}");
}

#[test]
fn inert_strips_mention_and_span_syntax() {
    assert_eq!(super::inert("<@123> a`b@c"), "  123> a b c");
}

/// A quoted prompt keeps its words and its lines, each under `> `, while
/// anything that would ping, format, or link is escaped to show as typed.
#[test]
fn a_quoted_prompt_shows_as_typed_and_cannot_ping_or_format() {
    assert_eq!(
        super::quoted("  a cat\nin *space* <@111> [x](https://e.example)\n"),
        "> a cat\n> in \\*space\\* \\<@111\\> \\[x\\]\\(https://e.example\\)"
    );
    assert_eq!(super::quoted("   "), "");
}

/// A long prompt is cut on a character boundary and says so.
#[test]
fn a_long_quoted_prompt_is_cut_short() {
    let long = "e\u{301}".repeat(super::MAX_QUOTED_CHARS);
    let shown = super::quoted(&long);
    assert!(shown.ends_with("..."), "{shown}");
    assert_eq!(
        shown
            .trim_start_matches("> ")
            .trim_end_matches("...")
            .chars()
            .count(),
        super::MAX_QUOTED_CHARS
    );
}

/// A model listing is turned a provider at a time, small providers sharing
/// a page, and every page says what a new choice is measured against.
#[test]
fn a_model_listing_is_paged_by_provider_with_the_intro_on_every_page() {
    let mut list = vec![String::new()];
    for provider in ["kenari", "ant"] {
        list.push(format!("**{provider}**"));
        list.extend((0..60).map(|n| format!("- `{provider}-model-{n}`")));
        list.push(String::new());
    }
    list.push("**tiny**".to_owned());
    list.push("- `one`".to_owned());

    let pages = model_pages("runs on `kenari/a`", &list);

    let labels: Vec<&str> = pages.iter().map(|page| page.label.as_str()).collect();
    assert_eq!(labels, ["kenari", "ant, tiny"]);
    assert!(
        pages
            .iter()
            .all(|page| page.description.starts_with("runs on `kenari/a`\n\n"))
    );
    assert!(pages.last().unwrap().description.contains("**tiny**"));
}

/// The figures a settled turn reports sit in labeled cells, and the ones the
/// agent did not report are left out rather than drawn empty.
#[test]
fn a_turn_card_lays_out_what_was_reported() {
    let usage = Usage {
        input: 700.0,
        cache_read: 300.0,
        total_tokens: 10_900.0,
        cost: 0.0003,
        context_tokens: 3_300.0,
        context_window: 200_000.0,
        output_rate: Some(41.6),
        ..Usage::default()
    };
    let card = turn_card(
        Some("sonnet"),
        Some((Some(1_600), 2_400)),
        Some(&usage),
        true,
        None,
    );

    assert_eq!(card.color, CardColor::Ready);
    assert_eq!(card.title, "turn done");
    assert!(card.description.is_empty());
    let cells: Vec<(&str, &str)> = card
        .fields
        .iter()
        .map(|field| (field.name.as_str(), field.value.as_str()))
        .collect();
    assert_eq!(
        cells,
        [
            ("Model", "`sonnet`"),
            ("Took", "2.4s"),
            ("First word", "1.6s"),
            ("Speed", "42 tok/s"),
            ("Context", "3.3k/200k (2%)"),
            ("Session cost", "$0.0003"),
        ]
    );
    assert!(card.fields.iter().all(|field| field.inline));
    assert_eq!(
        card.footer.as_deref(),
        Some("10.9k tokens this session, 30% cached")
    );

    let bare = turn_card(None, None, None, false, None);
    assert!(bare.fields.is_empty());
    assert!(bare.footer.is_none());
    assert_eq!(
        bare.description,
        "The turn finished without producing any output."
    );
}

/// A failed turn is a warning that says why, whether or not it produced
/// anything first.
#[test]
fn a_failed_turn_card_says_why() {
    let card = turn_card(
        Some("sonnet"),
        None,
        None,
        true,
        Some("the provider refused"),
    );

    assert_eq!(card.color, CardColor::Warning);
    assert_eq!(card.title, "turn failed");
    assert_eq!(card.description, "the provider refused");
}

/// A session's spending reads as labeled figures, context included, and a
/// figure the agent never reported is left out rather than shown as zero.
#[test]
fn usage_cells_label_what_was_reported() {
    let cells = usage_cells(&Usage {
        input: 700.0,
        cache_read: 300.0,
        total_tokens: 10_900.0,
        cost: 0.0003,
        context_tokens: 3_300.0,
        context_window: 200_000.0,
        ..Usage::default()
    });
    assert_eq!(
        cells,
        [
            ("Tokens", "10.9k (30% cached)".to_owned()),
            ("Context", "3.3k/200k (2%)".to_owned()),
            ("Cost", "$0.0003".to_owned()),
        ]
    );
    assert!(usage_cells(&Usage::default()).is_empty());
}
