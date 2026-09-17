//! Tests for logging, ported from `log_test.ts`.

use std::sync::{Arc, Mutex};

use super::{LogFields, LogLevel, LogValue, Logger, fields, format_line, to_ascii};

/// 2026-09-02T15:47:13.204Z, as epoch milliseconds.
fn at() -> i64 {
    "2026-09-02T15:47:13.204Z"
        .parse::<jiff::Timestamp>()
        .expect("a timestamp")
        .as_millisecond()
}

fn collected() -> (
    Arc<Mutex<Vec<(LogLevel, String)>>>,
    impl Fn(LogLevel, &str) + Send + Sync,
) {
    let lines: Arc<Mutex<Vec<(LogLevel, String)>>> = Arc::new(Mutex::new(Vec::new()));
    let sink_lines = Arc::clone(&lines);
    let sink = move |level: LogLevel, line: &str| {
        sink_lines.lock().unwrap().push((level, line.to_owned()));
    };
    (lines, sink)
}

#[test]
fn a_line_carries_the_time_the_level_and_the_fields() {
    assert_eq!(
        format_line(
            LogLevel::Info,
            "session started",
            &fields([("session", "a1".into()), ("turns", LogValue::Number(3))]),
            at(),
        ),
        "2026-09-02T15:47:13.204Z [info] session started session=a1 turns=3"
    );
}

#[test]
fn a_line_with_nothing_to_add_ends_after_the_message() {
    assert_eq!(
        format_line(LogLevel::Warn, "no project", &LogFields::new(), at()),
        "2026-09-02T15:47:13.204Z [warn] no project"
    );
}

#[test]
fn non_ascii_is_escaped_rather_than_dropped_in_messages_and_in_fields() {
    assert_eq!(to_ascii("ok"), "ok");
    assert_eq!(to_ascii("done \u{2713}"), "done \\u{2713}");
    assert!(
        format_line(LogLevel::Info, "posted \u{1F50C}", &LogFields::new(), at())
            .contains("\\u{1F50C}")
    );
    assert!(
        format_line(
            LogLevel::Info,
            "x",
            &fields([("name", "caf\u{e9}".into())]),
            at(),
        )
        .contains("name=caf\\u{00E9}")
    );
}

#[test]
fn bound_fields_are_on_every_line_and_a_later_field_wins() {
    let (lines, sink) = collected();
    let log = Logger::new(fields([("session", "a1".into())]), Arc::new(sink));

    log.info("first", &LogFields::new());
    log.with(fields([("turn", LogValue::Number(2))]))
        .warn("second", &fields([("session", "a2".into())]));

    let taken = lines.lock().unwrap();
    assert!(taken[0].1.contains("session=a1"));
    assert!(taken[1].1.contains("turn=2"));
    assert!(taken[1].1.contains("session=a2"));
}

#[test]
fn severity_reaches_the_sink_so_errors_can_go_elsewhere() {
    let (lines, sink) = collected();
    let log = Logger::new(LogFields::new(), Arc::new(sink));

    log.info("a", &LogFields::new());
    log.warn("b", &LogFields::new());
    log.error("c", &LogFields::new());

    let taken = lines.lock().unwrap();
    assert_eq!(
        taken.iter().map(|(level, _)| *level).collect::<Vec<_>>(),
        vec![LogLevel::Info, LogLevel::Warn, LogLevel::Error]
    );
}

/// The value forms a field takes; kept where a reader of the port finds them.
#[test]
fn field_values_render_as_the_text_they_would_have_been() {
    let rendered = fields([
        ("text", LogValue::Text("s".to_owned())),
        ("number", LogValue::Number(7)),
        ("flag", LogValue::Flag(true)),
    ]);
    assert_eq!(rendered.get("text"), Some(&LogValue::Text("s".to_owned())));
    assert_eq!(rendered.get("number"), Some(&LogValue::Number(7)));
    assert_eq!(rendered.get("flag"), Some(&LogValue::Flag(true)));
}
