import { assertEquals, assertStringIncludes, assertThrows } from "@std/assert";
import { join } from "@std/path";
import { Scheduler } from "../admission/scheduler.ts";
import type { AgentProcess } from "../agent/client.ts";
import { validateConfig } from "../config/validate.ts";
import { createLogger } from "../log.ts";
import type {
  CapabilityReport,
  Sandbox,
  SandboxHandle,
  SandboxLaunch,
} from "../sandbox/backend.ts";
import { hostPathUnder } from "../sandbox/paths.ts";
import { SessionManager, type ThreadFactory } from "../session/manager.ts";
import type { EndReason, ThreadPort } from "../session/port.ts";
import { ThreadRegistry } from "../session/registry.ts";
import { WebInterfaceError, WebServer } from "./server.ts";

const encoder = new TextEncoder();
const OWNER = "100000000000000001";

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

function fakeSandbox(): Sandbox {
  return {
    name: "bailey",
    probe: (): Promise<CapabilityReport> =>
      Promise.resolve({ backend: "bailey", gaps: [], notes: [] }),
    launch: (launch: SandboxLaunch): Promise<SandboxHandle> => {
      const agent = new QuietAgent();
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
}

function silentPort(): ThreadPort {
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
    close: (_reason: EndReason) => nothing(),
  };
}

function fakeThreads(): ThreadFactory {
  let next = 1;
  const made = (): { id: string; port: ThreadPort } => {
    const id = `thread-${next}`;
    next += 1;
    return { id, port: silentPort() };
  };
  return {
    create: () => Promise.resolve(made()),
    open: () => Promise.resolve(made()),
    portFor: () => Promise.resolve(silentPort()),
    release: () => {},
  };
}

interface Harness {
  server: WebServer;
  manager: SessionManager;
  root: string;
  get(path: string): Promise<Response>;
  post(path: string, body: unknown): Promise<Response>;
}

/** A port nothing else is likely to be on, chosen per test run. */
let nextPort = 39_000 + Math.floor(Math.random() * 2_000);

async function withServer(
  run: (harness: Harness) => Promise<void>,
  options: { observer?: boolean; assets?: boolean } = {},
): Promise<void> {
  const root = await Deno.makeTempDir({ prefix: "errand-web-" });
  const assets = join(root, "assets");
  if (options.assets !== false) {
    Deno.mkdirSync(assets, { recursive: true });
    Deno.writeTextFileSync(join(assets, "index.html"), "<!doctype html><title>errand</title>");
    Deno.writeTextFileSync(join(assets, "app.js"), "export const ready = true;\n");
  }

  const config = validateConfig({
    chat: { token: "a.token.value", channelId: "chan", allowedUserIds: [OWNER] },
    agent: { provider: "anthropic", credentialName: "ANTHROPIC_API_KEY", credential: "secret" },
    projectRoot: join(root, "projects"),
    stateDir: join(root, "state"),
    web: { host: "127.0.0.1", port: nextPort, observer: options.observer === true },
  });
  nextPort += 1;

  const log = createLogger({}, () => {});
  const scheduler = new Scheduler(config.limits);
  const manager = new SessionManager({
    config,
    sandbox: fakeSandbox(),
    scheduler,
    threads: fakeThreads(),
    registry: new ThreadRegistry(join(root, "threads.json"), log),
    log,
    makeId: () => "s1",
  });

  const server = new WebServer(
    config.web!,
    manager,
    assets,
    log,
    "guild-1",
    (id) => id === OWNER ? "amelia" : undefined,
  );

  const base = `http://127.0.0.1:${config.web!.port}`;
  try {
    if (options.assets !== false) server.start();
    await run({
      server,
      manager,
      root,
      get: (path) => fetch(`${base}${path}`),
      post: (path, body) => fetch(`${base}${path}`, { method: "POST", body: JSON.stringify(body) }),
    });
  } finally {
    await server.stop();
    await manager.shutdown();
    scheduler.shutdown();
    await Deno.remove(root, { recursive: true });
  }
}

Deno.test("the interface says what it will and will not allow", () =>
  withServer(async ({ get }) => {
    const answer = await (await get("/api/interface")).json();

    assertEquals(answer, { observer: false, guildId: "guild-1" });
  }));

Deno.test("a session that is running is listed, with what it was asked", () =>
  withServer(async ({ get, manager }) => {
    await manager.start({ id: "m1", authorId: OWNER, authorName: "amelia", content: "demo: go" });

    const listed = await (await get("/api/sessions")).json();

    assertEquals(listed.length, 1);
    assertEquals(listed[0].id, "demo-s1");
    assertEquals(listed[0].project, "demo");
    assertEquals(listed[0].live, true);
    assertEquals(listed[0].opening, "go");
    assertEquals(listed[0].threadId, "thread-1");
  }));

/** A session that stopped is not finished: sending to it picks it back up. */
Deno.test("a session whose sandbox has gone is still listed", () =>
  withServer(async ({ get, manager }) => {
    await manager.start({ id: "m1", authorId: OWNER, content: "demo: go" });
    await manager.endThread("thread-1", "idle");

    const listed = await (await get("/api/sessions")).json();

    assertEquals(listed.length, 1);
    assertEquals(listed[0].live, false);
    assertEquals(listed[0].id, "demo-s1");
  }));

Deno.test("a session can be started from the interface", () =>
  withServer(async ({ post, manager }) => {
    const answer = await post("/api/sessions", { project: "demo", prompt: "fix the parser" });

    assertEquals(answer.status, 200);
    assertEquals((await answer.json()).id, "demo-s1");
    assertEquals(manager.sessions.length, 1);
  }));

Deno.test("a session with nothing asked of it is refused", () =>
  withServer(async ({ post }) => {
    const answer = await post("/api/sessions", { project: "demo", prompt: "  " });

    assertEquals(answer.status, 400);
    assertStringIncludes((await answer.json()).error, "a prompt is required");
  }));

Deno.test("a message reaches the session it names", () =>
  withServer(async ({ post, manager }) => {
    await manager.start({ id: "m1", authorId: OWNER, content: "demo: go" });

    const answer = await post("/api/sessions/demo-s1/send", { text: "carry on" });

    assertEquals(answer.status, 200);
    assertEquals((await answer.json()).accepted, true);
  }));

Deno.test("a message to a session that never existed says so", () =>
  withServer(async ({ post }) => {
    const answer = await post("/api/sessions/nobody/send", { text: "hello" });

    assertEquals(answer.status, 404);
  }));

/** An observer that drew a composer would offer what it always refuses. */
Deno.test("an observing interface changes nothing and says why", () =>
  withServer(async ({ get, post }) => {
    assertEquals((await (await get("/api/interface")).json()).observer, true);

    const started = await post("/api/sessions", { project: "demo", prompt: "go" });
    const sent = await post("/api/sessions/demo-s1/send", { text: "hello" });

    assertEquals(started.status, 403);
    assertEquals(sent.status, 403);
    assertStringIncludes((await sent.json()).error, "cannot change anything");
  }, { observer: true }));

Deno.test("a live session is streamed, starting with a reset", () =>
  withServer(async ({ get, manager }) => {
    await manager.start({ id: "m1", authorId: OWNER, content: "demo: go" });

    const answer = await get("/api/sessions/demo-s1/stream");
    assertEquals(answer.headers.get("content-type"), "text/event-stream");

    const reader = (answer.body as ReadableStream<Uint8Array>).getReader();
    const first = new TextDecoder().decode((await reader.read()).value);
    assertStringIncludes(first, "event: reset");
    await reader.cancel();
  }));

Deno.test("a stream for a session that is not live is refused", () =>
  withServer(async ({ get }) => {
    const answer = await get("/api/sessions/nobody/stream");

    assertEquals(answer.status, 404);
    await answer.body?.cancel();
  }));

/** A stopped session has no stream, so its history is read back from disk. */
Deno.test("a stopped session's transcript is read from where it was written", () =>
  withServer(async ({ get, manager }) => {
    await manager.start({ id: "m1", authorId: OWNER, authorName: "amelia", content: "demo: go" });
    await manager.endThread("thread-1", "idle");

    const answer = await (await get("/api/sessions/demo-s1/transcript")).json();

    assertEquals(Array.isArray(answer.entries), true);
    assertEquals(answer.grouped, true);
    assertEquals(answer.entries.some((entry: { kind: string }) => entry.kind === "prompt"), true);
    assertEquals(answer.dropped, 0);
  }));

Deno.test("a transcript for a session nobody has heard of is not found", () =>
  withServer(async ({ get }) => {
    assertEquals((await get("/api/sessions/nobody/transcript")).status, 404);
  }));

Deno.test("a session's project can be listed and read", () =>
  withServer(async ({ get, manager, root }) => {
    await manager.start({ id: "m1", authorId: OWNER, content: "demo: go" });
    Deno.writeTextFileSync(join(root, "projects", "demo", "readme.md"), "hello\n");

    const tree = await (await get("/api/sessions/demo-s1/tree")).json();
    const file = await (await get("/api/sessions/demo-s1/file?path=readme.md")).json();

    assertEquals(tree.some((entry: { name: string }) => entry.name === "readme.md"), true);
    assertEquals(file.text, "hello\n");
    assertEquals(file.language, "md");
  }));

/** The same containment the sandbox applies, not a second looser one. */
Deno.test("a path outside the project is refused", () =>
  withServer(async ({ get, manager }) => {
    await manager.start({ id: "m1", authorId: OWNER, content: "demo: go" });

    const answer = await get("/api/sessions/demo-s1/file?path=../../../etc/passwd");

    assertEquals(answer.status, 403);
    assertStringIncludes((await answer.json()).error, "outside this session's project");
  }));

Deno.test("a file can be downloaded as itself", () =>
  withServer(async ({ get, manager, root }) => {
    await manager.start({ id: "m1", authorId: OWNER, content: "demo: go" });
    Deno.writeTextFileSync(join(root, "projects", "demo", "notes.txt"), "some notes\n");

    const answer = await get("/api/sessions/demo-s1/download?path=notes.txt");

    assertStringIncludes(answer.headers.get("content-disposition") ?? "", 'filename="notes.txt"');
    assertEquals(await answer.text(), "some notes\n");
  }));

Deno.test("the built interface is served, and a deep path lands on it", () =>
  withServer(async ({ get }) => {
    const index = await get("/");
    const asset = await get("/app.js");
    const deep = await get("/session/s1/whatever");

    assertStringIncludes(await index.text(), "<title>errand</title>");
    assertStringIncludes(asset.headers.get("content-type") ?? "", "text/javascript");
    assertStringIncludes(await deep.text(), "<title>errand</title>");
    await asset.body?.cancel();
  }));

/** A traversal in a request must not reach outside the built assets. */
Deno.test("an asset path that climbs out is not served", () =>
  withServer(async ({ get, root }) => {
    Deno.writeTextFileSync(join(root, "secret.txt"), "not for you");

    const answer = await get("/../secret.txt");

    assertEquals((await answer.text()).includes("not for you"), false);
  }));

Deno.test("an unknown API route is not the interface", () =>
  withServer(async ({ get }) => {
    const answer = await get("/api/nothing-here");

    assertEquals(answer.status, 404);
    assertStringIncludes((await answer.json()).error, "no such route");
  }));

/** Serving something broken is worse than saying it is not there. */
Deno.test("an interface that was never built refuses to serve", () =>
  withServer(({ server }) => {
    assertThrows(() => server.start(), WebInterfaceError, "not built");
    return Promise.resolve();
  }, { assets: false }));

/** A warning in a log is not a control, so a public bind is a refusal. */
Deno.test("an interface asked to bind publicly refuses to start", async () => {
  const root = await Deno.makeTempDir({ prefix: "errand-web-" });
  try {
    Deno.writeTextFileSync(join(root, "index.html"), "<!doctype html>");
    const server = new WebServer(
      { host: "0.0.0.0", port: 39_999, observer: false, publicUrl: undefined },
      // deno-lint-ignore no-explicit-any
      {} as any,
      root,
      createLogger({}, () => {}),
    );

    assertThrows(() => server.start(), WebInterfaceError, "every interface");
  } finally {
    await Deno.remove(root, { recursive: true });
  }
});
