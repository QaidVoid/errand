import { assertEquals } from "@std/assert";
import { resolveModel, selectModel } from "./model.ts";

Deno.test("a leading --model is read off, and the prompt keeps the rest", () => {
  assertEquals(selectModel("--model meta/muse-spark-1.3-contributor:max build it"), {
    value: "meta/muse-spark-1.3-contributor:max",
    prompt: "build it",
  });
  // Both spellings, because a person writes whichever they are used to.
  assertEquals(selectModel("--model=glm-5.3 go"), { value: "glm-5.3", prompt: "go" });
  // A flag with nothing after it selects nothing and stays in the prompt.
  assertEquals(selectModel("just do the thing"), {
    value: undefined,
    prompt: "just do the thing",
  });
});

/** A prompt that talks about the flag must not be taken as using it. */
Deno.test("--model is only an instruction at the very start", () => {
  assertEquals(selectModel("explain --model to me"), {
    value: undefined,
    prompt: "explain --model to me",
  });
});

Deno.test("a known leading segment is the provider, anything else is the model", () => {
  const known = ["zai-coding-cn", "meta"];
  // Named provider, and the model keeps its own slash and thinking level.
  assertEquals(resolveModel("meta/muse-spark-1.3-contributor:max", known), {
    provider: "meta",
    model: "muse-spark-1.3-contributor:max",
  });
  // A model id that merely contains a slash is not a provider.
  assertEquals(resolveModel("meta/muse-spark-1.3-contributor", ["openrouter"]), {
    provider: undefined,
    model: "meta/muse-spark-1.3-contributor",
  });
  // A plain id keeps the configured provider.
  assertEquals(resolveModel("glm-5.3-flash", known), {
    provider: undefined,
    model: "glm-5.3-flash",
  });
});
