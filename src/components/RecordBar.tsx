import type { RecorderStatus } from "../types";
import { clock } from "../lib/format";
import { Icon } from "./Icon";
import { LevelMeter } from "./LevelMeter";

/** Library header: start a meeting, or follow and control the live one. */
export function RecordBar({
  recorder,
  shortcutLabel,
  busy,
  onStart,
  onStop,
  onPause,
  onResume,
  onOpenLive,
}: {
  recorder: RecorderStatus;
  shortcutLabel: string | null;
  busy: boolean;
  onStart: () => void;
  onStop: () => void;
  onPause: () => void;
  onResume: () => void;
  onOpenLive: () => void;
}) {
  if (recorder.state === "idle") {
    return (
      <div className="record-bar">
        <button
          type="button"
          className="btn btn-record"
          onClick={onStart}
          disabled={busy}
        >
          <Icon name="record" size={14} />
          {busy ? "Starting…" : "Record meeting"}
        </button>
        <p className="record-bar-hint">
          Records your mic and the call audio.
          {shortcutLabel && (
            <>
              {" "}
              Shortcut <kbd className="kbd">{shortcutLabel}</kbd>
            </>
          )}
        </p>
      </div>
    );
  }
  const paused = recorder.state === "paused";
  return (
    <div className={`record-bar record-bar--live${paused ? " is-paused" : ""}`}>
      <button type="button" className="rec-pill" onClick={onOpenLive}>
        <span className="rec-dot" aria-hidden="true" />
        <span>{paused ? "Paused" : "Recording"}</span>
        <span className="rec-time">{clock(recorder.elapsed_ms)}</span>
      </button>
      <span className="record-bar-title" title={recorder.title ?? ""}>
        {recorder.title}
      </span>
      <div className="record-bar-meters">
        <LevelMeter label="Me" tone="me" value={recorder.me_level} />
        <LevelMeter
          label="Them"
          tone="them"
          value={recorder.them_level}
          disabled={!recorder.system_audio}
        />
      </div>
      <div className="record-bar-actions">
        <button
          type="button"
          className="icon-btn"
          onClick={paused ? onResume : onPause}
          aria-label={paused ? "Resume recording" : "Pause recording"}
          title={paused ? "Resume" : "Pause"}
        >
          <Icon name={paused ? "record" : "pause"} size={16} />
        </button>
        <button
          type="button"
          className="btn btn-stop btn-sm"
          onClick={onStop}
          disabled={busy}
        >
          <Icon name="stop" size={12} />
          {busy ? "Stopping…" : "Stop"}
        </button>
      </div>
    </div>
  );
}
