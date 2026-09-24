//! Tests for reading the `-v` flags.

use super::take_verbosity;
use crate::log::LogLevel;

fn taken(args: &[&str]) -> (LogLevel, Vec<String>) {
    let mut args: Vec<String> = args.iter().map(|arg| (*arg).to_owned()).collect();
    let level = take_verbosity(&mut args);
    (level, args)
}

#[test]
fn the_flags_add_up_wherever_they_sit_and_leave_the_command() {
    assert_eq!(taken(&["run"]), (LogLevel::Info, vec!["run".to_owned()]));
    assert_eq!(
        taken(&["run", "-v"]),
        (LogLevel::Debug, vec!["run".to_owned()])
    );
    assert_eq!(
        taken(&["-vv", "run"]),
        (LogLevel::Trace, vec!["run".to_owned()])
    );
    assert_eq!(
        taken(&["-v", "run", "--verbose", "-v"]),
        (LogLevel::Wire, vec!["run".to_owned()])
    );
    // A lone dash, and a flag that only starts with v, are someone else's.
    assert_eq!(
        taken(&["threads", "-", "-vx"]),
        (
            LogLevel::Info,
            vec!["threads".to_owned(), "-".to_owned(), "-vx".to_owned()]
        )
    );
}
