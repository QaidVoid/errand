/** Small formatters shared by the views. */

/** A time of day, for a line in the transcript. */
export function clock(at: number): string {
  return new Date(at).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
}

/** The full date and time, for the title of a shortened one. */
export function exact(at: number): string {
  return new Date(at).toLocaleString();
}

/** How long ago something was, in the coarsest unit that still says something. */
export function ago(at: number, now: number = Date.now()): string {
  const seconds = Math.max(0, Math.round((now - at) / 1000));
  if (seconds < 45) return "just now";

  const minutes = Math.round(seconds / 60);
  if (minutes < 60) return `${minutes}m ago`;

  const hours = Math.round(minutes / 60);
  if (hours < 24) return `${hours}h ago`;

  return `${Math.round(hours / 24)}d ago`;
}

/** A byte count, at a glance. */
export function humanSize(bytes: number): string {
  if (bytes < 1024) return `${bytes}B`;
  if (bytes < 1024 * 1024) return `${Math.round(bytes / 1024)}K`;
  return `${(bytes / (1024 * 1024)).toFixed(1)}M`;
}

/** A token count, short enough to sit in a status bar. */
export function tokens(count: number): string {
  const magnitudes: [number, string][] = [
    [1_000_000_000, "B"],
    [1_000_000, "M"],
    [1_000, "k"],
  ];
  const size = Math.abs(count);
  for (let index = 0; index < magnitudes.length; index += 1) {
    const [magnitude, suffix] = magnitudes[index] as [number, string];
    if (size < magnitude) continue;

    const scaled = count / magnitude;
    // One decimal until three digits, then none: 12.3M, but 123M.
    const digits = Math.abs(scaled) >= 100 ? 0 : 1;

    // Rounding can push a value into the next magnitude: 999,999 would read as
    // 1000k, which is a magnitude out. Carry it up instead.
    if (Math.abs(Number(scaled.toFixed(digits))) >= 1000 && index > 0) {
      const [bigger, biggerSuffix] = magnitudes[index - 1] as [number, string];
      return `${(count / bigger).toFixed(1)}${biggerSuffix}`;
    }
    return `${scaled.toFixed(digits)}${suffix}`;
  }
  return String(count);
}

/** A cost, at a precision that does not read as zero for a cheap session. */
export function money(amount: number): string {
  if (amount === 0) return "$0";
  return amount < 0.01 ? `$${amount.toFixed(4)}` : `$${amount.toFixed(2)}`;
}

/**
 * The share of everything sent that came from cache.
 *
 * Returns undefined when nothing has been sent, because zero percent of
 * nothing is a made up number.
 */
export function cacheRate(input: number, cacheRead: number): number | undefined {
  const sent = input + cacheRead;
  return sent === 0 ? undefined : Math.round((cacheRead / sent) * 100);
}
