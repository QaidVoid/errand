import { assertEquals, assertStringIncludes } from "@std/assert";
import { createLogger, formatLine, type LogLevel, toAscii } from "./log.ts";

const AT = new Date("2026-09-02T15:47:13.204Z");

function collected(): { lines: [LogLevel, string][]; sink: (l: LogLevel, s: string) => void } {
  const lines: [LogLevel, string][] = [];
  return { lines, sink: (level, line) => lines.push([level, line]) };
}

Deno.test("a line carries the time, the level, and the fields", () => {
  assertEquals(
    formatLine("info", "session started", { session: "a1", turns: 3 }, AT),
    "2026-09-02T15:47:13.204Z [info] session started session=a1 turns=3",
  );
});

Deno.test("a line with nothing to add ends after the message", () => {
  assertEquals(
    formatLine("warn", "no project", {}, AT),
    "2026-09-02T15:47:13.204Z [warn] no project",
  );
});

Deno.test("non-ascii is escaped rather than dropped, in messages and in fields", () => {
  assertEquals(toAscii("ok"), "ok");
  assertEquals(toAscii("done \u2713"), "done \\u{2713}");
  assertStringIncludes(formatLine("info", "posted \u{1F50C}", {}, AT), "\\u{1F50C}");
  assertStringIncludes(formatLine("info", "x", { name: "caf\u00e9" }, AT), "name=caf\\u{00E9}");
});

Deno.test("bound fields are on every line, and a later field wins", () => {
  const { lines, sink } = collected();
  const log = createLogger({ session: "a1" }, sink);

  log.info("first");
  log.with({ turn: 2 }).warn("second", { session: "a2" });

  assertStringIncludes(lines[0]?.[1] ?? "", "session=a1");
  assertStringIncludes(lines[1]?.[1] ?? "", "turn=2");
  assertStringIncludes(lines[1]?.[1] ?? "", "session=a2");
});

Deno.test("severity reaches the sink, so errors can go elsewhere", () => {
  const { lines, sink } = collected();
  const log = createLogger({}, sink);

  log.info("a");
  log.warn("b");
  log.error("c");

  assertEquals(lines.map(([level]) => level), ["info", "warn", "error"]);
});
