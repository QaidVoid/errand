import { assertEquals, assertRejects, assertStringIncludes } from "@std/assert";
import { join } from "@std/path";
import type { AgentConfig } from "../config/schema.ts";
import { agentDirectory, readModels, seesImages, visionModel } from "./models.ts";
import {
  describedBlock,
  describeImages,
  imageDescriber,
  type Post,
  VisionError,
} from "./vision.ts";

const IMAGE = { type: "image" as const, data: "aGVsbG8=", mimeType: "image/png" };

const STORE = {
  "zai-coding-cn": {
    models: [
      {
        id: "glm-5.3",
        baseUrl: "https://api.example/v1",
        input: ["text"],
        cost: { input: 0.6 },
      },
      {
        id: "glm-5.3-flash",
        baseUrl: "https://api.example/v1",
        input: ["text", "image"],
        cost: { input: 0.1 },
      },
      {
        id: "glm-5.3-vision-pro",
        baseUrl: "https://api.example/v1",
        input: ["text", "image"],
        cost: { input: 2 },
      },
    ],
  },
};

function agent(overrides: Partial<AgentConfig> = {}): AgentConfig {
  return {
    provider: "zai-coding-cn",
    model: "glm-5.3",
    visionModel: undefined,
    delegate: undefined,
    providers: {},
    rulesPath: undefined,
    credentialName: "ZAI_CODING_CN_API_KEY",
    credential: "secret-key",
    ...overrides,
  };
}

async function withStore(
  run: (directory: string) => Promise<void> | void,
  contents: unknown = STORE,
): Promise<void> {
  const directory = await Deno.makeTempDir({ prefix: "errand-models-" });
  try {
    Deno.writeTextFileSync(join(directory, "models-store.json"), JSON.stringify(contents));
    await run(directory);
  } finally {
    await Deno.remove(directory, { recursive: true });
  }
}

/** Answers as the provider does, and records what it was asked. */
function fakePost(answer: { status: number; body: unknown }) {
  const calls: { url: string; body: Record<string, unknown> }[] = [];
  const post: Post = (url, init) => {
    calls.push({ url, body: JSON.parse(init.body) as Record<string, unknown> });
    return Promise.resolve(answer);
  };
  return { post, calls };
}

const DESCRIBED = {
  status: 200,
  body: { choices: [{ message: { content: "  a stack trace saying ENOSPC  " } }] },
};

Deno.test("the store says which models can be shown an image", () =>
  withStore((directory) => {
    const models = readModels(directory, "zai-coding-cn");

    assertEquals(models.length, 3);
    assertEquals(seesImages(models[0]), false);
    assertEquals(seesImages(models[1]), true);
  }));

Deno.test("a host with no store, or an unreadable one, lists nothing", () =>
  withStore((directory) => {
    assertEquals(readModels(directory, "somebody-else"), []);
    assertEquals(readModels("/nowhere/at/all", "zai-coding-cn"), []);
    assertEquals(readModels(undefined, "zai-coding-cn"), []);
  }));

Deno.test("a store that is not JSON is treated as no store at all", () =>
  withStore((directory) => {
    Deno.writeTextFileSync(join(directory, "models-store.json"), "{ not json");
    assertEquals(readModels(directory, "zai-coding-cn"), []);
  }));

/** Describing an image is a paragraph, and the work is done by another model. */
Deno.test("the cheapest model that can see is the one chosen", () =>
  withStore((directory) => {
    const chosen = visionModel(readModels(directory, "zai-coding-cn"));

    assertEquals(chosen?.id, "glm-5.3-flash");
  }));

Deno.test("a model named in the configuration wins over the cheapest", () =>
  withStore((directory) => {
    const models = readModels(directory, "zai-coding-cn");

    assertEquals(visionModel(models, "glm-5.3-vision-pro")?.id, "glm-5.3-vision-pro");
    // Named but unable to see, or not in the store: no routing rather than a
    // silent fallback to something nobody asked for.
    assertEquals(visionModel(models, "glm-5.3"), undefined);
    assertEquals(visionModel(models, "not-a-model"), undefined);
  }));

Deno.test("a session whose model can already see routes nothing", () =>
  withStore((directory) => {
    assertEquals(imageDescriber(agent({ model: "glm-5.3-flash" }), directory), undefined);
  }));

/**
 * A pattern rather than an id is not in the store. Guessing it cannot see
 * would take images away from a model that can.
 */
