//! What can be typed at a session, who may type it, and how it reads.
//!
//! Kept apart from the session that runs the commands so that the rule about
//! who may do what is one table rather than a check scattered through a
//! lifecycle. Everything here is pure, so the access rule can be tested
//! against every command without a session, a sandbox, or a connection.

use CommandAccess::{Anyone, Guest, Host, Owner};
use CommandGroup::{People, Project, Session, You};

/// Who may run a command.
///
/// `anyone` is any permitted account. `guest` adds the owner, the operators,
/// and anyone the owner has invited to this thread. `owner` is the owner and
/// the operators alone. `host` acts on the machine rather than on a session,
/// so no session role grants it.
///
/// Declared per command rather than kept in a separate list, so a command
/// added later cannot quietly default to being open to everyone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandAccess {
    /// Any permitted account.
    Anyone,
    /// The owner, operators, and invited guests.
    Guest,
    /// The owner and the operators.
    Owner,
    /// Acts on the machine; the daemon answers it itself.
    Host,
}

/// What a command is for, so help reads as groups rather than one long list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandGroup {
    /// Change what the session is doing.
    Session,
    /// Who may take part.
    People,
    /// Read the project.
    Project,
    /// About the caller, the daemon, or the provider.
    You,
}

/// One command: who may run it, what it does, and what it takes.
#[derive(Debug, Clone, Copy)]
pub struct CommandMeta {
    /// Who may run it.
    pub access: CommandAccess,
    /// What it is for, for grouping in help.
    pub group: CommandGroup,
    /// What it does.
    pub summary: &'static str,
    /// The argument, as a person would write it. None when it takes none.
    pub argument: Option<&'static str>,
}

/// Every in-thread command, and who may run it.
pub static COMMANDS: &[(&str, CommandMeta)] = &[
    // Change what the session is doing. The owner's alone: a guest is invited
    // to help, not to end the session.
    (
        "!stop",
        CommandMeta {
            access: Owner,
            group: Session,
            summary: "end the session and close the thread",
            argument: None,
        },
    ),
    (
        "!interrupt",
        CommandMeta {
            access: Owner,
            group: Session,
            summary: "abort the running turn",
            argument: None,
        },
    ),
    (
        "!steer",
        CommandMeta {
            access: Owner,
            group: Session,
            summary: "redirect the running turn",
            argument: Some("<instruction>"),
        },
    ),
    (
        "!then",
        CommandMeta {
            access: Guest,
            group: Session,
            summary: "hold a prompt until the running turn finishes",
            argument: Some("<prompt>"),
        },
    ),
    (
        "!pr",
        CommandMeta {
            access: Owner,
            group: Session,
            summary: "open a pull request for the work on this branch",
            argument: Some("<title>"),
        },
    ),
    (
        "!compact",
        CommandMeta {
            access: Owner,
            group: Session,
            summary: "summarise the conversation so far to free up context",
            argument: None,
        },
    ),
    (
        "!model",
        CommandMeta {
            access: Owner,
            group: Session,
            summary: "show the models available, or switch to one",
            argument: Some("[name]"),
        },
    ),
    // Who may take part. The owner's alone, for the same reason.
    (
        "!allow",
        CommandMeta {
            access: Owner,
            group: People,
            summary: "let another account take part in this thread",
            argument: Some("<user>"),
        },
    ),
    (
        "!deny",
        CommandMeta {
            access: Owner,
            group: People,
            summary: "withdraw another account from this thread",
            argument: Some("<user>"),
        },
    ),
    (
        "!guests",
        CommandMeta {
            access: Guest,
            group: People,
            summary: "who may take part in this thread",
            argument: None,
        },
    ),
    // What is remembered, which is what the agent is told before it answers. A
    // guest may ask, because the block about whoever is speaking is given to
    // the agent anyway and seeing it is no more than reading back what was
    // already said. Forgetting is the owner's: it changes what every later
    // session is told, not just this one.
    (
        "!facts",
        CommandMeta {
            access: Guest,
            group: People,
            summary: "what is remembered about somebody, or this project",
            argument: Some("[@somebody|project]"),
        },
    ),
    (
        "!forget",
        CommandMeta {
            access: Owner,
            group: People,
            summary: "drop what is remembered about somebody, or this project",
            argument: Some("<@somebody|project>"),
        },
    ),
    // Read the project's contents. Open to invited guests, because reading the
    // code is most of what taking part in a session means.
    (
        "!ls",
        CommandMeta {
            access: Guest,
            group: Project,
            summary: "list a directory",
            argument: Some("[path]"),
        },
    ),
    (
        "!cat",
        CommandMeta {
            access: Guest,
            group: Project,
            summary: "show a file",
            argument: Some("<path>"),
        },
    ),
    (
        "!file",
        CommandMeta {
            access: Guest,
            group: Project,
            summary: "upload a file",
            argument: Some("<path>"),
        },
    ),
    (
        "!pwd",
        CommandMeta {
            access: Guest,
            group: Project,
            summary: "show the project this session works in",
            argument: None,
        },
    ),
    // Harmless, or scoped to the caller's own data.
    (
        "!status",
        CommandMeta {
            access: Anyone,
            group: You,
            summary: "session state and queue",
            argument: None,
        },
    ),
    (
        "!help",
        CommandMeta {
            access: Anyone,
            group: You,
            summary: "list these commands",
            argument: None,
        },
    ),
    // About the provider rather than about a session, so the daemon answers it
    // and it works in the channel as well as in a thread.
    (
        "!usage",
        CommandMeta {
            access: Anyone,
            group: You,
            summary: "how much of the provider's usage window is left",
            argument: None,
        },
    ),
    // Acts on the machine, so the daemon answers it against its own list and a
    // session never sees it.
    (
        "!shutdown",
        CommandMeta {
            access: Host,
            group: You,
            summary: "power off the host this daemon runs on",
            argument: None,
        },
    ),
];

