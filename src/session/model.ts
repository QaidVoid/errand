/**
 * Choosing the model a session runs on, from the message that starts it.
 *
 * The configuration names one provider and model, which is what a session uses
 * unless the person starting it says otherwise. Saying otherwise has to happen
 * in the opening message, because by the time there is a thread to type
 * `!model` in, the session has already started against the wrong one.
 */

/** A `--model` on the opening message, and the prompt with it removed. */
export interface ModelSelection {
  /** What was asked for, as written, or undefined when nothing was. */
  value: string | undefined;
  /** The prompt the session actually receives. */
  prompt: string;
}

/**
 * Only at the very start, and only as a whole word.
 *
 * A prompt that mentions `--model` while describing something must not have it
 * taken as an instruction, and a session started with "explain --model to me"
 * is a likelier message than one that means to select a model halfway through
 * a sentence. Both `--model x` and `--model=x` are accepted, since a person
 * writing a flag will write whichever they are used to.
 */
const MODEL_FLAG = /^--model(?:=|\s+)(\S+)\s*/;

/** Reads a leading `--model` off the prompt, leaving the rest of it alone. */
export function selectModel(prompt: string): ModelSelection {
  const match = MODEL_FLAG.exec(prompt.trim());
  if (match === null || match[1] === undefined) {
    return { value: undefined, prompt: prompt.trim() };
  }
  return { value: match[1], prompt: prompt.trim().slice(match[0].length).trim() };
}

/** A provider and model a session was asked to run on. */
export interface ChosenModel {
  /** The provider, or undefined to keep the configured one. */
  provider: string | undefined;
  /** The model, as the agent should be given it. */
  model: string;
}

/**
 * Splits `provider/id` from a plain model id.
 *
 * A model id may itself hold a slash, `meta/muse-spark-1.3-contributor` among
 * them, so the leading segment is only read as a provider when it is one this
 * host actually knows. Otherwise the whole value is the model, which is what
 * somebody naming a model of the configured provider means. A thinking level
 * such as `:max` is part of the model and is left on it, because it is the
 * agent that understands what those mean.
 */
export function resolveModel(value: string, known: readonly string[]): ChosenModel {
  const slash = value.indexOf("/");
  if (slash > 0) {
    const head = value.slice(0, slash);
    if (known.includes(head)) return { provider: head, model: value.slice(slash + 1) };
  }
  return { provider: undefined, model: value };
}
