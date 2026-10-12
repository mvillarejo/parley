import { describe, expect, it } from "vitest";
import type { Meeting } from "../types";
import {
  canDelete,
  canRetranscribe,
  groupByDay,
  progressFraction,
  progressLabel,
} from "./meetings";

const meeting = (
  id: string,
  status: Meeting["status"],
  started: Date,
): Meeting => ({
  id,
  title: id,
  started_at: started.toISOString(),
  ended_at: null,
  duration_ms: 0,
  status,
  error: null,
  capture_warning: null,
  profile_name: null,
  provider: null,
  language: null,
  folder: `meetings/${id}`,
  speakers: [],
  segment_count: 0,
  updated_at: started.toISOString(),
});

const now = new Date(2026, 9, 8, 12);

describe("meeting actions", () => {
  it("never retranscribes or deletes what is live", () => {
    const ready = meeting("a", "ready", now);
    expect(canRetranscribe(ready, null)).toBe(true);
    expect(canRetranscribe(ready, "a")).toBe(false);
    expect(canRetranscribe(meeting("b", "transcribing", now), null)).toBe(
      false,
    );
    expect(canRetranscribe(meeting("c", "failed", now), null)).toBe(true);
    expect(canDelete(meeting("b", "transcribing", now), null)).toBe(false);
    expect(canDelete(meeting("c", "failed", now), null)).toBe(true);
    expect(canDelete(ready, "a")).toBe(false);
  });
});

describe("progress", () => {
  it("describes and measures progress across both tracks", () => {
    expect(progressLabel(undefined)).toBe("Transcribing…");
    const me = { meeting_id: "a", track: "me" as const, done: 3, total: 12 };
    expect(progressLabel(me)).toBe("Transcribing your mic… 3/12");
    expect(progressFraction(me)).toBeCloseTo(0.125);
    const them = { meeting_id: "a", track: "them" as const, done: 1, total: 2 };
    expect(progressLabel(them)).toBe("Transcribing the call… 1/2");
    expect(progressFraction(them)).toBeCloseTo(0.75);
    expect(progressFraction({ ...them, total: 0 })).toBe(0);
  });
});

describe("groupByDay", () => {
  it("groups consecutive meetings by day label", () => {
    const groups = groupByDay(
      [
        meeting("a", "ready", new Date(2026, 9, 8, 11)),
        meeting("b", "ready", new Date(2026, 9, 8, 9)),
        meeting("c", "ready", new Date(2026, 9, 7, 15)),
      ],
      now,
    );
    expect(groups.map((g) => [g.label, g.meetings.length])).toEqual([
      ["Today", 2],
      ["Yesterday", 1],
    ]);
    expect(groupByDay([], now)).toEqual([]);
  });
});
