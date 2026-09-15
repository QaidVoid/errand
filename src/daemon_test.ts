import { assertEquals, assertRejects, assertStringIncludes } from "@std/assert";
import { join } from "@std/path";
import { MemoryStore } from "./memory/store.ts";
import type { RawMessage } from "./chat/inbound.ts";
import { validateConfig } from "./config/validate.ts";
import type { Config } from "./config/schema.ts";
import { createLogger, type LogLevel } from "./log.ts";
import { Daemon, EnforcementGapError, inertSettings, renderStartupReport } from "./daemon.ts";
import type { CapabilityReport, Sandbox, SandboxHandle, SandboxLaunch } from "./sandbox/backend.ts";
import type { AgentProcess } from "./agent/client.ts";
import { hostPathUnder } from "./sandbox/paths.ts";
import type { EndReason, ThreadPort } from "./session/port.ts";
import type { ThreadFactory } from "./session/manager.ts";
import type { IncomingMessage } from "./session/session.ts";

const OWNER = "100000000000000001";
const encoder = new TextEncoder();

/** An agent that answers its readiness call and otherwise says nothing. */
class QuietAgent implements AgentProcess {
  readonly stdout: ReadableStream<Uint8Array>;
  readonly stderr: ReadableStream<Uint8Array>;
  readonly exited: Promise<number>;
  private out!: ReadableStreamDefaultController<Uint8Array>;
  private exit!: (code: number) => void;

  constructor() {
    this.stdout = new ReadableStream<Uint8Array>({ start: (c) => (this.out = c) });
    this.stderr = new ReadableStream<Uint8Array>({ start: (c) => c.close() });
    this.exited = new Promise<number>((resolve) => (this.exit = resolve));
  }

  write(bytes: Uint8Array): void {
    const id = (JSON.parse(new TextDecoder().decode(bytes).trim()) as { id?: string }).id;
    if (id !== undefined) {
      this.out.enqueue(encoder.encode(`${JSON.stringify({ type: "response", id })}\n`));
    }
  }

  end(): void {
    try {
      this.out.close();
    } catch {
      // Already closed.
    }
    this.exit(0);
  }
}

function fakeSandbox(report: CapabilityReport = { backend: "bailey", gaps: [], notes: [] }) {
  const launched: SandboxLaunch[] = [];
  const agents: QuietAgent[] = [];
  let probeFails: Error | null = null;

  const sandbox: Sandbox = {
    name: "bailey",
    probe: () => probeFails === null ? Promise.resolve(report) : Promise.reject(probeFails),
    launch: (launch: SandboxLaunch): Promise<SandboxHandle> => {
      launched.push(launch);
      const agent = new QuietAgent();
      agents.push(agent);
      return Promise.resolve({
        process: agent,
        name: `errand-${launch.sessionId}`,
        toHostPath: (path: string) => hostPathUnder("/workspace", launch.projectPath, path),
        stop: () => {
          agent.end();
          return Promise.resolve(false);
        },
      });
    },
    listOrphans: () => Promise.resolve([]),
    removeOrphans: () => Promise.resolve(0),
  };

  return {
    sandbox,
    launched,
    failProbeWith: (error: Error) => {
      probeFails = error;
    },
  };
}

function silentPort(closed: EndReason[]): ThreadPort {
  const nothing = (): Promise<void> => Promise.resolve();
  return {
    post: nothing,
    postNotice: nothing,
    postReply: nothing,
    noteToolResult: () => {},
    noteDelegation: () => {},
    beginTurn: () => {},
    noteThinking: () => {},
    notePrompt: nothing,
    noteAside: nothing,
    appendActivity: nothing,
    postDiff: nothing,
    setWaiting: nothing,
    setReaction: nothing,
    setUsage: () => {},
    setBusy: () => {},
    upload: nothing,
    close: (reason: EndReason) => {
      closed.push(reason);
      return nothing();
    },
  };
}

function fakeThreads() {
  const created: string[] = [];
  const closed: EndReason[] = [];
  let next = 1;

  const factory: ThreadFactory = {
    create: (_message, name) => {
      created.push(name);
      const id = `thread-${next}`;
      next += 1;
      return Promise.resolve({ id, port: silentPort(closed) });
    },
    open: (name) => {
      created.push(name);
      const id = `thread-${next}`;
      next += 1;
      return Promise.resolve({ id, port: silentPort(closed) });
    },
    portFor: () => Promise.resolve(silentPort(closed)),
    release: () => {},
  };
  return { factory, created, closed };
}