/// What marks a message the agent is never told about.
///
/// Three marks rather than one, so it cannot be typed by accident and cannot
/// collide with a command: a command is matched by its whole first word, and
/// no command begins with this.
pub const ASIDE: &str = "!!!";

/// The first whitespace-separated word, which is what names a command.
pub fn first_word(content: &str) -> &str {
    content.split_whitespace().next().unwrap_or("")
}

/// Whether a message names a command rather than something to send the agent.
pub fn is_command(content: &str) -> bool {
    let first = first_word(content);
    COMMANDS.iter().any(|(name, _)| *name == first)
}

/// Whether a message is addressed to a bot rather than to the agent.
///
/// `!` is the conventional prefix for a chat bot, and a served channel is
/// usually shared with others that answer to it. A message beginning with it
/// is a command: this system's, or somebody else's. Either way it is not a
/// prompt, and forwarding an unknown one to the agent means paying a model to
/// read a command meant for a different bot.
pub fn is_addressed_to_bot(content: &str) -> bool {
    content.trim_start().starts_with('!')
}

/// Whether a message is meant for the people in the thread, not the agent.
pub fn is_aside(content: &str) -> bool {
    content.trim_start().starts_with(ASIDE)
}

fn is_word_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

/// Every place `word` starts at a left boundary in `text`.
///
/// Only the left side is checked here; each pattern decides what may follow,
/// which is what a boundary before the verb but not after it means.
fn word_starts(text: &str, word: &str) -> Vec<usize> {
    let bytes = text.as_bytes();
    let mut starts = Vec::new();
    let mut from = 0;
    while let Some(found) = text[from..].find(word) {
        let start = from + found;
        if start == 0 || !is_word_byte(bytes[start - 1]) {
            starts.push(start);
        }
        from = start + 1;
    }
    starts
}

/// Whether `verb requests` is written, with an optional space or dash between.
///
/// The noun carries no left boundary of its own: `pullrequests` reads the
/// same as `pull requests`, as the pattern this reimplements has it.
fn verb_with_requests(text: &str, verb: &str) -> bool {
    let bytes = text.as_bytes();
    for start in word_starts(text, verb) {
        let mut after = start + verb.len();
        if text[after..]
            .chars()
            .next()
            .is_some_and(|character| character.is_whitespace() || character == '-')
        {
            after += text[after..]
                .chars()
                .next()
                .expect("checked above")
                .len_utf8();
        }
        if !text[after..].starts_with("request") {
            continue;
        }
        let mut end = after + "request".len();
        if bytes.get(end) == Some(&b's') {
            end += 1;
        }
        if end == bytes.len() || !is_word_byte(bytes[end]) {
            return true;
        }
    }
    false
}

/// Whether a message asks for a pull request.
///
/// The agent is told to request one only when it was asked to, and mostly
/// obeys. This is what stands behind the instruction, because the cost of a
/// model mistaking a finished branch for a request to publish it falls on
/// whoever maintains the repository, not on the session that got it wrong.
pub fn asks_for_pull_request(content: &str) -> bool {
    let lower = content.to_lowercase();
    if verb_with_requests(&lower, "pull") || verb_with_requests(&lower, "merge") {
        return true;
    }
    let bytes = lower.as_bytes();
    for start in word_starts(&lower, "pr") {
        let mut end = start + 2;
        if bytes.get(end) == Some(&b's') {
            end += 1;
        }
        if end == bytes.len() || !is_word_byte(bytes[end]) {
            return true;
        }
    }
    false
}

