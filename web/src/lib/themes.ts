/**
 * Which palette the interface is painted in.
 *
 * The palettes are kage's own: the same `[gui]` tokens its desktop and web
 * client draw with, under the same names, so a kage theme file is also an
 * errand theme. Every token is written to the root element as a custom
 * property (`accent_soft` becomes `--accent-soft`), and the stylesheet reads
 * nothing else for colour.
 *
 * A reader picks a theme by name, or follows the system, which shows one of a
 * dark and light pair. Themes a reader makes are kept in this browser only.
 */

/** Every colour a palette names. */
export const TOKENS = [
  "bg", "sidebar", "surface", "raised", "sunken", "deep", "well",
  "fill", "fill_hover",
  "ink", "ink_strong", "muted", "faint", "ghost",
  "line", "subtle", "line_strong",
  "hover", "selected", "selected_hover",
  "accent", "accent_hover", "accent_soft", "accent_bd",
  "ok", "ok_soft", "ok_bd", "ok_ink",
  "warn", "warn_soft", "warn_bd",
  "danger", "danger_soft", "danger_bd",
  "done", "done_soft", "done_bd",
  "info",
  "composer_bg", "composer_line", "composer_focus_line",
  "send_bg", "send_bg_hover", "send_icon", "send_bg_off", "send_icon_off",
  "stop_glyph",
  "diff_add", "diff_add_bg", "diff_del", "diff_del_bg",
  "bubble", "selection", "menu", "code_inline",
] as const;

/** One colour role. */
export type Token = (typeof TOKENS)[number];

/** A complete palette, and whether it is a dark one. */
export interface Palette {
  label: string;
  dark: boolean;
  colors: Record<Token, string>;
}

/** A palette a reader made: a bundled one with some colours changed. */
export interface CustomTheme {
  name: string;
  base: BundledName;
  colors: Partial<Record<Token, string>>;
}