function config(overrides: Record<string, unknown> = {}): Config {
  return validateConfig({
    chat: { token: "a.token.value", channelId: "chan", allowedUserIds: [OWNER] },
    agent: { provider: "anthropic", credentialName: "ANTHROPIC_API_KEY", credential: "secret" },
    projectRoot: "/tmp/errand-projects",
    stateDir: "/tmp/errand-state",
    ...overrides,
  });
}

function raw(content: string, options: Partial<RawMessage> = {}): RawMessage {
  return {
    id: "m1",
    authorId: OWNER,
    authorName: "amelia",
    authorIsBot: false,
    channelId: "chan",
    parentChannelId: undefined,
    content,
    attachments: [],
    ...options,
  };
}

interface Harness {
  daemon: Daemon;
  threads: ReturnType<typeof fakeThreads>;
  sandbox: ReturnType<typeof fakeSandbox>;
  replies: string[];
  lines: [LogLevel, string][];
  root: string;
}

async function withDaemon(
  run: (harness: Harness) => Promise<void>,
  options: {
    settings?: Record<string, unknown>;
    powerOff?: () => Promise<string | undefined>;
    describeUsage?: () => Promise<string>;
    report?: CapabilityReport;
    start?: boolean;
    memory?: MemoryStore;
  } = {},
): Promise<void> {
  const root = await Deno.makeTempDir({ prefix: "errand-daemon-" });
  const settings = config({
    projectRoot: join(root, "projects"),
    stateDir: join(root, "state"),
    ...options.settings,
  });
  const threads = fakeThreads();
  const sandbox = fakeSandbox(options.report);
  const replies: string[] = [];
  const lines: [LogLevel, string][] = [];

  const daemon = new Daemon({
    config: settings,
    sandbox: sandbox.sandbox,
    threads: threads.factory,
    log: createLogger({}, (level, line) => lines.push([level, line])),
    replyInChannel: (_message: IncomingMessage, text: string) => {
      replies.push(text);
      return Promise.resolve();
    },
    ...(options.powerOff === undefined ? {} : { powerOff: options.powerOff }),
    ...(options.describeUsage === undefined ? {} : { describeUsage: options.describeUsage }),
    ...(options.memory === undefined ? {} : { memory: options.memory }),
  });

  try {
    if (options.start !== false) await daemon.start();
    await run({ daemon, threads, sandbox, replies, lines, root });
  } finally {
    await daemon.shutdown();
    await Deno.remove(root, { recursive: true });
  }
}

Deno.test("starting reports the configuration with no secret in it", () =>
  withDaemon(({ daemon, lines }) => {
    const logged = lines.map(([, line]) => line).join("\n");

    assertEquals(daemon.isAccepting, true);
    assertStringIncludes(logged, "effective configuration");
    assertEquals(logged.includes("a.token.value"), false);
    assertStringIncludes(logged, "[redacted]");
    return Promise.resolve();
  }));

/** A crashed daemon must not leave agents running against a project. */
Deno.test("nothing is acted on before startup has finished", () =>
  withDaemon(async ({ daemon, threads }) => {
    await daemon.handle(raw("demo: go"), { kind: "start" });

    assertEquals(threads.created, []);
    assertEquals(
      await daemon.runCommand({
        threadId: "thread-1",
        userId: OWNER,
        userName: "amelia",
        content: "!status",
      }),
      "the daemon is still starting up",
    );
  }, { start: false }));

Deno.test("a message in the channel starts a session in a thread", () =>
  withDaemon(async ({ daemon, threads }) => {
    await daemon.handle(raw("demo: fix the parser"), { kind: "start" });

    assertEquals(threads.created, ["demo: fix the parser"]);
    assertEquals(daemon.sessions.sessions.length, 1);
  }));

/** The channel is where people talk; an aside there is not work to start. */
Deno.test("an aside in the channel starts nothing and says nothing", () =>
  withDaemon(async ({ daemon, threads, replies }) => {
    await daemon.handle(raw("!!! anyone around?"), { kind: "start" });

    assertEquals(threads.created, []);
    assertEquals(replies, []);
  }));

