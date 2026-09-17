//! Tests for the LF-only framing, ported from `framing_test.ts`.

use super::{LineFramer, RecordTooLargeError};

fn records(framer: &mut LineFramer, text: &str) -> Vec<String> {
    framer.push(text.as_bytes()).expect("records come out")
}

#[test]
fn a_record_split_across_reads_is_reassembled() {
    let mut framer = LineFramer::default();

    assert_eq!(
        records(&mut framer, "{\"type\":\"pro"),
        Vec::<String>::new()
    );
    assert_eq!(
        records(&mut framer, "mpt\"}\n"),
        vec!["{\"type\":\"prompt\"}".to_owned()]
    );
}

#[test]
fn several_records_in_one_chunk_all_come_out_in_order() {
    let mut framer = LineFramer::default();
    assert_eq!(
        records(&mut framer, "one\ntwo\nthree\n"),
        vec!["one".to_owned(), "two".to_owned(), "three".to_owned()]
    );
}

#[test]
fn chunking_does_not_change_what_comes_out() {
    let whole = "a\n{\"b\":1}\nc\n";
    let expected = vec!["a".to_owned(), "{\"b\":1}".to_owned(), "c".to_owned()];

    for size in [1, 2, 3, 5, 11] {
        let mut framer = LineFramer::default();
        let mut collected = Vec::new();
        for at in (0..whole.len()).step_by(size) {
            collected.extend(
                framer
                    .push(&whole.as_bytes()[at..(at + size).min(whole.len())])
                    .expect("records"),
            );
        }
        assert_eq!(collected, expected, "chunked by {size}");
    }
}

#[test]
fn a_carriage_return_before_the_line_feed_is_not_part_of_the_record() {
    let mut framer = LineFramer::default();
    assert_eq!(
        records(&mut framer, "one\r\ntwo\r\n"),
        vec!["one".to_owned(), "two".to_owned()]
    );
}

/// The reason framing works on bytes: these are legal inside a JSON string and
/// every general purpose line reader treats them as newlines, so a record
/// containing one would be split in half by a text-based reader.
#[test]
fn a_line_separator_inside_a_string_is_not_a_record_boundary() {
    let mut framer = LineFramer::default();
    let record = "{\"text\":\"before\u{2028}after\u{2029}end\"}";

    assert!(record.contains('\u{2028}'), "it must really contain one");
    assert_eq!(
        records(&mut framer, &format!("{record}\n")),
        vec![record.to_owned()]
    );
}

#[test]
fn a_multi_byte_character_split_across_chunks_survives() {
    let mut framer = LineFramer::default();
    let raw = format!("{{\"text\":\"\u{1F50C}\"}}\n").into_bytes();

    let first = framer.push(&raw[..11]).expect("held");
    let second = framer.push(&raw[11..]).expect("completed");

    assert_eq!(first, Vec::<String>::new());
    assert_eq!(second, vec!["{\"text\":\"\u{1F50C}\"}".to_owned()]);
}

#[test]
fn an_unterminated_record_past_the_ceiling_is_refused_not_buffered() {
    let mut framer = LineFramer::new(16);

    let error = framer.push("x".repeat(17).as_bytes()).expect_err("refused");
    let RecordTooLargeError { limit } = error;
    assert_eq!(limit, 16);
}

#[test]
fn what_is_held_for_an_unfinished_record_can_be_seen() {
    let mut framer = LineFramer::default();
    records(&mut framer, "half");
    assert_eq!(framer.pending_bytes(), 4);

    records(&mut framer, "\n");
    assert_eq!(framer.pending_bytes(), 0);
}
