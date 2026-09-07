/**
 * Native slash commands.
 *
 * A second front door onto the same commands, not a second implementation: an
 * interaction is translated into the identical text form the in-thread
 * commands already use and runs through the same code, so access rules and
 * behaviour cannot drift between the two.
 *
 * Registration is guild scoped, which takes effect immediately rather than
 * waiting for global propagation.
 */

import {
  type ChatInputCommandInteraction,
  type Client,
  type Guild,
  MessageFlags,
  REST,
  Routes,
  SlashCommandBuilder,
} from "discord.js";
import type { Logger } from "../log.ts";
import { type CommandAccess, COMMANDS } from "../session/commands.ts";

/** Commands that take a path or an instruction, so the client prompts for it. */
const TEXT_OPTION: Record<string, { name: string; description: string; required: boolean }> = {
  "!ls": {
    name: "path",
    description: "directory to list, relative to the project",
    required: false,
  },
  "!cat": { name: "path", description: "file to show, relative to the project", required: true },
  "!file": { name: "path", description: "file to upload, relative to the project", required: true },
  "!steer": { name: "instruction", description: "what to do instead", required: true },
  "!then": { name: "instruction", description: "what to do after this turn", required: true },
  "!pr": { name: "title", description: "title for the pull request", required: true },
};

/** Commands that name an account, so the client offers a member picker. */
const USER_OPTION: Record<string, string> = {
  "!allow": "who may take part in this thread",
  "!deny": "who to withdraw from this thread",
};

/** Raised when the service will not accept the commands, with what to do. */
export class CommandRegistrationError extends Error {
  constructor(detail: string) {
    super(
      [
        `the chat service refused to register slash commands: ${detail}`,
        "The bot needs the applications.commands scope, which is granted when it is invited.",
        "Re-invite it with both bot and applications.commands selected, then start again.",
      ].join(" "),
    );
    this.name = "CommandRegistrationError";
  }
}

/** Says who may run a command, briefly enough for the description limit. */
function describe(access: CommandAccess, summary: string): string {
  if (access === "host") return `${summary} (named accounts only)`;
  if (access === "owner") return `${summary} (owner only)`;
  if (access === "guest") return `${summary} (owner and invited)`;
  return summary;
}

/** Builds the slash command definitions from the single command table. */
export function buildCommands(): SlashCommandBuilder[] {
  return Object.entries(COMMANDS).map(([name, meta]) => {
    const builder = new SlashCommandBuilder()
      .setName(name.slice(1))
      .setDescription(describe(meta.access, meta.summary));

    const option = TEXT_OPTION[name];
    if (option !== undefined) {
      builder.addStringOption((input) =>
        input.setName(option.name).setDescription(option.description).setRequired(option.required)
      );
    }

    const user = USER_OPTION[name];
    if (user !== undefined) {
      builder.addUserOption((input) =>
        input.setName("user").setDescription(user).setRequired(true)
      );
    }
    return builder;
  });
}

/**
 * Registers the commands for one guild.
 *
 * @throws CommandRegistrationError when the service refuses, which in practice
 *   means the bot was invited without the commands scope.
 */
export async function registerCommands(
  token: string,
  applicationId: string,
  guild: Guild,
  log: Logger,
): Promise<void> {
  const rest = new REST().setToken(token);
  const body = buildCommands().map((command) => command.toJSON());

  try {
    await rest.put(Routes.applicationGuildCommands(applicationId, guild.id), { body });
    log.info("registered slash commands", { guild: guild.id, count: body.length });
  } catch (error) {
    throw new CommandRegistrationError(String(error));
  }
}

/** An interaction turned into the text command form the session already runs. */
export interface TranslatedCommand {
  /** The thread the command was used in, or undefined when used outside one. */
  threadId: string | undefined;
  userId: string;
  userName: string;
  /** The command as text, exactly as an in-thread message would have been. */
  content: string;
}

/** Translates an interaction into the text command the session understands. */
export function translate(interaction: ChatInputCommandInteraction): TranslatedCommand {
  const argument = interaction.options.getString("path") ??
    interaction.options.getString("instruction") ??
    interaction.options.getString("title") ??
    interaction.options.getUser("user")?.id ??
    "";
  const channel = interaction.channel;

  return {
    threadId: channel?.isThread() === true ? channel.id : undefined,
    userId: interaction.user.id,
    userName: interaction.user.displayName ?? interaction.user.username,
    content: `!${interaction.commandName}${argument.length > 0 ? ` ${argument}` : ""}`,
  };
}

/**
 * Acknowledges an interaction privately.
 *
 * The command's own output goes to the thread, where everyone following along
 * can see it and where it is kept. This only stops the service reporting the
 * interaction as having failed, so it is deliberately brief and private.
 */
export async function acknowledge(
  interaction: ChatInputCommandInteraction,
  text: string,
): Promise<void> {
  await interaction.reply({ content: text, flags: MessageFlags.Ephemeral }).catch(() => undefined);
}

/** The application id the bot is running as, for registering commands. */
export function applicationId(client: Client): string | undefined {
  return client.application?.id ?? client.user?.id;
}
