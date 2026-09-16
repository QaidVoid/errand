import { assertEquals } from "@std/assert";
import { expandAlias, resolveModel, selectModel } from "./model.ts";

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

Deno.test("a short name stands for the model it was given", () => {
  const aliases = { muse: "meta/muse-spark-1.3-contributor" };
  assertEquals(expandAlias("muse", aliases), "meta/muse-spark-1.3-contributor");
  // The level is split off before the name is looked up, then put back.
  assertEquals(expandAlias("muse:xhigh", aliases), "meta/muse-spark-1.3-contributor:xhigh");
  // A name standing for nothing is left exactly as written.
  assertEquals(expandAlias("glm-5.3-flash", aliases), "glm-5.3-flash");
  assertEquals(expandAlias("mistyped:max", aliases), "mistyped:max");
});

Deno.test("a level on the name beats one written into the alias", () => {
  const aliases = { muse: "meta/muse-spark-1.3-contributor:high" };
  // The alias carries a default...
  assertEquals(expandAlias("muse", aliases), "meta/muse-spark-1.3-contributor:high");
  // ...and being more specific replaces it rather than stacking on it.
  assertEquals(expandAlias("muse:xhigh", aliases), "meta/muse-spark-1.3-contributor:xhigh");
});

/** A colon in a model id must not be read as a thinking level. */
Deno.test("only a real level is split off the end", () => {
  const aliases = { weird: "provider/model:batch" };
  assertEquals(expandAlias("weird", aliases), "provider/model:batch");
  assertEquals(expandAlias("weird:max", aliases), "provider/model:batch:max");
});

/** Naming the model is typed often enough to be worth a short form. */
Deno.test("-m is the same flag as --model", () => {
  assertEquals(selectModel("-m musecringe:xhigh build it"), {
    value: "musecringe:xhigh",
    prompt: "build it",
  });
  assertEquals(selectModel("-m=glm go"), { value: "glm", prompt: "go" });

  // Still only at the very start, and still a whole word: a prompt about a
  // flag, and a word that merely begins with it, are left alone.
  assertEquals(selectModel("run it with -m glm"), {
    value: undefined,
    prompt: "run it with -m glm",
  });
  assertEquals(selectModel("-make the thing"), { value: undefined, prompt: "-make the thing" });
  assertEquals(selectModel("-m"), { value: undefined, prompt: "-m" });
});
