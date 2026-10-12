// File-based settings (`config.json`): transcription profiles, languages,
// vocabulary, microphone, storage location and the global shortcuts.
use crate::audio::AudioDeviceSelection;
use crate::vocabulary::{default_custom_vocabulary, CustomVocabEntry};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

static CONFIG_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

fn config_mutex() -> &'static Mutex<()> {
    CONFIG_LOCK.get_or_init(|| Mutex::new(()))
}

/// Defaults for the local Ollama profile a fresh install starts with.
pub const DEFAULT_STT_BASE_URL: &str = "http://localhost:11434/v1";
pub const DEFAULT_STT_MODEL: &str = "gemma4:e4b";
pub const DEFAULT_SHORTCUT: &str = "CommandOrControl+Shift+Alt+R";
pub const DEFAULT_MARK_SHORTCUT: &str = "CommandOrControl+Shift+Alt+M";
pub const DEFAULT_NOTE_SHORTCUT: &str = "CommandOrControl+Shift+Alt+N";

#[derive(serde::Serialize, serde::Deserialize, Clone, Default)]
#[serde(default)]
struct Config {
    languages: Option<Vec<String>>,
    custom_vocabulary: Option<Vec<CustomVocabEntry>>,
    audio_device_selection: Option<AudioDeviceSelection>,
    /// Whether to record the call/system audio track as well as the mic.
    capture_system_audio: Option<bool>,
    /// Data directory (database + meeting folders). Absent = default.
    #[serde(skip_serializing_if = "Option::is_none")]
    storage_dir: Option<String>,
    /// Global shortcut that starts/stops a recording.
    shortcut: Option<String>,
    shortcut_enabled: Option<bool>,
    /// Global shortcut that marks a highlight (registered only while recording).
    mark_shortcut: Option<String>,
    mark_shortcut_enabled: Option<bool>,
    /// Global shortcut that opens the recorder's note field (while recording).
    note_shortcut: Option<String>,
    note_shortcut_enabled: Option<bool>,
    /// Saved transcription setups; exactly one is active.
    profiles: Option<Vec<Profile>>,
    active_profile_id: Option<String>,
}

impl Config {
    /// Replace `None` with defaults for fields that should never be null on disk.
    fn fill_defaults(&mut self) {
        self.languages.get_or_insert_with(Vec::new);
        self.custom_vocabulary
            .get_or_insert_with(default_custom_vocabulary);
        self.audio_device_selection
            .get_or_insert(AudioDeviceSelection::Automatic);
        self.capture_system_audio.get_or_insert(true);
        self.shortcut
            .get_or_insert_with(|| DEFAULT_SHORTCUT.to_string());
        self.shortcut_enabled.get_or_insert(true);
        self.mark_shortcut
            .get_or_insert_with(|| DEFAULT_MARK_SHORTCUT.to_string());
        self.mark_shortcut_enabled.get_or_insert(true);
        self.note_shortcut
            .get_or_insert_with(|| DEFAULT_NOTE_SHORTCUT.to_string());
        self.note_shortcut_enabled.get_or_insert(true);
        if self
            .storage_dir
            .as_deref()
            .is_some_and(|d| d.trim().is_empty())
        {
            self.storage_dir = None;
        }
        if self.profiles.as_ref().is_none_or(|p| p.is_empty()) {
            self.profiles = Some(vec![Profile {
                id: DEFAULT_LOCAL_PROFILE_ID.to_string(),
                name: DEFAULT_LOCAL_PROFILE_NAME.to_string(),
                settings: ProfileSettings::OpenAiCompat {
                    base_url: DEFAULT_STT_BASE_URL.to_string(),
                    model: DEFAULT_STT_MODEL.to_string(),
                    api_key: String::new(),
                },
            }]);
        }
        // Repair a hand-edited active id rather than bricking transcription.
        let profiles = self.profiles.as_ref().expect("profiles set above");
        let active_exists = self
            .active_profile_id
            .as_ref()
            .is_some_and(|id| profiles.iter().any(|p| &p.id == id));
        if !active_exists {
            self.active_profile_id = Some(profiles[0].id.clone());
        }
    }

    fn profiles(&self) -> &[Profile] {
        self.profiles.as_deref().unwrap_or_default()
    }

    fn profiles_mut(&mut self) -> &mut Vec<Profile> {
        self.profiles.get_or_insert_with(Vec::new)
    }

