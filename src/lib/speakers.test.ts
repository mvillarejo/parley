import { describe, expect, it } from "vitest";
import { SPEAKER_SLOTS, speakerClass, speakerSlot } from "./speakers";

describe("speakerSlot", () => {
  it("gives Me and Them fixed colours", () => {
    expect(speakerSlot("Me")).toBe(0);
    expect(speakerSlot("Them")).toBe(1);
    expect(speakerClass("Me")).toBe("speaker-0");
  });

  it("numbers remote voices and never reuses Me's colour", () => {
    expect(speakerSlot("Them 1")).toBe(1);
    expect(speakerSlot("Them 2")).toBe(2);
    expect(speakerSlot("Them 6")).toBe(1);
    for (const name of ["Them 3", "Them 40", "Alex", "Board"]) {
      const slot = speakerSlot(name);
      expect(slot).toBeGreaterThanOrEqual(1);
      expect(slot).toBeLessThan(SPEAKER_SLOTS);
    }
    expect(speakerSlot("Alex")).toBe(speakerSlot("Alex"));
  });
});
