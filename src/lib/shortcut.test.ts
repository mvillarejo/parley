import { describe, expect, it } from "vitest";
import { formatShortcut } from "./shortcut";

describe("formatShortcut", () => {
  it.each([
    ["CommandOrControl+Shift+Alt+R", "⌥⇧⌘R"],
    ["Cmd+Shift+r", "⇧⌘R"],
    ["Control+Option+Space", "⌃⌥Space"],
    ["F13", "F13"],
    ["", ""],
  ])("%s → %s", (input, expected) => {
    expect(formatShortcut(input)).toBe(expected);
  });
});