    fn profiles_state(&self) -> ProfilesState {
        ProfilesState {
            profiles: self.profiles().to_vec(),
            active_profile_id: self.active_profile_id.clone().unwrap_or_default(),
        }
    }

    fn active_profile(&self) -> Result<Profile, String> {
        let id = self.active_profile_id.as_deref().unwrap_or_default();
        self.profiles()
            .iter()
            .find(|p| p.id == id)
            .cloned()
            .ok_or_else(|| "No active transcription profile".to_string())
    }
}

pub fn get_config_path() -> PathBuf {
    parley_lib::paths::config_path()
}

fn with_config(f: impl FnOnce(&mut Config)) -> Result<(), String> {
    let _guard = config_mutex().lock().unwrap();
    with_config_at(&get_config_path(), f)
}

fn with_config_at(path: &Path, f: impl FnOnce(&mut Config)) -> Result<(), String> {
    let mut config = load_config_from_path(path)?;
    f(&mut config);
    write_config_to_path(path, &config)
}

fn read_config<T>(f: impl FnOnce(&Config) -> T) -> Result<T, String> {
    let _guard = config_mutex().lock().unwrap();
    Ok(f(&load_config_from_path(&get_config_path())?))
}

fn load_config_from_path(path: &Path) -> Result<Config, String> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let mut config = Config::default();
            config.fill_defaults();
            return Ok(config);
        }
        Err(error) => {
            let message = format!("Failed to read config at {}: {error}", path.display());
            log::error!("[config] {message}");
            return Err(message);
        }
    };
    let mut config: Config = serde_json::from_str(&text).map_err(|error| {
        let message = format!("Failed to parse config at {}: {error}", path.display());
        log::error!("[config] {message}");
        message
    })?;
    config.fill_defaults();
    Ok(config)
}

fn write_config_to_path(path: &Path, config: &Config) -> Result<(), String> {
    let mut config = config.clone();
    config.fill_defaults();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let json = serde_json::to_string_pretty(&config).map_err(|e| e.to_string())?;
    let temp_path = path.with_extension("tmp");
    fs::write(&temp_path, json).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&temp_path, std::fs::Permissions::from_mode(0o600)).ok();
    }
    fs::rename(&temp_path, path).map_err(|e| e.to_string())?;
    Ok(())
}

// --- Recording settings ---

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct RecordingSettings {
    pub audio_device_selection: AudioDeviceSelection,
    pub capture_system_audio: bool,
    pub shortcut: String,
    pub shortcut_enabled: bool,
    #[serde(default = "default_mark_shortcut")]
    pub mark_shortcut: String,
    #[serde(default = "default_true")]
    pub mark_shortcut_enabled: bool,
    #[serde(default = "default_note_shortcut")]
    pub note_shortcut: String,
    #[serde(default = "default_true")]
    pub note_shortcut_enabled: bool,
}

fn default_mark_shortcut() -> String {
    DEFAULT_MARK_SHORTCUT.to_string()
}

fn default_note_shortcut() -> String {
    DEFAULT_NOTE_SHORTCUT.to_string()
}

fn default_true() -> bool {
    true
}

/// What a global shortcut does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShortcutAction {
    /// Start or stop a recording.
    Record,
    /// Mark a highlight at the current moment (only while recording).
    Mark,
    /// Open the recorder's note field (only while recording).
    Note,
}

impl ShortcutAction {
    pub fn only_while_recording(self) -> bool {
        self != ShortcutAction::Record
    }
}

impl RecordingSettings {
    /// The enabled shortcuts, trimmed.
    pub fn enabled_shortcuts(&self) -> Vec<(ShortcutAction, &str)> {
        [
            (
                ShortcutAction::Record,
                self.shortcut.as_str(),
                self.shortcut_enabled,
            ),
            (
                ShortcutAction::Mark,
                self.mark_shortcut.as_str(),
                self.mark_shortcut_enabled,
            ),
            (
                ShortcutAction::Note,
                self.note_shortcut.as_str(),
                self.note_shortcut_enabled,
            ),
        ]
        .into_iter()
        .filter(|(_, _, on)| *on)
        .map(|(what, s, _)| (what, s.trim()))
        .collect()
    }

