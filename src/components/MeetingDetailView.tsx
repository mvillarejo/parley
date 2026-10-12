import { useEffect, useRef, useState } from "react";
import type {
  MarkerKind,
  MeetingDetail,
  TranscriptionProgress,
} from "../types";
import type { ProfilesState } from "../lib/sttProvider";
import { dayLabel, humanDuration, timeOfDay, timestamp } from "../lib/format";
import {
  canDelete,
  canRetranscribe,
  progressFraction,
  progressLabel,
} from "../lib/meetings";
import { activeSegmentIndex } from "../lib/transcript";
import { AudioPlayer, type TrackChoice } from "./AudioPlayer";
import { ConfirmDialog } from "./ConfirmDialog";
import { CopyButton } from "./CopyButton";
import { Icon } from "./Icon";
import { StatusBadge } from "./MeetingList";
import { TranscriptView } from "./TranscriptView";

export interface MeetingActions {
  onRename: (id: string, title: string) => Promise<void>;
  onDelete: (id: string) => Promise<void>;
  onRetranscribe: (id: string, profileId: string) => Promise<void>;
  onReveal: (id: string) => void;
  getMarkdown: (id: string) => Promise<string>;
  audioSrc: (path: string) => string;
  onAddMarker: (
    meetingId: string,
    kind: MarkerKind,
    atMs: number,
    text: string | null,
  ) => Promise<void>;
  onUpdateMarker: (id: number, text: string) => Promise<void>;
  onDeleteMarker: (id: number) => Promise<void>;
}

/** Add a highlight or a note at the playback position. The note's moment is
 * where playback was when its field opened. */
