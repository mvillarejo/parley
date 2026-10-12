import type { Marker } from "../types";
import { clock } from "../lib/format";

/** The note being typed in the recorder. */
export interface NoteDraft {
  meetingId: string;
  /** The moment the note refers to: captured when the field opened. */
  atMs: number;
  text: string;
}

/** The live meeting and the current recording time (`begin_note`, or the
 * note shortcut's `widget-note` event). */
export interface LiveMoment {
  meeting_id: string;
  at_ms: number;
}

/** Open the note field at `moment`. Opening it again while a note is being
 * typed keeps that note, and its moment. */
export function openNote(
  current: NoteDraft | null,
  moment: LiveMoment,
): NoteDraft {
  return (
    current ?? { meetingId: moment.meeting_id, atMs: moment.at_ms, text: "" }
  );
}

/** Typing changes the text, never the moment. */
export function editNote(draft: NoteDraft, text: string): NoteDraft {
  return { ...draft, text };
}

/** The `add_marker` arguments for a draft, or null when it is blank (Enter on
 * an empty field just closes it). */
export function noteToSave(draft: NoteDraft): {
  meetingId: string;
  kind: "note";
  atMs: number;
  text: string;
} | null {
  const text = draft.text.trim();
  if (!text) return null;
  return { meetingId: draft.meetingId, kind: "note", atMs: draft.atMs, text };
}

export type WidgetInputMode = "none" | "title" | "note";

/** The recorder takes the keyboard only while one of its fields is in use; the
 * note field also makes the window taller. */
export function widgetInputMode(
  noteOpen: boolean,
  titleFocused: boolean,
): WidgetInputMode {
  if (noteOpen) return "note";
  return titleFocused ? "title" : "none";
}

/** The brief confirmation after a marker is saved. */
export function markerFlash(marker: Marker): string {
  return marker.kind === "note"
    ? `Note saved · ${clock(marker.at_ms)}`
    : `Marked ${clock(marker.at_ms)}`;
}