    /// Shortcuts can't be empty, and two enabled ones can't be the same keys.
    pub fn validate(&self) -> Result<(), String> {
        for (label, s) in [
            ("Shortcut", &self.shortcut),
            ("Mark shortcut", &self.mark_shortcut),
            ("Note shortcut", &self.note_shortcut),
        ] {
            if s.trim().is_empty() {
                return Err(format!("{label} can't be empty"));
            }
        }
        let enabled = self.enabled_shortcuts();
        for (i, (_, a)) in enabled.iter().enumerate() {
            if enabled[i + 1..]
                .iter()
                .any(|(_, b)| a.eq_ignore_ascii_case(b))
            {
                return Err(format!("{a} is used for two shortcuts"));
            }
        }
        Ok(())
    }
}

pub fn recording_settings() -> Result<RecordingSettings, String> {
    read_config(|c| RecordingSettings {
        audio_device_selection: c
            .audio_device_selection
            .clone()
            .unwrap_or(AudioDeviceSelection::Automatic),
        capture_system_audio: c.capture_system_audio.unwrap_or(true),
        shortcut: c
            .shortcut
            .clone()
            .unwrap_or_else(|| DEFAULT_SHORTCUT.to_string()),
        shortcut_enabled: c.shortcut_enabled.unwrap_or(true),
        mark_shortcut: c
            .mark_shortcut
            .clone()
            .unwrap_or_else(default_mark_shortcut),
        mark_shortcut_enabled: c.mark_shortcut_enabled.unwrap_or(true),
        note_shortcut: c
            .note_shortcut
            .clone()
            .unwrap_or_else(default_note_shortcut),
        note_shortcut_enabled: c.note_shortcut_enabled.unwrap_or(true),
    })
}

pub fn save_recording_settings(settings: RecordingSettings) -> Result<(), String> {
    settings.validate()?;
    with_config(|c| {
        c.audio_device_selection = Some(settings.audio_device_selection);
        c.capture_system_audio = Some(settings.capture_system_audio);
        c.shortcut = Some(settings.shortcut.trim().to_string());
        c.shortcut_enabled = Some(settings.shortcut_enabled);
        c.mark_shortcut = Some(settings.mark_shortcut.trim().to_string());
        c.mark_shortcut_enabled = Some(settings.mark_shortcut_enabled);
        c.note_shortcut = Some(settings.note_shortcut.trim().to_string());
        c.note_shortcut_enabled = Some(settings.note_shortcut_enabled);
    })
}

pub fn languages() -> Result<Vec<String>, String> {
    read_config(|c| c.languages.clone().unwrap_or_default())
}

pub fn save_languages(languages: Vec<String>) -> Result<(), String> {
    let languages = languages
        .into_iter()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect();
    with_config(|c| c.languages = Some(languages))
}

pub fn set_storage_dir(dir: Option<String>) -> Result<(), String> {
    with_config(|c| c.storage_dir = dir.filter(|d| !d.trim().is_empty()))
}

// --- Transcription profiles ---

const MAX_PROFILE_NAME_CHARS: usize = 60;
pub const DEFAULT_LOCAL_PROFILE_ID: &str = "local-ollama";
pub const DEFAULT_LOCAL_PROFILE_NAME: &str = "Local Ollama";

/// Kind-specific part of a profile, tagged with `kind` = `"gladia"` or
/// `"openai_compat"`. Fields are typed and required, so explicit nulls and
/// wrong types are rejected at the IPC boundary.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind")]
pub enum ProfileSettings {
    /// Gladia pre-recorded API (upload, then poll). Diarizes the call track.
    #[serde(rename = "gladia")]
    Gladia {
        api_key: String,
        /// Detect the language per utterance (multilingual meetings).
        code_switching: bool,
    },
    #[serde(rename = "openai_compat")]
    OpenAiCompat {
        base_url: String,
        model: String,
        /// Optional bearer key (unused by local Ollama).
        #[serde(default)]
        api_key: String,
    },
}