export const BUNDLED = {
  "kage-shadow": {
    label: "kage shadow",
    dark: true,
    colors: {
      bg: "#0f0e13", sidebar: "#0b0a0e", surface: "#17151d", raised: "#221f2a", sunken: "#0b0a0e",
      deep: "#09080c", well: "#17151d", fill: "#e8e2f50d", fill_hover: "#e8e2f516", ink: "#ece8f6db",
      ink_strong: "#f4f1fa", muted: "#ece8f694", faint: "#ece8f66b", ghost: "#ece8f640",
      line: "#e8e2f51c", subtle: "#e8e2f50d", line_strong: "#e8e2f530", hover: "#e8e2f50d",
      selected: "#e8e2f516", selected_hover: "#e8e2f51f", accent: "#f2a65a", accent_hover: "#f6b674",
      accent_soft: "#f2a65a1f", accent_bd: "#f2a65a57", ok: "#8bd49c", ok_soft: "#8bd49c1c",
      ok_bd: "#8bd49c4d", ok_ink: "#0b1a10", warn: "#e8c96a", warn_soft: "#e8c96a1c",
      warn_bd: "#e8c96a4d", danger: "#f2727f", danger_soft: "#f2727f1c", danger_bd: "#f2727f4d",
      done: "#a98bfa", done_soft: "#a98bfa1f", done_bd: "#a98bfa57", info: "#9ab4ff",
      composer_bg: "#15131b", composer_line: "#e8e2f51c", composer_focus_line: "#f2a65a6b",
      send_bg: "#f2a65a", send_bg_hover: "#f6b674", send_icon: "#1a1208", send_bg_off: "#e8e2f514",
      send_icon_off: "#e8e2f542", stop_glyph: "#f2727fbf", diff_add: "#8bd49c",
      diff_add_bg: "#8bd49c1f", diff_del: "#f2727f", diff_del_bg: "#f2727f1f", bubble: "#221f2a",
      selection: "#f2a65a42", menu: "#16141cf2", code_inline: "#e8e2f514",
    },
  },
  "kage-dawn": {
    label: "kage dawn",
    dark: false,
    colors: {
      bg: "#f7f5f9", sidebar: "#eeebf2", surface: "#efecf3", raised: "#e4dfeb", sunken: "#eeebf2",
      deep: "#ffffff", well: "#efecf3", fill: "#1a142e0b", fill_hover: "#1a142e12", ink: "#1a142eed",
      ink_strong: "#15121c", muted: "#1a142eb8", faint: "#1a142e7d", ghost: "#1a142e4d",
      line: "#1a142e1f", subtle: "#1a142e0d", line_strong: "#1a142e33", hover: "#1a142e0b",
      selected: "#1a142e12", selected_hover: "#1a142e1a", accent: "#a8580f", accent_hover: "#924e11",
      accent_soft: "#a8580f1a", accent_bd: "#a8580f4d", ok: "#2b7443", ok_soft: "#2b74431a",
      ok_bd: "#2b74434d", ok_ink: "#0b1a10", warn: "#836409", warn_soft: "#8364091a",
      warn_bd: "#8364094d", danger: "#bf3649", danger_soft: "#bf36491a", danger_bd: "#bf36494d",
      done: "#6a47cc", done_soft: "#6a47cc1a", done_bd: "#6a47cc4d", info: "#3456c2",
      composer_bg: "#ffffff", composer_line: "#1a142e1f", composer_focus_line: "#a8580f73",
      send_bg: "#a8580f", send_bg_hover: "#924e11", send_icon: "#ffffff", send_bg_off: "#1a142e14",
      send_icon_off: "#1a142e4d", stop_glyph: "#bf3649bf", diff_add: "#2b7443",
      diff_add_bg: "#2b74431f", diff_del: "#bf3649", diff_del_bg: "#bf36491f", bubble: "#e4dfeb",
      selection: "#a8580f33", menu: "#fffffff2", code_inline: "#1a142e0f",
    },
  },
  "kimi-dark": {
    label: "kimi dark",
    dark: true,
    colors: {
      bg: "#121212", sidebar: "#0d0d0d", surface: "#1f1f1f", raised: "#292929", sunken: "#121212",
      deep: "#0d0d0d", well: "#1f1f1f", fill: "#ffffff0d", fill_hover: "#ffffff1a", ink: "#ffffffd6",
      ink_strong: "#ffffff", muted: "#ffffff8f", faint: "#ffffff6b", ghost: "#ffffff42",
      line: "#ffffff1f", subtle: "#ffffff0d", line_strong: "#ffffff2e", hover: "#ffffff0d",
      selected: "#ffffff1a", selected_hover: "#ffffff24", accent: "#1a88ff", accent_hover: "#258eff",
      accent_soft: "#1a88ff1a", accent_bd: "#1a88ff47", ok: "#3fb950", ok_soft: "#3fb95024",
      ok_bd: "#3fb95047", ok_ink: "#0d0d0d", warn: "#d29922", warn_soft: "#d2992224",
      warn_bd: "#d2992247", danger: "#f85149", danger_soft: "#f8514924", danger_bd: "#f8514947",
      done: "#a371f7", done_soft: "#a371f724", done_bd: "#a371f747", info: "#1a88ff",
      composer_bg: "#1f1f1f", composer_line: "#ffffff1f", composer_focus_line: "#ffffff40",
      send_bg: "#ffffffd6", send_bg_hover: "#ffffffd8", send_icon: "#1f1f1f", send_bg_off: "#ffffff1a",
      send_icon_off: "#ffffff47", stop_glyph: "#f85149b8", diff_add: "#3fb950",
      diff_add_bg: "#3fb95024", diff_del: "#f85149", diff_del_bg: "#f8514924", bubble: "#292929",
      selection: "#1a88ff45", menu: "#292929f2", code_inline: "#ffffff1a",
    },
  },
  "kimi-light": {
    label: "kimi light",
    dark: false,
    colors: {
      bg: "#ffffff", sidebar: "#f9fbfc", surface: "#f5f5f5", raised: "#ffffff", sunken: "#f5f5f5",
      deep: "#ffffff", well: "#f5f5f5", fill: "#00000008", fill_hover: "#0000000d", ink: "#000000e6",
      ink_strong: "#000000", muted: "#00000099", faint: "#00000073", ghost: "#0000004d",
      line: "#00000021", subtle: "#0000000d", line_strong: "#00000026", hover: "#00000008",
      selected: "#0000000d", selected_hover: "#00000014", accent: "#1783ff", accent_hover: "#167ff7",
      accent_soft: "#e8f3ff", accent_bd: "#1783ff40", ok: "#0e7a38", ok_soft: "#e7f6ee",
      ok_bd: "#bfe3cc", ok_ink: "#ffffff", warn: "#a9610a", warn_soft: "#fbf1e0", warn_bd: "#f0d9b8",
      danger: "#c0392b", danger_soft: "#fbeaea", danger_bd: "#f0cccc", done: "#8250df",
      done_soft: "#f3e8ff", done_bd: "#e0ccff", info: "#1783ff", composer_bg: "#ffffff",
      composer_line: "#00000021", composer_focus_line: "#00000040", send_bg: "#000000e6",
      send_bg_hover: "#252525", send_icon: "#ffffff", send_bg_off: "#0000000d",
      send_icon_off: "#00000045", stop_glyph: "#c0392b", diff_add: "#16c456",
      diff_add_bg: "#16c4561a", diff_del: "#ff4756", diff_del_bg: "#ff4d4d1a", bubble: "#f5f5f5",
      selection: "#1783ff58", menu: "#fffffff2", code_inline: "#00000008",
    },
  },
} satisfies Record<string, Palette>;

