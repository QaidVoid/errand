/**
 * Which theme the interface is shown in.
 *
 * The system decides unless a reader has said otherwise. A choice is written to
 * the root element, where the stylesheet's `[data-theme]` rules win over both
 * the default and the `prefers-color-scheme` block, and is remembered so that a
 * reload does not undo it.
 */

/** What a reader can ask for. Following the system is the absence of a choice. */
export type Choice = "light" | "dark" | "system";

/** What is actually being shown, once the system has been consulted. */
export type Theme = "light" | "dark";

const KEY = "theme";

function stored(): Choice {
  const held = localStorage.getItem(KEY);
  return held === "light" || held === "dark" ? held : "system";
}

/** What the system is asking for right now. */
export function preferred(): Theme {
  return window.matchMedia("(prefers-color-scheme: light)").matches ? "light" : "dark";
}

/** Applies a choice to the page, and remembers it. */
export function apply(choice: Choice): void {
  if (choice === "system") {
    localStorage.removeItem(KEY);
    document.documentElement.removeAttribute("data-theme");
    return;
  }

  localStorage.setItem(KEY, choice);
  document.documentElement.setAttribute("data-theme", choice);
}

/** The choice in force, which is what a control should show as selected. */
export function choice(): Choice {
  return stored();
}

/** The theme actually being shown, following the system when nothing is chosen. */
export function current(): Theme {
  const held = stored();
  return held === "system" ? preferred() : held;
}

/**
 * Puts the remembered choice into effect.
 *
 * Called once at startup. Without it a chosen theme would be forgotten on every
 * reload, because the attribute the stylesheet reads lives on the page rather
 * than in storage.
 */
export function restore(): void {
  apply(stored());
}

/**
 * Reports when the shown theme changes, whether from the system or a choice.
 *
 * @returns a function that stops watching.
 */
export function watch(onChange: (theme: Theme) => void): () => void {
  const query = window.matchMedia("(prefers-color-scheme: light)");
  const react = () => {
    // A system change only shows when nothing has been chosen, which is what
    // makes a choice a choice rather than a starting point.
    if (stored() === "system") onChange(preferred());
  };

  query.addEventListener("change", react);
  return () => query.removeEventListener("change", react);
}