impl ProfileSettings {
    pub fn kind(&self) -> &'static str {
        match self {
            ProfileSettings::Gladia { .. } => "gladia",
            ProfileSettings::OpenAiCompat { .. } => "openai_compat",
        }
    }

    pub fn batch_settings(&self) -> Option<crate::batch_stt::BatchSettings> {
        match self {
            ProfileSettings::OpenAiCompat {
                base_url,
                model,
                api_key,
            } => Some(crate::batch_stt::BatchSettings {
                base_url: base_url.clone(),
                model: model.clone(),
                api_key: Some(api_key.clone()).filter(|key| !key.trim().is_empty()),
            }),
            ProfileSettings::Gladia { .. } => None,
        }
    }

    /// Trim and validate before anything reaches disk.
    fn normalized(self) -> Result<Self, String> {
        match self {
            ProfileSettings::Gladia {
                api_key,
                code_switching,
            } => Ok(ProfileSettings::Gladia {
                api_key: api_key.trim().to_string(),
                code_switching,
            }),
            ProfileSettings::OpenAiCompat {
                base_url,
                model,
                api_key,
            } => {
                let base_url = base_url.trim().trim_end_matches('/').to_string();
                let parsed = url::Url::parse(&base_url)
                    .map_err(|_| format!("Base URL is not a valid URL: {base_url}"))?;
                if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
                    return Err("Base URL must start with http:// or https://".to_string());
                }
                let model = model.trim().to_string();
                if model.is_empty() {
                    return Err("Model is required".to_string());
                }
                Ok(ProfileSettings::OpenAiCompat {
                    base_url,
                    model,
                    api_key: api_key.trim().to_string(),
                })
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Profile {
    pub id: String,
    pub name: String,
    #[serde(flatten)]
    pub settings: ProfileSettings,
}

/// A profile as sent by the UI on create (the backend assigns the id).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct NewProfile {
    pub name: String,
    #[serde(flatten)]
    pub settings: ProfileSettings,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct ProfilesState {
    pub profiles: Vec<Profile>,
    pub active_profile_id: String,
}

/// Trimmed, non-empty, and unique (case-insensitive) among the other profiles.
fn validate_profile_name(
    name: &str,
    profiles: &[Profile],
    exclude_id: Option<&str>,
) -> Result<String, String> {
    let name = name.trim().to_string();
    if name.is_empty() {
        return Err("Profile name is required".to_string());
    }
    if name.chars().count() > MAX_PROFILE_NAME_CHARS {
        return Err(format!(
            "Profile name must be at most {MAX_PROFILE_NAME_CHARS} characters"
        ));
    }
    let key = name.to_lowercase();
    let taken = profiles
        .iter()
        .filter(|p| Some(p.id.as_str()) != exclude_id)
        .any(|p| p.name.trim().to_lowercase() == key);
    if taken {
        return Err(format!("A profile named \"{name}\" already exists"));
    }
    Ok(name)
}

fn generate_profile_id(profiles: &[Profile]) -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    loop {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos())
            .unwrap_or_default();
        let id = format!("p-{nanos:x}-{:x}", COUNTER.fetch_add(1, Ordering::Relaxed));
        if !profiles.iter().any(|p| p.id == id) {
            return id;
        }
    }
}

fn create_profile_in(config: &mut Config, new: NewProfile) -> Result<Profile, String> {
    let name = validate_profile_name(&new.name, config.profiles(), None)?;
    let settings = new.settings.normalized()?;
    let profile = Profile {
        id: generate_profile_id(config.profiles()),
        name,
        settings,
    };
    config.profiles_mut().push(profile.clone());
    Ok(profile)
}

fn update_profile_in(config: &mut Config, profile: Profile) -> Result<(), String> {
    let index = config
        .profiles()
        .iter()
        .position(|p| p.id == profile.id)
        .ok_or_else(|| format!("Unknown profile: {}", profile.id))?;
    let name = validate_profile_name(&profile.name, config.profiles(), Some(&profile.id))?;
    let settings = profile.settings.normalized()?;
    config.profiles_mut()[index] = Profile {
        id: profile.id,
        name,
        settings,
    };
    Ok(())
}

fn delete_profile_in(config: &mut Config, id: &str) -> Result<(), String> {
    let index = config
        .profiles()
        .iter()
        .position(|p| p.id == id)
        .ok_or_else(|| format!("Unknown profile: {id}"))?;
    if config.profiles().len() <= 1 {
        return Err("You can't delete the last profile".to_string());
    }
    if config.active_profile_id.as_deref() == Some(id) {
        return Err("Switch to another profile before deleting the active one".to_string());
    }
    config.profiles_mut().remove(index);
    Ok(())
}

fn set_active_profile_in(config: &mut Config, id: &str) -> Result<(), String> {
    if !config.profiles().iter().any(|p| p.id == id) {
        return Err(format!("Unknown profile: {id}"));
    }
    config.active_profile_id = Some(id.to_string());
    Ok(())
}

/// Run a fallible edit; the file is only written when `f` succeeds.
fn edit_profiles_at(
    path: &Path,
    f: impl FnOnce(&mut Config) -> Result<(), String>,
) -> Result<ProfilesState, String> {
    let mut config = load_config_from_path(path)?;
    f(&mut config)?;
    write_config_to_path(path, &config)?;
    Ok(config.profiles_state())
}