/** The name of a palette that ships with the interface. */
export type BundledName = keyof typeof BUNDLED;

/** Following the system, rather than one theme by name. */
export const SYSTEM = "system";

const CHOICE_KEY = "errand-theme";
const PAIR_KEY = "errand-theme-pair";
const CUSTOM_KEY = "errand-themes";

const COLOR = /^#(?:[0-9a-f]{6}|[0-9a-f]{8})$/i;

/** What a reader has chosen and made, as kept in this browser. */
export interface Settings {
  /** A theme name, or [SYSTEM]. */
  choice: string;
  /** What the system choice shows on a dark and on a light desktop. */
  pair: { dark: string; light: string };
  custom: CustomTheme[];
}

function isBundled(name: string): name is BundledName {
  return Object.hasOwn(BUNDLED, name);
}

function readJson<T>(key: string, fallback: T): T {
  try {
    const held = localStorage.getItem(key);
    return held === null ? fallback : (JSON.parse(held) as T);
  } catch {
    return fallback;
  }
}

/** What is kept, with anything unreadable put back to its default. */
export function settings(): Settings {
  const custom = readJson<CustomTheme[]>(CUSTOM_KEY, []).filter(
    (theme) => typeof theme?.name === "string" && isBundled(theme.base),
  );
  const pair = readJson(PAIR_KEY, { dark: "kage-shadow", light: "kage-dawn" });
  return {
    choice: localStorage.getItem(CHOICE_KEY) ?? SYSTEM,
    pair: {
      dark: typeof pair.dark === "string" ? pair.dark : "kage-shadow",
      light: typeof pair.light === "string" ? pair.light : "kage-dawn",
    },
    custom,
  };
}

/** Keeps what a reader chose and made. */
export function save(next: Settings): void {
  localStorage.setItem(CHOICE_KEY, next.choice);
  localStorage.setItem(PAIR_KEY, JSON.stringify(next.pair));
  localStorage.setItem(CUSTOM_KEY, JSON.stringify(next.custom));
}

/** Every theme a reader can pick, bundled first. */
export function names(held: Settings): { name: string; label: string; dark: boolean }[] {
  return [
    ...Object.entries(BUNDLED).map(([name, palette]) => ({
      name,
      label: palette.label,
      dark: palette.dark,
    })),
    ...held.custom.map((theme) => ({
      name: theme.name,
      label: theme.name,
      dark: BUNDLED[theme.base].dark,
    })),
  ];
}

/**
 * The palette a name stands for.
 *
 * An unknown name falls back to kage shadow rather than leaving the page
 * unpainted, which is what a theme deleted in another tab would otherwise do.
 */