function MarkerComposer({
  timeMs,
  onAdd,
}: {
  timeMs: number;
  onAdd: (kind: MarkerKind, atMs: number, text: string | null) => Promise<void>;
}) {
  const [note, setNote] = useState<{ atMs: number; text: string } | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const add = async (kind: MarkerKind, atMs: number, text: string | null) => {
    setBusy(true);
    setError(null);
    try {
      await onAdd(kind, atMs, text);
      setNote(null);
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="marker-composer">
      <div className="marker-composer-buttons">
        <button
          type="button"
          className="btn btn-ghost btn-sm marker-add marker-add--highlight"
          disabled={busy}
          title={`Add a highlight at ${timestamp(timeMs)}`}
          onClick={() => void add("highlight", Math.round(timeMs), null)}
        >
          <Icon name="star" size={14} />
          Highlight
        </button>
        <button
          type="button"
          className="btn btn-ghost btn-sm marker-add marker-add--note"
          disabled={busy}
          aria-pressed={note !== null}
          title={`Add a note at ${timestamp(timeMs)}`}
          onClick={() =>
            setNote((n) => n ?? { atMs: Math.round(timeMs), text: "" })
          }
        >
          <Icon name="note" size={14} />
          Note
        </button>
      </div>
      {note && (
        <div className="marker-note-field">
          <span className="marker-note-time">{timestamp(note.atMs)}</span>
          <input
            className="marker-input"
            value={note.text}
            autoFocus
            maxLength={2000}
            disabled={busy}
            placeholder="Note · Enter saves, Esc cancels"
            aria-label={`Note at ${timestamp(note.atMs)}`}
            onChange={(e) => setNote({ ...note, text: e.target.value })}
            onKeyDown={(e) => {
              if (e.key === "Enter" && note.text.trim())
                void add("note", note.atMs, note.text);
              if (e.key === "Escape") {
                setNote(null);
                setError(null);
              }
            }}
          />
        </div>
      )}
      {error && (
        <p className="form-footnote form-footnote--danger" role="alert">
          {error}
        </p>
      )}
    </div>
  );
}

function TitleEditor({
  title,
  onSave,
}: {
  title: string;
  onSave: (title: string) => Promise<void>;
}) {
  const [value, setValue] = useState(title);
  const [error, setError] = useState<string | null>(null);
  useEffect(() => setValue(title), [title]);

  const commit = async () => {
    const next = value.trim();
    if (!next) {
      setValue(title);
      return;
    }
    if (next === title) return;
    try {
      setError(null);
      await onSave(next);
    } catch (e) {
      setError(String(e));
      setValue(title);
    }
  };

  return (
    <div className="title-editor">
      <input
        className="title-input"
        value={value}
        aria-label="Meeting title"
        maxLength={200}
        onChange={(e) => setValue(e.target.value)}
        onBlur={() => void commit()}
        onKeyDown={(e) => {
          if (e.key === "Enter") e.currentTarget.blur();
          if (e.key === "Escape") {
            setValue(title);
            e.currentTarget.blur();
          }
        }}
      />
      {error && (
        <p className="form-footnote form-footnote--danger" role="alert">
          {error}
        </p>
      )}
    </div>
  );
}

export function MeetingDetailView({
  detail,
  recordingId,
  progress,
  profiles,
  actions,
}: {
  detail: MeetingDetail;
  recordingId: string | null;
  progress: TranscriptionProgress | undefined;
  profiles: ProfilesState;
  actions: MeetingActions;
}) {
  const { meeting, segments, markers, files } = detail;
  const audioRef = useRef<HTMLAudioElement | null>(null);
  const [timeMs, setTimeMs] = useState(0);
  const [filter, setFilter] = useState("");
  const [following, setFollowing] = useState(false);
  const [profileId, setProfileId] = useState(profiles.active_profile_id);
  const [confirmDelete, setConfirmDelete] = useState(false);
  const [busy, setBusy] = useState(false);
  const [actionError, setActionError] = useState<string | null>(null);

  useEffect(() => {
    setTimeMs(0);
    setFilter("");
    setActionError(null);
    setConfirmDelete(false);
  }, [meeting.id]);
  useEffect(
    () => setProfileId(profiles.active_profile_id),
    [profiles.active_profile_id],
  );

  const isLive = meeting.id === recordingId;
  const sources: Partial<Record<TrackChoice, string>> = isLive
    ? {}
    : {
        mixed: files.mixed ? actions.audioSrc(files.mixed) : undefined,
        mic: files.mic ? actions.audioSrc(files.mic) : undefined,
        system: files.system ? actions.audioSrc(files.system) : undefined,
      };

  const seek = (ms: number) => {
    const audio = audioRef.current;
    if (!audio) return;
    audio.currentTime = ms / 1000;
    setFollowing(true);
    void audio.play();
  };

  const run = async (fn: () => Promise<void>) => {
    setBusy(true);
    setActionError(null);
    try {
      await fn();
    } catch (e) {
      setActionError(String(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <article className="meeting-detail" aria-label={meeting.title}>
      <header className="meeting-head">
        <TitleEditor
          title={meeting.title}
          onSave={(title) => actions.onRename(meeting.id, title)}
        />
        <div className="meeting-meta">
          <span>
            {dayLabel(meeting.started_at)}, {timeOfDay(meeting.started_at)}
          </span>
          {!isLive && <span>{humanDuration(meeting.duration_ms)}</span>}
          <StatusBadge status={meeting.status} />
          {meeting.profile_name && meeting.status !== "recording" && (
            <span className="meeting-meta-muted">
              via {meeting.profile_name}
            </span>
          )}
        </div>
      </header>

      {meeting.capture_warning && (
        <div className="notice notice--warning" role="note">
          <Icon name="warning" size={16} />
          <span>{meeting.capture_warning}</span>
        </div>
      )}
      {meeting.status === "failed" && (
        <div className="notice notice--danger" role="alert">
          <Icon name="warning" size={16} />
          <span>{meeting.error ?? "Transcription failed."}</span>
          <button
            type="button"
            className="btn btn-secondary btn-sm"
            disabled={busy}
            onClick={() =>
              void run(() => actions.onRetranscribe(meeting.id, profileId))
            }
          >
            <Icon name="refresh" size={14} />
            Retry
          </button>
        </div>
      )}
      {meeting.status === "transcribing" && (
        <div className="progress" role="status" aria-live="polite">
          <span>{progressLabel(progress)}</span>
          <div className="progress-track">
            <div
              className={`progress-fill${progress ? "" : " is-indeterminate"}`}
              style={
                progress
                  ? {
                      transform: `scaleX(${Math.max(0.03, progressFraction(progress))})`,
                    }
                  : undefined
              }
            />
          </div>
        </div>
      )}
      {isLive && (
        <div className="notice notice--live" role="status">
          <span className="rec-dot" aria-hidden="true" />
          <span>
            Recording now. The transcript appears here after you stop.
          </span>
        </div>
      )}

      {!isLive && (
        <div className="meeting-toolbar">
          <AudioPlayer
            key={meeting.id}
            sources={sources}
            audioRef={audioRef}
            onTime={setTimeMs}
          />
          <div className="meeting-actions">
            <CopyButton
              label="Copy as Markdown"
              getText={() => actions.getMarkdown(meeting.id)}
            />
            <button
              type="button"
              className="btn btn-ghost btn-sm"
              onClick={() => actions.onReveal(meeting.id)}
            >
              <Icon name="folder" size={15} />
              Show in Finder
            </button>
            <span className="meeting-actions-spacer" />
            <div className="retranscribe">
              <select
                className="form-input form-input--sm"
                aria-label="Profile for re-transcription"
                value={profileId}
                onChange={(e) => setProfileId(e.target.value)}
              >
                {profiles.profiles.map((p) => (
                  <option key={p.id} value={p.id}>
                    {p.name}
                  </option>
                ))}
              </select>
              <button
                type="button"
                className="btn btn-secondary btn-sm"
                disabled={busy || !canRetranscribe(meeting, recordingId)}
                onClick={() =>
                  void run(() => actions.onRetranscribe(meeting.id, profileId))
                }
              >
                <Icon name="refresh" size={14} />
                Re-transcribe
              </button>
            </div>
            <button
              type="button"
              className="icon-btn icon-btn--danger"
              aria-label="Delete meeting"
              title="Delete meeting"
              disabled={!canDelete(meeting, recordingId)}
              onClick={() => setConfirmDelete(true)}
            >
              <Icon name="trash" size={16} />
            </button>
          </div>
          {actionError && (
            <p className="form-footnote form-footnote--danger" role="alert">
              {actionError}
            </p>
          )}
        </div>
      )}

      {meeting.status === "ready" && (
        <section className="transcript-panel">
          <div className="transcript-tools">
            <label className="search-field search-field--compact">
              <Icon name="search" size={14} />
              <input
                type="search"
                value={filter}
                placeholder="Find in transcript"
                aria-label="Find in transcript"
                onChange={(e) => setFilter(e.target.value)}
              />
            </label>
            <span className="transcript-count">
              {segments.length} segment{segments.length === 1 ? "" : "s"} ·{" "}
              {meeting.speakers.join(", ") || "no speakers"}
              {markers.length > 0 && ` · ${markers.length} marked`}
            </span>
          </div>
          <MarkerComposer
            key={meeting.id}
            timeMs={timeMs}
            onAdd={(kind, atMs, text) =>
              actions.onAddMarker(meeting.id, kind, atMs, text)
            }
          />
          <TranscriptView
            segments={segments}
            markers={markers}
            activeIndex={activeSegmentIndex(segments, timeMs)}
            filter={filter}
            onSeek={seek}
            followPlayback={following}
            editing={{
              onEdit: actions.onUpdateMarker,
              onDelete: actions.onDeleteMarker,
            }}
          />
        </section>
      )}

      <ConfirmDialog
        open={confirmDelete}
        title={`Delete "${meeting.title}"?`}
        confirmLabel="Delete meeting"
        busyLabel="Deleting…"
        isBusy={busy}
        error={actionError}
        onCancel={() => {
          setConfirmDelete(false);
          setActionError(null);
        }}
        onConfirm={() =>
          void run(async () => {
            await actions.onDelete(meeting.id);
            setConfirmDelete(false);
          })
        }
      >
        <p>
          This permanently deletes the audio files and the transcript. Claude
          will no longer see this meeting.
        </p>
      </ConfirmDialog>
    </article>
  );
}
