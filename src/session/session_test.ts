import { assertEquals, assertStringIncludes } from "@std/assert";
import { existsSync } from "@std/fs";
import { join } from "@std/path";
import { Scheduler } from "../admission/scheduler.ts";
import type { AgentImage } from "../agent/protocol.ts";
import type { AgentProcess } from "../agent/client.ts";
import type { Config } from "../config/schema.ts";
import { validateConfig } from "../config/validate.ts";
import { createLogger } from "../log.ts";
import { MemoryStore } from "../memory/store.ts";
import type {
  CapabilityReport,
  Sandbox,
  SandboxHandle,
  SandboxLaunch,
} from "../sandbox/backend.ts";
import { hostPathUnder } from "../sandbox/paths.ts";
import type {
  Delegated,
  EndReason,
  NoticeLevel,
  ReactionOutcome,
  SessionUsage,
  ThreadPort,
  ToolResult,
} from "./port.ts";
import type { Request as PullRequest } from "./pr.ts";
import { type IncomingMessage, Session, type Timers } from "./session.ts";

const encoder = new TextEncoder();

/** A process a test drives, standing in for the agent inside a sandbox. */
class FakeAgent implements AgentProcess {
  readonly stdout: ReadableStream<Uint8Array>;
  readonly stderr: ReadableStream<Uint8Array>;
  readonly exited: Promise<number>;
  readonly written: string[] = [];
  writesFail = false;

  private out!: ReadableStreamDefaultController<Uint8Array>;
  private err!: ReadableStreamDefaultController<Uint8Array>;
  private exit!: (code: number) => void;
  private ended = false;

  constructor() {
    this.stdout = new ReadableStream<Uint8Array>({ start: (c) => (this.out = c) });
    this.stderr = new ReadableStream<Uint8Array>({ start: (c) => (this.err = c) });
    this.exited = new Promise<number>((resolve) => (this.exit = resolve));
  }

  write(bytes: Uint8Array): void {
    if (this.writesFail) throw new Error("the agent has gone");
    this.written.push(new TextDecoder().decode(bytes).trim());
  }

  /** Sends one record to the client. */
  send(record: unknown): void {
    this.out.enqueue(encoder.encode(`${JSON.stringify(record)}\n`));
  }

  /** Answers the most recent request, by its correlation id. */
  answer(data: unknown = {}): void {
    const last = [...this.written].reverse().find((line) => line.includes('"id"'));
    const id = last === undefined ? undefined : (JSON.parse(last) as { id?: string }).id;
    this.send({ type: "response", id, success: true, data });
  }

  /** Reports a whole turn: it starts, speaks, costs something, and settles. */
  runTurn(text = "done", usage = { input: 10, output: 2, cacheRead: 0, cacheWrite: 0 }): void {
    this.send({ type: "agent_start" });
    this.send({
      type: "message_end",
      message: { role: "assistant", content: [{ type: "text", text }] },
    });
    this.send({ type: "turn_end", usage: { ...usage, totalTokens: 12, cost: 0.01 } });
    this.send({ type: "agent_settled" });
  }

  /** Writes to stderr, as a dying process does. */
  complain(text: string): void {
    this.err.enqueue(encoder.encode(`${text}\n`));
  }

  end(code = 0): void {
    if (this.ended) return;
    this.ended = true;
    for (const stream of [this.out, this.err]) {
      try {
        stream.close();
      } catch {
        // Already closed.
      }
    }
    this.exit(code);
  }
}

/** Records everything a session reports. */
class FakeThread implements ThreadPort {
  readonly posts: string[] = [];
  readonly notices: { text: string; level: NoticeLevel }[] = [];
  readonly replies: { text: string; command: string }[] = [];
  readonly prompts: string[] = [];
  readonly asides: string[] = [];
  readonly activity: string[] = [];
  readonly diffs: { path: string; added: number; removed: number }[] = [];
  readonly reactions: { messageId: string; outcome: ReactionOutcome }[] = [];
  readonly uploads: { name: string; size: number }[] = [];
  readonly turns: number[] = [];
  readonly results: ToolResult[] = [];
  readonly delegations: Delegated[] = [];
  usage: SessionUsage | undefined;
  waiting: string | null = null;
  busy = false;
  closed: EndReason | undefined;