Deno.test("a model the store does not list is left alone", () =>
  withStore((directory) => {
    assertEquals(imageDescriber(agent({ model: "glm-5.3-*" }), directory), undefined);
    assertEquals(imageDescriber(agent({ model: undefined }), directory), undefined);
  }));

Deno.test("a provider with nothing that can see routes nothing", () =>
  withStore((directory) => {
    assertEquals(imageDescriber(agent(), directory), undefined);
  }, {
    "zai-coding-cn": {
      models: [{ id: "glm-5.3", baseUrl: "https://api.example/v1", input: ["text"] }],
    },
  }));

/** Knowing a model can see is no use without knowing where to reach it. */
Deno.test("a model with nowhere to reach it is not chosen", () =>
  withStore((directory) => {
    assertEquals(imageDescriber(agent(), directory), undefined);
  }, {
    "zai-coding-cn": {
      models: [
        { id: "glm-5.3", baseUrl: "https://api.example/v1", input: ["text"] },
        { id: "glm-5.3-flash", input: ["text", "image"] },
      ],
    },
  }));

Deno.test("a text-only model gets a description from the one that can see", () =>
  withStore(async (directory) => {
    const answering = fakePost(DESCRIBED);
    const describer = imageDescriber(agent(), directory, answering.post);

    assertEquals(describer?.model, "glm-5.3-flash");
    const block = await describer?.describe([IMAGE], "what does this say?");

    assertStringIncludes(block ?? "", "cannot see images");
    assertStringIncludes(block ?? "", "glm-5.3-flash");
    assertStringIncludes(block ?? "", "a stack trace saying ENOSPC");
    assertEquals(answering.calls[0]?.url, "https://api.example/v1/chat/completions");
    assertEquals(answering.calls[0]?.body.model, "glm-5.3-flash");
  }));

/** A description that catalogues the picture answers nobody's question. */
Deno.test("what was asked is passed along with the image", async () => {
  const answering = fakePost(DESCRIBED);

  await describeImages(
    { baseUrl: "https://api.example/v1/", model: "seer", credential: "k" },
    [IMAGE],
    "  what is the error?  ",
    answering.post,
  );

  const content = (answering.calls[0]?.body.messages as { content: unknown[] }[])[0]?.content ?? [];
  assertStringIncludes(JSON.stringify(content[0]), "what is the error?");
  assertStringIncludes(JSON.stringify(content[1]), "data:image/png;base64,aGVsbG8=");
});

Deno.test("a trailing slash on the endpoint does not double up", async () => {
  const answering = fakePost(DESCRIBED);

  await describeImages(
    { baseUrl: "https://api.example/v1///", model: "seer", credential: "k" },
    [IMAGE],
    "",
    answering.post,
  );

  assertEquals(answering.calls[0]?.url, "https://api.example/v1/chat/completions");
});

Deno.test("a provider that refuses says so in words worth posting", async () => {
  const answering = fakePost({ status: 429, body: { error: { message: "rate limited" } } });

  const error = await assertRejects(
    () =>
      describeImages(
        { baseUrl: "https://api.example/v1", model: "seer", credential: "k" },
        [IMAGE],
        "",
        answering.post,
      ),
    VisionError,
  );

  assertStringIncludes(String(error), "rate limited");
  assertStringIncludes(String(error), "seer");
});

Deno.test("an answer with no description in it is a failure, not an empty note", async () => {
  const answering = fakePost({ status: 200, body: { choices: [{ message: { content: "   " } }] } });

  await assertRejects(
    () =>
      describeImages(
        { baseUrl: "https://api.example/v1", model: "seer", credential: "k" },
        [IMAGE],
        "",
        answering.post,
      ),
    VisionError,
  );
});

/** The agent is told it is reading a description, not looking at the image. */
Deno.test("the note says plainly what the agent is being given", () => {
  const block = describedBlock("seer", "a terminal showing a failing test");

  assertStringIncludes(block, "not the image");
  assertStringIncludes(block, "a terminal showing a failing test");
});

Deno.test("the agent directory is found by override, then by convention", () =>
  withStore((directory) => {
    assertEquals(agentDirectory({ PI_CODING_AGENT_DIR: directory }), directory);
    assertEquals(agentDirectory({ HOME: "/nowhere", PI_CODING_AGENT_DIR: " " }), undefined);
  }));
