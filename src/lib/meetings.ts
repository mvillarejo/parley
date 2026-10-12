import type { Meeting, MeetingStatus, TranscriptionProgress } from "../types";
import { dayLabel } from "./format";

export const STATUS_LABELS: Record<MeetingStatus, string> = {
  recording: "Recording",
  transcribing: "Transcribing",
  ready: "Ready",
  failed: "Failed",
};

export function canRetranscribe(
  meeting: Meeting,
  recordingId: string | null,
): boolean {
  return (
    meeting.id !== recordingId &&
    meeting.status !== "recording" &&
    meeting.status !== "transcribing"
  );
}

export function canDelete(
  meeting: Meeting,
  recordingId: string | null,
): boolean {
  return meeting.id !== recordingId && meeting.status !== "transcribing";
}

/** "Transcribing your mic… 3/12", "Transcribing the call… 1/4". */
export function progressLabel(
  progress: TranscriptionProgress | undefined,
): string {
  if (!progress) return "Transcribing…";
  const who = progress.track === "me" ? "your mic" : "the call";
  return progress.total > 0
    ? `Transcribing ${who}… ${progress.done}/${progress.total}`
    : `Transcribing ${who}…`;
}

/** 0..1 across both tracks (mic first, then call). */
export function progressFraction(
  progress: TranscriptionProgress | undefined,
): number {
  if (!progress || progress.total === 0) return 0;
  const within = progress.done / progress.total;
  return progress.track === "me" ? within / 2 : 0.5 + within / 2;
}

export interface DayGroup {
  label: string;
  meetings: Meeting[];
}

/** Meetings (newest first) grouped under day headings. */
export function groupByDay(
  meetings: Meeting[],
  now: Date = new Date(),
): DayGroup[] {
  const groups: DayGroup[] = [];
  for (const meeting of meetings) {
    const label = dayLabel(meeting.started_at, now);
    const last = groups[groups.length - 1];
    if (last && last.label === label) last.meetings.push(meeting);
    else groups.push({ label, meetings: [meeting] });
  }
  return groups;
}
