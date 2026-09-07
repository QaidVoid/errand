import { assertEquals, assertStringIncludes } from "@std/assert";
import { join } from "@std/path";
import { Scheduler } from "../admission/scheduler.ts";
import type { AgentProcess } from "../agent/client.ts";
import type { Config } from "../config/schema.ts";
import { validateConfig } from "../config/validate.ts";
import { createLogger } from "../log.ts";
import type {
  CapabilityReport,
  Sandbox,
  SandboxHandle,
  SandboxLaunch,
} from "../sandbox/backend.ts";
import { hostPathUnder } from "../sandbox/paths.ts";
import { SessionManager, type ThreadFactory } from "./manager.ts";
import type {
  EndReason,
  NoticeLevel,
  ReactionOutcome,
  SessionUsage,
  ThreadPort,
  ToolResult,
} from "./port.ts";
import { ThreadRegistry } from "./registry.ts";
import type { IncomingMessage } from "./session.ts";
import { TRANSCRIPT_FILENAME } from "./transcript.ts";

const encoder = new TextEncoder();
const OWNER = "100000000000000001";

/** An agent that answers its readiness call and then says nothing. */
class QuietAgent implements AgentProcess {
  readonly stdout: ReadableStream<Uint8Array>;
  readonly stderr: ReadableStream<Uint8Array>;
  readonly exited: Promise<number>;
  readonly written: string[] = [];

  private out!: ReadableStreamDefaultController<Uint8Array>;
  private exit!: (code: number) => void;

  constructor() {
    this.stdout = new ReadableStream<Uint8Array>({ start: (c) => (this.out = c) });
    this.stderr = new ReadableStream<Uint8Array>({ start: (c) => c.close() });
    this.exited = new Promise<number>((resolve) => (this.exit = resolve));
  }

  write(bytes: Uint8Array): void {
    const line = new TextDecoder().decode(bytes).trim();
    this.written.push(line);
    const id = (JSON.parse(line) as { id?: string }).id;
    if (id !== undefined) {
      this.out.enqueue(encoder.encode(`${JSON.stringify({ type: "response", id })}\n`));
    }
  }

  end(code = 0): void {
    try {
      this.out.close();
    } catch {
      // Already closed.
    }
    this.exit(code);
  }
}

class FakeSandbox implements Sandbox {
  readonly name = "bailey" as const;
  readonly launched: SandboxLaunch[] = [];
  readonly agents: QuietAgent[] = [];
  orphans: string[] = [];
  removed: string[] = [];

  probe(): Promise<CapabilityReport> {
    return Promise.resolve({ backend: "bailey", gaps: [], notes: [] });
  }

  launch(launch: SandboxLaunch): Promise<SandboxHandle> {
    this.launched.push(launch);
    const agent = new QuietAgent();
    this.agents.push(agent);
    return Promise.resolve({
      process: agent,
      name: `errand-${launch.sessionId}`,
      toHostPath: (path: string) => hostPathUnder("/workspace", launch.projectPath, path),
      stop: () => {
        agent.end(0);
        return Promise.resolve(false);
      },
    });
  }

  listOrphans(): Promise<string[]> {
    return Promise.resolve(this.orphans);
  }

  removeOrphans(names: readonly string[]): Promise<number> {
    this.removed.push(...names);
    return Promise.resolve(names.length);
  }
}

/** A thread port that keeps only what a manager test needs to look at. */
function port(closed: EndReason[] = []): ThreadPort {
  const nothing = (): Promise<void> => Promise.resolve();
  return {
    post: nothing,
    postNotice: (_text: string, _level: NoticeLevel) => nothing(),
    postReply: nothing,
    noteToolResult: (_result: ToolResult) => {},
    beginTurn: () => {},
    noteThinking: () => {},
    notePrompt: nothing,
    noteAside: nothing,
    appendActivity: nothing,
    postDiff: nothing,
    setWaiting: nothing,
    setReaction: (_id: string, _outcome: ReactionOutcome) => nothing(),
    setUsage: (_usage: SessionUsage) => {},
    setBusy: () => {},
    upload: nothing,
    close: (reason: EndReason) => {
      closed.push(reason);
      return nothing();
    },
  };
}

