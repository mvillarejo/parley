import { useEffect, useRef, useState, type RefObject } from "react";
import { clock } from "../lib/format";
import { Icon } from "./Icon";

export type TrackChoice = "mixed" | "mic" | "system";

const TRACK_LABELS: Record<TrackChoice, string> = {
  mixed: "Both",
  mic: "Me",
  system: "Them",
};

export function AudioPlayer({
  sources,
  audioRef,
  onTime,
}: {
  /** URL per available track. */
  sources: Partial<Record<TrackChoice, string>>;
  audioRef: RefObject<HTMLAudioElement | null>;
  onTime: (ms: number) => void;
}) {
  const available = (Object.keys(TRACK_LABELS) as TrackChoice[]).filter(
    (t) => sources[t],
  );
  const [track, setTrack] = useState<TrackChoice>(available[0] ?? "mixed");
  const [playing, setPlaying] = useState(false);
  const [time, setTime] = useState(0);
  const [duration, setDuration] = useState(0);
  const resumeAt = useRef<number | null>(null);

  useEffect(() => {
    if (!sources[track] && available[0]) setTrack(available[0]);
  }, [sources, track, available]);

  const src = sources[track];

  const switchTrack = (next: TrackChoice) => {
    const audio = audioRef.current;
    resumeAt.current = audio ? audio.currentTime : null;
    setTrack(next);
  };

  if (!src) {
    return (
      <div className="player player--empty">
        <Icon name="warning" size={16} />
        <span>No audio files for this meeting.</span>
      </div>
    );
  }

  return (
    <div className="player">
      <audio
        ref={audioRef}
        src={src}
        preload="metadata"
        onLoadedMetadata={(e) => {
          setDuration(e.currentTarget.duration || 0);
          if (resumeAt.current !== null) {
            e.currentTarget.currentTime = resumeAt.current;
            if (playing) void e.currentTarget.play();
            resumeAt.current = null;
          }
        }}
        onTimeUpdate={(e) => {
          setTime(e.currentTarget.currentTime);
          onTime(e.currentTarget.currentTime * 1000);
        }}
        onPlay={() => setPlaying(true)}
        onPause={() => setPlaying(false)}
        onEnded={() => setPlaying(false)}
      />
      <button
        type="button"
        className="player-play"
        aria-label={playing ? "Pause" : "Play"}
        onClick={() => {
          const audio = audioRef.current;
          if (!audio) return;
          if (audio.paused) void audio.play();
          else audio.pause();
        }}
      >
        <Icon name={playing ? "pause" : "play"} size={16} />
      </button>
      <span className="player-time">{clock(time * 1000)}</span>
      <input
        type="range"
        className="player-scrub"
        aria-label="Playback position"
        min={0}
        max={duration || 0}
        step={0.1}
        value={Math.min(time, duration || 0)}
        onChange={(e) => {
          const audio = audioRef.current;
          if (audio) audio.currentTime = Number(e.target.value);
        }}
        style={{
          ["--progress" as string]: `${duration ? (time / duration) * 100 : 0}%`,
        }}
      />
      <span className="player-time player-time--total">
        {clock(duration * 1000)}
      </span>
      {available.length > 1 && (
        <div className="segmented" role="radiogroup" aria-label="Audio track">
          {available.map((t) => (
            <button
              key={t}
              type="button"
              role="radio"
              aria-checked={track === t}
              className={`segmented-item${track === t ? " is-active" : ""}`}
              onClick={() => switchTrack(t)}
            >
              {TRACK_LABELS[t]}
            </button>
          ))}
        </div>
      )}
    </div>
  );
}