fn edit_profiles(
    f: impl FnOnce(&mut Config) -> Result<(), String>,
) -> Result<ProfilesState, String> {
    let _guard = config_mutex().lock().unwrap();
    edit_profiles_at(&get_config_path(), f)
}

pub fn profiles_state() -> Result<ProfilesState, String> {
    read_config(Config::profiles_state)
}

pub fn active_profile() -> Result<Profile, String> {
    read_config(Config::active_profile)?
}

pub fn profile_by_id(id: &str) -> Result<Profile, String> {
    read_config(|c| {
        c.profiles()
            .iter()
            .find(|p| p.id == id)
            .cloned()
            .ok_or_else(|| format!("Unknown profile: {id}"))
    })?
}

pub fn create_profile(new: NewProfile) -> Result<ProfilesState, String> {
    edit_profiles(|c| create_profile_in(c, new).map(|_| ()))
}

pub fn update_profile(profile: Profile) -> Result<ProfilesState, String> {
    edit_profiles(|c| update_profile_in(c, profile))
}

pub fn delete_profile(id: &str) -> Result<ProfilesState, String> {
    edit_profiles(|c| delete_profile_in(c, id))
}

pub fn set_active_profile(id: &str) -> Result<ProfilesState, String> {
    edit_profiles(|c| set_active_profile_in(c, id))
}

pub fn save_custom_vocabulary(vocabulary: Vec<CustomVocabEntry>) -> Result<(), String> {
    with_config(|c| c.custom_vocabulary = Some(vocabulary))
}

pub fn get_custom_vocabulary() -> Result<Vec<CustomVocabEntry>, String> {
    read_config(|c| c.custom_vocabulary.clone().unwrap_or_default())
}

/// Move an unreadable config aside (after explicit confirmation) and start
/// from defaults. Refuses when the config is readable.
#[tauri::command]
pub async fn reset_corrupted_config(confirmed: bool) -> Result<String, String> {
    let _guard = config_mutex().lock().unwrap();
    let timestamp = chrono::Utc::now().format("%Y%m%dT%H%M%S%3fZ").to_string();
    reset_corrupted_config_at(&get_config_path(), confirmed, &timestamp)
        .map(|path| path.to_string_lossy().into_owned())
}

