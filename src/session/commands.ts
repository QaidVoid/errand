/**
 * What can be typed at a session, who may type it, and how it reads.
 *
 * Kept apart from the session that runs the commands so that the rule about
 * who may do what is one table rather than a check scattered through a
 * lifecycle. Everything here is pure, so the access rule can be tested against
 * every command without a session, a sandbox, or a connection.
 */

/**
 * Who may run a command.
 *
 * `anyone` is any permitted account. `guest` adds the owner, the operators,
 * and anyone the owner has invited to this thread. `owner` is the owner and
 * the operators alone. `host` acts on the machine rather than on a session, so
 * no session role grants it.
 *
 * Declared per command rather than kept in a separate list, so a command added
 * later cannot quietly default to being open to everyone.
 */
export type CommandAccess = "anyone" | "guest" | "owner" | "host";

/** What a command is for, so help reads as groups rather than one long list. */
export type CommandGroup = "session" | "people" | "project" | "you";

/** One command: who may run it, what it does, and what it takes. */
export interface CommandMeta {
  access: CommandAccess;
  group: CommandGroup;
  summary: string;
  /** The argument, as a person would write it. Absent when it takes none. */
  argument?: string;
}

/** Every in-thread command, and who may run it. */
export const COMMANDS: Record<string, CommandMeta> = {
  // Change what the session is doing. The owner's alone: a guest is invited to
  // help, not to end the session.
  "!stop": { access: "owner", group: "session", summary: "end the session and close the thread" },
  "!interrupt": { access: "owner", group: "session", summary: "abort the running turn" },
  "!steer": {
    access: "owner",
    group: "session",
    summary: "redirect the running turn",
    argument: "<instruction>",
  },
  "!then": {
    access: "guest",
    group: "session",
    summary: "hold a prompt until the running turn finishes",
    argument: "<prompt>",
  },
  "!pr": {
    access: "owner",
    group: "session",
    summary: "open a pull request for the work on this branch",
    argument: "<title>",
  },
  "!compact": {
    access: "owner",
    group: "session",
    summary: "summarise the conversation so far to free up context",
  },
  "!model": {
    access: "owner",
    group: "session",
    summary: "show the models available, or switch to one",
    argument: "[name]",
  },

  // Who may take part. The owner's alone, for the same reason.
  "!allow": {
    access: "owner",
    group: "people",
    summary: "let another account take part in this thread",
    argument: "<user>",
  },
  "!deny": {
    access: "owner",
    group: "people",
    summary: "withdraw another account from this thread",
    argument: "<user>",
  },
  "!guests": { access: "guest", group: "people", summary: "who may take part in this thread" },
  /**
   * What is remembered, which is what the agent is told before it answers.
   *
   * A guest may ask, because the block about whoever is speaking is given to
   * the agent anyway and seeing it is no more than reading back what was
   * already said. Forgetting is the owner's: it changes what every later
   * session is told, not just this one.
   */
  "!facts": {
    access: "guest",
    group: "people",
    summary: "what is remembered about somebody, or this project",
    argument: "[@somebody|project]",
  },
  "!forget": {
    access: "owner",
    group: "people",
    summary: "drop what is remembered about somebody, or this project",
    argument: "<@somebody|project>",
  },

  // Read the project's contents. Open to invited guests, because reading the
  // code is most of what taking part in a session means.
  "!ls": { access: "guest", group: "project", summary: "list a directory", argument: "[path]" },
  "!cat": { access: "guest", group: "project", summary: "show a file", argument: "<path>" },
  "!file": { access: "guest", group: "project", summary: "upload a file", argument: "<path>" },
  "!pwd": { access: "guest", group: "project", summary: "show the project this session works in" },

  // Harmless, or scoped to the caller's own data.
  "!status": { access: "anyone", group: "you", summary: "session state and queue" },
  "!help": { access: "anyone", group: "you", summary: "list these commands" },
  /**
   * About the provider rather than about a session, so the daemon answers it
   * and it works in the channel as well as in a thread.
   */
  "!usage": {
    access: "anyone",
    group: "you",
    summary: "how much of the provider's usage window is left",
  },

  /**
   * Acts on the machine, so the daemon answers it against its own list and a
   * session never sees it.
   */
  "!shutdown": {
    access: "host",
    group: "you",
    summary: "power off the host this daemon runs on",
  },
};

/**
 * What marks a message the agent is never told about.
 *
 * Three marks rather than one, so it cannot be typed by accident and cannot
 * collide with a command: a command is matched by its whole first word, and no
 * command begins with this.
 */
export const ASIDE = "!!!";

/** The first whitespace-separated word, which is what names a command. */
export function firstWord(content: string): string {
  return content.trim().split(/\s+/)[0] ?? "";
}

/** Whether a message names a command rather than something to send the agent. */
export function isCommand(content: string): boolean {
  return firstWord(content) in COMMANDS;
}

