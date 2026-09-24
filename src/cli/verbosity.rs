//! The `-v` flags, which say how much the log shows.

use crate::log::LogLevel;

/// Takes every `-v`, `-vv`, `-vvv`, and `--verbose` out of the arguments,
/// wherever they sit, and returns the level they add up to.
///
/// Accepted anywhere so `errand run -vv` and `errand -vv run` both work, and
/// taken out so no subcommand has to know about them.
pub fn take_verbosity(args: &mut Vec<String>) -> LogLevel {
    let mut count = 0;
    args.retain(|arg| {
        let flags = match arg.strip_prefix('-') {
            Some("-verbose") => 1,
            Some(vs) if !vs.is_empty() && vs.bytes().all(|byte| byte == b'v') => vs.len(),
            _ => return true,
        };
        count += flags;
        false
    });
    LogLevel::from_verbosity(count)
}

#[cfg(test)]
mod tests;
