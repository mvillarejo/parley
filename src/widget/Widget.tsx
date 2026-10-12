import { useEffect, useRef, useState } from "react";
import type { RecorderStatus } from "../types";
import { clock } from "../lib/format";
import { Icon } from "../components/Icon";
import { LevelMeter } from "../components/LevelMeter";
import type { NoteDraft } from "./noteDraft";

export interface WidgetProps {
  recorder: RecorderStatus;
  /** Status line under the controls, e.g. "Transcribing your mic… 3/12". */
  statusLine: string | null;
  statusAction?: { label: string; onClick: () => void } | null;
  shortcutLabel: string | null;
  markShortcutLabel?: string | null;
  noteShortcutLabel?: string | null;
  /** Brief confirmation ("Marked 12:34") or a problem, shown while live. */
  flash?: { text: string; tone: "ok" | "warn" } | null;
  /** The note being typed, if the note field is open. */
  note?: NoteDraft | null;
  busy: boolean;
  onStart: (title: string) => void;
  onStop: () => void;
  onPause: () => void;
  onResume: () => void;
  onRename: (title: string) => void;
  onOpenLibrary: () => void;
  onHide: () => void;
  onMark?: () => void;
  onNoteOpen?: () => void;
  onNoteChange?: (text: string) => void;
  onNoteSave?: () => void;
  onNoteCancel?: () => void;
  onTitleFocus?: (focused: boolean) => void;
}

const withKeys = (label: string, keys: string | null | undefined) =>
  keys ? `${label} (${keys})` : label;

