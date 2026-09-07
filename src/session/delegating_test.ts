import { assertEquals, assertStringIncludes } from "@std/assert";
import { join } from "@std/path";
import { Scheduler } from "../admission/scheduler.ts";
import { TurnDelegations } from "../agent/delegate.ts";
import { DELEGATE_DIR } from "../agent/requests.ts";
import { createLogger } from "../log.ts";
import { Delegating, labelled, type Outcome } from "./delegating.ts";

/** A model that answers whatever it is asked, and records the request. */
function answering(text = "it says ENOSPC") {
  const asked: { url: string; body: Record<string, unknown> }[] = [];
  const send = (url: string, init: RequestInit): Promise<Response> => {
    asked.push({ url, body: JSON.parse(String(init.body)) });
    return Promise.resolve(
      new Response(
        JSON.stringify({ choices: [{ message: { content: text } }], usage: { total_tokens: 12 } }),
      ),
    );
  };
  return { send, asked };
}

interface Harness {
  root: string;
  directory: string;
  watcher: Delegating;
  reported: Outcome[];
  asked: { url: string; body: Record<string, unknown> }[];
  request(id: string, body: unknown): void;
  answerOf(id: string): string | undefined;
  refusalOf(id: string): string | undefined;
}

async function withWatcher(
  run: (harness: Harness) => Promise<void>,
  options: { perTurn?: number; noTurn?: boolean; answer?: string } = {},
): Promise<void> {
  const root = await Deno.makeTempDir({ prefix: "errand-delegating-" });
  const directory = join(root, DELEGATE_DIR);
  Deno.mkdirSync(directory, { recursive: true });
  Deno.mkdirSync(join(root, "project"), { recursive: true });

  const scheduler = new Scheduler({
    maxConcurrentTurns: 4,
    maxLiveSessions: 4,
    maxQueueLength: 8,
    maxQueueWaitMs: 1_000,
  });
  const model = answering(options.answer);
  const reported: Outcome[] = [];

  const delegations = new TurnDelegations({
    sessionId: "s1",
    endpoint: { baseUrl: "https://api.example/v1", model: "flash", credential: "k" },
    scheduler,
    sources: {
      projectRoot: join(root, "project"),
      readFile: (path: string) => Promise.resolve(Deno.readTextFileSync(path)),
      outputOf: (callId: string) => (callId === "t1" ? "ENOSPC: no space left" : undefined),
      attachment: () => undefined,
    },
    deadlineMs: 5_000,
    perTurn: options.perTurn ?? 8,
    send: model.send,
  });

  const watcher = new Delegating({
    stateDir: root,
    delegations: () => (options.noTurn === true ? undefined : delegations),
    report: (outcome) => reported.push(outcome),
    log: createLogger({}, () => {}),
    setTimeout: () => 0,
    clearTimeout: () => {},
  });

  const read = (id: string, kind: string): string | undefined => {
    try {
      return Deno.readTextFileSync(join(directory, `${id}.${kind}`));
    } catch {
      return undefined;
    }
  };

  try {
    await run({
      root,
      directory,
      watcher,
      reported,
      asked: model.asked,
      request: (id, body) => {
        Deno.writeTextFileSync(join(directory, `${id}.request`), JSON.stringify(body));
      },
      answerOf: (id) => read(id, "answer"),
      refusalOf: (id) => read(id, "refused"),
    });
  } finally {
    watcher.stop();
    scheduler.shutdown();
    await Deno.remove(root, { recursive: true });
  }
}

Deno.test("a question about a call's output is asked and answered", () =>
  withWatcher(async (harness) => {
    harness.request("d1", { question: "what failed?", callId: "t1" });

    await harness.watcher.sweep();

    const answer = harness.answerOf("d1") ?? "";
    assertStringIncludes(answer, "it says ENOSPC");
    assertStringIncludes(answer, "flash was asked about");
    assertStringIncludes(answer, "description rather than the thing itself");
  }));

/** The model is shown one artefact and nothing about the session. */
Deno.test("only the question and the artefact are sent", () =>
  withWatcher(async (harness) => {
    harness.request("d1", { question: "what failed?", callId: "t1" });

    await harness.watcher.sweep();

    const body = harness.asked[0]?.body ?? {};
    const sent = JSON.stringify(body);
    assertEquals(body.tools, undefined);
    assertStringIncludes(sent, "what failed?");
    assertStringIncludes(sent, "ENOSPC: no space left");
    assertEquals(Array.isArray(body.messages) && body.messages.length, 1);
  }));

