import { describe, expect, it } from "vitest";
import { clock, dayLabel, humanDuration, timeOfDay, timestamp } from "./format";

describe("format", () => {
  it.each([
    [0, "00:00:00"],
    [61_999, "00:01:01"],
    [3_723_000, "01:02:03"],
    [-10, "00:00:00"],
  ])("timestamp(%s) = %s", (ms, expected) => {
    expect(timestamp(ms)).toBe(expected);
  });

  it.each([
    [0, "00:00"],
    [65_000, "01:05"],
    [3_600_000, "1:00:00"],
    [3_725_000, "1:02:05"],
  ])("clock(%s) = %s", (ms, expected) => {
    expect(clock(ms)).toBe(expected);
  });

  it.each([
    [10_000, "< 1 min"],
    [90_000, "2 min"],
    [3_030_000, "51 min"],
    [3_900_000, "1 h 05 min"],
  ])("humanDuration(%s) = %s", (ms, expected) => {
    expect(humanDuration(ms)).toBe(expected);
  });

  it("labels days relative to now", () => {
    const now = new Date(2026, 9, 8, 15, 0);
    expect(dayLabel(new Date(2026, 9, 8, 9, 0).toISOString(), now)).toBe(
      "Today",
    );
    expect(dayLabel(new Date(2026, 9, 7, 23, 0).toISOString(), now)).toBe(
      "Yesterday",
    );
    expect(dayLabel(new Date(2026, 9, 5, 10, 0).toISOString(), now)).toBe(
      "Monday 5 October",
    );
    expect(dayLabel(new Date(2026, 8, 1, 10, 0).toISOString(), now)).toBe(
      "1 September",
    );
    expect(dayLabel(new Date(2025, 8, 1, 10, 0).toISOString(), now)).toBe(
      "1 September 2025",
    );
    expect(dayLabel("nope", now)).toBe("");
  });

  it("formats time of day", () => {
    expect(timeOfDay(new Date(2026, 9, 8, 9, 5).toISOString())).toBe("09:05");
    expect(timeOfDay("bad")).toBe("");
  });
});
