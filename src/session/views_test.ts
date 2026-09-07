import { assertEquals, assertStringIncludes } from "@std/assert";
import { createLogger } from "../log.ts";
import type { SessionUsage, ThreadPort } from "./port.ts";
import { type Recorded, ViewFanOut } from "./views.ts";

/** A view that writes down everything it was told, in order. */
function fakeView(name = "view") {
  const seen: string[] = [];
  const view: ThreadPort = {
    post: (text) => {
      seen.push(`post:${text}`);
      return Promise.resolve();
    },
    postNotice: (text, level) => {
      seen.push(`notice:${level}:${text}`);
      return Promise.resolve();
    },
    postReply: (text, command) => {
      seen.push(`reply:${command}:${text}`);
      return Promise.resolve();
    },
    noteToolResult: (result) => seen.push(`result:${result.id}:${result.failed}`),
    noteDelegation: (delegated) => seen.push(`delegated:${delegated.model ?? delegated.refused}`),
    beginTurn: (turn) => seen.push(`turn:${turn}`),
    noteThinking: (text) => seen.push(`thinking:${text}`),
    notePrompt: (author, text) => {
      seen.push(`prompt:${author}:${text}`);
      return Promise.resolve();
    },
    noteAside: (author, text) => {
      seen.push(`aside:${author}:${text}`);
      return Promise.resolve();
    },
    appendActivity: (line) => {
      seen.push(`activity:${line}`);
      return Promise.resolve();
    },
    postDiff: (path, added, removed) => {
      seen.push(`diff:${path}:+${added}-${removed}`);
      return Promise.resolve();
    },
    setWaiting: (text) => {
      seen.push(`waiting:${text}`);
      return Promise.resolve();
    },
    setReaction: (messageId, outcome) => {
      seen.push(`reaction:${messageId}:${outcome}`);
      return Promise.resolve();
    },
    setUsage: (usage) => seen.push(`usage:${usage.totalTokens}`),
    setBusy: (busy) => seen.push(`busy:${busy}`),
    upload: () => Promise.resolve(),
    close: () => Promise.resolve(),
  };
  return { name, seen, view };
}

const USAGE: SessionUsage = {
  input: 10,
  output: 2,
  cacheRead: 90,
  cacheWrite: 0,
  totalTokens: 102,
  cost: 0.01,
  contextTokens: 100,
  turns: 1,
};

function fanOut(limit?: number, recorder?: { append(entry: Recorded, turn: number): void }) {
  return new ViewFanOut(createLogger({}, () => {}), limit, recorder);
}

Deno.test("everything reported reaches every attached view", async () => {
  const fan = fanOut();
  const first = fakeView("a");
  const second = fakeView("b");
  await fan.attach(first.view);
  await fan.attach(second.view);

  await fan.post("hello");
  await fan.postNotice("started", "started");

  assertEquals(first.seen, ["post:hello", "notice:started:started"]);
  assertEquals(second.seen, first.seen);
});

/** A browser that closed must not stop the chat thread being told. */
Deno.test("a view that fails is skipped, and the others still hear it", async () => {
  const fan = fanOut();
  const good = fakeView();
  const broken = fakeView();
  broken.view.post = () => Promise.reject(new Error("this view is gone"));

  await fan.attach(broken.view);
  await fan.attach(good.view);
  await fan.post("still delivered");

  assertEquals(good.seen, ["post:still delivered"]);
});

/** Several of these are synchronous, so a throw would escape rather than settle. */
Deno.test("a view that throws synchronously is skipped too", async () => {
  const fan = fanOut();
  const good = fakeView();
  const broken = fakeView();
  broken.view.setBusy = () => {
    throw new Error("this view is gone");
  };
  broken.view.beginTurn = () => {
    throw new Error("this view is gone");
  };

  await fan.attach(broken.view);
  await fan.attach(good.view);

  fan.beginTurn(3);
  fan.setBusy(true);
  await fan.post("still delivered");

  assertEquals(good.seen, ["turn:3", "busy:true", "post:still delivered"]);
});

Deno.test("a view that attaches late is shown what it missed", async () => {
  const fan = fanOut();
  await fan.post("before it joined");
  await fan.appendActivity("ran something");

  const late = fakeView();
  await fan.attach(late.view);

  assertEquals(late.seen, ["turn:0", "post:before it joined", "activity:ran something"]);
});

Deno.test("detaching leaves the session and the other views alone", async () => {
  const fan = fanOut();
  const staying = fakeView();
  const leaving = fakeView();
  await fan.attach(staying.view);
  const detach = await fan.attach(leaving.view);

  detach();
  await fan.post("after one left");

  assertEquals(fan.size, 1);
  assertEquals(staying.seen, ["post:after one left"]);
  assertEquals(leaving.seen, []);
});

