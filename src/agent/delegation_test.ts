import { assertEquals } from "@std/assert";
import {
  type Delegation,
  isRefused,
  parseDelegation,
  resolveSource,
  type Sources,
} from "./delegation.ts";

const ROOT = "/projects/demo";

function sources(overrides: Partial<Sources> = {}): Sources {
  return {
    projectRoot: ROOT,
    readFile: (path: string) => Promise.resolve(`contents of ${path}`),
    outputOf: (callId: string) => (callId === "c1" ? "test result: 3 failed" : undefined),
    attachment: (name: string) => (name === "shot.png" ? "an image" : undefined),
    ...overrides,
  };
}

function accepted(raw: unknown): Delegation {
  const parsed = parseDelegation(raw);
  if (isRefused(parsed)) throw new Error(`expected acceptance, got: ${parsed.refused}`);
  return parsed;
}

Deno.test("a question about a file is accepted", () => {
  assertEquals(accepted({ question: "which tests fail?", path: "out.log" }), {
    question: "which tests fail?",
    source: { kind: "file", path: "out.log" },
  });
});

Deno.test("a question about a call's output or an attachment is accepted", () => {
  assertEquals(accepted({ question: "what broke?", callId: "c1" }).source, {
    kind: "output",
    callId: "c1",
  });
  assertEquals(accepted({ question: "what is shown?", attachment: "shot.png" }).source, {
    kind: "attachment",
    name: "shot.png",
  });
});

/**
 * A delegation with no artefact is a conversation with a second model, which
 * is the thing this deliberately is not.
 */
Deno.test("a delegation that names nothing is refused, with a reason", () => {
  const refused = parseDelegation({ question: "what should I do about the flaky test?" });
  assertEquals(isRefused(refused), true);
  assertEquals(
    isRefused(refused) ? refused.refused.includes("must name what to look at") : false,
    true,
  );
});

Deno.test("a delegation with no question is refused", () => {
  assertEquals(isRefused(parseDelegation({ path: "out.log" })), true);
  assertEquals(isRefused(parseDelegation({ question: "   ", path: "out.log" })), true);
});

Deno.test("a delegation names one thing, not several", () => {
  assertEquals(isRefused(parseDelegation({ question: "q", path: "a.log", callId: "c1" })), true);
});

Deno.test("anything that is not a request at all is refused", () => {
  assertEquals(isRefused(parseDelegation(null)), true);
  assertEquals(isRefused(parseDelegation("please describe the log")), true);
});

Deno.test("a file is read and named for attribution", async () => {
  const resolved = await resolveSource(
    accepted({ question: "q", path: "logs/out.txt" }),
    sources(),
  );

  assertEquals(isRefused(resolved), false);
  if (isRefused(resolved)) return;
  assertEquals(resolved.describes, "logs/out.txt");
  assertEquals(resolved.content, "contents of /projects/demo/logs/out.txt");
});

/** A delegation must not read what the session itself could not. */
Deno.test("a file outside the project is refused, however it is spelled", async () => {
  for (const path of ["../secrets.env", "/etc/passwd", "a/../../out"]) {
    const resolved = await resolveSource(accepted({ question: "q", path }), sources());
    assertEquals(isRefused(resolved), true, path);
    assertEquals(
      isRefused(resolved) ? resolved.refused.includes("outside this session's project") : false,
      true,
      path,
    );
  }
});

Deno.test("a file that cannot be read is refused rather than throwing", async () => {
  const resolved = await resolveSource(
    accepted({ question: "q", path: "missing.txt" }),
    sources({ readFile: () => Promise.reject(new Error("no such file")) }),
  );

  assertEquals(isRefused(resolved) ? resolved.refused.includes("could not be read") : false, true);
});

Deno.test("a call's output is found by id, and an unknown id is refused", async () => {
  const found = await resolveSource(accepted({ question: "q", callId: "c1" }), sources());
  assertEquals(isRefused(found) ? "" : found.content, "test result: 3 failed");

  const missing = await resolveSource(accepted({ question: "q", callId: "c9" }), sources());
  assertEquals(isRefused(missing), true);
});

Deno.test("an attachment is found by name, and an unknown one is refused", async () => {
  const found = await resolveSource(accepted({ question: "q", attachment: "shot.png" }), sources());
  assertEquals(isRefused(found) ? "" : found.describes, "shot.png");

  const missing = await resolveSource(
    accepted({ question: "q", attachment: "other.png" }),
    sources(),
  );
  assertEquals(isRefused(missing), true);
});
