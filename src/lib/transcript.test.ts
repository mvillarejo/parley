import { describe, expect, it } from "vitest";
import type { Marker, Segment } from "../types";
import {
  activeSegmentIndex,
  buildTimeline,
  groupTurns,
  markerMatches,
  segmentMatches,
} from "./transcript";

const seg = (
  start: number,
  end: number,
  speaker: string,
  text = "x",
): Segment => ({
  start_ms: start,
  end_ms: end,
  speaker,
  text,
});

describe("groupTurns", () => {
  it("joins close segments of the same speaker", () => {
    const turns = groupTurns([
      seg(0, 1000, "Me"),
      seg(1500, 3000, "Me"),
      seg(3500, 5000, "Them"),
      seg(20_000, 21_000, "Them"),
    ]);
    expect(turns.map((t) => [t.speaker, t.segments.length])).toEqual([
      ["Me", 2],
      ["Them", 1],
      ["Them", 1],
    ]);
    expect(turns[0].end_ms).toBe(3000);
    expect(groupTurns([])).toEqual([]);
  });
});

describe("activeSegmentIndex", () => {
  const segments = [
    seg(1000, 2000, "Me"),
    seg(3000, 4000, "Them"),
    seg(5000, 6000, "Me"),
  ];
  it.each([
    [0, -1],
    [1000, 0],
    [2500, 0],
    [3000, 1],
    [5999, 2],
    [60_000, -1],
  ])("at %s ms is %s", (t, expected) => {
    expect(activeSegmentIndex(segments, t)).toBe(expected);
  });
  it("handles an empty transcript", () => {
    expect(activeSegmentIndex([], 100)).toBe(-1);
  });
});

describe("segmentMatches", () => {
  it("matches every word in text or speaker", () => {
    const s = seg(0, 1, "Them 2", "The Budget is approved");
    expect(segmentMatches(s, "budget approved")).toBe(true);
    expect(segmentMatches(s, "them budget")).toBe(true);
    expect(segmentMatches(s, "budget rejected")).toBe(false);
    expect(segmentMatches(s, "   ")).toBe(true);
  });
});

const mark = (
  id: number,
  at: number,
  kind: Marker["kind"] = "highlight",
  text: string | null = null,
): Marker => ({
  id,
  meeting_id: "m",
  at_ms: at,
  kind,
  text,
  created_at: "2026-10-08T10:00:00.000Z",
});

describe("buildTimeline", () => {
  const segments = [
    seg(0, 1000, "Me", "One."),
    seg(1500, 2500, "Me", "Two."),
    seg(3000, 4000, "Them", "Three."),
  ];
  const shape = (markers: Marker[]) =>
    buildTimeline(segments, markers).map((e) =>
      e.type === "turn"
        ? `${e.turn.speaker}:${e.turn.segments.map((s) => s.text).join(" ")}`
        : `marker@${e.marker.at_ms}`,
    );

  it("is just the turns without markers", () => {
    expect(shape([])).toEqual(["Me:One. Two.", "Them:Three."]);
  });

  it("places markers at their moment, splitting a turn, like transcript.md", () => {
    expect(shape([mark(2, 5000, "note", "after"), mark(1, 1500)])).toEqual([
      "Me:One.",
      "marker@1500",
      "Me:Two.",
      "Them:Three.",
      "marker@5000",
    ]);
    // Inside a segment: after it. Before any speech: first. Ties by id.
    expect(shape([mark(5, 500), mark(4, 0), mark(3, 0)])).toEqual([
      "marker@0",
      "marker@0",
      "Me:One.",
      "marker@500",
      "Me:Two.",
      "Them:Three.",
    ]);
    const ids = buildTimeline(segments, [mark(4, 0), mark(3, 0)]).flatMap(
      (e) => (e.type === "marker" ? [e.marker.id] : []),
    );
    expect(ids).toEqual([3, 4]);
  });

  it("shows markers when nothing was transcribed", () => {
    expect(buildTimeline([], [mark(1, 10)])).toHaveLength(1);
  });
});

describe("markerMatches", () => {
  it("matches the kind label and the note text", () => {
    expect(
      markerMatches(mark(1, 0, "note", "Call Finance back"), "finance"),
    ).toBe(true);
    expect(
      markerMatches(mark(1, 0, "note", "Call Finance back"), "note call"),
    ).toBe(true);
    expect(markerMatches(mark(1, 0), "highlight")).toBe(true);
    expect(markerMatches(mark(1, 0), "budget")).toBe(false);
    expect(markerMatches(mark(1, 0), "  ")).toBe(true);
  });
});