export function resolve(name: string, held: Settings): Palette {
  if (isBundled(name)) return BUNDLED[name];
  const custom = held.custom.find((theme) => theme.name === name);
  if (custom === undefined) return BUNDLED["kage-shadow"];
  const base = BUNDLED[custom.base];
  return { label: custom.name, dark: base.dark, colors: { ...base.colors, ...custom.colors } };
}

function systemIsDark(): boolean {
  return !window.matchMedia("(prefers-color-scheme: light)").matches;
}

/** The theme being shown, once the system has been asked. */
export function showing(held: Settings): string {
  if (held.choice !== SYSTEM) return held.choice;
  return systemIsDark() ? held.pair.dark : held.pair.light;
}

/** Paints the page in a palette. */
export function paint(palette: Palette): void {
  const root = document.documentElement;
  for (const token of TOKENS) {
    root.style.setProperty(`--${token.replaceAll("_", "-")}`, palette.colors[token]);
  }
  root.style.colorScheme = palette.dark ? "dark" : "light";
  root.dataset.theme = palette.dark ? "dark" : "light";
}

/** Paints the page in what is chosen. */
export function apply(held: Settings = settings()): void {
  paint(resolve(showing(held), held));
}

/**
 * Repaints when the system switches between dark and light.
 *
 * @returns a function that stops watching.
 */
export function watch(): () => void {
  const query = window.matchMedia("(prefers-color-scheme: light)");
  const react = () => apply();
  query.addEventListener("change", react);
  return () => query.removeEventListener("change", react);
}

/** Why a theme file could not be read. */
export class ThemeError extends Error {}

/**
 * Reads a kage theme file.
 *
 * Only what the interface can use is read: `base` and the `[gui]` table. The
 * terminal's `[colors]` and `[groups]` are skipped, as kage's own clients skip
 * them. An unknown token or an unreadable colour is skipped too, so a file
 * written for a newer kage still loads.
 */
export function parseTheme(name: string, source: string): CustomTheme {
  let section = "";
  let base: BundledName | undefined;
  const colors: Partial<Record<Token, string>> = {};

  for (const raw of source.split("\n")) {
    const line = withoutComment(raw).trim();
    if (line.length === 0) continue;
    const header = /^\[([^\]]+)\]$/.exec(line);
    if (header !== null) {
      section = header[1]!.trim();
      continue;
    }
    const pair = /^([A-Za-z0-9_-]+)\s*=\s*(.+)$/.exec(line);
    if (pair === null) continue;
    const key = pair[1]!;
    const value = unquote(pair[2]!.trim());

    if (section === "" && key === "base") {
      if (value === "default") continue;
      if (!isBundled(value)) throw new ThemeError(`base "${value}" is not a bundled theme`);
      base = value;
    } else if (section === "gui" && (TOKENS as readonly string[]).includes(key) && COLOR.test(value)) {
      colors[key as Token] = value.toLowerCase();
    }
  }

  if (Object.keys(colors).length === 0 && base === undefined) {
    throw new ThemeError("nothing in it is a base or a [gui] colour");
  }
  return { name, base: base ?? "kage-shadow", colors };
}

/** Writes a theme back out in kage's format, so it can be used there too. */
export function formatTheme(theme: CustomTheme): string {
  const lines = [`base = "${theme.base}"`, "", "[gui]"];
  for (const token of TOKENS) {
    const color = theme.colors[token];
    if (color !== undefined) lines.push(`${token} = "${color}"`);
  }
  return `${lines.join("\n")}\n`;
}

/** Whether a string is a colour a theme may hold. */
export function isColor(value: string): boolean {
  return COLOR.test(value);
}

// A `#` starts a comment only outside a string, since every colour has one.
function withoutComment(line: string): string {
  let quoted = false;
  for (let at = 0; at < line.length; at++) {
    const char = line[at];
    if (char === '"') quoted = !quoted;
    else if (char === "#" && !quoted) return line.slice(0, at);
  }
  return line;
}

function unquote(value: string): string {
  return /^".*"$/.test(value) || /^'.*'$/.test(value) ? value.slice(1, -1) : value;
}
