import { assertEquals } from "@std/assert";
import { REDACTION } from "../config/redact.ts";
import type { SessionUsage, ThreadPort } from "./port.ts";
import { redacting } from "./redacted.ts";

const SECRET = "sk-live-9f3c7a11d4e6";

const USAGE: SessionUsage = {
  input: 1,
  output: 1,
  cacheRead: 0,
  cacheWrite: 0,
  totalTokens: 2,
  cost: 0,
  contextTokens: 1,
  turns: 1,
};

/** Records every argument every method was called with, as text. */
function recordingPort(): { port: ThreadPort; seen: string[] } {
  const seen: string[] = [];
  const note = (...values: unknown[]) => {
    seen.push(values.map((value) => JSON.stringify(value)).join(" "));
  };
  const port: ThreadPort = {
    post: (...a) => (note(...a), Promise.resolve()),
    postNotice: (...a) => (note(...a), Promise.resolve()),
    postReply: (...a) => (note(...a), Promise.resolve()),
    noteToolResult: (...a) => note(...a),
    noteDelegation: (...a) => note(...a),
    beginTurn: (...a) => note(...a),
    noteThinking: (...a) => note(...a),
    notePrompt: (...a) => (note(...a), Promise.resolve()),
    noteAside: (...a) => (note(...a), Promise.resolve()),
    appendActivity: (...a) => (note(...a), Promise.resolve()),
    postDiff: (...a) => (note(...a), Promise.resolve()),
    setWaiting: (...a) => (note(...a), Promise.resolve()),
    setReaction: (...a) => (note(...a), Promise.resolve()),
    setUsage: (...a) => note(...a),
    setBusy: (...a) => note(...a),
    upload: (name, bytes, caption) => (note(name, bytes.length, caption), Promise.resolve()),
    close: () => (note(), Promise.resolve()),
  };
  return { port, seen };
}

/**
 * Every call, with the secret in every string the agent can influence.
 *
 * A message id is not one of them: it is minted by the chat service and never
 * carries agent output, and neither does a turn number, a reaction, or a
 * usage total.
 */
const CALLS: [keyof ThreadPort, unknown[]][] = [
  ["post", [`the key is ${SECRET}`]],
  ["postNotice", [`starting with ${SECRET}`, "started"]],
  ["postReply", [`output ${SECRET}`, `!cat ${SECRET}`]],
  ["noteToolResult", [{ id: "1", name: "bash", failed: false, output: `env: ${SECRET}` }]],
  ["noteDelegation", [{
    question: `what is ${SECRET}`,
    model: "flash",
    answer: `it is ${SECRET}`,
  }]],
  ["beginTurn", [3]],
  ["noteThinking", [`I should use ${SECRET}`]],
  ["notePrompt", ["amelia", `use ${SECRET}`]],
  ["noteAside", ["amelia", `never mind ${SECRET}`]],
  ["appendActivity", [`ran ${SECRET}`, { name: "bash", target: `echo ${SECRET}` }]],
  ["postDiff", [`${SECRET}.ts`, 1, 0, `+ ${SECRET}`, `write ${SECRET}`]],
  ["setWaiting", [`waiting on ${SECRET}`]],
  ["setReaction", ["123456789", "succeeded"]],
  ["setUsage", [USAGE]],
  ["setBusy", [true]],
  ["upload", [`${SECRET}.txt`, new Uint8Array([1, 2]), `here is ${SECRET}`]],
  ["close", ["idle"]],
];

Deno.test("nothing a session reports carries the credential through", async () => {
  const inner = recordingPort();
  const wrapped = redacting(inner.port, [SECRET]);

  for (const [method, args] of CALLS) {
    // deno-lint-ignore no-explicit-any
    await (wrapped[method] as any)(...args);
  }

  const all = inner.seen.join("\n");
  assertEquals(all.includes(SECRET), false, all);
  assertEquals(all.includes(REDACTION), true);
});

/**
 * The point of the table above. A method added to the port and forgotten here
 * would otherwise be scrubbed by nobody and tested by nobody.
 */
Deno.test("every way of reporting something is covered", () => {
  const inner = recordingPort();
  const covered = new Set(CALLS.map(([method]) => method));

  assertEquals(
    Object.keys(inner.port).filter((name) => !covered.has(name as keyof ThreadPort)),
    [],
  );
});

/** With nothing to scrub the wrapper is not worth the indirection. */
Deno.test("with no secrets the port is passed through untouched", () => {
  const inner = recordingPort();

  assertEquals(redacting(inner.port, []), inner.port);
});

Deno.test("what is reported is otherwise unchanged", async () => {
  const inner = recordingPort();
  const wrapped = redacting(inner.port, [SECRET]);

  await wrapped.post("nothing secret here");
  wrapped.setBusy(true);

  assertEquals(inner.seen, ['"nothing secret here"', "true"]);
});