/** Opening a thread and a sandbox to print a list is not an answer. */
Deno.test("help in the channel is answered without starting anything", () =>
  withDaemon(async ({ daemon, threads, replies }) => {
    await daemon.handle(raw("!help"), { kind: "start" });

    assertEquals(threads.created, []);
    assertStringIncludes(replies[0] ?? "", "!steer");
  }));

Deno.test("a command needing a session is left alone in the channel", () =>
  withDaemon(async ({ daemon, threads, replies }) => {
    await daemon.handle(raw("!ls src"), { kind: "start" });
    await daemon.handle(raw("!somebodyelses thing", { id: "m2" }), { kind: "start" });

    assertEquals(threads.created, []);
    assertEquals(replies, []);
  }));

Deno.test("a refusal to start is said in the channel, where it was asked", () =>
  withDaemon(async ({ daemon, replies }) => {
    await daemon.handle(raw("demo: first"), { kind: "start" });
    await daemon.handle(raw("demo: second", { id: "m2" }), { kind: "start" });

    assertStringIncludes(replies[0] ?? "", "already has a live session");
  }));

Deno.test("a message in a thread reaches its session", () =>
  withDaemon(async ({ daemon, replies }) => {
    await daemon.handle(raw("demo: go"), { kind: "start" });

    await daemon.handle(
      raw("carry on", { id: "m2", channelId: "thread-1", parentChannelId: "chan" }),
      { kind: "thread", threadId: "thread-1" },
    );

    assertEquals(replies, []);
  }));

/** The agent's history outlives the sandbox, so a restart does not end it. */
Deno.test("a message in a sleeping thread wakes the session", () =>
  withDaemon(async ({ daemon }) => {
    await daemon.handle(raw("demo: go"), { kind: "start" });
    await daemon.sessions.endThread("thread-1", "idle");
    assertEquals(daemon.sessions.sessions.length, 0);

    await daemon.handle(raw("carry on", { id: "m2" }), { kind: "thread", threadId: "thread-1" });

    assertEquals(daemon.sessions.sessions.length, 1);
  }));

Deno.test("a message in a thread that is over says where to start a new one", () =>
  withDaemon(async ({ daemon, replies }) => {
    await daemon.handle(raw("demo: go"), { kind: "start" });
    await daemon.sessions.endThread("thread-1", "stopped");

    await daemon.handle(raw("hello?", { id: "m2" }), { kind: "thread", threadId: "thread-1" });

    assertStringIncludes(replies[0] ?? "", "post in the channel to start a new one");
  }));

Deno.test("a thread archived from outside ends its session", () =>
  withDaemon(async ({ daemon }) => {
    await daemon.handle(raw("demo: go"), { kind: "start" });

    await daemon.threadClosed("thread-1");

    assertEquals(daemon.sessions.sessions.length, 0);
  }));

/**
 * Whoever starts a thread owns it, and owning a thread is no reason to be able
 * to turn the computer off. The only list that counts is the daemon's own.
 */
Deno.test("nobody powers off the host unless the daemon's own list says so", () =>
  withDaemon(async ({ daemon, replies, threads }) => {
    await daemon.handle(raw("!shutdown"), { kind: "start" });

    assertStringIncludes(replies[0] ?? "", "nobody may power off this host");
    assertEquals(threads.created, []);
  }));

Deno.test("an account not on the shutdown list is refused", () =>
  withDaemon(async ({ daemon, replies }) => {
    await daemon.handle(raw("!shutdown", { authorId: "999" }), { kind: "start" });

    assertStringIncludes(replies[0] ?? "", "not on the list");
  }, { settings: { shutdown: { allowedUserIds: [OWNER] } } }));

Deno.test("an account on the list powers the host off", () =>
  withDaemon(async ({ daemon, replies, lines }) => {
    await daemon.handle(raw("!shutdown"), { kind: "start" });

    assertStringIncludes(replies[0] ?? "", "powering off now");
    assertStringIncludes(lines.map(([, line]) => line).join("\n"), "powering off on request");
  }, {
    settings: { shutdown: { allowedUserIds: [OWNER] } },
    powerOff: () => Promise.resolve(undefined),
  }));

Deno.test("a power off that fails says what went wrong", () =>
  withDaemon(async ({ daemon, replies }) => {
    await daemon.handle(raw("!shutdown"), { kind: "start" });

    assertStringIncludes(replies[0] ?? "", "systemctl refused");
  }, {
    settings: { shutdown: { allowedUserIds: [OWNER] } },
    powerOff: () => Promise.resolve("systemctl refused"),
  }));