  post(text: string): Promise<void> {
    this.posts.push(text);
    return Promise.resolve();
  }
  postNotice(text: string, level: NoticeLevel): Promise<void> {
    this.notices.push({ text, level });
    return Promise.resolve();
  }
  postReply(text: string, command: string): Promise<void> {
    this.replies.push({ text, command });
    return Promise.resolve();
  }
  noteToolResult(result: ToolResult): void {
    this.results.push(result);
  }
  noteDelegation(delegated: Delegated): void {
    this.delegations.push(delegated);
  }
  beginTurn(turn: number): void {
    this.turns.push(turn);
  }
  noteThinking(): void {}
  notePrompt(author: string, text: string): Promise<void> {
    this.prompts.push(`${author}: ${text}`);
    return Promise.resolve();
  }
  noteAside(author: string, text: string): Promise<void> {
    this.asides.push(`${author}: ${text}`);
    return Promise.resolve();
  }
  appendActivity(line: string): Promise<void> {
    this.activity.push(line);
    return Promise.resolve();
  }
  postDiff(path: string, added: number, removed: number): Promise<void> {
    this.diffs.push({ path, added, removed });
    return Promise.resolve();
  }
  setWaiting(text: string | null): Promise<void> {
    this.waiting = text;
    return Promise.resolve();
  }
  setReaction(messageId: string, outcome: ReactionOutcome): Promise<void> {
    this.reactions.push({ messageId, outcome });
    return Promise.resolve();
  }
  setUsage(usage: SessionUsage): void {
    this.usage = usage;
  }
  setBusy(busy: boolean): void {
    this.busy = busy;
  }
  upload(name: string, bytes: Uint8Array): Promise<void> {
    this.uploads.push({ name, size: bytes.length });
    return Promise.resolve();
  }
  close(reason: EndReason): Promise<void> {
    this.closed = reason;
    return Promise.resolve();
  }

  /** Everything said, however it was said, for one assertion over the lot. */
  everything(): string {
    return [
      ...this.posts,
      ...this.notices.map((notice) => notice.text),
      ...this.replies.map((reply) => reply.text),
    ].join("\n");
  }

  /** The reaction a reader is left looking at. */
  finalReaction(messageId: string): ReactionOutcome | undefined {
    return this.reactions.filter((entry) => entry.messageId === messageId).pop()?.outcome;
  }
}

/** A sandbox whose launches are fake agents the test drives. */
class FakeSandbox implements Sandbox {
  readonly name = "bailey" as const;
  readonly agents: FakeAgent[] = [];
  readonly launched: SandboxLaunch[] = [];
  readonly stopped: string[] = [];
  launchFails: string | null = null;

  probe(): Promise<CapabilityReport> {
    return Promise.resolve({ backend: "bailey", gaps: [], notes: [] });
  }

  launch(launch: SandboxLaunch): Promise<SandboxHandle> {
    if (this.launchFails !== null) return Promise.reject(new Error(this.launchFails));
    this.launched.push(launch);
    const agent = new FakeAgent();
    this.agents.push(agent);

    return Promise.resolve({
      process: agent,
      name: `errand-${launch.sessionId}`,
      // The real containment rule, not an approximation: a double that is more
      // permissive than production tests nothing worth knowing.
      toHostPath: (path: string) => hostPathUnder("/workspace", launch.projectPath, path),
      stop: () => {
        this.stopped.push(launch.sessionId);
        agent.end(0);
        return Promise.resolve(false);
      },
    });
  }

  listOrphans(): Promise<string[]> {
    return Promise.resolve([]);
  }
  removeOrphans(): Promise<number> {
    return Promise.resolve(0);
  }

  get latest(): FakeAgent {
    const agent = this.agents[this.agents.length - 1];
    if (agent === undefined) throw new Error("no agent has been launched");
    return agent;
  }
}

/** Timers the test drives, so a deadline is a decision rather than a wait. */
class TestTimers implements Timers {
  private now = 0;
  private next = 1;
  private pending: { at: number; handler: () => void; handle: number }[] = [];

  setTimeout(handler: () => void, ms: number): unknown {
    const handle = this.next;
    this.next += 1;
    this.pending.push({ at: this.now + ms, handler, handle });
    return handle;
  }

  clearTimeout(handle: unknown): void {
    this.pending = this.pending.filter((timer) => timer.handle !== handle);
  }

  /** Advances time, firing everything due, in order. */
  advance(ms: number): void {
    const target = this.now + ms;
    for (;;) {
      const due = this.pending.filter((timer) => timer.at <= target)
        .sort((left, right) => left.at - right.at)[0];
      if (due === undefined) break;
      this.pending = this.pending.filter((timer) => timer.handle !== due.handle);
      this.now = due.at;
      due.handler();
    }
    this.now = target;
  }
}

