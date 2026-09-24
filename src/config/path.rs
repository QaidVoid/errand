//! Where a value sits in a JSON answer, as `discover` and `usage` name it.
//!
//! A path is dotted keys, as `limiting.resetsAt`. A key may pick one entry out
//! of a list by a field that entry carries, as
//! `windows[id=window-share:300m].usedPercent`, since the order of a list is
//! not something a provider promises.

use serde_json::Value;

/// One step down a path.
enum Step<'a> {
    /// Into an object, by key.
    Key(&'a str),
    /// Into an object by key, then to the entry of that list whose `field`
    /// reads as `equals`. An empty key picks from the value itself.
    Pick {
        key: &'a str,
        field: &'a str,
        equals: &'a str,
    },
}

/// Says what is wrong with a path, if anything.
pub fn check(path: &str) -> Result<(), String> {
    steps(path).map(|_| ())
}

/// The value a path names, or nothing where it leads nowhere.
pub fn at<'a>(value: &'a Value, path: &str) -> Option<&'a Value> {
    let mut inside = value;
    for step in steps(path).ok()? {
        inside = match step {
            Step::Key(key) => inside.get(key)?,
            Step::Pick { key, field, equals } => {
                let list = if key.is_empty() {
                    inside
                } else {
                    inside.get(key)?
                };
                list.as_array()?.iter().find(|entry| {
                    entry
                        .get(field)
                        .is_some_and(|found| reads_as(found, equals))
                })?
            }
        };
    }
    Some(inside)
}

/// Whether a field reads as the text a path compares it with.
fn reads_as(value: &Value, text: &str) -> bool {
    match value {
        Value::String(found) => found == text,
        Value::Number(_) | Value::Bool(_) => text.parse::<Value>().is_ok_and(|read| read == *value),
        _ => false,
    }
}

/// Splits a path into its steps, on dots outside brackets.
fn steps(path: &str) -> Result<Vec<Step<'_>>, String> {
    let mut found = Vec::new();
    let mut start = 0;
    let mut depth = 0_u32;
    for (at, character) in path.char_indices().chain([(path.len(), '.')]) {
        match character {
            '[' => depth += 1,
            ']' => depth = depth.saturating_sub(1),
            '.' if depth == 0 => {
                found.push(step(&path[start..at], path)?);
                start = at + 1;
            }
            _ => {}
        }
    }
    if depth == 0 {
        Ok(found)
    } else {
        Err(format!("`{path}` opens a bracket it never closes"))
    }
}

/// Reads one step, `key` or `key[field=value]`.
fn step<'a>(segment: &'a str, path: &str) -> Result<Step<'a>, String> {
    let broken = || format!("`{path}` is not a path; write keys as a.b, or a[field=value].b");
    let Some((key, rest)) = segment.split_once('[') else {
        return if segment.is_empty() || segment.contains(']') {
            Err(broken())
        } else {
            Ok(Step::Key(segment))
        };
    };
    let pick = rest.strip_suffix(']').ok_or_else(broken)?;
    let (field, equals) = pick.split_once('=').ok_or_else(broken)?;
    if field.is_empty() || equals.is_empty() || pick.contains(['[', ']']) {
        return Err(broken());
    }
    Ok(Step::Pick { key, field, equals })
}

#[cfg(test)]
mod tests;
