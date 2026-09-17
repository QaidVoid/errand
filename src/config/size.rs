//! Sizes as people write them in configuration, and as bytes.
//!
//! One definition, used both to validate what was configured and to convert it
//! for whatever enforces it. Two would eventually disagree, and the way that
//! shows up is a limit that validates and then is not applied.

/// Reads a size such as `512m` or `4g` as a number of bytes.
///
/// Returns nothing when the text is not a size, so a caller can refuse it
/// rather than guess.
pub fn parse_size(text: &str) -> Option<u64> {
    // The shape is a whole or decimal number, optional whitespace, and a
    // suffix, which is how such a setting is written by hand.
    let text = text.trim();
    let digits = text
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(text.len());
    if digits == 0 {
        return None;
    }
    let (amount_text, rest) = text.split_at(digits);

    let number_end = amount_text.len()
        + if let Some(after_dot) = rest.strip_prefix('.') {
            let fraction = after_dot
                .find(|c: char| !c.is_ascii_digit())
                .unwrap_or(after_dot.len());
            if fraction == 0 {
                return None;
            }
            1 + fraction
        } else {
            0
        };
    let amount: f64 = text[..number_end].parse().ok()?;

    let suffix = text[number_end..].trim_start();
    if !suffix.chars().all(|c| c.is_ascii_alphabetic()) {
        return None;
    }
    let scale = suffix_scale(&suffix.to_ascii_lowercase())?;
    if !amount.is_finite() {
        return None;
    }
    #[allow(
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "a size that large is beyond what any limit means; the floor keeps the whole bytes either way"
    )]
    Some((amount * scale as f64).floor() as u64)
}

/// How many bytes a written suffix stands for.
fn suffix_scale(suffix: &str) -> Option<u64> {
    match suffix {
        "b" | "" => Some(1),
        "k" | "kb" => Some(1024),
        "m" | "mb" => Some(1024_u64.pow(2)),
        "g" | "gb" => Some(1024_u64.pow(3)),
        "t" | "tb" => Some(1024_u64.pow(4)),
        _ => None,
    }
}

#[cfg(test)]
mod tests;