/// Reads an account id from a mention or a bare id.
///
/// A client sends a mention as `<@id>`, and somebody typing by hand will paste
/// the id on its own, so both are accepted. Anything else is not an account,
/// and is refused rather than guessed at: `!deny` acting on the wrong id
/// removes the wrong person.
pub fn parse_user_id(text: &str) -> Option<String> {
    fn is_id(digits: &str) -> bool {
        (5..=25).contains(&digits.len()) && digits.bytes().all(|byte| byte.is_ascii_digit())
    }

    let trimmed = text.trim();
    if let Some(rest) = trimmed.strip_prefix("<@") {
        let rest = rest.strip_prefix('!').unwrap_or(rest);
        if let Some(digits) = rest.strip_suffix('>') {
            return if is_id(digits) {
                Some(digits.to_owned())
            } else {
                None
            };
        }
        return None;
    }
    if is_id(trimmed) {
        return Some(trimmed.to_owned());
    }
    None
}

/// What a session knows about the account that sent a message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Standing {
    /// The account that opened the session.
    pub is_owner: bool,
    /// An account the owner invited to this thread.
    pub is_guest: bool,
}

/// Whether an account may run a command at the given access level.
///
/// `host` is never granted here. It is answered by the daemon, which is the
/// only thing that knows who may turn the machine off, and a session that
/// answered it would be deciding on the machine's behalf.
pub fn may_run(access: CommandAccess, standing: Standing) -> bool {
    match access {
        Host => false,
        Anyone => true,
        Guest => standing.is_owner || standing.is_guest,
        Owner => standing.is_owner,
    }
}

/// Answers a command that needs no session, or nothing when it needs one.
///
/// Used where there is nothing to run a command against: a message in the
/// channel rather than in a thread. Without it `!help` starts a session and is
/// answered by the agent, or refused because the provider is busy, neither of
/// which is an answer to "what can I type".
pub fn answer_without_session(content: &str) -> Option<String> {
    (first_word(content) == "!help").then(help_text)
}

const GROUP_TITLES: [(CommandGroup, &str); 4] = [
    (Session, "THE SESSION"),
    (People, "WHO TAKES PART"),
    (Project, "THE PROJECT"),
    (You, "YOU"),
];

/// How access is shown, when it is worth showing at all.
fn access_note(access: CommandAccess) -> &'static str {
    match access {
        Host => "named accounts",
        Owner => "owner",
        Guest => "invited",
        Anyone => "",
    }
}

/// The command list, grouped and aligned.
///
/// Rendered into one fenced block: a fence is shown in a monospaced font, so
/// the columns line up for every reader, which a bulleted list does not.
/// Nothing here is decorated with a glyph, because every glyph this system
/// emits names one state and none of them names "a command exists".
pub fn help_text() -> String {
    let spelled = |name: &str, meta: &CommandMeta| match meta.argument {
        None => name.to_owned(),
        Some(argument) => format!("{name} {argument}"),
    };
    let width = COMMANDS
        .iter()
        .map(|(name, meta)| spelled(name, meta).chars().count())
        .max()
        .unwrap_or(0);

    let mut lines: Vec<String> = Vec::new();
    for (group, title) in GROUP_TITLES {
        let in_group: Vec<&(&str, CommandMeta)> = COMMANDS
            .iter()
            .filter(|(_, meta)| meta.group == group)
            .collect();
        if in_group.is_empty() {
            continue;
        }
        if !lines.is_empty() {
            lines.push(String::new());
        }
        lines.push(title.to_owned());
        for (name, meta) in in_group {
            let note = access_note(meta.access);
            let spelled_name = spelled(name, meta);
            let padding = " ".repeat(width - spelled_name.chars().count());
            lines.push(format!(
                "  {spelled_name}{padding}  {}{}",
                meta.summary,
                if note.is_empty() {
                    String::new()
                } else {
                    format!(" ({note})")
                },
            ));
        }
    }

    [
        "Type these in the thread, or use the same name as a slash command.".to_owned(),
        "```".to_owned(),
    ]
    .into_iter()
    .chain(lines)
    .chain([
        "```".to_owned(),
        "Anything else is a prompt for the agent. A message starting `!!!` is an".to_owned(),
        "aside: the agent is never told about it.".to_owned(),
    ])
    .collect::<Vec<_>>()
    .join("\n")
}

#[cfg(test)]
mod tests;
