import { describe, expect, it } from "vitest";
import type { Marker } from "../types";
import {
  editNote,
  markerFlash,
  noteToSave,
  openNote,
  widgetInputMode,
} from "./noteDraft";

describe("note drafts", () => {
  it("keep the moment the field opened, not the moment it is saved", () => {
    // Opened at 12:34 into the recording…
    let draft = openNote(null, { meeting_id: "m1", at_ms: 754_000 });
    expect(draft).toEqual({ meetingId: "m1", atMs: 754_000, text: "" });
    // …typed for a while (the recording clock moves on)…
    draft = editNote(draft, "Finance agreed to");
    draft = editNote(draft, "Finance agreed to the budget ");
    // …and saved at the original moment.
    expect(noteToSave(draft)).toEqual({
      meetingId: "m1",
      kind: "note",
      atMs: 754_000,
      text: "Finance agreed to the budget",
    });
  });

  it("keep the open note when the field is opened again", () => {
    const first = editNote(
      openNote(null, { meeting_id: "m1", at_ms: 1_000 }),
      "a",
    );
    expect(openNote(first, { meeting_id: "m1", at_ms: 9_000 })).toBe(first);
  });

  it("save nothing when blank", () => {
    const draft = openNote(null, { meeting_id: "m1", at_ms: 0 });
    expect(noteToSave(draft)).toBeNull();
    expect(noteToSave(editNote(draft, "   \n "))).toBeNull();
  });
});

describe("widgetInputMode", () => {
  it("takes the keyboard only while a field is in use", () => {
    expect(widgetInputMode(false, false)).toBe("none");
    expect(widgetInputMode(false, true)).toBe("title");
    expect(widgetInputMode(true, false)).toBe("note");
    expect(widgetInputMode(true, true)).toBe("note");
  });
});

describe("markerFlash", () => {
  const marker = (kind: Marker["kind"], at_ms: number): Marker => ({
    id: 1,
    meeting_id: "m1",
    at_ms,
    kind,
    text: kind === "note" ? "x" : null,
    created_at: "",
  });
  it("confirms with the recorder clock time", () => {
    expect(markerFlash(marker("highlight", 754_000))).toBe("Marked 12:34");
    expect(markerFlash(marker("note", 3_723_000))).toBe("Note saved · 1:02:03");
  });
});