function configWith(overrides: Record<string, unknown> = {}): Config {
  return validateConfig({
    chat: { token: "a.token.value", channelId: "chan", allowedUserIds: ["100000000000000001"] },
    agent: { provider: "anthropic", credentialName: "ANTHROPIC_API_KEY", credential: "secret" },
    projectRoot: "/tmp/errand-projects",
    stateDir: "/tmp/errand-state",
    ...overrides,
  });
}

const OWNER = "100000000000000001";
const GUEST = "200000000000000002";
const STRANGER = "300000000000000003";

function message(content: string, from = OWNER, id = "m1"): IncomingMessage {
  return { id, authorId: from, authorName: from === OWNER ? "amelia" : from, content };
}

interface Harness {
  session: Session;
  thread: FakeThread;
  sandbox: FakeSandbox;
  timers: TestTimers;
  scheduler: Scheduler;
  root: string;
  stateDir: string;
  ended: EndReason[];
  agent(): FakeAgent;
}

/** Starts a session against fakes, and tears down whatever it created. */
async function withSession(
  run: (harness: Harness) => Promise<void>,
  options: {
    config?: Config;
    first?: IncomingMessage;
    guestIds?: string[];
    memory?: MemoryStore;
    describeImages?: (images: AgentImage[], question: string) => Promise<string>;
    fetchAttachment?: (url: string) => Promise<Uint8Array>;
    openPullRequest?: (request: PullRequest) => Promise<string>;
    unavailable?: () => Promise<string | undefined>;
    start?: boolean;
  } = {},
): Promise<void> {
  const root = await Deno.makeTempDir({ prefix: "errand-session-" });
  const projectPath = join(root, "project");
  const stateDir = join(root, "state");
  Deno.mkdirSync(projectPath, { recursive: true });
  Deno.mkdirSync(stateDir, { recursive: true });

  const config = options.config ?? configWith();
  const thread = new FakeThread();
  const sandbox = new FakeSandbox();
  const timers = new TestTimers();
  const scheduler = new Scheduler(config.limits);
  const ended: EndReason[] = [];

  const session = new Session({
    id: "s1",
    project: { name: "demo", path: projectPath, prompt: "do the thing", wasExplicit: true },
    stateDir,
    thread,
    sandbox,
    scheduler,
    config,
    log: createLogger({}, () => {}),
    timers,
    ownerId: OWNER,
    ownerName: "amelia",
    operatorIds: [],
    guestIds: options.guestIds ?? [],
    ...(options.memory === undefined ? {} : { memory: options.memory }),
    ...(options.openPullRequest === undefined ? {} : { openPullRequest: options.openPullRequest }),
    ...(options.unavailable === undefined ? {} : { unavailable: options.unavailable }),
    ...(options.describeImages === undefined ? {} : { describeImages: options.describeImages }),
    ...(options.fetchAttachment === undefined ? {} : { fetchAttachment: options.fetchAttachment }),
    onEnded: (reason) => ended.push(reason),
  });

  const harness: Harness = {
    session,
    thread,
    sandbox,
    timers,
    scheduler,
    root,
    stateDir,
    ended,
    agent: () => sandbox.latest,
  };

  try {
    if (options.start !== false) {
      const starting = session.start(options.first ?? message("do the thing"));
      await settle();
      sandbox.latest.answer({ model: { contextWindow: 200_000 } });
      await starting;
    }
    await run(harness);
  } finally {
    if (!session.isEnded) await session.stop("shutdown");
    scheduler.shutdown();
    await Deno.remove(root, { recursive: true });
  }
}

/** Lets everything already queued as a microtask or a read settle. */
function settle(rounds = 6): Promise<void> {
  return new Promise((resolve) => {
    let left = rounds;
    const tick = (): void => {
      left -= 1;
      if (left <= 0) resolve();
      else setTimeout(tick, 0);
    };
    setTimeout(tick, 0);
  });
}

Deno.test("a session starts, says so, and sends its first prompt", () =>
  withSession(async ({ thread, agent }) => {
    await settle();

    assertStringIncludes(thread.notices[0]?.text ?? "", "ready, working in demo");
    assertEquals(thread.notices[0]?.level, "started");
    assertEquals(thread.turns, [1]);
    assertStringIncludes(agent().written.join("\n"), "do the thing");
    assertEquals(thread.busy, true);
  }));

