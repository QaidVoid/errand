/**
 * Sizes as people write them in configuration, and as bytes.
 *
 * One definition, used both to validate what was configured and to convert it
 * for whatever enforces it. Two would eventually disagree, and the way that
 * shows up is a limit that validates and then is not applied.
 */

const SUFFIXES: Record<string, number> = {
  b: 1,
  k: 1024,
  kb: 1024,
  m: 1024 ** 2,
  mb: 1024 ** 2,
  g: 1024 ** 3,
  gb: 1024 ** 3,
  t: 1024 ** 4,
  tb: 1024 ** 4,
};

/**
 * Reads a size such as `512m` or `4g` as a number of bytes.
 *
 * @returns the size in bytes, or undefined when it is not a size.
 */
export function parseSize(text: string): number | undefined {
  const match = /^(\d+(?:\.\d+)?)\s*([a-z]*)$/i.exec(text.trim());
  if (match === null) return undefined;

  const amount = Number(match[1]);
  if (!Number.isFinite(amount)) return undefined;

  const suffix = (match[2] ?? "").toLowerCase();
  if (suffix.length === 0) return Math.floor(amount);

  const scale = SUFFIXES[suffix];
  return scale === undefined ? undefined : Math.floor(amount * scale);
}
