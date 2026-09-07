import { assertEquals, assertStringIncludes } from "@std/assert";
import { MessageFlags, type ThreadChannel } from "discord.js";
import { createLogger } from "../log.ts";
import { ChatThread, plain } from "./threads.ts";

/** One message the thread sent, and whatever it was later edited to. */
interface Sent {
  id: string;
  content: string;
  flags: unknown;
  edits: string[];
  reactions: string[];
  removed: string[];
}

/**
 * A thread channel as this module uses it.
 *
 * Hand-made rather than mocked: what matters is what was sent, what was
 * edited, and what was archived, which is all observable from here.
 */
function fakeChannel() {
  const sent: Sent[] = [];
  const deleted: string[] = [];
  let archived = false;
  let typing = 0;
  let next = 1;

  const message = (id: string, content: string, flags: unknown): Sent => {
    const record: Sent = { id, content, flags, edits: [], reactions: [], removed: [] };
    return record;
  };

  const asMessage = (record: Sent) => ({
    id: record.id,
    edit: (options: { content: string }) => {
      record.edits.push(options.content);
      record.content = options.content;
      return Promise.resolve(asMessage(record));
    },
    react: (glyph: string) => {
      record.reactions.push(glyph);
      return Promise.resolve({});
    },
    reactions: {
      cache: {
        get: (glyph: string) => ({
          users: {
            remove: () => {
              record.removed.push(glyph);
              return Promise.resolve({});
            },
          },
        }),
      },
    },
    client: { user: { id: "bot" } },
  });

  const channel = {
    id: "thread-1",
    parent: null,
    get archived(): boolean {
      return archived;
    },
    send: (options: { content: string; flags?: unknown }) => {
      const record = message(`sent-${next}`, options.content, options.flags);
      next += 1;
      sent.push(record);
      return Promise.resolve(asMessage(record));
    },
    sendTyping: () => {
      typing += 1;
      return Promise.resolve();
    },
    setArchived: (value: boolean) => {
      archived = value;
      return Promise.resolve(channel);
    },
    messages: {
      fetch: (id: string) => {
        const found = sent.find((record) => record.id === id);
        return found === undefined
          ? Promise.reject(new Error("unknown message"))
          : Promise.resolve(asMessage(found));
      },
      delete: (id: string) => {
        deleted.push(id);
        return Promise.resolve({});
      },
    },
  };

  return {
    channel: channel as unknown as ThreadChannel,
    sent,
    deleted,
    isArchived: () => archived,
    typingCount: () => typing,
  };
}

function thread(forwardToolOutput = false) {
  const fake = fakeChannel();
  return {
    ...fake,
    port: new ChatThread(fake.channel, createLogger({}, () => {}), forwardToolOutput),
  };
}

Deno.test("what is posted arrives, and nothing carries a link preview", async () => {
  const { port, sent } = thread();

  await port.post("here is https://example.com/thing");
  await port.flushForTest();

  assertEquals(sent.length, 1);
  assertEquals(sent[0]?.content, "here is https://example.com/thing");
  assertEquals(sent[0]?.flags, MessageFlags.SuppressEmbeds);
  assertEquals(plain("x").flags, MessageFlags.SuppressEmbeds);
});

Deno.test("a long message is split into ones the service will take", async () => {
  const { port, sent } = thread();

  await port.post(Array.from({ length: 400 }, (_unused, index) => `line ${index}`).join("\n"));
  await port.flushForTest();

  assertEquals(sent.length > 1, true);
  for (const message of sent) assertEquals([...message.content].length <= 2_000, true);
});

/** A run of tool calls is one thing the agent is doing, not ten messages. */
Deno.test("consecutive tool activity edits one message instead of posting more", async () => {
  const { port, sent } = thread();

  await port.appendActivity("ran `ls`");
  await port.appendActivity("ran `cat`");
  await port.appendActivity("ran `grep`");
  await port.flushForTest();

  assertEquals(sent.length, 1);
  assertStringIncludes(sent[0]?.content ?? "", "ran `ls`");
  assertStringIncludes(sent[0]?.content ?? "", "ran `grep`");
  assertEquals(sent[0]?.edits.length, 2);
});

/** The agent speaking ends the block, or its words land under the tool calls. */
Deno.test("the agent speaking starts a fresh activity block", async () => {
  const { port, sent } = thread();

  await port.appendActivity("ran `ls`");
  await port.post("here is what I found");
  await port.appendActivity("ran `cat`");
  await port.flushForTest();

  assertEquals(sent.length, 3);
  assertEquals(sent[2]?.content, "ran `cat`");
  assertEquals(sent[2]?.edits, []);
});

/** One entry can exceed a whole message on its own, and none is dropped. */
Deno.test("an activity line too long for one message is split, not lost", async () => {
  const { port, sent } = thread();

  await port.appendActivity(`ran \`${"x".repeat(3_000)}\``);
  await port.flushForTest();

  assertEquals(sent.length, 2);
  assertEquals(sent.map((message) => message.content).join("").includes("x".repeat(2_500)), true);
});