/** The credential is the whole reason the agent can reach a model at all. */
Deno.test("the sandbox is launched with the project, state and credential", () =>
  withSession(({ sandbox }) => {
    const launch = sandbox.launched[0];

    assertEquals(launch?.provider, "anthropic");
    assertEquals(launch?.env.ANTHROPIC_API_KEY, "secret");
    assertEquals(launch?.resume, false);
    return Promise.resolve();
  }));

Deno.test("a sandbox that will not start ends the session, saying why", () =>
  withSession(async ({ session, sandbox, thread, ended }) => {
    sandbox.launchFails = "no room on this host";

    assertEquals(await session.start(message("go")), false);

    assertStringIncludes(thread.everything(), "could not start this session");
    assertStringIncludes(thread.everything(), "no room on this host");
    assertEquals(ended, ["startup failed"]);
  }, { start: false, config: configWith() }));

Deno.test("a turn is reported, priced, and closed by pinging whoever asked", () =>
  withSession(async ({ thread, agent }) => {
    await settle();
    agent().runTurn("I did the thing");
    await settle();

    assertEquals(thread.posts.includes("I did the thing"), true);
    assertStringIncludes(thread.everything(), `<@${OWNER}>`);
    assertEquals(thread.usage?.turns, 1);
    assertEquals(thread.usage?.contextWindow, 200_000);
    assertEquals(thread.busy, false);
    assertEquals(thread.finalReaction("m1"), "succeeded");
  }));

/** Saying something to a working agent redirects it: same turn, no new slot. */
Deno.test("a message during a running turn steers it rather than queueing", () =>
  withSession(async ({ session, thread, agent }) => {
    await settle();
    agent().send({ type: "agent_start" });
    await settle();

    await session.handle(message("actually, do it this way", OWNER, "m2"));

    assertEquals(thread.turns, [1]);
    assertStringIncludes(agent().written.join("\n"), '"steer"');
    assertEquals(thread.finalReaction("m2"), "accepted");
  }));

Deno.test("an aside is kept for the thread and never reaches the agent", () =>
  withSession(async ({ session, thread, agent }) => {
    await settle();
    const before = agent().written.length;

    await session.handle(message("!!! ignore this, talking to you two", OWNER, "m2"));

    assertEquals(thread.asides, ["amelia: ignore this, talking to you two"]);
    assertEquals(agent().written.length, before);
    assertEquals(thread.finalReaction("m2"), "succeeded");
  }));

/** Deciding this later would make the marker unreliable, which defeats it. */
Deno.test("an aside that reads like a command still runs nothing", () =>
  withSession(async ({ session, thread }) => {
    await settle();

    await session.handle(message("!!! !stop", OWNER, "m2"));

    assertEquals(thread.asides.length, 1);
    assertEquals(thread.closed, undefined);
  }));

/** Another bot's command is not this one's to answer, or to pay a model for. */
Deno.test("an unknown command is left alone entirely", () =>
  withSession(async ({ session, thread, agent }) => {
    await settle();
    const before = agent().written.length;

    await session.handle(message("!somebodyelses thing", OWNER, "m2"));

    assertEquals(agent().written.length, before);
    assertEquals(thread.reactions.filter((entry) => entry.messageId === "m2"), []);
  }));

Deno.test("a stranger is refused once, but answered every time", () =>
  withSession(async ({ session, thread }) => {
    await settle();

    await session.handle(message("let me in", STRANGER, "m2"));
    await session.handle(message("please", STRANGER, "m3"));

    assertEquals(thread.replies.filter((reply) => reply.command === "refused").length, 1);
    assertEquals(thread.finalReaction("m2"), "failed");
    assertEquals(thread.finalReaction("m3"), "failed");
  }));

Deno.test("a guest may prompt, and only the owner may end the session", () =>
  withSession(async ({ session, thread }) => {
    await settle();

    await session.handle(message("have a look at the tests", GUEST, "m2"));
    await session.handle(message("!stop", GUEST, "m3"));

    assertEquals(thread.prompts.some((line) => line.includes("have a look")), true);
    assertStringIncludes(thread.everything(), "who started this session");
    assertEquals(thread.closed, undefined);
  }, { guestIds: [GUEST] }));