Deno.test("a question about a project file reads that file", () =>
  withWatcher(async (harness) => {
    Deno.writeTextFileSync(join(harness.root, "project", "main.ts"), "export const x = 1;\n");
    harness.request("d1", { question: "what does it export?", path: "main.ts" });

    await harness.watcher.sweep();

    assertStringIncludes(JSON.stringify(harness.asked[0]?.body), "export const x = 1;");
  }));

/** The same containment the agent is held to, not a second looser one. */
Deno.test("a file outside the project is refused, and nothing is asked", () =>
  withWatcher(async (harness) => {
    harness.request("d1", { question: "read this", path: "../../../etc/passwd" });

    await harness.watcher.sweep();

    assertEquals(harness.asked.length, 0);
    assertStringIncludes(harness.refusalOf("d1") ?? "", "outside this session's project");
    assertStringIncludes(harness.refusalOf("d1") ?? "", "carry on yourself");
  }));

/** A delegation with no artefact is a conversation, which is what this is not. */
Deno.test("a request naming nothing to look at is refused", () =>
  withWatcher(async (harness) => {
    harness.request("d1", { question: "what should I do about the parser?" });

    await harness.watcher.sweep();

    assertEquals(harness.asked.length, 0);
    assertStringIncludes(harness.refusalOf("d1") ?? "", "must name what to look at");
  }));

Deno.test("a request naming several things is refused", () =>
  withWatcher(async (harness) => {
    harness.request("d1", { question: "look", path: "main.ts", callId: "t1" });

    await harness.watcher.sweep();

    assertStringIncludes(harness.refusalOf("d1") ?? "", "one thing to look at, not several");
  }));

Deno.test("a request that is not a request at all is refused, not thrown", () =>
  withWatcher(async (harness) => {
    Deno.writeTextFileSync(join(harness.directory, "d1.request"), "{ not json");

    await harness.watcher.sweep();

    assertStringIncludes(harness.refusalOf("d1") ?? "", "could not be read");
  }));

/** The cap is per turn, so a loop cannot become a stream of requests. */
Deno.test("a turn stops delegating once it has used its allowance", () =>
  withWatcher(async (harness) => {
    for (const id of ["d1", "d2", "d3"]) {
      harness.request(id, { question: "what failed?", callId: "t1" });
    }

    await harness.watcher.sweep();

    assertEquals(harness.asked.length, 2);
    assertStringIncludes(harness.refusalOf("d3") ?? "", "already delegated 2 times");
  }, { perTurn: 2 }));

Deno.test("a delegation with no turn running is refused rather than queued", () =>
  withWatcher(async (harness) => {
    harness.request("d1", { question: "what failed?", callId: "t1" });

    await harness.watcher.sweep();

    assertEquals(harness.asked.length, 0);
    assertStringIncludes(harness.refusalOf("d1") ?? "", "no turn running");
  }, { noTurn: true }));

/** Answered twice is worse than answered late, so the request goes first. */
Deno.test("a request is taken away before it is run", () =>
  withWatcher(async (harness) => {
    harness.request("d1", { question: "what failed?", callId: "t1" });

    await harness.watcher.sweep();
    await harness.watcher.sweep();

    assertEquals(harness.asked.length, 1);
  }));

Deno.test("what happened is reported, with what it cost and saved", () =>
  withWatcher(async (harness) => {
    harness.request("d1", { question: "what failed?", callId: "t1" });

    await harness.watcher.sweep();

    const outcome = harness.reported[0];
    assertEquals(outcome?.asked, "what failed?");
    assertEquals("text" in (outcome ?? {}) ? (outcome as { model: string }).model : "", "flash");
    assertEquals("keptOut" in (outcome ?? {}) ? (outcome as { keptOut: number }).keptOut : 0, 21);
  }));

/** An agent that forgets it did not read the thing asserts a description. */
Deno.test("an answer says which model produced it and that it is a description", () => {
  const said = labelled({
    text: "the log shows a failed write",
    model: "glm-5.3-flash",
    describes: "the output of call t1",
    tokens: 12,
    keptOut: 4_000,
  });

  assertStringIncludes(said, "glm-5.3-flash was asked about the output of call t1");
  assertStringIncludes(said, "description rather than the thing itself");
  assertStringIncludes(said, "the log shows a failed write");
});
