export type AudioDeviceSelection =
  | { mode: "automatic" }
  | { mode: "system_default" }
  | { mode: "specific"; device_id: string; device_name: string };

export interface AudioDeviceInfo {
  id: string;
  name: string;
  transport: string;
  isDefault: boolean;
  isBuiltIn: boolean;
}

export interface CustomVocabEntry {
  value: string;
  pronunciations?: string[];
  language?: string;
  intensity?: number;
}

export interface VocabularySettings {
  customVocabulary: CustomVocabEntry[];
}

export type MeetingStatus = "recording" | "transcribing" | "ready" | "failed";

/** Mirrors `parley_lib::store::Meeting`. */
export interface Meeting {
  id: string;
  title: string;
  started_at: string;
  ended_at: string | null;
  duration_ms: number;
  status: MeetingStatus;
  error: string | null;
  capture_warning: string | null;
  profile_name: string | null;
  provider: string | null;
  language: string | null;
  folder: string;
  speakers: string[];
  segment_count: number;
  updated_at: string;
}

export interface Segment {
  start_ms: number;
  end_ms: number;
  speaker: string;
  text: string;
}

export type MarkerKind = "highlight" | "note";

/** Mirrors `parley_lib::store::Marker`. `at_ms` is on the recording clock,
 * like `Segment.start_ms`. */
export interface Marker {
  id: number;
  meeting_id: string;
  at_ms: number;
  kind: MarkerKind;
  text: string | null;
  created_at: string;
}

export interface MeetingFiles {
  folder: string;
  mic: string | null;
  system: string | null;
  mixed: string | null;
  transcript: string | null;
}

export interface MeetingDetail {
  meeting: Meeting;
  segments: Segment[];
  markers: Marker[];
  files: MeetingFiles;
}

export type RecorderState = "idle" | "recording" | "paused";

export interface RecorderStatus {
  state: RecorderState;
  meeting_id: string | null;
  title: string | null;
  elapsed_ms: number;
  me_level: number;
  them_level: number;
  system_audio: boolean;
  warning: string | null;
  marker_count: number;
}

export const IDLE_RECORDER: RecorderStatus = {
  state: "idle",
  meeting_id: null,
  title: null,
  elapsed_ms: 0,
  me_level: 0,
  them_level: 0,
  system_audio: false,
  warning: null,
  marker_count: 0,
};

export interface TranscriptionProgress {
  meeting_id: string;
  track: "me" | "them";
  done: number;
  total: number;
}

export interface RecordingSettings {
  audio_device_selection: AudioDeviceSelection;
  capture_system_audio: boolean;
  shortcut: string;
  shortcut_enabled: boolean;
  mark_shortcut: string;
  mark_shortcut_enabled: boolean;
  note_shortcut: string;
  note_shortcut_enabled: boolean;
}

export interface StorageInfo {
  data_dir: string;
  db_path: string;
  default_dir: string;
  is_default: boolean;
}

export interface McpInfo {
  binary_path: string;
  binary_exists: boolean;
  db_path: string;
  claude_code_command: string;
  desktop_config: string;
}

export type SystemAudioPermission =
  "authorized" | "denied" | "not_determined" | "unknown" | "unsupported";

export interface PermissionsState {
  microphone: string;
  system_audio: SystemAudioPermission;
}
