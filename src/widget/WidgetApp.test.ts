import { describe, expect, it } from "vitest";
import { lastMeetingLine } from "./WidgetApp";

describe("lastMeetingLine", () => {
  it("describes the meeting that was just recorded", () => {
    expect(lastMeetingLine(null)).toBeNull();
    expect(lastMeetingLine({ id: "a", status: "transcribing" })).toBe(
      "Transcribing…",
    );
    expect(
      lastMeetingLine({
        id: "a",
        status: "transcribing",
        progress: { meeting_id: "a", track: "them", done: 2, total: 5 },
      }),
    ).toBe("Transcribing the call… 2/5");
    expect(lastMeetingLine({ id: "a", status: "ready" })).toBe(
      "Transcript ready.",
    );
    expect(
      lastMeetingLine({ id: "a", status: "failed", error: "Ollama is down" }),
    ).toBe("Transcription failed: Ollama is down");
    expect(lastMeetingLine({ id: "a", status: "failed" })).toBe(
      "Transcription failed.",
    );
  });
});
