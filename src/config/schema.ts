/**
 * The shape of the daemon's configuration, and its documented defaults.
 *
 * Every optional field on disk is a required field here, filled from
 * {@link DEFAULTS}, so nothing downstream reasons about absence.
 *
 * This grows with the daemon. A field is added when something reads it, not in
 * anticipation of something that might.
 */

/** Which sandbox confines a session's agent. There is no unsandboxed option. */
export type SandboxBackend = "podman" | "bailey";

/** How much network a session gets. `none` disables it entirely. */
export type NetworkMode = "restricted" | "none";

/** Chat connection and who may drive the bot. */
export interface ChatConfig {
  /** Bot token. Secret. Never enters a sandbox. */
  token: string;
  /** The single channel the daemon serves. Everything else is ignored. */
  channelId: string;
  /**
   * Accounts permitted to drive sessions. Must not be empty.
   *
   * The single entry {@link ALLOW_EVERY_USER} opens it to everyone who can
   * post in the served channel. That is a deliberate, visible choice: an empty
   * list is still a refusal to start, so nobody arrives at open access by
   * leaving a field blank.
   */
  allowedUserIds: string[];
  /**
   * Accounts refused before anything else is considered.
   *
   * Ahead of the allowlist and of any session role, so excluding somebody is
   * one decision rather than an audit of every list they appear on.
   */
  blockedUserIds: string[];
  /** Accounts that may control any session, not only their own. */
  operatorUserIds: string[];
}

/** Which model the agent talks to, and the credential it reaches it with. */
export interface AgentConfig {
  /** Provider id, such as `anthropic` or `zai-coding-cn`. */
  provider: string;
  /** Model pattern or id. Omit to use the provider's default. */
  model: string | undefined;
  /**
   * Model an image is shown to when the working model cannot see one.
   *
   * Omit and the cheapest model of the same provider that accepts images is
   * used. Naming one here is how that choice is made deliberately.
   */
  visionModel: string | undefined;
  /** Environment variable the agent reads, such as `ANTHROPIC_API_KEY`. */
  credentialName: string;
  /** The credential value. Secret. */
  credential: string;
}

/**
 * The GitHub identity a session works with, when one is configured.
 *
 * Optional: a session with no GitHub configuration still runs, and simply has
 * no credential to reach a repository with.
 */
export interface GithubConfig {
  /** Token the agent authenticates with. Secret, and reachable by the agent. */
  token: string;
  /**
   * Name commits are authored with. Free text: where a fork lands is read back
   * from the API, so this does not have to be the bot's login.
   */
  userName: string;
  /** Email commits are authored with. */
  userEmail: string;
}

/** What a session may consume, and what the backend enforces. */
export interface SandboxConfig {
  /** Which backend confines sessions. */
  backend: SandboxBackend;
  /**
   * Refuse to start when the backend cannot enforce every configured
   * guarantee on this host, rather than reporting the gap and continuing.
   */
  requireFullEnforcement: boolean;
  /** Network exposure granted to a session. */
  network: NetworkMode;
  /** Container image the podman backend runs. Inert under bailey. */
  image: string;
  /** Memory ceiling per session, in size syntax such as `4g`. */
  memory: string;
  /** CPU ceiling per session, in cores. */
  cpus: number;
  /** Process count ceiling per session. */
  pids: number;
  /** Largest single file a session may write, in size syntax. */
  fileMax: string;
  /**
   * How much a session may add to its project and state together.
   *
   * Measured rather than enforced, because no backend caps what a process tree
   * writes in aggregate without root. Passing it ends the session.
   */
  disk: string;
  /** How often a session's disk use is measured, in milliseconds. */
  diskCheckMs: number;
  /** How long a sandbox may take to stop before it is killed. */
  gracePeriodMs: number;
}

/**
 * Who may power off the host from a chat message.
 *
 * Off unless the list has somebody in it. This is the one command that acts on
 * the machine rather than on a session, so it is not covered by any session
 * role: whoever starts a thread owns it, and owning a thread is no reason to
 * be able to turn the computer off.
 */
export interface ShutdownConfig {
  /** Account ids permitted to power off the host. */
  allowedUserIds: string[];
}

/** How much of the agent's activity reaches the thread. */
export interface OutputConfig {
  /** Whether tool output bodies are posted, not just that a tool ran. */
  forwardToolOutput: boolean;
  /** Longest tool output posted before it is truncated and marked as such. */
  maxToolOutputChars: number;
  /**
   * Largest attached file taken into a session, in bytes.
   *
   * Well under what the chat service itself allows, on purpose: the limit is
   * what is sensible to hand an agent, not what can be uploaded.
   */
  maxAttachmentBytes: number;
  /** Most attached files taken from one message. The rest are refused. */
  maxAttachmentsPerMessage: number;
  /**
   * Post a diff after the agent changes a file.
   *
   * A diff shows intent rather than contents, which is both smaller and less
   * likely to put something private in a channel than uploading whole files.
   */
  postDiffs: boolean;
}

