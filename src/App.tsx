import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { convertFileSrc, invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import {
  IDLE_RECORDER,
  type AudioDeviceInfo,
  type McpInfo,
  type Meeting,
  type MeetingDetail,
  type PermissionsState,
  type RecorderStatus,
  type RecordingSettings,
  type StorageInfo,
  type TranscriptionProgress,
  type VocabularySettings,
} from "./types";
import { loadSavedProfiles, resetCorruptedConfig } from "./lib/configLoad";
import { EMPTY_PROFILES_STATE, type ProfilesState } from "./lib/sttProvider";
import { formatShortcut } from "./lib/shortcut";
import { SidebarNav, type NavScreen } from "./components/SidebarNav";
import { LibraryView } from "./components/LibraryView";
import type { MeetingActions } from "./components/MeetingDetailView";
import { TranscriptionSettingsView } from "./components/TranscriptionSettingsView";
import { CustomVocabularyView } from "./components/CustomVocabularyView";
import { RecordingSettingsView } from "./components/RecordingSettingsView";
import { ClaudeView } from "./components/ClaudeView";
import { PermissionsView } from "./components/PermissionsView";
import { ConfigResetDialog } from "./components/ConfigResetDialog";

const SEARCH_DEBOUNCE_MS = 200;

export default function App() {
  const [screen, setScreen] = useState<NavScreen>("library");
  const [configError, setConfigError] = useState<string | null>(null);
  const [resetOpen, setResetOpen] = useState(false);
  const [resetting, setResetting] = useState(false);
  const [resetError, setResetError] = useState<string | null>(null);

  const [meetings, setMeetings] = useState<Meeting[]>([]);
  const [loadingMeetings, setLoadingMeetings] = useState(true);
  const [query, setQuery] = useState("");
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [detail, setDetail] = useState<MeetingDetail | null>(null);
  const [recorder, setRecorder] = useState<RecorderStatus>(IDLE_RECORDER);
  const [recorderBusy, setRecorderBusy] = useState(false);
  const [recorderError, setRecorderError] = useState<string | null>(null);
  const [progress, setProgress] = useState<
    Record<string, TranscriptionProgress>
  >({});

  const [profiles, setProfiles] = useState<ProfilesState>(EMPTY_PROFILES_STATE);
  const [languages, setLanguages] = useState<string[]>([]);
  const [vocab, setVocab] = useState<VocabularySettings>({
    customVocabulary: [],
  });
  const vocabLoaded = useRef(false);
  const [recordingSettings, setRecordingSettings] =
    useState<RecordingSettings | null>(null);
  const [devices, setDevices] = useState<AudioDeviceInfo[]>([]);
  const [storage, setStorage] = useState<StorageInfo | null>(null);
  const [mcpInfo, setMcpInfo] = useState<McpInfo | null>(null);
  const [permissions, setPermissions] = useState<PermissionsState | null>(null);

  const selectedRef = useRef<string | null>(null);
  selectedRef.current = selectedId;
  const queryRef = useRef(query);
  queryRef.current = query;

  const refreshMeetings = useCallback(async () => {
    try {
      const list = await invoke<Meeting[]>("list_meetings", {
        query: queryRef.current.trim() || null,
      });
      setMeetings(list);
    } finally {
      setLoadingMeetings(false);
    }
  }, []);

  const refreshDetail = useCallback(async (id: string | null) => {
    if (!id) {
      setDetail(null);
      return;
    }
    try {
      const next = await invoke<MeetingDetail>("get_meeting", { id });
      if (selectedRef.current === id) setDetail(next);
    } catch {
      if (selectedRef.current === id) {
        setDetail(null);
        setSelectedId(null);
      }
    }
  }, []);

  const refreshPermissions = useCallback(async () => {
    const next = await invoke<PermissionsState>("get_permissions");
    setPermissions(next);
    return next;
  }, []);

  // Ask for anything macOS hasn't asked about yet, then send the user to
  // Permissions if something is still missing.
  const requestMissingPermissions = useCallback(async () => {
    let current = await refreshPermissions();
    if (current.microphone === "not_determined") {
      await invoke("request_microphone_permission");
      current = await refreshPermissions();
    }
    if (current.system_audio === "not_determined") {
      try {
        await invoke("request_system_audio_permission");
      } finally {
        current = await refreshPermissions();
      }
    }
    const sysOk =
      current.system_audio === "authorized" ||
      current.system_audio === "unsupported";
    if (current.microphone !== "authorized" || !sysOk) {
      setScreen("permissions");
    }
  }, [refreshPermissions]);

  // Initial load.
  useEffect(() => {
    void (async () => {
      const loaded = await loadSavedProfiles();
      if (!loaded.ok) {
        setConfigError(loaded.message);
        return;
      }
      setProfiles(loaded.profiles);
      setLanguages(await invoke<string[]>("get_languages"));
      setVocab({ customVocabulary: await invoke("get_custom_vocabulary") });
      vocabLoaded.current = true;
      setRecordingSettings(
        await invoke<RecordingSettings>("get_recording_settings"),
      );
      setStorage(await invoke<StorageInfo>("get_storage_info"));
      setRecorder(await invoke<RecorderStatus>("get_recorder_status"));
      void refreshMeetings();
      void invoke<McpInfo>("get_mcp_info").then(setMcpInfo);
      void invoke<AudioDeviceInfo[]>("list_audio_device_info").then(setDevices);
      void requestMissingPermissions();
    })();
  }, [refreshMeetings, requestMissingPermissions]);

  // Persist vocabulary edits.
  useEffect(() => {
    if (!vocabLoaded.current) return;
    void invoke("save_custom_vocabulary", {
      vocabulary: vocab.customVocabulary,
    });
  }, [vocab]);

  // Debounced search.
  useEffect(() => {
    const t = window.setTimeout(
      () => void refreshMeetings(),
      SEARCH_DEBOUNCE_MS,
    );
    return () => window.clearTimeout(t);
  }, [query, refreshMeetings]);

  useEffect(() => {
    void refreshDetail(selectedId);
  }, [selectedId, refreshDetail]);

  // Backend events.
  useEffect(() => {
    const unlisten = [
      listen<RecorderStatus>("recorder-status", (e) => setRecorder(e.payload)),
      listen<string | null>("meetings-changed", (e) => {
        void refreshMeetings();
        if (!e.payload || e.payload === selectedRef.current) {
          void refreshDetail(selectedRef.current);
        }
      }),
      listen<TranscriptionProgress>("transcription-progress", (e) =>
        setProgress((p) => ({ ...p, [e.payload.meeting_id]: e.payload })),
      ),
      listen<string>("recorder-error", (e) => setRecorderError(e.payload)),
      listen<ProfilesState>("profiles-changed", (e) => setProfiles(e.payload)),
      listen<string>("open-meeting", (e) => {
        setScreen("library");
        setSelectedId(e.payload);
      }),
    ];
    return () => {
      unlisten.forEach((p) => void p.then((f) => f()));
    };
  }, [refreshMeetings, refreshDetail]);

  const recorderAction = async (command: string) => {
    setRecorderBusy(true);
    setRecorderError(null);
    try {
      const status = await invoke<RecorderStatus>(command);
      setRecorder(status);
      if (command === "start_recording" && status.meeting_id) {
        setSelectedId(status.meeting_id);
      }
    } catch (e) {
      setRecorderError(String(e));
    } finally {
      setRecorderBusy(false);
    }
  };

  const actions: MeetingActions = useMemo(
    () => ({
      onRename: async (id, title) => {
        await invoke("rename_meeting", { id, title });
      },
      onDelete: async (id) => {
        await invoke("delete_meeting", { id });
        setSelectedId(null);
      },
      onRetranscribe: async (id, profileId) => {
        await invoke("retranscribe_meeting", { id, profileId });
      },
      onReveal: (id) => void invoke("reveal_meeting", { id }),
      getMarkdown: (id) => invoke<string>("get_meeting_markdown", { id }),
      audioSrc: (path) => convertFileSrc(path),
      onAddMarker: async (meetingId, kind, atMs, text) => {
        await invoke("add_marker", { meetingId, kind, atMs, text });
      },
      onUpdateMarker: async (id, text) => {
        await invoke("update_marker", { id, text });
      },
      onDeleteMarker: async (id) => {
        await invoke("delete_marker", { id });
      },
    }),
    [],
  );

  if (configError) {
    return (
      <div className="layout layout-gate">
        <main className="main-content">
          <div className="content">
            <div className="setup-step setup-step-center config-error-shell">
              <h2 className="setup-title">Settings couldn’t be read</h2>
              <p className="setup-desc">{configError}</p>
              <button
                className="btn btn-ghost config-reset-trigger"
                onClick={() => setResetOpen(true)}
              >
                Reset settings…
              </button>
            </div>
          </div>
        </main>
        <ConfigResetDialog
          open={resetOpen}
          isResetting={resetting}
          error={resetError}
          onCancel={() => setResetOpen(false)}
          onConfirm={async () => {
            setResetting(true);
            const result = await resetCorruptedConfig();
            setResetting(false);
            if (result.ok) window.location.reload();
            else setResetError(result.message);
          }}
        />
      </div>
    );
  }

  const shortcutLabel =
    recordingSettings?.shortcut_enabled && recordingSettings.shortcut
      ? formatShortcut(recordingSettings.shortcut)
      : null;

  return (
    <div className="layout">
      <SidebarNav
        activeScreen={screen}
        onNavigate={setScreen}
        footer={
          recorder.state !== "idle" ? (
            <button
              type="button"
              className="sidebar-live"
              onClick={() => {
                setScreen("library");
                setSelectedId(recorder.meeting_id);
              }}
            >
              <span className="rec-dot" aria-hidden="true" />
              {recorder.state === "paused" ? "Paused" : "Recording"}
            </button>
          ) : null
        }
      />
      <main className="main-content">
        {recorderError && (
          <div className="toast toast--danger" role="alert">
            <span>{recorderError}</span>
            <button
              className="toast-close"
              aria-label="Dismiss"
              onClick={() => setRecorderError(null)}
            >
              ×
            </button>
          </div>
        )}
        <div
          className={`content${screen === "library" ? " content--full" : ""}`}
        >
          {screen === "library" && (
            <LibraryView
              meetings={meetings}
              loading={loadingMeetings}
              query={query}
              onQueryChange={setQuery}
              selectedId={selectedId}
              onSelect={setSelectedId}
              detail={detail}
              recorder={recorder}
              recorderBusy={recorderBusy}
              shortcutLabel={shortcutLabel}
              progress={progress}
              profiles={profiles}
              actions={actions}
              onStart={() => void recorderAction("start_recording")}
              onStop={() => void recorderAction("stop_recording")}
              onPause={() => void recorderAction("pause_recording")}
              onResume={() => void recorderAction("resume_recording")}
            />
          )}
          {screen === "transcription" && (
            <TranscriptionSettingsView
              languages={languages}
              onLanguagesChange={(next) => {
                setLanguages(next);
                void invoke("save_languages", { languages: next });
              }}
              profilesState={profiles}
              onProfilesChange={setProfiles}
            />
          )}
          {screen === "vocabulary" && (
            <CustomVocabularyView settings={vocab} setSettings={setVocab} />
          )}
          {screen === "settings" && recordingSettings && (
            <RecordingSettingsView
              settings={recordingSettings}
              devices={devices}
              storage={storage}
              onSave={async (next) => {
                await invoke("save_recording_settings", { settings: next });
                setRecordingSettings(next);
              }}
              onChooseStorage={() =>
                invoke<string | null>("choose_storage_dir")
              }
              onMoveStorage={async (dir) => {
                setStorage(await invoke<StorageInfo>("move_storage", { dir }));
                setMcpInfo(await invoke<McpInfo>("get_mcp_info"));
              }}
            />
          )}
          {screen === "claude" && <ClaudeView info={mcpInfo} />}
          {screen === "permissions" && (
            <PermissionsView
              permissions={permissions}
              onGrantMicrophone={async () => {
                await invoke("request_microphone_permission");
                await refreshPermissions();
              }}
              onRequestSystemAudio={async () => {
                try {
                  await invoke("request_system_audio_permission");
                } finally {
                  await refreshPermissions();
                }
              }}
              onOpenSettings={(panel) =>
                void invoke("open_system_settings", { panel })
              }
              onRecheck={refreshPermissions}
            />
          )}
        </div>
      </main>
    </div>
  );
}