Deno.test("the owner can invite somebody and withdraw them again", () =>
  withSession(async ({ session, thread }) => {
    await settle();
    const changes: string[][] = [];

    await session.handle(message(`!allow <@${GUEST}>`, OWNER, "m2"));
    changes.push(session.guestList);
    await session.handle(message(`!deny ${GUEST}`, OWNER, "m3"));
    changes.push(session.guestList);

    assertEquals(changes, [[GUEST], []]);
    assertStringIncludes(thread.everything(), "can now prompt this session");
  }));

Deno.test("inviting somebody who is not named is refused, not guessed at", () =>
  withSession(async ({ session, thread }) => {
    await settle();

    await session.handle(message("!allow amelia", OWNER, "m2"));

    assertEquals(session.guestList, []);
    assertStringIncludes(thread.everything(), "say who");
  }));

Deno.test("a command's answer is marked as that command's reply", () =>
  withSession(async ({ session, thread }) => {
    await settle();

    await session.handle(message("!pwd", OWNER, "m2"));

    assertEquals(thread.replies.some((reply) => reply.command === "!pwd"), true);
    assertEquals(thread.posts.some((post) => post.includes("demo")), false);
  }));

Deno.test("the project can be listed and read without asking the agent", () =>
  withSession(async ({ session, thread, root, agent }) => {
    Deno.writeTextFileSync(join(root, "project", "readme.md"), "hello\n");
    await settle();
    const before = agent().written.length;

    await session.handle(message("!ls", OWNER, "m2"));
    await session.handle(message("!cat readme.md", OWNER, "m3"));

    const said = thread.replies.map((reply) => reply.text).join("\n");
    assertStringIncludes(said, "readme.md");
    assertStringIncludes(said, "hello");
    assertEquals(agent().written.length, before);
  }));

/** The same containment the sandbox applies, so nothing else can be read. */
Deno.test("a path outside the project is refused", () =>
  withSession(async ({ session, thread }) => {
    await settle();

    await session.handle(message("!cat ../../etc/passwd", OWNER, "m2"));

    assertStringIncludes(thread.everything(), "is not inside this session's project");
  }));

Deno.test("a file can be uploaded from the project on request", () =>
  withSession(async ({ session, thread, root }) => {
    Deno.writeTextFileSync(join(root, "project", "notes.txt"), "some notes\n");
    await settle();

    await session.handle(message("!file notes.txt", OWNER, "m2"));

    assertEquals(thread.uploads[0]?.name, "notes.txt");
    assertEquals(thread.uploads[0]?.size, 11);
  }));

Deno.test("stopping ends the session once and tears the sandbox down", () =>
  withSession(async ({ session, thread, ended, sandbox }) => {
    await settle();

    await session.handle(message("!stop", OWNER, "m2"));
    await session.stop("stopped");

    assertEquals(ended, ["stopped"]);
    assertEquals(thread.closed, "stopped");
    assertEquals(sandbox.stopped, ["s1"]);
    assertEquals(session.isEnded, true);
  }));

Deno.test("a session that hears nothing for long enough ends itself", () =>
  withSession(async ({ thread, timers, ended }) => {
    await settle();

    timers.advance(1_800_001);
    await settle();

    assertEquals(ended, ["idle"]);
    assertStringIncludes(thread.everything(), "nothing happened for a while");
  }));

/** 137 is how a container killed for passing a limit ends. */
Deno.test("an agent killed for a resource limit names the limit", () =>
  withSession(async ({ agent, thread, ended }) => {
    await settle();

    agent().end(137);
    await settle();

    assertEquals(ended, ["resource limit"]);
    assertStringIncludes(thread.everything(), "resource limit");
  }));

Deno.test("an agent that dies unexpectedly ends the session as a crash", () =>
  withSession(async ({ agent, thread, ended }) => {
    await settle();

    agent().end(1);
    await settle();

    assertEquals(ended, ["crashed"]);
    assertStringIncludes(thread.everything(), "exit code 1");
  }));

/**
 * A prompt can wait in the queue for as long as the queue allows, and the
 * window can be spent in that time. A slot held by a turn that never ran
 * shrinks the concurrency cap for good.
 */
Deno.test("a spent provider window refuses a prompt before it takes a slot", () =>
  withSession(async ({ session, thread, scheduler }) => {
    await settle();

    await session.handle(message("do more work", OWNER, "m2"));

    assertStringIncludes(thread.everything(), "come back at nine");
    assertEquals(thread.finalReaction("m2"), "failed");
    assertEquals(scheduler.turnsInFlight, 0);
  }, { unavailable: () => Promise.resolve("come back at nine") }));

