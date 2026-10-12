import type { Marker, MarkerKind, Segment } from "../types";

export interface Turn {
  speaker: string;
  start_ms: number;
  end_ms: number;
  /** The segments in this turn (for click-to-seek and highlighting). */
  segments: Segment[];
}

/** Same speaker with less than this between segments: one turn. Matches the
 * grouping in `transcript.md`. */
export const TURN_GAP_MS = 4_000;

export function groupTurns(segments: Segment[]): Turn[] {
  const turns: Turn[] = [];
  for (const segment of segments) {
    const last = turns[turns.length - 1];
    if (
      last &&
      last.speaker === segment.speaker &&
      segment.start_ms - last.end_ms < TURN_GAP_MS
    ) {
      last.segments.push(segment);
      last.end_ms = Math.max(last.end_ms, segment.end_ms);
    } else {
      turns.push({
        speaker: segment.speaker,
        start_ms: segment.start_ms,
        end_ms: segment.end_ms,
        segments: [segment],
      });
    }
  }
  return turns;
}

/** Index of the segment playing at `timeMs` (the latest one that started), or
 * -1 before the first segment. Segments are sorted by start. */
export function activeSegmentIndex(
  segments: Segment[],
  timeMs: number,
): number {
  let lo = 0;
  let hi = segments.length - 1;
  let found = -1;
  while (lo <= hi) {
    const mid = (lo + hi) >> 1;
    if (segments[mid].start_ms <= timeMs) {
      found = mid;
      lo = mid + 1;
    } else {
      hi = mid - 1;
    }
  }
  if (found >= 0 && timeMs > segments[found].end_ms + TURN_GAP_MS) return -1;
  return found;
}

/** Case-insensitive match of every word, for the in-transcript filter. */
export function segmentMatches(segment: Segment, query: string): boolean {
  const words = query.toLowerCase().split(/\s+/).filter(Boolean);
  if (words.length === 0) return true;
  const text = `${segment.speaker} ${segment.text}`.toLowerCase();
  return words.every((w) => text.includes(w));
}

export type TimelineEntry =
  { type: "turn"; turn: Turn } | { type: "marker"; marker: Marker };

/** Turns with the markers interleaved by time, as in `transcript.md`: a
 * marker goes before the first segment that starts at or after it, splitting
 * the turn it falls in. */
export function buildTimeline(
  segments: Segment[],
  markers: Marker[],
): TimelineEntry[] {
  const pending = [...markers].sort((a, b) => a.at_ms - b.at_ms || a.id - b.id);
  const out: TimelineEntry[] = [];
  let chunk: Segment[] = [];
  let next = 0;
  const flush = () => {
    for (const turn of groupTurns(chunk)) out.push({ type: "turn", turn });
    chunk = [];
  };
  for (const segment of segments) {
    if (next < pending.length && pending[next].at_ms <= segment.start_ms) {
      flush();
      while (next < pending.length && pending[next].at_ms <= segment.start_ms)
        out.push({ type: "marker", marker: pending[next++] });
    }
    chunk.push(segment);
  }
  flush();
  while (next < pending.length)
    out.push({ type: "marker", marker: pending[next++] });
  return out;
}

export const MARKER_LABEL: Record<MarkerKind, string> = {
  highlight: "Highlight",
  note: "Note",
};

/** The in-transcript filter for markers: every word in the label or text. */
export function markerMatches(marker: Marker, query: string): boolean {
  const words = query.toLowerCase().split(/\s+/).filter(Boolean);
  if (words.length === 0) return true;
  const text =
    `${MARKER_LABEL[marker.kind]} ${marker.text ?? ""}`.toLowerCase();
  return words.every((w) => text.includes(w));
}
