//! Lengths of time as people write them in configuration, and as milliseconds.
//!
//! One definition, for validating what was configured and for the sweep that
//! acts on it, so the two cannot disagree on what `30d` means.

/// Reads a length of time such as `30d`, `7d 12h` or `1mo` as milliseconds.
///
/// Each part is a whole number and a unit: `s`, `m`, `h`, `d`, `w`, `mo` or
/// `y`. A month is 30 days and a year 365, since an age compared against a
/// threshold has no calendar to count from. Parts may be separated by spaces
/// or written together. Returns nothing for anything else, or for a total of
/// zero, so a caller refuses it rather than guess.
pub fn parse_duration(text: &str) -> Option<i64> {
    const SECOND: i64 = 1_000;
    const DAY: i64 = 24 * 60 * 60 * SECOND;
    let mut rest = text.trim();
    if rest.is_empty() {
        return None;
    }
    let mut total: i64 = 0;
    while !rest.is_empty() {
        let digits = rest
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(rest.len());
        if digits == 0 {
            return None;
        }
        let amount: i64 = rest[..digits].parse().ok()?;
        rest = &rest[digits..];
        let unit = rest
            .find(|c: char| !c.is_ascii_alphabetic())
            .unwrap_or(rest.len());
        let each = match &rest[..unit] {
            "s" => SECOND,
            "m" => 60 * SECOND,
            "h" => 60 * 60 * SECOND,
            "d" => DAY,
            "w" => 7 * DAY,
            "mo" => 30 * DAY,
            "y" => 365 * DAY,
            _ => return None,
        };
        total = total.checked_add(amount.checked_mul(each)?)?;
        rest = rest[unit..].trim_start();
    }
    (total > 0).then_some(total)
}

#[cfg(test)]
mod tests;