/**
 * Whether a message is addressed to a bot rather than to the agent.
 *
 * `!` is the conventional prefix for a chat bot, and a served channel is
 * usually shared with others that answer to it. A message beginning with it is
 * a command: this system's, or somebody else's. Either way it is not a prompt,
 * and forwarding an unknown one to the agent means paying a model to read a
 * command meant for a different bot.
 */
export function isAddressedToBot(content: string): boolean {
  return content.trimStart().startsWith("!");
}

/** Whether a message is meant for the people in the thread, not the agent. */
export function isAside(content: string): boolean {
  return content.trimStart().startsWith(ASIDE);
}

/**
 * Whether a message asks for a pull request.
 *
 * The agent is told to request one only when it was asked to, and mostly
 * obeys. This is what stands behind the instruction, because the cost of a
 * model mistaking a finished branch for a request to publish it falls on
 * whoever maintains the repository, not on the session that got it wrong.
 */
export function asksForPullRequest(content: string): boolean {
  return /\bpull[\s-]?requests?\b|\bPRs?\b|\bmerge[\s-]?requests?\b/i.test(content);
}

/**
 * Reads an account id from a mention or a bare id.
 *
 * A client sends a mention as `<@id>`, and somebody typing by hand will paste
 * the id on its own, so both are accepted. Anything else is not an account,
 * and is refused rather than guessed at: `!deny` acting on the wrong id
 * removes the wrong person.
 */
export function parseUserId(text: string): string | undefined {
  const trimmed = text.trim();
  const mention = /^<@!?(\d{5,25})>$/.exec(trimmed);
  if (mention !== null) return mention[1];
  return /^\d{5,25}$/.test(trimmed) ? trimmed : undefined;
}

/** What a session knows about the account that sent a message. */
export interface Standing {
  /** The account that opened the session. */
  isOwner: boolean;
  /** An account the owner invited to this thread. */
  isGuest: boolean;
}

/**
 * Whether an account may run a command at the given access level.
 *
 * `host` is never granted here. It is answered by the daemon, which is the
 * only thing that knows who may turn the machine off, and a session that
 * answered it would be deciding on the machine's behalf.
 */
export function mayRun(access: CommandAccess, standing: Standing): boolean {
  if (access === "host") return false;
  if (access === "anyone") return true;
  if (access === "guest") return standing.isOwner || standing.isGuest;
  return standing.isOwner;
}

/**
 * Answers a command that needs no session, or undefined when it needs one.
 *
 * Used where there is nothing to run a command against: a message in the
 * channel rather than in a thread. Without it `!help` starts a session and is
 * answered by the agent, or refused because the provider is busy, neither of
 * which is an answer to "what can I type".
 */
export function answerWithoutSession(content: string): string | undefined {
  return firstWord(content) === "!help" ? helpText() : undefined;
}

/**
 * Commands the daemon answers itself, wherever they are typed.
 *
 * They are about the machine or the provider rather than about a session, so
 * a thread is neither needed to run one nor a reason to be allowed to.
 */
export const DAEMON_COMMANDS = ["!shutdown", "!usage"] as const;

/** Whether the daemon answers this command rather than a session. */
export function isDaemonCommand(content: string): boolean {
  return (DAEMON_COMMANDS as readonly string[]).includes(firstWord(content));
}

const GROUP_TITLES: [CommandGroup, string][] = [
  ["session", "THE SESSION"],
  ["people", "WHO TAKES PART"],
  ["project", "THE PROJECT"],
  ["you", "YOU"],
];

/** How access is shown, when it is worth showing at all. */
const ACCESS_NOTE: Record<CommandAccess, string> = {
  host: "named accounts",
  owner: "owner",
  guest: "invited",
  anyone: "",
};

/**
 * The command list, grouped and aligned.
 *
 * Rendered into one fenced block: a fence is shown in a monospaced font, so
 * the columns line up for every reader, which a bulleted list does not.
 * Nothing here is decorated with a glyph, because every glyph this system
 * emits names one state and none of them names "a command exists".
 */
export function helpText(commands: Record<string, CommandMeta> = COMMANDS): string {
  const entries = Object.entries(commands);
  const spelled = (name: string, meta: CommandMeta) =>
    `${name}${meta.argument === undefined ? "" : ` ${meta.argument}`}`;
  const width = Math.max(...entries.map(([name, meta]) => spelled(name, meta).length));

  const lines: string[] = [];
  for (const [group, title] of GROUP_TITLES) {
    const inGroup = entries.filter(([, meta]) => meta.group === group);
    if (inGroup.length === 0) continue;
    if (lines.length > 0) lines.push("");
    lines.push(title);
    for (const [name, meta] of inGroup) {
      const note = ACCESS_NOTE[meta.access];
      lines.push(
        `  ${spelled(name, meta).padEnd(width)}  ${meta.summary}${note === "" ? "" : ` (${note})`}`,
      );
    }
  }

  return [
    "Type these in the thread, or use the same name as a slash command.",
    "```",
    ...lines,
    "```",
    "Anything else is a prompt for the agent. A message starting `!!!` is an",
    "aside: the agent is never told about it.",
  ].join("\n");
}