Deno.test("the queue position is one message, updated and then taken away", async () => {
  const { port, sent, deleted } = thread();

  await port.setWaiting("waiting for a turn slot, position 2");
  await port.setWaiting("waiting for a turn slot, position 1");
  await port.setWaiting(null);

  assertEquals(sent.length, 1);
  assertStringIncludes(sent[0]?.edits[0] ?? "", "position 1");
  assertEquals(deleted, ["sent-1"]);
});

/** A scrolled-back thread should read as final state, not as a history. */
Deno.test("a reaction replaces the one before it", async () => {
  const { port, sent } = thread();
  await port.post("something to react to");
  await port.flushForTest();
  const target = sent[0] as Sent;

  await port.setReaction(target.id, "accepted");
  await port.setReaction(target.id, "succeeded");

  assertEquals(target.reactions.length, 2);
  assertEquals(target.removed.length, 1);
  assertEquals(target.removed[0], target.reactions[0]);
});

Deno.test("the same reaction twice is not set twice", async () => {
  const { port, sent } = thread();
  await port.post("something");
  await port.flushForTest();
  const target = sent[0] as Sent;

  await port.setReaction(target.id, "accepted");
  await port.setReaction(target.id, "accepted");

  assertEquals(target.reactions.length, 1);
});

/** Losing an acknowledgement must not disturb the session it acknowledged. */
Deno.test("a reaction on a message that is gone is not an error", async () => {
  const { port } = thread();

  await port.setReaction("no-such-message", "failed");
});

Deno.test("tool output is kept out of the thread unless it was asked for", async () => {
  const quiet = thread(false);
  const loud = thread(true);
  const result = { id: "t1", name: "bash", failed: false, output: "a listing" };

  quiet.port.noteToolResult(result);
  loud.port.noteToolResult(result);
  await quiet.port.flushForTest();
  await loud.port.flushForTest();

  assertEquals(quiet.sent, []);
  assertStringIncludes(loud.sent[0]?.content ?? "", "a listing");
});

Deno.test("an empty tool result is not posted even when output is forwarded", async () => {
  const { port, sent } = thread(true);

  port.noteToolResult({ id: "t1", name: "bash", failed: false, output: "   " });
  await port.flushForTest();

  assertEquals(sent, []);
});

/**
 * A thread archived because its session idled out drops off the sidebar, and
 * the people who were in it have to go hunting for it.
 */
Deno.test("a session that idled out leaves the thread open", async () => {
  const { port, isArchived } = thread();

  await port.close("idle");

  assertEquals(isArchived(), false);
  assertEquals(port.isClosed, true);
});

Deno.test("only a deliberate stop archives the thread", async () => {
  const stopped = thread();
  const crashed = thread();

  await stopped.port.close("stopped");
  await crashed.port.close("crashed");

  assertEquals(stopped.isArchived(), true);
  assertEquals(crashed.isArchived(), false);
});

/** Everything already said has to arrive before the thread is finished with. */
Deno.test("closing sends what is still queued first", async () => {
  const { port, sent } = thread();

  await port.post("the last thing I said");
  await port.close("stopped");

  assertEquals(sent.some((message) => message.content === "the last thing I said"), true);
});

Deno.test("a closed thread accepts nothing more", async () => {
  const { port, sent } = thread();

  await port.close("stopped");
  await port.post("too late");
  await port.flushForTest();

  assertEquals(sent, []);
});

/** The only sign during a long tool loop that a session is alive. */
Deno.test("the typing indicator is held while a turn runs and dropped after", () => {
  const { port, typingCount } = thread();

  port.setBusy(true);
  port.setBusy(true);
  assertEquals(typingCount(), 1);

  port.setBusy(false);
  port.setBusy(false);
});

Deno.test("an upload carries the file and its caption", async () => {
  const { port, sent } = thread();

  await port.upload("notes.txt", new TextEncoder().encode("hello"), "`notes.txt` 5 bytes");
  await port.flushForTest();

  assertStringIncludes(sent[0]?.content ?? "", "notes.txt");
});

Deno.test("what a thread does not show, it says nothing about", async () => {
  const { port, sent } = thread();

  await port.notePrompt();
  await port.noteAside();
  port.setUsage();
  port.noteThinking();
  port.beginTurn();
  await port.flushForTest();

  assertEquals(sent, []);
});

/** Output posted into a closed connection is lost, so it waits instead. */
Deno.test("nothing is sent while the connection is down", async () => {
  const { port, sent } = thread();

  port.setConnected(false);
  await port.post("held until it is back");
  await port.flushForTest();
  assertEquals(sent, []);

  port.setConnected(true);
  await port.flushForTest();
  assertEquals(sent.length, 1);
});
