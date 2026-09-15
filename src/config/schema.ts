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

/**
 * How outbound is bounded for a session that has a network.
 *
 * `open` is the port-only rule the bailey backend has always applied: a session
 * may reach any host on an allowed port. It cannot tell the model provider from
 * anywhere else on 443, so a session can dial an arbitrary service, and a
 * userspace VPN turns that into a two-way channel.
 *
 * `proxy` forces every connection through a broker the daemon runs outside the
 * sandbox. The broker permits only an allowlist of hosts, so a session reaches
 * the provider and whatever else is named and nothing else, and it injects the
 * provider credential itself, so the key never enters the sandbox.
 */
export type EgressMode = "open" | "proxy";

/** What a session may reach outbound, and how that is enforced. */
export interface EgressConfig {
  /** Whether egress is port-only (`open`) or forced through the broker (`proxy`). */
  mode: EgressMode;
  /**
   * Hosts the broker permits under `proxy` mode, on top of the provider.
   *
   * The model provider is always allowed, since a session cannot work without
   * it. Everything else a session legitimately fetches is named here: a code
   * host, a package registry, a mirror. A leading `*.` matches subdomains, so
   * `*.githubusercontent.com` covers the hosts a clone pulls from. A lone `*`
   * admits any host, keeping the broker as an audit pass-through that still
   * gates the port and logs every connection but restricts no host. Ignored
   * under `open` mode, where nothing consults an allowlist.
   */
  allow: string[];
}

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
  /**
   * Require a message to mention the bot before it starts a session.
   *
   * Off by default, so every top-level message in the served channel starts
   * one. Turn it on where the channel is also used for talking: people can
   * then hold an ordinary conversation in it, and only a message addressed to
   * the bot opens a thread and a sandbox.
   */
  startOnMention: boolean;
}

/**
 * A cheaper model the session's model may ask about one artefact.
 *
 * Absent means no delegation at all: the session's own model does everything,
 * which is what it did before this existed.
 */
export interface DelegateConfig {
  /** The model asked. Must be one the provider serves under the same key. */
  model: string;
  /** How many delegations one turn may make before the rest stay at home. */
  perTurn: number;
  /** How long one delegation may take before it is abandoned. */
  deadlineMs: number;
  /**
   * Where the provider is reached, when the model store does not say.
   *
   * Read from the agent's own model store by default, so the same endpoint
   * that serves the session is the one asked.
   */
  baseUrl: string | undefined;
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
  /** A cheaper model to ask about one artefact, or undefined for none. */
  delegate: DelegateConfig | undefined;
  /**
   * File of standing instructions given to every session, or undefined for
   * none.
   *
   * A project's own `AGENTS.md` says how to work on that project and the agent
   * reads it without help. This is what the operator wants of every session,
   * whatever it is working on: the conventions of the house rather than of one
   * repository. Absolute, and read on the host, so the path needs no grant and
   * a session never sees the file itself.
   *
   * Defaults to `AGENTS.md` beside the configuration file when one is there,
   * so the common case needs no setting. Naming a path that cannot be read is
   * a refusal to start; the default simply not being there is not, since a
   * default nobody asked for must not be able to stop the daemon.
   *
   * Re-read for each session, so editing it does not need a restart. It is
   * paid for in the agent's context on every turn, which is the reason to keep
   * it to the rules that actually matter.
   */
  rulesPath: string | undefined;
  /**
   * Providers the operator defines, beyond the ones the agent knows itself.
   *
   * Written into the agent's own configuration directory verbatim, in the
   * shape that configuration uses: a `baseUrl`, which API it speaks, and the
   * models it serves. Passed through rather than restated here, because the
   * schema belongs to the agent and copying it would mean this refusing a
   * field the agent had just learned.
   *
   * A definition is how a provider the agent has no entry for is reached at
   * all, and how one it does know is pointed somewhere else. The key is the
   * provider name `agent.provider` and `--model provider/id` refer to.
   */
  providers: Record<string, unknown>;
  /**
   * Short names for models, so a session is started without spelling one out.
   *
   * The value is what the name stands for, `provider/id` or a bare id, exactly
   * as `--model` takes it. A thinking level may be written into it as a
   * default, and one given on the name itself wins: with `muse` standing for
   * `meta/muse-spark-1.3-contributor:high`, `--model muse` asks for high and
   * `--model muse:xhigh` asks for xhigh.
   */
  aliases: Record<string, string>;
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

/**
 * Paths granted to a session on top of what the daemon already grants.
 *
 * Additive only. The generated policy still places the project and the state
 * directory, still bounds the environment, and still clears the backend's own
 * profile first; this names more that a session may reach, and can take
 * nothing away. Every path is absolute, since a relative one in a policy means
 * nothing.
 *
 * What is granted here is reported at startup, so the enforcement report never
 * describes a tighter boundary than the one actually applied.
 */
export interface PolicyExtraConfig {
  /** Directories or files a session may read. */
  read: string[];
  /** Directories or files a session may write. Widens what it can change. */
  write: string[];
  /** Directories a session may execute from. */
  execute: string[];
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
  /**
   * Ports a session may open outbound, when the network is not `none`.
   *
   * The default is HTTPS alone, which is all a model provider needs. Adding a
   * port widens what a session can reach: 80 lets it speak plaintext HTTP, so
   * a mirror or a redirect that has not moved to TLS resolves. Enforced by the
   * generated policy, so it holds under the bailey backend; podman bounds the
   * network by namespace rather than by port.
   */
  egressPorts: number[];
  /** What a session may reach outbound, and whether it is brokered. */
  egress: EgressConfig;
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
  /**
   * Hide the host's network identity from a session, when it has a network.
   *
   * A session shares the host's network namespace by default, so it can read
   * the host address, the MAC, and the ARP neighbours. With this set, the
   * bailey backend runs egress through a private namespace instead, so a
   * session sees a synthetic address and MAC. The namespace gets IPv6 as well
   * where the host has it, and the ports a session may open are unchanged. The
   * podman backend already gives each session its own network, so this does
   * not apply to it.
   */
  hideHostAddress: boolean;
  /** Paths granted on top of the generated policy, or undefined for none. */
  policyExtra: PolicyExtraConfig | undefined;
  /**
   * Directories added to a session's PATH, or undefined for none.
   *
   * Named after the agent's own wrappers and ahead of the system directories,
   * so a program named here is the one a session finds. This adds nothing a
   * session may reach: a directory that is not also granted under
   * `PolicyExtraConfig` is a name on a path leading nowhere. The podman
   * backend takes its PATH from the image and ignores this.
   */
  pathExtra: string[] | undefined;
  /**
   * Variables set in every session's environment, or undefined for none.
   *
   * The environment is built rather than inherited, so a toolchain that reads
   * one has no other way to be told. `PATH` and `HOME` are refused, since the
   * daemon sets both to paths the policy places, and so is the name carrying
   * the provider credential. A name the daemon sets itself keeps the daemon's
   * value. Every value reaches the agent, so nothing secret belongs here.
   */
  env: Record<string, string> | undefined;
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
  chat: {
    startOnMention: false,
  },
  sandbox: {
    backend: "bailey" as SandboxBackend,
    requireFullEnforcement: true,
    network: "restricted" as NetworkMode,
    egressPorts: [443],
    egress: { mode: "proxy" as EgressMode, allow: ["*"] as string[] },
    hideHostAddress: false,
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
  delegate: {
    perTurn: 8,
    deadlineMs: 60_000,
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