fn reset_corrupted_config_at(
    path: &Path,
    confirmed: bool,
    timestamp: &str,
) -> Result<PathBuf, String> {
    if !confirmed {
        return Err("Settings reset requires explicit user confirmation.".into());
    }
    if load_config_from_path(path).is_ok() {
        return Err("Settings reset refused because the configuration is readable.".into());
    }
    let backup_path = path.with_file_name(format!("config.broken-{timestamp}.json"));
    if backup_path.exists() {
        return Err(format!(
            "Settings backup already exists at {}.",
            backup_path.display()
        ));
    }
    fs::rename(path, &backup_path).map_err(|e| {
        format!(
            "Failed to back up unreadable settings to {}: {e}",
            backup_path.display()
        )
    })?;
    if let Err(write_error) = write_config_to_path(path, &Config::default()) {
        let _ = fs::remove_file(path.with_extension("tmp"));
        if let Err(restore_error) = fs::rename(&backup_path, path) {
            return Err(format!(
                "Failed to create fresh settings ({write_error}) and failed to restore the backup ({restore_error}). Backup remains at {}.",
                backup_path.display()
            ));
        }
        return Err(format!(
            "Failed to create fresh settings; the original file was restored: {write_error}"
        ));
    }
    Ok(backup_path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn batch(base_url: &str, model: &str, api_key: &str) -> ProfileSettings {
        ProfileSettings::OpenAiCompat {
            base_url: base_url.to_string(),
            model: model.to_string(),
            api_key: api_key.to_string(),
        }
    }

    fn gladia(api_key: &str) -> ProfileSettings {
        ProfileSettings::Gladia {
            api_key: api_key.to_string(),
            code_switching: false,
        }
    }

    fn new_profile(name: &str, settings: ProfileSettings) -> NewProfile {
        NewProfile {
            name: name.to_string(),
            settings,
        }
    }

    fn fresh() -> Config {
        let mut c = Config::default();
        c.fill_defaults();
        c
    }

    /// Local Ollama (active) + Gladia.
    fn two_profiles() -> Config {
        let mut c = fresh();
        create_profile_in(&mut c, new_profile("Gladia", gladia("g-key"))).unwrap();
        c
    }

    #[test]
    fn fresh_config_defaults() {
        let c = fresh();
        let state = c.profiles_state();
        assert_eq!(state.profiles.len(), 1);
        assert_eq!(state.active_profile_id, DEFAULT_LOCAL_PROFILE_ID);
        assert_eq!(
            state.profiles[0].settings,
            batch(DEFAULT_STT_BASE_URL, DEFAULT_STT_MODEL, "")
        );
        assert_eq!(c.capture_system_audio, Some(true));
        assert_eq!(c.shortcut.as_deref(), Some(DEFAULT_SHORTCUT));
        assert_eq!(c.mark_shortcut.as_deref(), Some(DEFAULT_MARK_SHORTCUT));
        assert_eq!(c.mark_shortcut_enabled, Some(true));
        assert_eq!(c.note_shortcut.as_deref(), Some(DEFAULT_NOTE_SHORTCUT));
        assert_eq!(c.storage_dir, None);
        let json = serde_json::to_string(&c).unwrap();
        assert!(!json.contains("null"), "{json}");
        assert!(!json.contains("storage_dir"));
    }

    #[test]
    fn explicit_nulls_are_filled_on_load() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        fs::write(
            &path,
            r#"{"languages":null,"custom_vocabulary":null,"audio_device_selection":null,
                "capture_system_audio":null,"storage_dir":"  ","shortcut":null,
                "shortcut_enabled":null,"profiles":null,"active_profile_id":null}"#,
        )
        .unwrap();
        let c = load_config_from_path(&path).unwrap();
        assert_eq!(c.languages, Some(vec![]));
        assert_eq!(c.capture_system_audio, Some(true));
        assert_eq!(c.storage_dir, None);
        assert_eq!(c.profiles().len(), 1);
    }

    #[test]
    fn marker_shortcuts_default_and_fill_explicit_nulls() {
        let mut c: Config = serde_json::from_str(
            r#"{"mark_shortcut":null,"mark_shortcut_enabled":null,
                "note_shortcut":null,"note_shortcut_enabled":null}"#,
        )
        .unwrap();
        c.fill_defaults();
        assert_eq!(c.mark_shortcut.as_deref(), Some(DEFAULT_MARK_SHORTCUT));
        assert_eq!(c.note_shortcut.as_deref(), Some(DEFAULT_NOTE_SHORTCUT));
        assert_eq!(c.mark_shortcut_enabled, Some(true));
        assert_eq!(c.note_shortcut_enabled, Some(true));
        // Settings sent without the new fields (an older window) get defaults.
        let s: RecordingSettings = serde_json::from_value(serde_json::json!({
            "audio_device_selection": { "mode": "automatic" },
            "capture_system_audio": true,
            "shortcut": DEFAULT_SHORTCUT,
            "shortcut_enabled": true
        }))
        .unwrap();
        assert_eq!(s.mark_shortcut, DEFAULT_MARK_SHORTCUT);
        assert!(s.mark_shortcut_enabled && s.note_shortcut_enabled);
        assert!(s.validate().is_ok());
        // Wrong types and explicit nulls are rejected, not stored.
        for bad in [
            serde_json::json!({ "mark_shortcut": 5 }),
            serde_json::json!({ "mark_shortcut": null }),
            serde_json::json!({ "mark_shortcut_enabled": "yes" }),
            serde_json::json!({ "note_shortcut_enabled": null }),
        ] {
            let mut v = serde_json::to_value(&s).unwrap();
            for (k, val) in bad.as_object().unwrap() {
                v[k] = val.clone();
            }
            assert!(
                serde_json::from_value::<RecordingSettings>(v).is_err(),
                "{bad}"
            );
        }
    }

    #[test]
    fn shortcut_settings_are_validated() {
        let base = RecordingSettings {
            audio_device_selection: AudioDeviceSelection::Automatic,
            capture_system_audio: true,
            shortcut: DEFAULT_SHORTCUT.into(),
            shortcut_enabled: true,
            mark_shortcut: DEFAULT_MARK_SHORTCUT.into(),
            mark_shortcut_enabled: true,
            note_shortcut: DEFAULT_NOTE_SHORTCUT.into(),
            note_shortcut_enabled: true,
        };
        assert!(base.validate().is_ok());
        assert_eq!(
            base.enabled_shortcuts(),
            vec![
                (ShortcutAction::Record, DEFAULT_SHORTCUT),
                (ShortcutAction::Mark, DEFAULT_MARK_SHORTCUT),
                (ShortcutAction::Note, DEFAULT_NOTE_SHORTCUT)
            ]
        );
        let with = |f: fn(&mut RecordingSettings)| {
            let mut s = base.clone();
            f(&mut s);
            s
        };
        assert!(with(|s| s.mark_shortcut = "  ".into()).validate().is_err());
        assert!(with(|s| s.note_shortcut = String::new())
            .validate()
            .is_err());
        // The same keys twice, even with different case or spacing.
        assert!(
            with(|s| s.mark_shortcut = " commandorcontrol+shift+alt+r ".into())
                .validate()
                .is_err()
        );
        assert!(with(|s| s.note_shortcut = DEFAULT_MARK_SHORTCUT.into())
            .validate()
            .is_err());
        // …unless one of them is off.
        let off = with(|s| {
            s.note_shortcut = DEFAULT_MARK_SHORTCUT.into();
            s.note_shortcut_enabled = false;
        });
        assert!(off.validate().is_ok());
        assert_eq!(off.enabled_shortcuts().len(), 2);
        assert_eq!(
            with(|s| s.mark_shortcut = " Alt+M ".into()).enabled_shortcuts()[1],
            (ShortcutAction::Mark, "Alt+M")
        );
        assert!(!ShortcutAction::Record.only_while_recording());
        assert!(ShortcutAction::Mark.only_while_recording());
        assert!(ShortcutAction::Note.only_while_recording());
    }

    #[test]
    fn unknown_active_id_is_repaired() {
        let mut c: Config = serde_json::from_str(r#"{"active_profile_id":"gone"}"#).unwrap();
        c.fill_defaults();
        assert_eq!(c.active_profile().unwrap().id, DEFAULT_LOCAL_PROFILE_ID);
    }

    #[test]
    fn malformed_config_is_an_error_and_is_not_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        fs::write(&path, "{oops").unwrap();
        assert!(load_config_from_path(&path).is_err());
        assert!(with_config_at(&path, |c| c.shortcut = Some("X".into())).is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), "{oops");
    }

    #[test]
    fn storage_dir_is_shared_with_the_mcp_path_resolution() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        with_config_at(&path, |c| c.storage_dir = Some("/Volumes/X/parley".into())).unwrap();
        assert_eq!(
            parley_lib::paths::storage_dir_from_config(&path),
            Some(PathBuf::from("/Volumes/X/parley"))
        );
    }

    #[test]
    fn profile_settings_normalize_and_reject_bad_input() {
        assert_eq!(
            batch(
                " https://api.groq.com/openai/v1/ ",
                " whisper-large-v3 ",
                " k "
            )
            .normalized(),
            Ok(batch(
                "https://api.groq.com/openai/v1",
                "whisper-large-v3",
                "k"
            ))
        );
        assert_eq!(gladia(" key ").normalized(), Ok(gladia("key")));
        for bad in [
            batch("localhost:11434/v1", "m", ""),
            batch("ftp://localhost/v1", "m", ""),
            batch("not a url", "m", ""),
            batch("", "m", ""),
            batch("http://", "m", ""),
            batch(DEFAULT_STT_BASE_URL, "  ", ""),
        ] {
            assert!(bad.clone().normalized().is_err(), "{bad:?}");
        }
    }

    #[test]
    fn profile_json_rejects_nulls_wrong_types_and_unknown_kinds() {
        let valid_gladia = serde_json::json!({"id":"g","name":"G","kind":"gladia","api_key":"k","code_switching":false});
        let valid_batch = serde_json::json!({"id":"b","name":"B","kind":"openai_compat","base_url":"http://h/v1","model":"m","api_key":""});
        assert!(serde_json::from_value::<Profile>(valid_gladia.clone()).is_ok());
        assert!(serde_json::from_value::<Profile>(valid_batch.clone()).is_ok());
        let cases: Vec<(serde_json::Value, &str, serde_json::Value)> = vec![
            (valid_gladia.clone(), "id", serde_json::Value::Null),
            (valid_gladia.clone(), "name", serde_json::Value::Null),
            (valid_gladia.clone(), "kind", serde_json::Value::Null),
            (valid_gladia.clone(), "api_key", serde_json::Value::Null),
            (
                valid_gladia.clone(),
                "code_switching",
                serde_json::Value::Null,
            ),
            (valid_batch.clone(), "base_url", serde_json::Value::Null),
            (valid_batch.clone(), "model", serde_json::Value::Null),
            (valid_batch.clone(), "api_key", serde_json::Value::Null),
            (valid_gladia.clone(), "name", serde_json::json!(42)),
            (valid_gladia.clone(), "kind", serde_json::json!("whisper")),
            (
                valid_gladia.clone(),
                "code_switching",
                serde_json::json!("yes"),
            ),
            (valid_batch.clone(), "model", serde_json::json!(false)),
        ];
        for (mut value, field, bad) in cases {
            value[field] = bad.clone();
            assert!(
                serde_json::from_value::<Profile>(value).is_err(),
                "{field} = {bad} accepted"
            );
        }
        let mut value = valid_gladia;
        value.as_object_mut().unwrap().remove("api_key");
        assert!(serde_json::from_value::<Profile>(value).is_err());
        let mut value = valid_batch;
        value.as_object_mut().unwrap().remove("api_key");
        assert!(serde_json::from_value::<Profile>(value).is_ok());
    }

    #[test]
    fn create_update_delete_and_activate_profiles() {
        let mut c = two_profiles();
        let created = create_profile_in(
            &mut c,
            new_profile(
                "  Groq ",
                batch("https://api.groq.com/openai/v1/", "whisper-large-v3", "gsk"),
            ),
        )
        .unwrap();
        assert_eq!(created.name, "Groq");
        assert!(created.id.starts_with("p-"));
        assert_eq!(c.profiles().len(), 3);
        assert_eq!(
            c.active_profile_id.as_deref(),
            Some(DEFAULT_LOCAL_PROFILE_ID)
        );

        for (name, settings) in [
            ("", batch("http://h/v1", "m", "")),
            ("gladia", batch("http://h/v1", "m", "")),
            ("New", batch("ftp://h/v1", "m", "")),
        ] {
            assert!(create_profile_in(&mut c, new_profile(name, settings)).is_err());
        }
        assert!(create_profile_in(
            &mut c,
            new_profile(&"x".repeat(61), batch("http://h/v1", "m", ""))
        )
        .is_err());
        assert_eq!(c.profiles().len(), 3);

        let mut groq = created.clone();
        groq.name = "local OLLAMA".into();
        assert!(update_profile_in(&mut c, groq.clone()).is_err());
        groq.name = "Groq fast".into();
        groq.settings = batch("not a url", "m", "");
        assert!(update_profile_in(&mut c, groq.clone()).is_err());
        groq.settings = gladia("k2");
        update_profile_in(&mut c, groq.clone()).unwrap();
        assert_eq!(c.profiles()[2].settings, gladia("k2"));

        assert!(set_active_profile_in(&mut c, "nope").is_err());
        set_active_profile_in(&mut c, &groq.id).unwrap();
        assert_eq!(
            delete_profile_in(&mut c, &groq.id),
            Err("Switch to another profile before deleting the active one".to_string())
        );
        assert!(delete_profile_in(&mut c, "nope").is_err());
        delete_profile_in(&mut c, DEFAULT_LOCAL_PROFILE_ID).unwrap();
        let gladia_id = c.profiles()[0].id.clone();
        delete_profile_in(&mut c, &gladia_id).unwrap();
        assert_eq!(
            delete_profile_in(&mut c, &groq.id),
            Err("You can't delete the last profile".to_string())
        );
    }

    #[test]
    fn failed_profile_writes_leave_the_file_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        write_config_to_path(&path, &two_profiles()).unwrap();
        let before = fs::read_to_string(&path).unwrap();
        assert!(
            edit_profiles_at(&path, |c| delete_profile_in(c, DEFAULT_LOCAL_PROFILE_ID)).is_err()
        );
        assert!(edit_profiles_at(&path, |c| set_active_profile_in(c, "nope")).is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), before);
    }

    #[test]
    fn corrupted_config_reset_requires_confirmation_and_backs_up() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        fs::write(&path, "{bad").unwrap();
        assert!(reset_corrupted_config_at(&path, false, "T").is_err());
        let backup = reset_corrupted_config_at(&path, true, "T").unwrap();
        assert_eq!(fs::read_to_string(backup).unwrap(), "{bad");
        assert!(load_config_from_path(&path).is_ok());
        // A readable config is never reset.
        assert!(reset_corrupted_config_at(&path, true, "T2").is_err());
    }
}
