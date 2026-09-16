import { assertEquals } from "@std/assert";
import { STATUS_LIMIT, usageStatus, type Window } from "./usage.ts";

function window(provider: string, percentage: number, relative?: string): Window {
  return { provider, quota: { percentage }, ...(relative === undefined ? {} : { relative }) };
}

/** One provider reads as a sentence: there is room, and naming it says nothing. */
Deno.test("a single provider is not named", () => {
  assertEquals(usageStatus([window("zai-coding-cn", 12, "in 2h")]), "88% usage left, resets in 2h");
  assertEquals(usageStatus([window("zai-coding-cn", 12)]), "88% usage left");
  assertEquals(usageStatus([window("zai-coding-cn", 100, "in 30m")]), "usage spent, back in 30m");
});

/** Several are named, because which one has room is then the whole question. */
Deno.test("several providers are named and cut to what is left", () => {
  assertEquals(
    usageStatus([window("zai-coding-cn", 12, "in 2h"), window("ajamxhacker", 0, "in 5h")]),
    "zai-coding-cn 88% | ajamxhacker 100%",
  );
  // A spent one says when it is back, which is all that is left to know.
  assertEquals(
    usageStatus([window("zai", 100, "in 30m"), window("meta", 40)]),
    "zai spent, back in 30m | meta 60%",
  );
});

/** A host asking two must not lose the answer it has because the other failed. */
Deno.test("a provider that did not answer is simply absent", () => {
  assertEquals(usageStatus([window("meta", 25)]), "75% usage left");
  assertEquals(usageStatus([]), undefined);
});

/** The service refuses a long status silently, so it is trimmed here. */
Deno.test("the status is kept inside what the service accepts", () => {
  const many = Array.from(
    { length: 12 },
    (_, index) => window(`provider-with-a-long-name-${index}`, index, "in 3h"),
  );
  const status = usageStatus(many) as string;
  assertEquals(status.length <= STATUS_LIMIT, true, `status was ${status.length}`);
  // Dropped whole rather than truncated: half a name reads as another provider.
  assertEquals(status.endsWith("|"), false);
  assertEquals(status.includes("provider-with-a-long-name-0"), true);
});
