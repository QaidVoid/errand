import { assertEquals, assertStringIncludes } from "@std/assert";
import type { SessionUsage } from "../session/port.ts";
import { WebView, withoutMentions } from "./view.ts";

const USAGE: SessionUsage = {
  input: 10,
  output: 2,
  cacheRead: 0,
  cacheWrite: 0,
  totalTokens: 12,
  cost: 0.01,
  contextTokens: 10,
  turns: 1,
};

/** Reads the stream as the browser does, one event at a time. */
async function eventsOf(view: WebView, count: number): Promise<{ event: string; data: string }[]> {
  const reader = view.body.getReader();
  const decoder = new TextDecoder();
  const found: { event: string; data: string }[] = [];
  let buffered = "";

  while (found.length < count) {
    const { value, done } = await reader.read();
    if (done) break;
    buffered += decoder.decode(value);
    for (const block of buffered.split("\n\n")) {
      const event = /^event: (.+)$/m.exec(block)?.[1];
      const data = /^data: (.+)$/m.exec(block)?.[1];
      if (event !== undefined && data !== undefined) found.push({ event, data });
    }
    buffered = "";
  }
  reader.releaseLock();
  return found.slice(0, count);
}

/** A reconnecting browser must end up correct rather than doubled. */
Deno.test("the stream opens by telling the browser to clear what it has", async () => {
  const view = new WebView();

  assertEquals((await eventsOf(view, 1))[0]?.event, "reset");
});

Deno.test("what a session says becomes one event each, with its turn", async () => {
  const view = new WebView();
  view.beginTurn(3);
  await view.post("I did the thing");
  await view.appendActivity("ran `ls`", { name: "bash", target: "ls" });

  const events = await eventsOf(view, 3);

  assertEquals(events[1]?.event, "entry");
  const message = JSON.parse(events[1]?.data ?? "{}");
  assertEquals(message.kind, "message");
  assertEquals(message.text, "I did the thing");
  assertEquals(message.turn, 3);
  assertEquals(JSON.parse(events[2]?.data ?? "{}").tool.name, "bash");
});

Deno.test("state is sent as state, not as something said", async () => {
  const view = new WebView();
  view.setBusy(true);
  view.setUsage(USAGE);
  await view.setWaiting("waiting for a turn slot, position 2");

  const events = await eventsOf(view, 4);

  assertEquals(events.slice(1).map((event) => event.event), ["state", "state", "state"]);
  assertEquals(JSON.parse(events[1]?.data ?? "{}").busy, true);
  assertEquals(JSON.parse(events[2]?.data ?? "{}").usage.totalTokens, 12);
});

/**
 * A stopped session can be picked up again, and a browser watching one should
 * see that happen rather than be disconnected from it.
 */
Deno.test("a session ending is state, and the stream stays open", async () => {
  const view = new WebView();
  await view.close("idle");

  const events = await eventsOf(view, 2);

  assertEquals(JSON.parse(events[1]?.data ?? "{}"), { ended: true, busy: false });
  assertEquals(view.isClosed, false);
});

Deno.test("a detached view ends the stream so the browser stops waiting", async () => {
  const view = new WebView();
  view.stop();

  assertEquals(view.isClosed, true);
  const reader = view.body.getReader();
  await reader.read();
  assertEquals((await reader.read()).done, true);
});

/** Nothing said after the browser has gone is worth trying to send. */
Deno.test("a stopped view accepts calls and sends nothing", async () => {
  const view = new WebView();
  view.stop();

  await view.post("into the void");
  view.setBusy(true);

  assertEquals(view.isClosed, true);
});

/** A command's answer belongs to the thread it was asked in. */
Deno.test("what a thread shows and the interface does not is not sent", async () => {
  const view = new WebView();
  await view.postReply();
  await view.setReaction();
  await view.post("the only thing said");

  const events = await eventsOf(view, 2);

  assertEquals(events.length, 2);
  assertStringIncludes(events[1]?.data ?? "", "the only thing said");
});

/** `<@1523363748427993218> [done]` names nobody to a reader in a browser. */
Deno.test("a mention becomes a name, or goes if the name is not known", () => {
  const names = (id: string) => (id === "111" ? "amelia" : undefined);

  assertEquals(withoutMentions("<@111> [done] 12k tokens", names), "@amelia [done] 12k tokens");
  assertEquals(withoutMentions("<@999> [done]", names), "[done]");
  assertEquals(withoutMentions("<@!111> hello", names), "@amelia hello");
  assertEquals(withoutMentions("nothing to rewrite", names), "nothing to rewrite");
});

Deno.test("what a session says reaches the browser with mentions rewritten", async () => {
  const view = new WebView((id) => (id === "111" ? "amelia" : undefined));
  await view.postNotice("<@111> [done]", "done");

  const events = await eventsOf(view, 2);

  assertStringIncludes(events[1]?.data ?? "", "@amelia [done]");
});
