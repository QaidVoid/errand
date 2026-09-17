//! Native slash commands.
//!
//! A second front door onto the same commands, not a second implementation:
//! an interaction is translated into the identical text form the in-thread
//! commands already use and runs through the same code, so access rules and
//! behaviour cannot drift between the two.
//!
//! Registration is guild scoped, which takes effect immediately rather than
//! waiting for global propagation.

use serenity::builder::{
    CreateCommand, CreateCommandOption, CreateInteractionResponse, CreateInteractionResponseMessage,
};
use serenity::client::Context;
use serenity::model::application::CommandDataOption;
use serenity::model::application::CommandInteraction;
use serenity::model::id::GuildId;

use crate::log::{LogValue, Logger, fields};
use crate::session::commands::{COMMANDS, CommandAccess};

/// What the slash commands are built from, before the library sees them.
pub struct CommandDefinitions {
    /// The guild-scoped definitions to register.
    pub commands: Vec<CreateCommand>,
}

/// Commands that take a path or an instruction, so the client prompts for it.
const TEXT_OPTION: &[(&str, &str, &str, bool)] = &[
    (
        "!ls",
        "path",
        "directory to list, relative to the project",
        false,
    ),
    (
        "!cat",
        "path",
        "file to show, relative to the project",
        true,
    ),
    (
        "!file",
        "path",
        "file to upload, relative to the project",
        true,
    ),
    ("!steer", "instruction", "what to do instead", true),
    ("!then", "instruction", "what to do after this turn", true),
    ("!pr", "title", "title for the pull request", true),
];

/// Commands that name an account, so the client offers a member picker.
const USER_OPTION: &[(&str, &str)] = &[
    ("!allow", "who may take part in this thread"),
    ("!deny", "who to withdraw from this thread"),
];

/// The names of the options whose values are read back in `translate`.
const OPTION_NAMES: [&str; 3] = ["path", "instruction", "title"];

/// Raised when the service will not accept the commands, with what to do.
#[derive(Debug, thiserror::Error)]
#[error(
    "the chat service refused to register slash commands: {detail}. The bot needs the \
     applications.commands scope, which is granted when it is invited. Re-invite it with both \
     bot and applications.commands selected, then start again."
)]
pub struct CommandRegistrationError {
    pub detail: String,
}

/// Says who may run a command, briefly enough for the description limit.
fn describe(access: CommandAccess, summary: &str) -> String {
    match access {
        CommandAccess::Host => format!("{summary} (named accounts only)"),
        CommandAccess::Owner => format!("{summary} (owner only)"),
        CommandAccess::Guest => format!("{summary} (owner and invited)"),
        CommandAccess::Anyone => summary.to_owned(),
    }
}

/// The slash name an in-thread command is registered under.
fn slash_name(name: &str) -> &str {
    name.strip_prefix('!').unwrap_or(name)
}

/// Builds the slash command definitions from the single command table.
pub fn build_commands() -> Vec<CreateCommand> {
    COMMANDS
        .iter()
        .map(|(name, meta)| {
            let mut builder = CreateCommand::new(slash_name(name))
                .description(describe(meta.access, meta.summary));

            for (command, option_name, description, required) in TEXT_OPTION {
                if *command == *name {
                    builder = builder.add_option(
                        CreateCommandOption::new(
                            serenity::model::application::CommandOptionType::String,
                            *option_name,
                            *description,
                        )
                        .required(*required),
                    );
                }
            }
            for (command, description) in USER_OPTION {
                if *command == *name {
                    builder = builder.add_option(
                        CreateCommandOption::new(
                            serenity::model::application::CommandOptionType::User,
                            "user",
                            *description,
                        )
                        .required(true),
                    );
                }
            }
            builder
        })
        .collect()
}

/// An interaction turned into the text command form the session already runs.
#[derive(Debug, Clone, PartialEq)]
pub struct TranslatedCommand {
    /// The thread the command was used in, or none when used outside one.
    pub thread_id: Option<String>,
    pub user_id: String,
    pub user_name: String,
    /// The command as text, exactly as an in-thread message would have been.
    pub content: String,
}

/// Translates an interaction into the text command the session understands.
///
/// The pieces are passed rather than the library's whole interaction, so the
/// translation is testable without a connection.
pub fn translate(
    command_name: &str,
    options: &[CommandDataOption],
    channel_id: &str,
    is_thread: bool,
    user_id: &str,
    user_name: &str,
) -> TranslatedCommand {
    let string_named = |wanted: &str| {
        options
            .iter()
            .find(|option| option.name == wanted)
            .and_then(|option| match &option.value {
                serenity::model::application::CommandDataOptionValue::String(text) => {
                    Some(text.clone())
                }
                _ => None,
            })
    };
    let argument = OPTION_NAMES
        .iter()
        .find_map(|name| string_named(name))
        .or_else(|| {
            options
                .iter()
                .find(|option| option.name == "user")
                .and_then(|option| match &option.value {
                    serenity::model::application::CommandDataOptionValue::User(id) => {
                        Some(id.get().to_string())
                    }
                    _ => None,
                })
        })
        .unwrap_or_default();

    TranslatedCommand {
        thread_id: is_thread.then(|| channel_id.to_owned()),
        user_id: user_id.to_owned(),
        user_name: user_name.to_owned(),
        content: if argument.is_empty() {
            format!("!{command_name}")
        } else {
            format!("!{command_name} {argument}")
        },
    }
}

/// Registers the commands for one guild.
///
/// The HTTP client must already know the application id, which the client
/// builder sets from the gateway handshake. Fails with
/// [`CommandRegistrationError`] when the service refuses, which in practice
/// means the bot was invited without the commands scope.
pub async fn register_commands(
    http: &serenity::http::Http,
    guild_id: GuildId,
    log: &Logger,
) -> Result<(), CommandRegistrationError> {
    let count = COMMANDS.len();
    match guild_id.set_commands(http, build_commands()).await {
        Ok(_) => {
            log.info(
                "registered slash commands",
                &fields([
                    ("guild", LogValue::from(guild_id.get().to_string())),
                    ("count", LogValue::from(count)),
                ]),
            );
            Ok(())
        }
        Err(error) => Err(CommandRegistrationError {
            detail: error.to_string(),
        }),
    }
}

/// Acknowledges an interaction privately.
///
/// The command's own output goes to the thread, where everyone following
/// along can see it and where it is kept. This only stops the service
/// reporting the interaction as having failed, so it is deliberately brief
/// and private.
pub async fn acknowledge(ctx: &Context, interaction: &CommandInteraction, text: &str) {
    let _ = interaction
        .create_response(
            ctx,
            CreateInteractionResponse::Message(
                CreateInteractionResponseMessage::new()
                    .content(text)
                    .ephemeral(true),
            ),
        )
        .await;
}

#[cfg(test)]
#[path = "commands/tests.rs"]
mod tests;

/// Says who may run a command, exposed for the tests' sake.
#[cfg(test)]
pub(crate) fn describe_for_test(access: CommandAccess, summary: &str) -> String {
    describe(access, summary)
}
