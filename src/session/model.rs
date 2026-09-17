//! Choosing the model a session runs on, from the message that starts it.
//!
//! The configuration names one provider and model, which is what a session
//! uses unless the person starting it says otherwise. Saying otherwise has to
//! happen in the opening message, because by the time there is a thread to
//! type `!model` in, the session has already started against the wrong one.

use std::collections::BTreeMap;

/// A `--model` on the opening message, and the prompt with it removed.
#[derive(Debug, Clone, PartialEq)]
pub struct ModelSelection {
    /// What was asked for, as written, or none when nothing was.
    pub value: Option<String>,
    /// The prompt the session actually receives.
    pub prompt: String,
}

/// Reads a leading `--model` off the prompt, leaving the rest of it alone.
///
/// Only at the very start, and only as a whole word. A prompt that mentions
/// `--model` while describing something must not have it taken as an
/// instruction, and a session started with "explain --model to me" is a
/// likelier message than one that means to select a model halfway through a
/// sentence. Both `--model x` and `--model=x` are accepted, since a person
/// writing a flag will write whichever they are used to, and `-m` with them,
/// because naming the model is the one thing typed often enough to shorten.
pub fn select_model(prompt: &str) -> ModelSelection {
    let trimmed = prompt.trim();
    let after_flag = if let Some(rest) = trimmed.strip_prefix("--model") {
        rest
    } else if let Some(rest) = trimmed.strip_prefix("-m") {
        rest
    } else {
        return ModelSelection {
            value: None,
            prompt: trimmed.to_owned(),
        };
    };

    let after_separator = if let Some(rest) = after_flag.strip_prefix('=') {
        rest
    } else {
        match after_flag.find(|character: char| !character.is_whitespace()) {
            // A flag with nothing after it selects nothing and stays put.
            Some(0) | None => {
                return ModelSelection {
                    value: None,
                    prompt: trimmed.to_owned(),
                };
            }
            Some(run) => &after_flag[run..],
        }
    };

    let value_end = after_separator
        .find(char::is_whitespace)
        .unwrap_or(after_separator.len());
    if value_end == 0 {
        return ModelSelection {
            value: None,
            prompt: trimmed.to_owned(),
        };
    }

    ModelSelection {
        value: Some(after_separator[..value_end].to_owned()),
        prompt: after_separator[value_end..].trim().to_owned(),
    }
}

/// A provider and model a session was asked to run on.
#[derive(Debug, Clone, PartialEq)]
pub struct ChosenModel {
    /// The provider, or none to keep the configured one.
    pub provider: Option<String>,
    /// The model, as the agent should be given it.
    pub model: String,
}

/// Splits `provider/id` from a plain model id.
///
/// A model id may itself hold a slash, `meta/muse-spark-1.3-contributor` among
/// them, so the leading segment is only read as a provider when it is one this
/// host actually knows. Otherwise the whole value is the model, which is what
/// somebody naming a model of the configured provider means. A thinking level
/// such as `:max` is part of the model and is left on it, because it is the
/// agent that understands what those mean.
pub fn resolve_model(value: &str, known: &[&str]) -> ChosenModel {
    if let Some(slash) = value.find('/')
        && slash > 0
        && known.contains(&&value[..slash])
    {
        return ChosenModel {
            provider: Some(value[..slash].to_owned()),
            model: value[slash + 1..].to_owned(),
        };
    }
    ChosenModel {
        provider: None,
        model: value.to_owned(),
    }
}

/// Thinking levels, which are what a colon on the end of a model introduces.
///
/// Named so that a colon in a model id is not mistaken for one. Only a suffix
/// that is actually a level is split off; anything else is part of the name.
const LEVELS: [&str; 7] = ["off", "minimal", "low", "medium", "high", "xhigh", "max"];

/// Splits a trailing `:level` from a model, when the suffix really is one.
fn split_level(value: &str) -> (String, String) {
    let Some(colon) = value.rfind(':') else {
        return (value.to_owned(), String::new());
    };
    if colon == 0 {
        return (value.to_owned(), String::new());
    }
    let suffix = value[colon + 1..].to_lowercase();
    if LEVELS.contains(&suffix.as_str()) {
        (value[..colon].to_owned(), value[colon..].to_owned())
    } else {
        (value.to_owned(), String::new())
    }
}

/// Puts a short name back to what it stands for.
///
/// The level is taken off first, so `muse:xhigh` finds the alias `muse` rather
/// than looking for one that includes the level. A level written into the
/// alias is a default: one given on the name replaces it, because the person
/// typing it is being more specific than the configuration was.
///
/// A name that stands for nothing is returned unchanged, so a model spelled
/// out in full keeps working and a mistyped alias fails against the provider
/// with its own name rather than a substituted one.
pub fn expand_alias(value: &str, aliases: &BTreeMap<String, String>) -> String {
    let (asked, level) = split_level(value.trim());
    let Some(target) = aliases.get(&asked) else {
        return value.trim().to_owned();
    };
    if level.is_empty() {
        return target.clone();
    }
    format!("{}{level}", split_level(target).0)
}

#[cfg(test)]
mod tests;