/**
 * The local web interface.
 *
 * Absent means no listener at all, which is how the daemon behaves without
 * one. There is no login: the address it binds to is the access control, and
 * that address is checked rather than trusted.
 */
export interface WebConfig {
  /** Address to bind to. Must be loopback, private, or a tailnet address. */
  host: string;
  port: number;
  /**
   * When true the interface may watch and read but not start, prompt, or
   * control anything.
   */
  observer: boolean;
  /**
   * Where the interface is reachable from outside, such as behind a tunnel.
   *
   * Used to link a session from somewhere that is not the chat service, so a
   * pull request can name the conversation that asked for it. Absent when the
   * interface is not published, in which case no such link is offered.
   */
  publicUrl: string | undefined;
}

/** Bounds on how much work exists at once. */
export interface LimitsConfig {
  /** Sessions that may have a model turn in flight simultaneously. */
  maxConcurrentTurns: number;
  /** Sessions that may exist at all. */
  maxLiveSessions: number;
  /** Prompts that may wait for a turn slot. */
  maxQueueLength: number;
  /** How long a queued prompt may wait before it expires unsent. */
  maxQueueWaitMs: number;
}

/** Deadlines that end or unblock a session. */
export interface TimeoutsConfig {
  /** No message and no agent activity for this long ends the session. */
  idleMs: number;
  /** How long the agent has to become ready before it is abandoned. */
  startupMs: number;
  /** How long a question posted to a thread waits for an answer. */
  questionMs: number;
  /** How long an abort waits for the agent before it is forced. */
  abortMs: number;
}

/** The fully resolved configuration the daemon runs on. */
export interface Config {
  chat: ChatConfig;
  agent: AgentConfig;
  /** How a session reaches GitHub, or undefined when none is configured. */
  github: GithubConfig | undefined;
  /**
   * Absolute path under which every session's project directory lives.
   *
   * A session works in a subdirectory of this and nowhere else.
   */
  projectRoot: string;
  /** Where per-session state directories are created on the host. */
  stateDir: string;
  sandbox: SandboxConfig;
  output: OutputConfig;
  /** Who may power off the host. Empty means nobody, which is the default. */
  shutdown: ShutdownConfig;
  /** The web interface, or undefined when none is served. */
  web: WebConfig | undefined;
  limits: LimitsConfig;
  timeouts: TimeoutsConfig;
}

/**
 * Allowlist entry that admits everyone who can post in the served channel.
 *
 * Anyone admitted can run code in a sandbox with write access to the project
 * root, so this is only reasonable when the channel itself is the boundary.
 */
export const ALLOW_EVERY_USER = "*";

/** Field paths whose values must never be logged, posted, or reported. */
export const SECRET_PATHS = ["chat.token", "agent.credential", "github.token"] as const;

/** Values used for any optional field the configuration file omits. */
export const DEFAULTS = {
  sandbox: {
    backend: "bailey" as SandboxBackend,
    requireFullEnforcement: true,
    network: "restricted" as NetworkMode,
    image: "localhost/errand-agent:latest",
    memory: "4g",
    cpus: 2,
    pids: 512,
    fileMax: "1g",
    disk: "5g",
    diskCheckMs: 30_000,
    gracePeriodMs: 10_000,
  },
  output: {
    forwardToolOutput: false,
    maxToolOutputChars: 1_500,
    maxAttachmentBytes: 5 * 1024 * 1024,
    maxAttachmentsPerMessage: 4,
    postDiffs: true,
  },
  limits: {
    maxConcurrentTurns: 2,
    maxLiveSessions: 4,
    maxQueueLength: 32,
    maxQueueWaitMs: 900_000,
  },
  web: {
    host: "127.0.0.1",
    port: 8787,
    observer: false,
  },
  timeouts: {
    idleMs: 1_800_000,
    startupMs: 60_000,
    questionMs: 300_000,
    abortMs: 15_000,
  },
} as const;

/**
 * Every reason the configuration was rejected, not just the first.
 *
 * Reporting one at a time makes a first run a guessing game, so the daemon
 * refuses with the whole list.
 */
export class ConfigError extends Error {
  readonly problems: readonly string[];

  constructor(problems: readonly string[]) {
    super(`configuration rejected:\n${problems.map((problem) => `  - ${problem}`).join("\n")}`);
    this.name = "ConfigError";
    this.problems = problems;
  }
}