Deno.test("what is remembered is written into the agent's own prompt", async () => {
  const memory = new MemoryStore(":memory:");
  memory.rememberUser(OWNER, "amelia");
  memory.remember("user", OWNER, "prefers jj over git", "earlier");
  memory.remember("project", "demo", "the build is deno task check", "earlier");

  await withSession(({ sandbox, stateDir }) => {
    const path = sandbox.launched[0]?.systemPromptPath;
    assertEquals(path, join(stateDir, "memory.md"));
    const written = Deno.readTextFileSync(path as string);

    assertStringIncludes(written, "You are talking to amelia.");
    assertStringIncludes(written, "prefers jj over git");
    assertStringIncludes(written, "the build is deno task check");
    return Promise.resolve();
  }, { memory });
  memory.close();
});

/** Facts are about the person talking, not about whoever opened the thread. */
Deno.test("what the agent writes down is remembered when the turn settles", async () => {
  const memory = new MemoryStore(":memory:");

  await withSession(async ({ agent, stateDir }) => {
    await settle();
    Deno.writeTextFileSync(join(stateDir, "remember.md"), "- likes short commits\n");
    Deno.writeTextFileSync(join(stateDir, "project-notes.md"), "- the tests live in src\n");

    agent().runTurn();
    await settle();

    assertEquals(memory.factsFor("user", OWNER).map((f) => f.fact), ["likes short commits"]);
    assertEquals(memory.factsFor("project", "demo").map((f) => f.fact), ["the tests live in src"]);
    // Emptied, so the same line is never ingested twice.
    assertEquals(Deno.readTextFileSync(join(stateDir, "remember.md")), "");
  }, { memory });
  memory.close();
});

Deno.test("a pull request the agent asked for opens when somebody asked too", async () => {
  const opened: { title: string; requestedBy: string }[] = [];

  await withSession(async ({ session, agent, stateDir, thread }) => {
    await settle();
    await session.handle(message("open a pull request when you are done", OWNER, "m2"));
    Deno.writeTextFileSync(join(stateDir, "pull-request.txt"), "Fix the parser\n");

    agent().runTurn();
    await settle();

    assertEquals(opened[0]?.title, "Fix the parser");
    assertEquals(opened[0]?.requestedBy, "amelia");
    assertStringIncludes(thread.everything(), "https://github.com/x/y/pull/1");
    // Cleared, so it is not reopened after every turn that follows.
    assertEquals(existsSync(join(stateDir, "pull-request.txt")), false);
  }, {
    config: configWith({
      github: { token: "ghp", userName: "errand-bot", userEmail: "bot@example.com" },
    }),
    openPullRequest: (request) => {
      opened.push(request as { title: string; requestedBy: string });
      return Promise.resolve("https://github.com/x/y/pull/1");
    },
  });
});

/** An unasked-for pull request spends somebody else's review time. */
Deno.test("a pull request nobody asked for is refused and cleared", async () => {
  let opened = 0;

  await withSession(async ({ agent, stateDir, thread }) => {
    await settle();
    Deno.writeTextFileSync(join(stateDir, "pull-request.txt"), "Unasked for\n");

    agent().runTurn();
    await settle();

    assertEquals(opened, 0);
    assertStringIncludes(thread.everything(), "not by anyone here");
    assertEquals(existsSync(join(stateDir, "pull-request.txt")), false);
  }, {
    config: configWith({
      github: { token: "ghp", userName: "errand-bot", userEmail: "bot@example.com" },
    }),
    openPullRequest: () => {
      opened += 1;
      return Promise.resolve("https://github.com/x/y/pull/1");
    },
  });
});

Deno.test("a session with no GitHub identity says so rather than failing", () =>
  withSession(async ({ session, thread }) => {
    await settle();

    await session.handle(message("!pr Do the thing", OWNER, "m2"));

    assertStringIncludes(thread.everything(), "no GitHub identity is configured");
    assertEquals(thread.finalReaction("m2"), "failed");
  }));

Deno.test("the git identity and the gh wrapper are in place before the launch", () =>
  withSession(({ stateDir, sandbox }) => {
    const config = Deno.readTextFileSync(join(stateDir, "home", ".gitconfig"));
    const shim = Deno.readTextFileSync(join(stateDir, "home", "bin", "gh"));

    assertStringIncludes(config, "errand-bot");
    assertStringIncludes(shim, "pull requests here are opened by the daemon");
    assertEquals(sandbox.launched[0]?.env.GIT_AUTHOR_NAME, "errand-bot");
    assertEquals(sandbox.launched[0]?.env.GH_TOKEN, "ghp");
    return Promise.resolve();
  }, {
    config: configWith({
      github: { token: "ghp", userName: "errand-bot", userEmail: "bot@example.com" },
    }),
  }));