/** The floating recorder: small, always on top, draggable by its background. */
export function Widget({
  recorder,
  statusLine,
  statusAction,
  shortcutLabel,
  markShortcutLabel,
  noteShortcutLabel,
  flash,
  note,
  busy,
  onStart,
  onStop,
  onPause,
  onResume,
  onRename,
  onOpenLibrary,
  onHide,
  onMark,
  onNoteOpen,
  onNoteChange,
  onNoteSave,
  onNoteCancel,
  onTitleFocus,
}: WidgetProps) {
  const live = recorder.state !== "idle";
  const paused = recorder.state === "paused";
  const [title, setTitle] = useState(recorder.title ?? "");
  const [editing, setEditing] = useState(false);
  const noteRef = useRef<HTMLInputElement | null>(null);
  const noteAt = note?.atMs;

  // Focus the note field when it opens (button or shortcut).
  useEffect(() => {
    if (noteAt !== undefined) noteRef.current?.focus();
  }, [noteAt]);

  useEffect(() => {
    if (!editing) setTitle(recorder.title ?? "");
  }, [recorder.title, editing]);

  const commitTitle = () => {
    setEditing(false);
    onTitleFocus?.(false);
    const next = title.trim();
    if (live && next && next !== recorder.title) onRename(next);
    if (live && !next) setTitle(recorder.title ?? "");
  };

  return (
    <div
      className={`widget${live ? " widget--live" : ""}${paused ? " widget--paused" : ""}`}
      data-tauri-drag-region
    >
      <div className="widget-row" data-tauri-drag-region>
        {live ? (
          <span className="widget-rec" aria-live="polite">
            <span className="rec-dot" aria-hidden="true" />
            <span className="widget-rec-label">
              {paused ? "PAUSED" : "REC"}
            </span>
            <span className="widget-time">{clock(recorder.elapsed_ms)}</span>
          </span>
        ) : (
          <span className="widget-brand" data-tauri-drag-region>
            <img
              src="/Images/parley-mark.svg"
              alt=""
              width={18}
              height={18}
              draggable={false}
            />
            Parley
          </span>
        )}
        <input
          className="widget-title"
          value={title}
          placeholder={live ? "Meeting title" : "Title (optional)"}
          aria-label="Meeting title"
          maxLength={200}
          onFocus={() => {
            setEditing(true);
            onTitleFocus?.(true);
          }}
          onChange={(e) => setTitle(e.target.value)}
          onBlur={commitTitle}
          onKeyDown={(e) => {
            if (e.key === "Enter") {
              if (!live) onStart(title);
              e.currentTarget.blur();
            }
            if (e.key === "Escape") {
              setTitle(recorder.title ?? "");
              e.currentTarget.blur();
            }
          }}
        />
        <button
          type="button"
          className="widget-icon"
          aria-label="Hide recorder"
          title="Hide (recording continues)"
          onClick={onHide}
        >
          <Icon name="close" size={14} />
        </button>
      </div>

      {live ? (
        <div className="widget-live" data-tauri-drag-region>
          <div className="widget-meters" data-tauri-drag-region>
            <LevelMeter label="Me" tone="me" value={recorder.me_level} />
            <LevelMeter
              label="Them"
              tone="them"
              value={recorder.them_level}
              disabled={!recorder.system_audio}
            />
          </div>
          <div className="widget-marks">
            <button
              type="button"
              className="widget-mark widget-mark--highlight"
              aria-label={
                recorder.marker_count > 0
                  ? `Mark this moment (${recorder.marker_count} marked)`
                  : "Mark this moment"
              }
              title={withKeys("Mark this moment", markShortcutLabel)}
              onClick={onMark}
            >
              <Icon name="star" size={14} />
              {recorder.marker_count > 0 && (
                <span className="widget-mark-count">
                  {recorder.marker_count}
                </span>
              )}
            </button>
            <button
              type="button"
              className={`widget-mark widget-mark--note${note ? " is-active" : ""}`}
              aria-label="Add a note"
              aria-pressed={!!note}
              title={withKeys("Add a note", noteShortcutLabel)}
              onClick={() => {
                // A note with text stays open: go back to typing it.
                if (note?.text.trim()) noteRef.current?.focus();
                else onNoteOpen?.();
              }}
            >
              <Icon name="note" size={14} />
            </button>
          </div>
        </div>
      ) : (
        <p className="widget-hint" data-tauri-drag-region>
          Records your mic and the call audio
          {shortcutLabel && (
            <>
              {" · "}
              <kbd className="kbd kbd--sm">{shortcutLabel}</kbd>
            </>
          )}
        </p>
      )}

      {note && (
        <div className="widget-note">
          <span className="widget-note-time" aria-hidden="true">
            <Icon name="note" size={12} />
            {clock(note.atMs)}
          </span>
          <input
            ref={noteRef}
            className="widget-note-input"
            value={note.text}
            placeholder="Note · Enter saves, Esc cancels"
            aria-label={`Note at ${clock(note.atMs)}`}
            maxLength={2000}
            onChange={(e) => onNoteChange?.(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter") {
                e.preventDefault();
                onNoteSave?.();
              }
              if (e.key === "Escape") {
                e.preventDefault();
                onNoteCancel?.();
              }
            }}
          />
        </div>
      )}

      <div className="widget-row widget-row--bottom" data-tauri-drag-region>
        <span
          className={`widget-status${
            live && (flash ? flash.tone === "warn" : recorder.warning)
              ? " widget-status--warn"
              : ""
          }${live && flash?.tone === "ok" ? " widget-status--ok" : ""}`}
          title={
            (live ? (flash?.text ?? recorder.warning) : statusLine) ?? undefined
          }
          role={live && flash ? "status" : undefined}
          data-tauri-drag-region
        >
          {live && flash?.tone === "ok" && <Icon name="check" size={11} />}
          {live
            ? (flash?.text ??
              recorder.warning ??
              "Everyone on the call should know you’re recording.")
            : statusLine}
          {!live && statusAction && (
            <button
              type="button"
              className="widget-link"
              onClick={statusAction.onClick}
            >
              {statusAction.label}
            </button>
          )}
        </span>
        <button
          type="button"
          className="widget-icon"
          aria-label="Open library"
          title="Open library"
          onClick={onOpenLibrary}
        >
          <Icon name="library" size={15} />
        </button>
        {live ? (
          <>
            <button
              type="button"
              className="widget-icon"
              aria-label={paused ? "Resume recording" : "Pause recording"}
              title={paused ? "Resume" : "Pause"}
              onClick={paused ? onResume : onPause}
            >
              <Icon name={paused ? "record" : "pause"} size={15} />
            </button>
            <button
              type="button"
              className="widget-stop"
              onClick={onStop}
              disabled={busy}
              aria-label="Stop recording"
            >
              <Icon name="stop" size={11} />
              Stop
            </button>
          </>
        ) : (
          <button
            type="button"
            className="widget-record"
            onClick={() => onStart(title)}
            disabled={busy}
          >
            <Icon name="record" size={11} />
            {busy ? "Starting…" : "Record"}
          </button>
        )}
      </div>
    </div>
  );
}
