const SYMBOLS: Record<string, string> = {
  commandorcontrol: "⌘",
  cmdorctrl: "⌘",
  command: "⌘",
  cmd: "⌘",
  super: "⌘",
  control: "⌃",
  ctrl: "⌃",
  shift: "⇧",
  alt: "⌥",
  option: "⌥",
};

const ORDER = ["⌃", "⌥", "⇧", "⌘"];

/** "CommandOrControl+Shift+Alt+R" → "⌥⇧⌘R" (macOS menu order). */
export function formatShortcut(accelerator: string): string {
  const parts = accelerator
    .split("+")
    .map((p) => p.trim())
    .filter(Boolean);
  const mods: string[] = [];
  const keys: string[] = [];
  for (const part of parts) {
    const symbol = SYMBOLS[part.toLowerCase()];
    if (symbol) mods.push(symbol);
    else keys.push(part.length === 1 ? part.toUpperCase() : part);
  }
  mods.sort((a, b) => ORDER.indexOf(a) - ORDER.indexOf(b));
  return [...new Set(mods)].join("") + keys.join("");
}
