/**
 * Session identifiers: how one is drawn, and what it reads as.
 *
 * An id is not only a key. It names the session's state directory, it names
 * the sandbox, and where a public interface is configured it is the whole of
 * the address a transcript is served at. That last one makes it a capability
 * rather than a label, which is why it is drawn from the system's random
 * source and not from `Math.random`.
 */

import type { ProjectSelection } from "./projects.ts";

/** Digits an id is written in: lower case, so it survives a case-blind path. */
const ALPHABET = "0123456789abcdefghijklmnopqrstuvwxyz";

/**
 * Characters of randomness in an id.
 *
 * Twelve gives about 62 bits. Collision alone would have been satisfied by
 * half of that, so the length is chosen against guessing instead: where
 * `web.publicUrl` is set the id is the entire address a transcript is served
 * at, nothing authenticates that address, and those addresses get published in
 * pull request descriptions. At twelve, a first collision is some 2.7 billion
 * sessions away and an id costs about 15 million years to find at ten thousand
 * guesses a second.
 */
export const TOKEN_LENGTH = 12;

/**
 * Fresh randomness for one session.
 *
 * Values at or above 252 are redrawn rather than folded, because 256 is not a
 * multiple of 36 and folding them would make the first four digits turn up
 * more often than the rest. Skewed digits are the difference between the
 * entropy this claims and the entropy it has.
 */
export function sessionToken(length: number = TOKEN_LENGTH): string {
  const byte = new Uint8Array(1);
  let out = "";
  while (out.length < length) {
    crypto.getRandomValues(byte);
    const value = byte[0] as number;
    if (value < 252) out += ALPHABET[value % 36];
  }
  return out;
}

/**
 * The id a session is known by, which says where it is working.
 *
 * A named project is written into the id, so `errand-6a82hff` says which tree
 * the transcript belongs to without anybody having to look it up. An unnamed
 * session is its token alone: its project directory is named after the token
 * already, and `6a82hff-6a82hff` says nothing twice.
 *
 * The token is always present and always last, so two sessions in one project
 * stay distinct and nothing has to parse the id to tell them apart.
 */
export function sessionId(project: ProjectSelection, token: string): string {
  return project.wasExplicit ? `${project.name}-${token}` : token;
}
