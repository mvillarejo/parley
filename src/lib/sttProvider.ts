export type SttProvider = "gladia" | "openai_compat" | "whisper_local";

export const DEFAULT_STT_BASE_URL = "http://localhost:11434/v1";
export const DEFAULT_STT_MODEL = "gemma4:e4b";
export const DEFAULT_LOCAL_PROFILE_NAME = "Local Ollama";
export const DEFAULT_GLADIA_PROFILE_NAME = "Gladia";
export const DEFAULT_WHISPER_PROFILE_NAME = "Local Whisper";
/** Shown as a hint only: the form cannot expand `~`, the user pastes the full path. */
export const DEFAULT_WHISPER_MODEL_PATH_HINT =
  "~/Library/Application Support/whisper.cpp/models/ggml-large-v3-turbo.bin";
const MAX_PROFILE_NAME_CHARS = 60;

export interface GladiaProfileSettings {
  kind: "gladia";
  api_key: string;
  code_switching: boolean;
}

export interface OpenAiCompatProfileSettings {
  kind: "openai_compat";
  base_url: string;
  model: string;
  api_key: string;
}

/** whisper.cpp run locally; the backend finds the `whisper-server` binary. */
export interface WhisperLocalProfileSettings {
  kind: "whisper_local";
  model_path: string;
}

export type ProfileSettings =
  | GladiaProfileSettings
  | OpenAiCompatProfileSettings
  | WhisperLocalProfileSettings;

/** Shape exchanged with the profile commands (snake_case, as on disk). */
export type Profile = { id: string; name: string } & ProfileSettings;
export type NewProfile = { name: string } & ProfileSettings;

export interface ProfilesState {
  profiles: Profile[];
  active_profile_id: string;
}

export const EMPTY_PROFILES_STATE: ProfilesState = {
  profiles: [],
  active_profile_id: "",
};

export const PROFILE_KIND_LABELS: Record<SttProvider, string> = {
  gladia: "Gladia",
  openai_compat: "OpenAI-compatible",
  whisper_local: "Local Whisper",
};

export function getActiveProfile(state: ProfilesState): Profile | null {
  return (
    state.profiles.find((profile) => profile.id === state.active_profile_id) ??
    null
  );
}

export function defaultProfileSettings(kind: SttProvider): ProfileSettings {
  switch (kind) {
    case "gladia":
      return { kind, api_key: "", code_switching: false };
    case "whisper_local":
      return { kind, model_path: "" };
    case "openai_compat":
      return {
        kind,
        base_url: DEFAULT_STT_BASE_URL,
        model: DEFAULT_STT_MODEL,
        api_key: "",
      };
  }
}

/** `base`, or `base 2`, `base 3`… — the first name no profile uses yet. */
export function uniqueProfileName(base: string, profiles: Profile[]): string {
  const taken = new Set(profiles.map((p) => p.name.trim().toLowerCase()));
  for (let n = 1; ; n++) {
    const candidate = n === 1 ? base : `${base} ${n}`;
    if (!taken.has(candidate.toLowerCase())) return candidate;
  }
}

/** A Gladia profile needs a saved API key; the other kinds do not. */
export function canTranscribe(profile: Profile | null): boolean {
  if (!profile) return false;
  return profile.kind !== "gladia" || profile.api_key.trim().length > 0;
}

/** Returns an error message, or null when the base URL is usable. */
export function validateSttBaseUrl(value: string): string | null {
  const trimmed = value.trim();
  if (!trimmed) return "Base URL is required";
  let url: URL;
  try {
    url = new URL(trimmed);
  } catch {
    return "Enter a full URL, e.g. http://localhost:11434/v1";
  }
  if (url.protocol !== "http:" && url.protocol !== "https:") {
    return "Base URL must start with http:// or https://";
  }
  return null;
}

export interface ProfileDraftErrors {
  name?: string;
  base_url?: string;
  model?: string;
  model_path?: string;
}

/** Mirrors the backend rules so the form can explain them before saving. */
export function validateProfileDraft(
  draft: NewProfile,
  profiles: Profile[],
  editingId: string | null,
): ProfileDraftErrors {
  const errors: ProfileDraftErrors = {};
  const name = draft.name.trim();
  if (!name) {
    errors.name = "Name is required";
  } else if (name.length > MAX_PROFILE_NAME_CHARS) {
    errors.name = `Use at most ${MAX_PROFILE_NAME_CHARS} characters`;
  } else if (
    profiles.some(
      (p) =>
        p.id !== editingId &&
        p.name.trim().toLowerCase() === name.toLowerCase(),
    )
  ) {
    errors.name = "Another profile already uses this name";
  }
  if (draft.kind === "openai_compat") {
    const urlError = validateSttBaseUrl(draft.base_url);
    if (urlError) errors.base_url = urlError;
    if (!draft.model.trim()) errors.model = "Model is required";
  }
  if (draft.kind === "whisper_local") {
    const modelPath = draft.model_path.trim();
    if (!modelPath) {
      errors.model_path = "Model file is required";
    } else if (!modelPath.startsWith("/")) {
      errors.model_path = "Use the full path to the model file";
    }
  }
  return errors;
}

export function hasDraftErrors(errors: ProfileDraftErrors): boolean {
  return Object.keys(errors).length > 0;
}
