import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import {
  IDLE_RECORDER,
  type Marker,
  type MeetingDetail,
  type RecorderStatus,
  type RecordingSettings,
  type TranscriptionProgress,
} from "../types";
import { formatShortcut } from "../lib/shortcut";
import { progressLabel } from "../lib/meetings";
import {
  editNote,
  markerFlash,
  noteToSave,
  openNote,
  widgetInputMode,
  type LiveMoment,
  type NoteDraft,
} from "./noteDraft";
import { Widget, type WidgetProps } from "./Widget";

const FLASH_MS = { ok: 2200, warn: 4500 };

type LastMeeting = {
  id: string;
  status: "transcribing" | "ready" | "failed";
  progress?: TranscriptionProgress;
  error?: string | null;
};

/** Status line for the meeting that was just recorded. */
export function lastMeetingLine(last: LastMeeting | null): string | null {
  if (!last) return null;
  if (last.status === "transcribing") return progressLabel(last.progress);
  if (last.status === "ready") return "Transcript ready.";
  return `Transcription failed${last.error ? `: ${last.error}` : "."}`;
}

export function WidgetApp() {
  const [recorder, setRecorder] = useState<RecorderStatus>(IDLE_RECORDER);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [last, setLast] = useState<LastMeeting | null>(null);
  const [shortcut, setShortcut] = useState<string | null>(null);
  const [markKeys, setMarkKeys] = useState<string | null>(null);
  const [noteKeys, setNoteKeys] = useState<string | null>(null);
  const [note, setNote] = useState<NoteDraft | null>(null);
  const [titleFocused, setTitleFocused] = useState(false);
  const [flash, setFlash] = useState<WidgetProps["flash"]>(null);
  const lastRef = useRef<LastMeeting | null>(null);
  lastRef.current = last;
  const liveId = useRef<string | null>(null);

  useEffect(() => {
    void invoke<RecorderStatus>("get_recorder_status").then(setRecorder);
    void invoke<RecordingSettings>("get_recording_settings").then((s) => {
      setShortcut(s.shortcut_enabled ? formatShortcut(s.shortcut) : null);
      setMarkKeys(
        s.mark_shortcut_enabled ? formatShortcut(s.mark_shortcut) : null,
      );
      setNoteKeys(
        s.note_shortcut_enabled ? formatShortcut(s.note_shortcut) : null,
      );
    });
    const subs = [
      listen<RecorderStatus>("recorder-status", (e) => {
        const status = e.payload;
        if (status.meeting_id) liveId.current = status.meeting_id;
        if (status.state === "idle" && liveId.current) {
          setLast({ id: liveId.current, status: "transcribing" });
          liveId.current = null;
        }
        setRecorder(status);
      }),
      listen<TranscriptionProgress>("transcription-progress", (e) => {
        setLast((l) =>
          l && l.id === e.payload.meeting_id
            ? { ...l, progress: e.payload }
            : l,
        );
      }),
      listen<string | null>("meetings-changed", (e) => {
        const current = lastRef.current;
        if (!current || (e.payload && e.payload !== current.id)) return;
        void invoke<MeetingDetail>("get_meeting", { id: current.id })
          .then((d) => {
            const status = d.meeting.status;
            if (
              status === "ready" ||
              status === "failed" ||
              status === "transcribing"
            ) {
              setLast((l) =>
                l && l.id === current.id
                  ? { ...l, status, error: d.meeting.error }
                  : l,
              );
            }
          })
          .catch(() => setLast(null));
      }),
      listen<string>("recorder-error", (e) => setError(e.payload)),
      // Saved from the widget or with the Mark shortcut.
      listen<Marker>("marker-added", (e) =>
        setFlash({ text: markerFlash(e.payload), tone: "ok" }),
      ),
      // The note shortcut: the backend captured the moment when it was pressed.
      listen<LiveMoment>("widget-note", (e) =>
        setNote((n) => openNote(n, e.payload)),
      ),
    ];
    return () => subs.forEach((p) => void p.then((f) => f()));
  }, []);

  useEffect(() => {
    if (!flash) return;
    const t = window.setTimeout(() => setFlash(null), FLASH_MS[flash.tone]);
    return () => window.clearTimeout(t);
  }, [flash]);

  // Take the keyboard only while typing; the call app keeps it otherwise.
  const inputMode = widgetInputMode(note !== null, titleFocused);
  useEffect(() => {
    void invoke("set_widget_input", { mode: inputMode }).catch(() => {});
  }, [inputMode]);

  const warn = (e: unknown) => setFlash({ text: String(e), tone: "warn" });

  const saveNote = async () => {
    if (!note) return;
    const args = noteToSave(note);
    if (!args) {
      setNote(null);
      return;
    }
    try {
      await invoke<Marker>("add_marker", args);
      setNote(null);
    } catch (e) {
      warn(e);
    }
  };

  const run = async (command: string, args?: Record<string, unknown>) => {
    setBusy(true);
    setError(null);
    try {
      const status = await invoke<RecorderStatus>(command, args);
      setRecorder(status);
      if (command === "start_recording") setLast(null);
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <Widget
      recorder={recorder}
      statusLine={error ?? lastMeetingLine(last)}
      statusAction={
        !error && last?.status === "ready"
          ? {
              label: "Open",
              onClick: () =>
                void invoke("open_library", { meetingId: last.id }),
            }
          : null
      }
      shortcutLabel={shortcut}
      markShortcutLabel={markKeys}
      noteShortcutLabel={noteKeys}
      flash={flash}
      note={note}
      busy={busy}
      onStart={(title) =>
        void run("start_recording", { title: title.trim() || null })
      }
      onStop={() => void run("stop_recording")}
      onPause={() => void run("pause_recording")}
      onResume={() => void run("resume_recording")}
      onRename={(title) => {
        if (recorder.meeting_id) {
          void invoke("rename_meeting", {
            id: recorder.meeting_id,
            title,
          }).catch((e) => setError(String(e)));
        }
      }}
      onOpenLibrary={() =>
        void invoke("open_library", {
          meetingId: recorder.meeting_id ?? last?.id ?? null,
        })
      }
      onHide={() => void invoke("hide_recorder_widget")}
      onMark={() => void invoke<Marker>("mark_moment").catch(warn)}
      onNoteOpen={() => {
        // The button closes a blank note; Widget refocuses one with text.
        if (note) {
          setNote(null);
          return;
        }
        // The note's moment is now, when the field opens.
        void invoke<LiveMoment>("begin_note")
          .then((moment) => setNote((n) => openNote(n, moment)))
          .catch(warn);
      }}
      onNoteChange={(text) => setNote((n) => (n ? editNote(n, text) : n))}
      onNoteSave={() => void saveNote()}
      onNoteCancel={() => setNote(null)}
      onTitleFocus={setTitleFocused}
    />
  );
}