/**
 * Only the latest total is meaningful, so a late view is told what it is now
 * rather than watching it climb through every turn.
 */
Deno.test("cost is state: the latest is given, none of it is replayed", async () => {
  const fan = fanOut();
  fan.setUsage({ ...USAGE, totalTokens: 100 });
  fan.setUsage({ ...USAGE, totalTokens: 250 });

  const late = fakeView();
  await fan.attach(late.view);

  assertEquals(late.seen.filter((line) => line.startsWith("usage:")), ["usage:250"]);
});

Deno.test("a late view is told the queue position and that work is running", async () => {
  const fan = fanOut();
  await fan.setWaiting("waiting for a turn slot, position 2");
  fan.setBusy(true);

  const late = fakeView();
  await fan.attach(late.view);

  assertStringIncludes(late.seen.join("\n"), "waiting:waiting for a turn slot, position 2");
  assertStringIncludes(late.seen.join("\n"), "busy:true");
});

/** A view that just attached already assumes nothing is running. */
Deno.test("a session that is not working says nothing about it", async () => {
  const fan = fanOut();
  fan.setBusy(false);

  const late = fakeView();
  await fan.attach(late.view);

  assertEquals(late.seen.filter((line) => line.startsWith("busy:")), []);
});

Deno.test("the record is bounded, and a replay admits what it dropped", async () => {
  const fan = fanOut(3);
  for (const text of ["one", "two", "three", "four", "five"]) await fan.post(text);

  const late = fakeView();
  await fan.attach(late.view);

  assertEquals(fan.droppedCount, 2);
  assertStringIncludes(late.seen[0] ?? "", "2 earlier line(s) not kept");
  // The notice about the drop is itself a post, so it is not one of the three.
  assertEquals(late.seen.slice(1).filter((line) => line.startsWith("post:")).length, 3);
});

Deno.test("a turn is announced once, at its boundary, not on every line", async () => {
  const fan = fanOut();
  fan.beginTurn(1);
  await fan.post("first");
  await fan.post("second");
  fan.beginTurn(2);
  await fan.post("third");

  const late = fakeView();
  await fan.attach(late.view);

  assertEquals(late.seen, ["turn:1", "post:first", "post:second", "turn:2", "post:third"]);
});

/** A command's answer belongs to whoever ran it, in the moment they ran it. */
Deno.test("a reply to a command is not part of the conversation", async () => {
  const fan = fanOut();
  const live = fakeView();
  await fan.attach(live.view);
  await fan.postReply("a listing", "!ls");

  const late = fakeView();
  await fan.attach(late.view);

  assertStringIncludes(live.seen.join("\n"), "reply:!ls:a listing");
  assertEquals(late.seen.filter((line) => line.startsWith("reply:")), []);
});

Deno.test("what is reported is written down for later, with its turn", async () => {
  const written: [number, string][] = [];
  const fan = fanOut(undefined, {
    append: (entry, turn) => written.push([turn, entry.call]),
  });

  fan.beginTurn(4);
  await fan.post("said something");
  await fan.appendActivity("did something");

  assertEquals(written, [[4, "post"], [4, "activity"]]);
});

/** Restoring twice would otherwise double a session's history. */
Deno.test("restoring replaces what is held rather than adding to it", async () => {
  const fan = fanOut();
  const held = [
    { turn: 1, entry: { call: "post", text: "from before" } as Recorded },
    { turn: 2, entry: { call: "usage", usage: { ...USAGE, totalTokens: 900 } } as Recorded },
  ];

  fan.restore(held);
  fan.restore(held);

  const late = fakeView();
  await fan.attach(late.view);

  assertEquals(late.seen.filter((line) => line === "post:from before").length, 1);
  assertEquals(fan.currentTurn, 2);
  assertEquals(late.seen.filter((line) => line.startsWith("usage:")), ["usage:900"]);
});

/** A resumed session must not label a new exchange with a number already used. */
Deno.test("restoring carries the turn numbering on", () => {
  const fan = fanOut();
  assertEquals(fan.currentTurn, 0);

  fan.restore([
    { turn: 1, entry: { call: "post", text: "one" } },
    { turn: 3, entry: { call: "post", text: "three" } },
  ]);

  assertEquals(fan.currentTurn, 3);
});

/** A resumed session must not lose the delegations it made. */
Deno.test("a delegation is replayed to a view that attaches later", async () => {
  const fan = fanOut();
  fan.noteDelegation({ question: "what failed?", model: "flash", describes: "call t1" });

  const late = fakeView();
  await fan.attach(late.view);

  assertEquals(late.seen, ["turn:0", "delegated:flash"]);
});
