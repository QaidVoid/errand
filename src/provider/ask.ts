/**
 * One question to one model, with no conversation and no tools.
 *
 * Deliberately not a client. There is no history, no system prompt describing
 * what the session is doing, and nothing the model can call: it is shown an
 * artefact and asked about it, and it answers in text.
 */

/** Where a model is reached, and what pays for it. */
export interface Endpoint {
  baseUrl: string;
  model: string;
  credential: string;
}

/** What the model was asked, and what it was shown. */
export interface Question {
  question: string;
  /** What the content is, so the answer can say what it describes. */
  describes: string;
  content: string;
}

/** What came back, and what it cost. */
export interface Answer {
  text: string;
  /** Tokens the provider charged, when it said. */
  tokens: number | undefined;
}

/** The model did not answer. Carries words worth showing in a thread. */
export class AskFailed extends Error {}

/** Performs the request. Injected so tests need no network. */
export type Send = (url: string, init: RequestInit) => Promise<Response>;

const INSTRUCTION = [
  "Answer the question about the material below, using only what is in it.",
  "Quote exactly when quoting: transcribe identifiers, paths, and messages",
  "character for character. Say plainly when the material does not answer the",
  "question. Do not suggest what to do about it.",
].join(" ");

/**
 * Asks the model, returning its answer or failing with a readable reason.
 *
 * The caller owns the deadline through `signal`, since abandoning a delegation
 * is the caller's decision rather than this function's.
 */
export async function ask(
  endpoint: Endpoint,
  asked: Question,
  signal: AbortSignal,
  send: Send = fetch,
): Promise<Answer> {
  const body = JSON.stringify({
    model: endpoint.model,
    messages: [
      {
        role: "user",
        content:
          `${INSTRUCTION}\n\nQuestion: ${asked.question}\n\n${asked.describes}:\n${asked.content}`,
      },
    ],
  });

  let response: Response;
  try {
    response = await send(`${endpoint.baseUrl.replace(/\/+$/, "")}/chat/completions`, {
      method: "POST",
      headers: {
        Authorization: `Bearer ${endpoint.credential}`,
        "Content-Type": "application/json",
      },
      body,
      signal,
    });
  } catch (error) {
    throw new AskFailed(
      signal.aborted ? "it did not answer in time" : `it could not be reached: ${error}`,
    );
  }

  if (!response.ok) {
    throw new AskFailed(`${endpoint.model} refused the question: ${response.status}`);
  }

  const parsed = await response.json().catch(() => ({}));
  const text = contentOf(parsed);
  if (text === undefined || text.trim().length === 0) {
    throw new AskFailed(`${endpoint.model} returned no answer`);
  }
  return { text: text.trim(), tokens: tokensOf(parsed) };
}

function contentOf(body: unknown): string | undefined {
  const choices = (body as { choices?: unknown })?.choices;
  if (!Array.isArray(choices)) return undefined;
  const message = (choices[0] as { message?: { content?: unknown } } | undefined)?.message;
  return typeof message?.content === "string" ? message.content : undefined;
}

function tokensOf(body: unknown): number | undefined {
  const usage = (body as { usage?: { total_tokens?: unknown } })?.usage;
  return typeof usage?.total_tokens === "number" ? usage.total_tokens : undefined;
}
