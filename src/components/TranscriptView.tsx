import { useEffect, useMemo, useRef, useState } from "react";
import type { Marker, Segment } from "../types";
import { timestamp } from "../lib/format";
import { speakerClass } from "../lib/speakers";
import {
  MARKER_LABEL,
  buildTimeline,
  markerMatches,
  segmentMatches,
} from "../lib/transcript";
import { Icon } from "./Icon";

export interface MarkerEditing {
  onEdit: (id: number, text: string) => Promise<void>;
  onDelete: (id: number) => Promise<void>;
}

/** A highlight or note inline in the transcript. */
function MarkerRow({
  marker,
  onSeek,
  editing,
}: {
  marker: Marker;
  onSeek: (ms: number) => void;
  editing?: MarkerEditing;
}) {
  const [draft, setDraft] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const label = MARKER_LABEL[marker.kind];
  const at = timestamp(marker.at_ms);

  const run = async (fn: () => Promise<void>) => {
    setBusy(true);
    setError(null);
    try {
      await fn();
      setDraft(null);
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <li className={`marker marker--${marker.kind}`}>
      <div className="turn-head">
        <span className="marker-chip">
          <Icon name={marker.kind === "note" ? "note" : "star"} size={13} />
          {label}
        </span>
        <button
          type="button"
          className="turn-time"
          onClick={() => onSeek(marker.at_ms)}
          aria-label={`Play from ${at}`}
        >
          {at}
        </button>
      </div>
      <div className="marker-body">
        {draft !== null ? (
          <input
            className="marker-input"
            value={draft}
            autoFocus
            maxLength={2000}
            disabled={busy}
            aria-label={`Edit note at ${at}`}
            onChange={(e) => setDraft(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter" && editing)
                void run(() => editing.onEdit(marker.id, draft));
              if (e.key === "Escape") {
                setDraft(null);
                setError(null);
              }
            }}
          />
        ) : (
          <p
            className="marker-text"
            onClick={() => onSeek(marker.at_ms)}
            title={`Play from ${at}`}
          >
            {marker.text ??
              (marker.kind === "highlight" ? "Marked moment" : "")}
          </p>
        )}
        {editing && (
          <div className="marker-actions">
            {marker.kind === "note" && draft === null && (
              <button
                type="button"
                className="icon-btn icon-btn--sm"
                aria-label={`Edit note at ${at}`}
                title="Edit note"
                onClick={() => setDraft(marker.text ?? "")}
              >
                <Icon name="pencil" size={14} />
              </button>
            )}
            <button
              type="button"
              className="icon-btn icon-btn--sm icon-btn--danger"
              aria-label={`Delete ${label.toLowerCase()} at ${at}`}
              title={`Delete ${label.toLowerCase()}`}
              disabled={busy}
              onClick={() => void run(() => editing.onDelete(marker.id))}
            >
              <Icon name="trash" size={14} />
            </button>
          </div>
        )}
        {error && (
          <p className="form-footnote form-footnote--danger" role="alert">
            {error}
          </p>
        )}
      </div>
    </li>
  );
}

export function TranscriptView({
  segments,
  markers = [],
  activeIndex,
  filter,
  onSeek,
  followPlayback,
  editing,
}: {
  segments: Segment[];
  markers?: Marker[];
  activeIndex: number;
  filter: string;
  onSeek: (ms: number) => void;
  followPlayback: boolean;
  editing?: MarkerEditing;
}) {
  const timeline = useMemo(() => {
    const query = filter.trim();
    return query
      ? buildTimeline(
          segments.filter((s) => segmentMatches(s, query)),
          markers.filter((m) => markerMatches(m, query)),
        )
      : buildTimeline(segments, markers);
  }, [segments, markers, filter]);
  const activeSegment = activeIndex >= 0 ? segments[activeIndex] : null;
  const activeRef = useRef<HTMLSpanElement | null>(null);

  useEffect(() => {
    if (followPlayback) {
      activeRef.current?.scrollIntoView({
        block: "nearest",
        behavior: "smooth",
      });
    }
  }, [activeSegment, followPlayback]);

  if (segments.length === 0 && markers.length === 0) {
    return (
      <p className="transcript-empty">
        No speech was transcribed in this meeting.
      </p>
    );
  }
  if (timeline.length === 0) {
    return (
      <p className="transcript-empty">
        Nothing in this transcript matches “{filter.trim()}”.
      </p>
    );
  }

  return (
    <ol className="transcript" aria-label="Transcript">
      {timeline.map((entry) => {
        if (entry.type === "marker") {
          return (
            <MarkerRow
              key={`marker-${entry.marker.id}`}
              marker={entry.marker}
              onSeek={onSeek}
              editing={editing}
            />
          );
        }
        const turn = entry.turn;
        return (
          <li
            key={`${turn.speaker}-${turn.start_ms}`}
            className={`turn ${speakerClass(turn.speaker)}`}
          >
            <div className="turn-head">
              <span className="speaker-chip">{turn.speaker}</span>
              <button
                type="button"
                className="turn-time"
                onClick={() => onSeek(turn.start_ms)}
                aria-label={`Play from ${timestamp(turn.start_ms)}`}
              >
                {timestamp(turn.start_ms)}
              </button>
            </div>
            <p className="turn-text">
              {turn.segments.map((s, i) => {
                const active = s === activeSegment;
                return (
                  <span
                    key={`${s.start_ms}-${i}`}
                    ref={active ? activeRef : undefined}
                    className={`seg${active ? " is-active" : ""}`}
                    onClick={() => onSeek(s.start_ms)}
                    title={timestamp(s.start_ms)}
                  >
                    {s.text}{" "}
                  </span>
                );
              })}
            </p>
          </li>
        );
      })}
    </ol>
  );
}
