import { assertEquals } from "@std/assert";
import { DEFAULT_RECONNECT, reconnectDelayMs, toRaw } from "./gateway.ts";
import { buildCommands, translate } from "./commands.ts";
import { COMMANDS } from "../session/commands.ts";

/**
 * A growing delay, then giving up. A sandbox nobody can reach or stop is
 * worse than no session, so this ends rather than retrying forever.
 */
Deno.test("each reconnection waits longer, up to a ceiling", () => {
  const delays = [1, 2, 3, 4, 5, 6, 7].map((attempt) =>
    reconnectDelayMs(attempt, DEFAULT_RECONNECT)
  );

  assertEquals(delays, [2_000, 4_000, 8_000, 16_000, 32_000, 60_000, 60_000]);
});

Deno.test("a policy of its own is followed, ceiling and all", () => {
  const policy = { baseDelayMs: 500, maxDelayMs: 1_000, maxAttempts: 3 };

  assertEquals(reconnectDelayMs(1, policy), 500);
  assertEquals(reconnectDelayMs(2, policy), 1_000);
  assertEquals(reconnectDelayMs(9, policy), 1_000);
});

/** A message with no author is one the library could not resolve. */
Deno.test("a message with no author is not a message", () => {
  // deno-lint-ignore no-explicit-any
  assertEquals(toRaw({ author: null, channel: {} } as any), null);
});

Deno.test("a library message is reduced to the facts the filter needs", () => {
  const raw = toRaw({
    id: "m1",
    author: { id: "u1", displayName: "amelia", username: "amelia1", bot: false },
    channel: { parentId: "chan" },
    channelId: "thread-1",
    content: "do the thing",
    attachments: new Map([["a1", {
      id: "a1",
      name: "shot.png",
      url: "https://files/shot.png",
      size: 12,
      contentType: "image/png",
    }]]),
    // deno-lint-ignore no-explicit-any
  } as any);

  assertEquals(raw?.authorId, "u1");
  assertEquals(raw?.authorName, "amelia");
  assertEquals(raw?.parentChannelId, "chan");
  assertEquals(raw?.channelId, "thread-1");
  assertEquals(raw?.attachments[0]?.name, "shot.png");
  assertEquals(raw?.attachments[0]?.contentType, "image/png");
});

/** A channel that is not a thread has no parent, and that is not an error. */
Deno.test("a top-level message has no parent channel", () => {
  const raw = toRaw({
    id: "m1",
    author: { id: "u1", username: "amelia", bot: false },
    channel: {},
    channelId: "chan",
    content: "hello",
    attachments: new Map(),
    // deno-lint-ignore no-explicit-any
  } as any);

  assertEquals(raw?.parentChannelId, undefined);
  assertEquals(raw?.authorName, "amelia");
});

/**
 * A slash command is the same command, not a second implementation: it becomes
 * the text a person would have typed and runs through the same code.
 */
Deno.test("an interaction becomes the text command it stands for", () => {
  const command = translate({
    commandName: "cat",
    options: {
      getString: (name: string) => (name === "path" ? "src/main.ts" : null),
      getUser: () => null,
    },
    channel: { isThread: () => true, id: "thread-1" },
    user: { id: "u1", displayName: "amelia", username: "amelia1" },
    // deno-lint-ignore no-explicit-any
  } as any);

  assertEquals(command.content, "!cat src/main.ts");
  assertEquals(command.threadId, "thread-1");
  assertEquals(command.userId, "u1");
  assertEquals(command.userName, "amelia");
});

Deno.test("a command naming an account carries the account id", () => {
  const command = translate({
    commandName: "allow",
    options: { getString: () => null, getUser: () => ({ id: "u2" }) },
    channel: { isThread: () => true, id: "thread-1" },
    user: { id: "u1", username: "amelia" },
    // deno-lint-ignore no-explicit-any
  } as any);

  assertEquals(command.content, "!allow u2");
});

Deno.test("a command with no argument is just the command", () => {
  const command = translate({
    commandName: "status",
    options: { getString: () => null, getUser: () => null },
    channel: { isThread: () => false },
    user: { id: "u1", username: "amelia" },
    // deno-lint-ignore no-explicit-any
  } as any);

  assertEquals(command.content, "!status");
  assertEquals(command.threadId, undefined);
});

/** One table, so what can be typed and what can be picked cannot drift. */
Deno.test("every command is offered as a slash command too", () => {
  const built = buildCommands().map((command) => command.name);

  assertEquals(built.sort(), Object.keys(COMMANDS).map((name) => name.slice(1)).sort());
});

Deno.test("a command that takes an argument declares it", () => {
  const definitions = new Map(buildCommands().map((command) => [command.name, command.toJSON()]));

  assertEquals(definitions.get("cat")?.options?.[0]?.required, true);
  assertEquals(definitions.get("ls")?.options?.[0]?.required, false);
  assertEquals(definitions.get("status")?.options ?? [], []);
});

/** Whoever reads the picker should know before they try. */
Deno.test("a command's description says who may run it", () => {
  const definitions = new Map(buildCommands().map((command) => [command.name, command.toJSON()]));

  assertEquals(definitions.get("stop")?.description.includes("owner only"), true);
  assertEquals(definitions.get("ls")?.description.includes("owner and invited"), true);
  assertEquals(definitions.get("shutdown")?.description.includes("named accounts"), true);
  assertEquals(definitions.get("status")?.description.includes("("), false);
});