/** Threads a test can inspect: what was created, and what was let go. */
function fakeThreads() {
  const created: string[] = [];
  const released: string[] = [];
  const closed: EndReason[] = [];
  let refuse: string | null = null;
  let next = 1;

  const factory: ThreadFactory = {
    create: (_message, name) => {
      if (refuse !== null) return Promise.reject(new Error(refuse));
      created.push(name);
      const id = `thread-${next}`;
      next += 1;
      return Promise.resolve({ id, port: port(closed) });
    },
    open: (name) => {
      if (refuse !== null) return Promise.reject(new Error(refuse));
      created.push(name);
      const id = `thread-${next}`;
      next += 1;
      return Promise.resolve({ id, port: port(closed) });
    },
    portFor: (threadId) =>
      Promise.resolve(threadId.startsWith("thread-") ? port(closed) : undefined),
    release: (threadId) => released.push(threadId),
  };

  return {
    factory,
    created,
    released,
    closed,
    refuseWith: (why: string | null) => {
      refuse = why;
    },
  };
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

function message(content: string, id = "m1"): IncomingMessage {
  return { id, authorId: OWNER, authorName: "amelia", content };
}

interface Harness {
  manager: SessionManager;
  sandbox: FakeSandbox;
  threads: ReturnType<typeof fakeThreads>;
  registry: ThreadRegistry;
  scheduler: Scheduler;
  root: string;
}

async function withManager(
  run: (harness: Harness) => Promise<void>,
  options: { unavailable?: () => Promise<string | undefined>; limits?: Record<string, number> } =
    {},
): Promise<void> {
  const root = await Deno.makeTempDir({ prefix: "errand-manager-" });
  const settings = config({
    projectRoot: join(root, "projects"),
    stateDir: join(root, "state"),
    ...(options.limits === undefined ? {} : { limits: options.limits }),
  });
  const sandbox = new FakeSandbox();
  const threads = fakeThreads();
  const log = createLogger({}, () => {});
  const registry = new ThreadRegistry(join(root, "threads.json"), log);
  const scheduler = new Scheduler(settings.limits);
  let id = 0;

  const manager = new SessionManager({
    config: settings,
    sandbox,
    scheduler,
    threads: threads.factory,
    registry,
    log,
    makeId: () => {
      id += 1;
      return `s${id}`;
    },
    ...(options.unavailable === undefined ? {} : { unavailable: options.unavailable }),
    now: () => 1_000,
  });

  try {
    await run({ manager, sandbox, threads, registry, scheduler, root });
  } finally {
    await manager.shutdown();
    scheduler.shutdown();
    await Deno.remove(root, { recursive: true });
  }
}

Deno.test("starting a session creates its thread, project and state", () =>
  withManager(async ({ manager, threads, sandbox, root, registry }) => {
    const outcome = await manager.start(message("demo: fix the parser"));

    assertEquals(outcome.status, "started");
    assertEquals(threads.created, ["demo: fix the parser"]);
    assertEquals(Deno.statSync(join(root, "projects", "demo")).isDirectory, true);
    assertEquals(sandbox.launched[0]?.projectPath, join(root, "projects", "demo"));
    assertEquals(registry.get("thread-1")?.projectName, "demo");
  }));

/** The prefix names the project; what follows it is what was asked. */
Deno.test("the project prefix is stripped from the prompt the agent sees", () =>
  withManager(async ({ manager, sandbox }) => {
    await manager.start(message("demo: fix the parser"));

    const sent = sandbox.agents[0]?.written.join("\n") ?? "";
    assertStringIncludes(sent, "fix the parser");
    assertEquals(sent.includes("demo: fix"), false);
  }));

/**
 * Two agents in one working tree edit the same files with neither able to see
 * the other's changes.
 */
Deno.test("a project that already has a session is refused a second one", () =>
  withManager(async ({ manager }) => {
    await manager.start(message("demo: first"));

    const second = await manager.start(message("demo: second", "m2"));

    assertEquals(second.status, "refused");
    assertStringIncludes(
      second.status === "refused" ? second.reason : "",
      "already has a live session",
    );
  }));

Deno.test("two different projects run side by side", () =>
  withManager(async ({ manager }) => {
    await manager.start(message("one: work"));
    const second = await manager.start(message("two: work", "m2"));

    assertEquals(second.status, "started");
    assertEquals(manager.sessions.length, 2);
  }));

/** A thread nobody can see would leave an agent running that nobody can stop. */
Deno.test("no thread means no sandbox, and the slot is given back", () =>
  withManager(async ({ manager, threads, sandbox, scheduler }) => {
    threads.refuseWith("the channel is gone");

    const outcome = await manager.start(message("demo: go"));

    assertEquals(outcome.status, "refused");
    assertEquals(sandbox.launched, []);
    assertEquals(manager.sessions, []);
    // The slot came back, so the next start is not refused for want of one.
    threads.refuseWith(null);
    assertEquals((await manager.start(message("demo: go", "m2"))).status, "started");
    assertEquals(scheduler.turnsInFlight >= 0, true);
  }));

Deno.test("a spent window refuses before a thread or a sandbox exists", () =>
  withManager(async ({ manager, threads, sandbox }) => {
    const outcome = await manager.start(message("demo: go"));

    assertEquals(outcome.status, "refused");
    assertStringIncludes(outcome.status === "refused" ? outcome.reason : "", "come back at nine");
    assertEquals(threads.created, []);
    assertEquals(sandbox.launched, []);
  }, { unavailable: () => Promise.resolve("come back at nine") }));

Deno.test("more sessions than the host allows are refused with a reason", () =>
  withManager(async ({ manager }) => {
    await manager.start(message("one: work"));

    const second = await manager.start(message("two: work", "m2"));

    assertEquals(second.status, "refused");
    assertStringIncludes(second.status === "refused" ? second.reason : "", "session");
  }, { limits: { maxLiveSessions: 1 } }));

Deno.test("a message reaches the session bound to its thread, and no other", () =>
  withManager(async ({ manager }) => {
    await manager.start(message("demo: go"));

    assertEquals(await manager.deliver("thread-1", message("more", "m2")), true);
    assertEquals(await manager.deliver("thread-404", message("more", "m3")), false);
  }));

Deno.test("a session that ends lets go of its thread and can be resumed", () =>
  withManager(async ({ manager, threads, registry }) => {
    await manager.start(message("demo: go"));

    await manager.endThread("thread-1", "idle");

    assertEquals(manager.forThread("thread-1"), undefined);
    assertEquals(manager.isFinishedThread("thread-1"), true);
    assertEquals(threads.released, ["thread-1"]);
    // Idling out is not being finished with: the history outlives the sandbox.
    assertEquals(registry.get("thread-1")?.sessionId, "s1");
    assertEquals(manager.canResume("thread-1"), true);
    assertEquals(manager.resumable().length, 1);
  }));

/** Stopping is how somebody says they are finished with a thread. */
Deno.test("a session that was stopped is not offered for resuming", () =>
  withManager(async ({ manager, registry }) => {
    await manager.start(message("demo: go"));

    await manager.endThread("thread-1", "stopped");

    assertEquals(registry.get("thread-1"), undefined);
    assertEquals(manager.canResume("thread-1"), false);
    assertEquals(manager.resumable(), []);
  }));

Deno.test("resuming picks the thread up where it stopped", () =>
  withManager(async ({ manager, sandbox, root }) => {
    await manager.start(message("demo: go"));
    await manager.endThread("thread-1", "idle");

    const outcome = await manager.resume("thread-1", message("carry on", "m2"));

    assertEquals(outcome.status, "started");
    assertEquals(outcome.status === "started" ? outcome.session.id : "", "s1");
    assertEquals(sandbox.launched[1]?.resume, true);
    assertEquals(sandbox.launched[1]?.stateDir, join(root, "state", "s1"));
  }));

/** A new exchange labelled with a number already used reads as the same one. */
Deno.test("a resumed session carries on the turn numbering", () =>
  withManager(async ({ manager, root }) => {
    await manager.start(message("demo: go"));
    await manager.endThread("thread-1", "idle");
    const transcript = join(root, "state", "s1", TRANSCRIPT_FILENAME);
    const written = Deno.readTextFileSync(transcript);
    assertEquals(written.includes('"turn":1'), true);

    await manager.resume("thread-1", message("carry on", "m2"));

    const after = Deno.readTextFileSync(transcript);
    assertEquals(after.includes('"turn":2'), true);
  }));

Deno.test("a thread this daemon never saw is not resumed", () =>
  withManager(async ({ manager }) => {
    const outcome = await manager.resume("thread-404", message("carry on"));

    assertEquals(outcome.status, "refused");
    assertStringIncludes(
      outcome.status === "refused" ? outcome.reason : "",
      "not one of mine to resume",
    );
  }));

Deno.test("a thread that already has a session is not resumed on top of it", () =>
  withManager(async ({ manager }) => {
    await manager.start(message("demo: go"));

    const outcome = await manager.resume("thread-1", message("again", "m2"));

    assertEquals(outcome.status, "refused");
    assertStringIncludes(
      outcome.status === "refused" ? outcome.reason : "",
      "already has a live session",
    );
  }));

/** Somebody invited before a restart must not be silently withdrawn. */
Deno.test("who was invited survives the session ending and coming back", () =>
  withManager(async ({ manager, registry }) => {
    const started = await manager.start(message("demo: go"));
    const session = started.status === "started" ? started.session : undefined;
    await session?.handle({
      id: "m2",
      authorId: OWNER,
      content: "!allow <@200000000000000002>",
    });
    assertEquals(registry.get("thread-1")?.guests, ["200000000000000002"]);

    await manager.endThread("thread-1", "idle");
    const resumed = await manager.resume("thread-1", message("carry on", "m3"));

    assertEquals(
      resumed.status === "started" ? resumed.session.guestList : [],
      ["200000000000000002"],
    );
  }));

Deno.test("a view can be attached to a live session and detached again", () =>
  withManager(async ({ manager }) => {
    await manager.start(message("demo: go"));
    const seen: string[] = [];
    const watcher = port();
    watcher.post = (text) => {
      seen.push(text);
      return Promise.resolve();
    };

    const detach = await manager.attachView("s1", watcher);
    assertEquals(typeof detach, "function");
    assertEquals(await manager.attachView("nobody", watcher), undefined);
    detach?.();
  }));

Deno.test("the thread a session belongs to is known live or remembered", () =>
  withManager(async ({ manager }) => {
    await manager.start(message("demo: go"));
    assertEquals(manager.threadIdFor("s1"), "thread-1");

    await manager.endThread("thread-1", "idle");

    assertEquals(manager.threadIdFor("s1"), "thread-1");
    assertEquals(manager.threadIdFor("s404"), undefined);
  }));

/** A crashed daemon must not leave containers running against a project. */
Deno.test("sandboxes left by a previous run are swept before anything starts", () =>
  withManager(async ({ manager, sandbox }) => {
    sandbox.orphans = ["errand-old-1", "errand-old-2"];

    assertEquals(await manager.sweepOrphans(), 2);
    assertEquals(sandbox.removed, ["errand-old-1", "errand-old-2"]);
  }));

Deno.test("shutting down ends every live session", () =>
  withManager(async ({ manager, threads }) => {
    await manager.start(message("one: work"));
    await manager.start(message("two: work", "m2"));

    await manager.shutdown();

    assertEquals(manager.sessions, []);
    assertEquals(threads.closed, ["shutdown", "shutdown"]);
  }));

/** A session begun at a keyboard is still announced where a phone will see it. */
Deno.test("a session started with no message still gets a thread", () =>
  withManager(async ({ manager, threads, registry }) => {
    const outcome = await manager.startDetached({
      project: "demo",
      prompt: "fix the parser",
      ownerId: "web-interface",
      ownerName: "the interface",
    });

    assertEquals(outcome.status, "started");
    assertEquals(threads.created, ["demo: fix the parser"]);
    assertEquals(registry.get("thread-1")?.ownerId, "web-interface");
  }));

Deno.test("a session started with no project named gets one of its own", () =>
  withManager(async ({ manager }) => {
    const outcome = await manager.startDetached({
      project: "",
      prompt: "have a look at this",
      ownerId: "web-interface",
    });

    assertEquals(outcome.status === "started" ? outcome.session.project.name : "", "s1");
  }));

/** The browser knows sessions, not threads: a thread is one of the surfaces. */
Deno.test("a session can be written to by its own identifier", () =>
  withManager(async ({ manager }) => {
    await manager.start(message("demo: go"));

    assertEquals(await manager.deliverToSession("s1", message("more", "m2")), true);
    assertEquals(await manager.deliverToSession("s404", message("more", "m3")), false);
  }));

/** Sending to a stopped session is asking for it back. */
Deno.test("writing to a session that stopped picks it up again", () =>
  withManager(async ({ manager }) => {
    await manager.start(message("demo: go"));
    await manager.endThread("thread-1", "idle");

    assertEquals(await manager.deliverToSession("s1", message("carry on", "m2")), true);
    assertEquals(manager.sessions.length, 1);
  }));