/** Shutting down from inside a thread must not be a way around the list. */
Deno.test("the shutdown list governs the slash command too", () =>
  withDaemon(async ({ daemon }) => {
    const answer = await daemon.runCommand({
      threadId: "thread-1",
      userId: "999",
      userName: "somebody",
      content: "!shutdown",
    });

    assertStringIncludes(answer, "not on the list");
  }, { settings: { shutdown: { allowedUserIds: [OWNER] } } }));

Deno.test("a slash command runs the same command a message would", () =>
  withDaemon(async ({ daemon }) => {
    await daemon.handle(raw("demo: go"), { kind: "start" });

    const answer = await daemon.runCommand({
      threadId: "thread-1",
      userId: OWNER,
      userName: "amelia",
      content: "!status",
    });

    assertEquals(answer, "ran !status");
  }));

/** A help listing wants to go back to whoever asked, not into the channel. */
Deno.test("a slash command that needs no session answers the caller directly", () =>
  withDaemon(async ({ daemon, replies }) => {
    const answer = await daemon.runCommand({
      threadId: undefined,
      userId: OWNER,
      userName: "amelia",
      content: "!help",
    });

    assertStringIncludes(answer, "!steer");
    assertEquals(replies, []);
  }));

Deno.test("a slash command outside a thread says where to use it", () =>
  withDaemon(async ({ daemon }) => {
    const answer = await daemon.runCommand({
      threadId: undefined,
      userId: OWNER,
      userName: "amelia",
      content: "!ls",
    });

    assertStringIncludes(answer, "post in the channel to start one");
  }));

Deno.test("a slash command in a sleeping thread says to wake it first", () =>
  withDaemon(async ({ daemon }) => {
    await daemon.handle(raw("demo: go"), { kind: "start" });
    await daemon.sessions.endThread("thread-1", "idle");

    const answer = await daemon.runCommand({
      threadId: "thread-1",
      userId: OWNER,
      userName: "amelia",
      content: "!ls",
    });

    assertStringIncludes(answer, "post a message in it to wake the session");
  }));

Deno.test("shutting down ends every session and stops accepting", () =>
  withDaemon(async ({ daemon, threads }) => {
    await daemon.handle(raw("demo: go"), { kind: "start" });

    await daemon.shutdown();

    assertEquals(daemon.isAccepting, false);
    assertEquals(daemon.sessions.sessions, []);
    assertEquals(threads.closed, ["shutdown"]);
  }));

/**
 * Presenting a weaker boundary as a stronger one is worse than the weaker
 * boundary, because it removes the chance to decide about it.
 */
Deno.test("the startup report always states what cannot be enforced", () => {
  const lines = renderStartupReport(
    { backend: "bailey", gaps: ["seccomp is unavailable"], notes: ["landlock v5"] },
    [],
  ).join("\n");

  assertStringIncludes(lines, "sandbox backend: bailey");
  assertStringIncludes(lines, "landlock v5");
  assertStringIncludes(lines, "1 guarantee(s) cannot be enforced");
  assertStringIncludes(lines, "seccomp is unavailable");
});

Deno.test("a backend with nothing missing says so plainly", () => {
  const lines = renderStartupReport({ backend: "podman", gaps: [], notes: [] }, []).join("\n");

  assertStringIncludes(lines, "enforces every configured guarantee");
});

/** Anyone who can post can run code, which is worth saying out loud. */
Deno.test("an open allowlist is reported as the decision it is", () => {
  const lines = renderStartupReport(
    { backend: "bailey", gaps: [], notes: [] },
    [],
    config({ chat: { token: "t", channelId: "c", allowedUserIds: ["*"], blockedUserIds: ["9"] } })
      .chat,
  ).join("\n");

  assertStringIncludes(lines, "open to everyone who can post");
  assertStringIncludes(lines, "1 blocked");
});

Deno.test("a setting the chosen backend ignores is reported as inert", () => {
  assertEquals(inertSettings(config()), []);
  assertEquals(
    inertSettings(config({ sandbox: { image: "localhost/mine:v2" } })),
    ["sandbox.image is set but only the podman backend uses it"],
  );
  assertEquals(
    inertSettings(config({ sandbox: { backend: "podman", image: "localhost/mine:v2" } })),
    [],
  );
});