Deno.test("what a tool did is reported, and its output truncated", () =>
  withSession(async ({ agent, thread }) => {
    await settle();
    agent().send({
      type: "tool_execution_start",
      toolCallId: "t1",
      toolName: "bash",
      args: { command: "ls -la" },
    });
    agent().send({
      type: "tool_execution_end",
      toolCallId: "t1",
      toolName: "bash",
      result: { content: [{ type: "text", text: "x".repeat(5_000) }] },
    });
    await settle();

    assertStringIncludes(thread.activity.join("\n"), "`bash`");
    assertEquals(thread.results[0]?.output.includes("truncated"), true);
  }));

Deno.test("an edit is shown as a diff of what actually changed", () =>
  withSession(async ({ agent, thread, root }) => {
    const file = join(root, "project", "main.ts");
    Deno.writeTextFileSync(file, "const x = 1;\n");
    await settle();

    agent().send({
      type: "tool_execution_start",
      toolCallId: "t1",
      toolName: "edit",
      args: { path: "/workspace/main.ts" },
    });
    await settle();
    Deno.writeTextFileSync(file, "const x = 2;\n");
    agent().send({
      type: "tool_execution_end",
      toolCallId: "t1",
      toolName: "edit",
      result: { content: [{ type: "text", text: "written" }] },
    });
    await settle();

    assertEquals(thread.diffs[0]?.path, "main.ts");
    assertEquals(thread.diffs[0]?.added, 1);
    assertEquals(thread.diffs[0]?.removed, 1);
  }));

Deno.test("a question from the agent is asked in the thread and answered back", () =>
  withSession(async ({ session, agent, thread }) => {
    await settle();
    agent().send({
      type: "extension_ui_request",
      id: "d1",
      method: "confirm",
      title: "Delete the branch?",
    });
    await settle();

    await session.handle(message("yes", OWNER, "m2"));

    assertStringIncludes(thread.everything(), "Delete the branch?");
    assertStringIncludes(agent().written.join("\n"), "extension_ui_response");
    assertEquals(thread.finalReaction("m2"), "accepted");
  }));

Deno.test("interrupting with nothing running says so and changes nothing", () =>
  withSession(async ({ session, agent, thread }) => {
    await settle();
    agent().runTurn();
    await settle();

    await session.handle(message("!interrupt", OWNER, "m2"));

    assertStringIncludes(thread.everything(), "nothing running to interrupt");
  }));

Deno.test("compacting is refused while a turn is running", () =>
  withSession(async ({ session, thread }) => {
    await settle();

    await session.handle(message("!compact", OWNER, "m2"));

    assertStringIncludes(thread.everything(), "a turn is running");
    assertEquals(thread.finalReaction("m2"), "failed");
  }));

Deno.test("the status says what the session and the queue are doing", () =>
  withSession(async ({ session, thread }) => {
    await settle();

    await session.handle(message("!status", OWNER, "m2"));

    const said = thread.replies.map((reply) => reply.text).join("\n");
    assertStringIncludes(said, "project: demo");
    assertStringIncludes(said, "running a turn");
  }));

Deno.test("help is listed without troubling the agent", () =>
  withSession(async ({ session, thread, agent }) => {
    await settle();
    const before = agent().written.length;

    await session.handle(message("!help", OWNER, "m2"));

    assertStringIncludes(thread.replies.map((reply) => reply.text).join("\n"), "!steer");
    assertEquals(agent().written.length, before);
  }));

/**
 * Reported once as "exit code 1", for a host that had simply run out of
 * space. That is a thing somebody can fix and a thing they cannot guess.
 */
Deno.test("an agent that died for want of disk says so, not just a code", () =>
  withSession(async ({ agent, thread, ended }) => {
    await settle();

    agent().complain("Error: ENOSPC: no space left on device, write");
    await settle();
    agent().end(1);
    await settle();

    assertStringIncludes(thread.everything(), "run out of disk space");
    assertEquals(thread.everything().includes("exit code 1"), false);
    assertEquals(ended, ["resource limit"]);
  }));

