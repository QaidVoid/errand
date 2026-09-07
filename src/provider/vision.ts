/**
 * Describing an image with a model that can see, for one that cannot.
 *
 * A session's model is chosen for the work, and the good ones for code are
 * often text only. Refusing every screenshot on that basis loses the most
 * ordinary thing somebody does in a chat, so an image is shown to a model from
 * the same provider that accepts one, and the agent is given what it said.
 *
 * The description is text, and text is all the agent gets. It is not the same
 * as having seen the image, and the note the agent receives says so rather
 * than pretending otherwise.
 */

import type { AgentImage } from "../agent/protocol.ts";
import type { AgentConfig } from "../config/schema.ts";
import { modelById, readModels, seesImages, visionModel } from "./models.ts";

/** How long a description may take before the turn goes on without it. */
export const TIMEOUT_MS = 60_000;

/** What the describing model is asked for. */
const INSTRUCTION = [
  "Describe this image for another model that cannot see it, in a way that lets",
  "it act. Transcribe any text, code, error message or stack trace exactly,",
  "including punctuation and line breaks. Describe the layout only where it",
  "carries meaning. Do not interpret, advise, or add anything that is not in",
  "the image.",
].join(" ");

/** Where and how to reach the describing model. */
export interface Describer {
  baseUrl: string;
  model: string;
  credential: string;
}

/** Sends the request. Injected so tests need no network. */
export type Post = (
  url: string,
  init: { headers: Record<string, string>; body: string; signal: AbortSignal },
) => Promise<{ status: number; body: unknown }>;

/** The description failed, in words worth posting into a thread. */
export class VisionError extends Error {}

function messageContent(body: unknown): string | undefined {
  const choices = (body as { choices?: unknown })?.choices;
  if (!Array.isArray(choices)) return undefined;
  const message = (choices[0] as { message?: { content?: unknown } } | undefined)?.message;
  return typeof message?.content === "string" ? message.content : undefined;
}

function detail(body: unknown): string {
  const error = (body as { error?: { message?: unknown } })?.error;
  if (typeof error?.message === "string") return error.message;
  const message = (body as { message?: unknown })?.message;
  return typeof message === "string" ? message : "it did not say why";
}

const defaultPost: Post = async function post(
  url: string,
  init: { headers: Record<string, string>; body: string; signal: AbortSignal },
): Promise<{ status: number; body: unknown }> {
  const response = await fetch(url, {
    method: "POST",
    headers: init.headers,
    body: init.body,
    signal: init.signal,
  });
  return { status: response.status, body: await response.json().catch(() => ({})) };
};

/**
 * Describes images, returning one block of text for the lot.
 *
 * `question` is what the person said, passed as context so the description
 * answers what was asked rather than cataloguing the whole picture.
 */
export async function describeImages(
  describer: Describer,
  images: AgentImage[],
  question: string,
  post: Post = defaultPost,
): Promise<string> {
  const asked = question.trim();
  const content: unknown[] = [
    { type: "text", text: asked.length === 0 ? INSTRUCTION : `${INSTRUCTION}\n\nAsked: ${asked}` },
    ...images.map((image) => ({
      type: "image_url",
      image_url: { url: `data:${image.mimeType};base64,${image.data}` },
    })),
  ];

  const answer = await post(`${describer.baseUrl.replace(/\/+$/, "")}/chat/completions`, {
    headers: {
      Authorization: `Bearer ${describer.credential}`,
      "Content-Type": "application/json",
    },
    body: JSON.stringify({ model: describer.model, messages: [{ role: "user", content }] }),
    signal: AbortSignal.timeout(TIMEOUT_MS),
  });

  if (answer.status >= 400) {
    throw new VisionError(`${describer.model} refused to describe it: ${detail(answer.body)}`);
  }

  const text = messageContent(answer.body);
  if (text === undefined || text.trim().length === 0) {
    throw new VisionError(`${describer.model} returned no description`);
  }
  return text.trim();
}

/** The note the agent is given in place of the images it cannot be shown. */
export function describedBlock(model: string, description: string): string {
  return [
    `An image was attached. This session's model cannot see images, so ${model}`,
    "was asked to describe it. What follows is that description, not the image:",
    "",
    description,
  ].join("\n");
}

/** Which model was chosen, and how to ask it. */
export interface ImageDescriber {
  model: string;
  describe: (images: AgentImage[], question: string) => Promise<string>;
}

/**
 * Decides how a session's images are handled, once, at startup.
 *
 * Routing happens only when the configured model is known to be text only. A
 * model the store does not list, which is what a pattern rather than an id
 * produces, is left alone: guessing that it cannot see would take images away
 * from a model that can.
 *
 * @returns nothing when there is nothing to do, whether because the model can
 *   see, because the provider has no model that can, or because the host has
 *   no agent installation to read the store from.
 */
export function imageDescriber(
  agent: AgentConfig,
  directory: string | undefined,
  post: Post = defaultPost,
): ImageDescriber | undefined {
  const models = readModels(directory, agent.provider);
  const own = modelById(models, agent.model);
  if (own === undefined || seesImages(own)) return undefined;

  const chosen = visionModel(models, agent.visionModel);
  if (chosen === undefined || chosen.baseUrl === undefined) return undefined;

  const describer: Describer = {
    baseUrl: chosen.baseUrl,
    model: chosen.id,
    credential: agent.credential,
  };
  return {
    model: chosen.id,
    describe: async (images, question) =>
      describedBlock(chosen.id, await describeImages(describer, images, question, post)),
  };
}