/** A guarantee that cannot be met must not be started around silently. */
Deno.test("a gap the configuration forbids stops the daemon starting", () =>
  withDaemon(async ({ daemon }) => {
    await assertRejects(() => daemon.start(), EnforcementGapError);
  }, { start: false, report: { backend: "bailey", gaps: ["no landlock here"], notes: [] } }));

Deno.test("the same gap is allowed when the configuration allows it", () =>
  withDaemon(async ({ daemon, lines }) => {
    await daemon.start();

    assertEquals(daemon.isAccepting, true);
    assertStringIncludes(lines.map(([, line]) => line).join("\n"), "no landlock here");
  }, {
    start: false,
    settings: { sandbox: { requireFullEnforcement: false } },
    report: { backend: "bailey", gaps: ["no landlock here"], notes: [] },
  }));

/** About the account the host shares, so a session is not needed to ask. */
Deno.test("the usage window is reported wherever it is asked about", () =>
  withDaemon(async ({ daemon, replies, threads }) => {
    await daemon.handle(raw("!usage"), { kind: "start" });

    assertStringIncludes(replies[0] ?? "", "58% of the provider's usage window is left");
    assertEquals(threads.created, []);
    assertEquals(
      await daemon.runCommand({
        threadId: undefined,
        userId: OWNER,
        userName: "amelia",
        content: "!usage",
      }),
      replies[0],
    );
  }, {
    describeUsage: () =>
      Promise.resolve("58% of the provider's usage window is left, and it resets in 2 hours"),
  }));

Deno.test("a provider that meters nothing says so rather than inventing a number", () =>
  withDaemon(async ({ daemon, replies }) => {
    await daemon.handle(raw("!usage"), { kind: "start" });

    assertStringIncludes(replies[0] ?? "", "does not report a usage window");
  }));

/**
 * Memory is about a person, not a session, so asking should not need a thread.
 */
Deno.test("!facts is answered in the channel, with no session running", async () => {
  const memory = new MemoryStore(":memory:");
  memory.remember("user", OWNER, "prefers jj over git", "earlier");

  await withDaemon(async ({ daemon, replies, threads }) => {
    await daemon.handle(raw("!facts"), { kind: "start" });

    assertStringIncludes(replies.join("\n"), "prefers jj over git");
    // Answered outright: no thread was opened and no session started.
    assertEquals(threads.created, []);
  }, { memory });
});

Deno.test("a project is a thread's own, so the channel says to ask there", async () => {
  const memory = new MemoryStore(":memory:");
  await withDaemon(async ({ daemon, replies }) => {
    await daemon.handle(raw("!facts project"), { kind: "start" });
    assertStringIncludes(replies.join("\n"), "ask in one");
  }, { memory });
});

/** Owner means nothing in a channel, so forgetting there is the operator's. */
Deno.test("!forget in the channel is refused to anyone but an operator", async () => {
  const memory = new MemoryStore(":memory:");
  memory.remember("user", OWNER, "prefers jj over git", "earlier");

  await withDaemon(async ({ daemon, replies }) => {
    await daemon.handle(raw(`!forget <@${OWNER}>`), { kind: "start" });

    assertStringIncludes(replies.join("\n"), "only an operator");
    assertEquals(memory.factsFor("user", OWNER).length, 1);
  }, { memory });
});

/** Inside a thread the session answers, because it knows the project. */
Deno.test("in a thread the daemon leaves memory to the session", async () => {
  const memory = new MemoryStore(":memory:");
  memory.remember("user", OWNER, "prefers jj over git", "earlier");

  await withDaemon(async ({ daemon, replies }) => {
    await daemon.handle(raw("!facts"), { kind: "thread", threadId: "t1" });
    // Not answered here, so it falls through to whatever the thread holds.
    assertEquals(replies.join("\n").includes("prefers jj over git"), false);
  }, { memory });
});

Deno.test("an operator may forget from the channel, and is told what went", async () => {
  const memory = new MemoryStore(":memory:");
  memory.remember("user", OWNER, "prefers jj over git", "earlier");

  await withDaemon(async ({ daemon, replies }) => {
    await daemon.handle(raw(`!forget <@${OWNER}>`), { kind: "start" });

    assertStringIncludes(replies.join("\n"), "forgot 1 fact about");
    assertEquals(memory.factsFor("user", OWNER).length, 0);
  }, {
    memory,
    settings: {
      chat: { token: "t", channelId: "chan", allowedUserIds: [OWNER], operatorUserIds: [OWNER] },
    },
  });
});