Deno.test("an agent that died for want of memory says that instead", () =>
  withSession(async ({ agent, thread }) => {
    await settle();

    agent().complain("FATAL ERROR: Cannot allocate memory");
    await settle();
    agent().end(1);
    await settle();

    assertStringIncludes(thread.everything(), "run out of memory");
  }));

/** A number on its own is not something a reader can act on. */
Deno.test("an unexplained crash still carries the agent's last words", () =>
  withSession(async ({ agent, thread, ended }) => {
    await settle();

    agent().complain("TypeError: cannot read properties of undefined");
    await settle();
    agent().end(1);
    await settle();

    assertStringIncludes(thread.everything(), "exit code 1");
    assertStringIncludes(thread.everything(), "TypeError");
    assertEquals(ended, ["crashed"]);
  }));

/**
 * A thread archived when its session ends drops out of the sidebar, and the
 * people who were in it then have to go hunting for it.
 */
Deno.test("a session that ends leaves its thread where people can find it", () =>
  withSession(async ({ session, thread, timers }) => {
    await settle();

    timers.advance(1_800_001);
    await settle();

    assertEquals(session.isEnded, true);
    assertEquals(thread.closed, "idle");
    assertStringIncludes(thread.notices[thread.notices.length - 1]?.text ?? "", "ended");
  }));

/** An explicit stop is somebody saying they are finished with the thread. */
Deno.test("stopping says so, so the thread can be archived", () =>
  withSession(async ({ session, thread }) => {
    await settle();

    await session.handle(message("!stop", OWNER, "m2"));

    assertEquals(thread.closed, "stopped");
  }));

const PNG = new Uint8Array([0x89, 0x50, 0x4e, 0x47]);

function withImage(content: string, id = "m2"): IncomingMessage {
  return {
    id,
    authorId: OWNER,
    authorName: "amelia",
    content,
    attachments: [{
      id: "a1",
      name: "screenshot.png",
      url: "https://files.example/screenshot.png",
      size: PNG.length,
      contentType: "image/png",
    }],
  };
}

Deno.test("an attached file is saved and the agent is told where it went", () =>
  withSession(async ({ session, agent, root }) => {
    await settle();

    await session.handle(withImage("what does this say?"));

    assertEquals(
      Deno.readFileSync(join(root, "project", "attachments", "screenshot.png")).length,
      PNG.length,
    );
    assertStringIncludes(agent().written.join("\n"), "attachments/screenshot.png");
  }, { fetchAttachment: () => Promise.resolve(PNG) }));

/** A message carrying only a file still says something: that a file arrived. */
Deno.test("a message with no text but a file still starts a turn", () =>
  withSession(async ({ session, thread, agent }) => {
    await settle();
    agent().runTurn();
    await settle();

    await session.handle(withImage(""));

    assertEquals(thread.turns, [1, 2]);
  }, { fetchAttachment: () => Promise.resolve(PNG) }));

/**
 * A model chosen for code is often text only. Handing it an image would fail
 * the turn, so one that can see is asked to describe it instead.
 */
Deno.test("an image is described for a model that cannot see it", () =>
  withSession(async ({ session, agent }) => {
    await settle();

    await session.handle(withImage("what does this error say?"));

    const sent = agent().written.join("\n");
    assertStringIncludes(sent, "it says ENOSPC");
    // The image itself is not handed over, which is the whole point.
    assertEquals(sent.includes('"images"'), false);
  }, {
    fetchAttachment: () => Promise.resolve(PNG),
    describeImages: (_images, question) =>
      Promise.resolve(`described: it says ENOSPC (${question})`),
  }));

/** Losing the description must not lose the message it came with. */
Deno.test("a description that fails leaves the path and says what went wrong", () =>
  withSession(async ({ session, agent, thread }) => {
    await settle();

    await session.handle(withImage("look at this"));

    assertStringIncludes(thread.everything(), "the describing model refused");
    assertStringIncludes(agent().written.join("\n"), "attachments/screenshot.png");
  }, {
    fetchAttachment: () => Promise.resolve(PNG),
    describeImages: () => Promise.reject(new Error("the describing model refused")),
  }));

/** With no describer the images go over as they are, which is the normal case. */
Deno.test("a model that can see is handed the image itself", () =>
  withSession(async ({ session, agent }) => {
    await settle();

    await session.handle(withImage("what is this?"));

    assertStringIncludes(agent().written.join("\n"), '"images"');
  }, { fetchAttachment: () => Promise.resolve(PNG) }));
