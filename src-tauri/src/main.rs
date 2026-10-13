#![cfg_attr(
    all(not(debug_assertions), target_os = "windows"),
    windows_subsystem = "windows"
)]

use chrono::{DateTime, Utc};
use parley_lib::store::{self, Marker, MarkerKind, Meeting, MeetingStatus, Segment};
use parley_lib::{markdown, paths};
use serde::Serialize;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Emitter, Manager, State};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut, ShortcutState};
use tokio::sync::Mutex as TokioMutex;

mod audio;
mod batch_stt;
mod config;
mod gladia;
mod library;
mod permissions;
mod recorder;
mod system_audio;
#[cfg(test)]
mod test_http;
mod transcribe;
mod vocabulary;
mod whisper_local;
mod widget_window;

use audio::{AudioCapture, AudioDeviceInfo};
use config::{
    NewProfile, Profile, ProfileSettings, ProfilesState, RecordingSettings, ShortcutAction,
};
use recorder::{RecordingClock, TrackHandle};
use system_audio::SystemAudioCapture;

const TRAY_ID: &str = "main-tray";
const TRAY_ICON_IDLE: &[u8] = include_bytes!("../icons/tray-idle.png");
const TRAY_ICON_RECORDING: &[u8] = include_bytes!("../icons/tray-recording.png");
const STATUS_TICK: Duration = Duration::from_millis(100);

// --- Recorder state ---

struct Session {
    meeting_id: String,
    title: String,
    started_at: DateTime<Utc>,
    folder: PathBuf,
    clock: Arc<RecordingClock>,
    mic: TrackHandle,
    system: Option<TrackHandle>,
    system_capture: Option<SystemAudioCapture>,
    warning: Option<String>,
    /// Highlights and notes saved for this meeting so far.
    marker_count: i64,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum RecorderState {
    Idle,
    Recording,
    Paused,
}

#[derive(Debug, Clone, Serialize)]
pub struct RecorderStatus {
    state: RecorderState,
    meeting_id: Option<String>,
    title: Option<String>,
    elapsed_ms: u64,
    me_level: f32,
    them_level: f32,
    system_audio: bool,
    warning: Option<String>,
    marker_count: i64,
}

impl RecorderStatus {
    fn idle() -> Self {
        Self {
            state: RecorderState::Idle,
            meeting_id: None,
            title: None,
            elapsed_ms: 0,
            me_level: 0.0,
            them_level: 0.0,
            system_audio: false,
            warning: None,
            marker_count: 0,
        }
    }
}

#[derive(Clone)]
pub struct AppState {
    audio: Arc<AudioCapture>,
    session: Arc<Mutex<Option<Session>>>,
    /// Serializes transcription jobs (one provider call stream at a time).
    job_lock: Arc<TokioMutex<()>>,
    /// Meetings queued or being transcribed.
    jobs: Arc<Mutex<HashSet<String>>>,
}

impl AppState {
    fn status(&self) -> RecorderStatus {
        let guard = self.session.lock().unwrap();
        match guard.as_ref() {
            None => RecorderStatus::idle(),
            Some(s) => RecorderStatus {
                state: if s.clock.is_running() {
                    RecorderState::Recording
                } else {
                    RecorderState::Paused
                },
                meeting_id: Some(s.meeting_id.clone()),
                title: Some(s.title.clone()),
                elapsed_ms: s.clock.elapsed().as_millis() as u64,
                me_level: s.mic.meter.get(),
                them_level: s.system.as_ref().map(|t| t.meter.get()).unwrap_or(0.0),
                system_audio: s.system.is_some(),
                warning: s.warning.clone(),
                marker_count: s.marker_count,
            },
        }
    }

    fn recording_id(&self) -> Option<String> {
        self.session
            .lock()
            .unwrap()
            .as_ref()
            .map(|s| s.meeting_id.clone())
    }
}

fn db() -> Result<rusqlite::Connection, String> {
    store::open_rw(&paths::db_path())
}

fn emit_meetings_changed(app: &AppHandle, id: Option<&str>) {
    let _ = app.emit("meetings-changed", id);
}

fn emit_status(app: &AppHandle, state: &AppState) {
    let status = state.status();
    refresh_tray(app, &status);
    let _ = app.emit("recorder-status", status);
}

/// Start the mic (required) and the system tap (best effort), and the writers.
fn start_session(app: &AppHandle, state: &AppState, title: Option<String>) -> Result<(), String> {
    if state.session.lock().unwrap().is_some() {
        return Err("A recording is already running".to_string());
    }
    let settings = config::recording_settings()?;
    let data_dir = paths::data_dir();
    let started_at = Utc::now();
    let id = store::new_meeting_id(started_at);
    let folder_rel = format!("{}/{id}", paths::MEETINGS_DIR_NAME);
    let folder = data_dir.join(&folder_rel);
    std::fs::create_dir_all(&folder)
        .map_err(|e| format!("Can't create {}: {e}", folder.display()))?;
    let title = title
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
        .unwrap_or_else(|| store::default_title(started_at));
    let conn = db()?;
    store::insert_meeting(
        &conn,
        &store::NewMeeting {
            id: &id,
            title: &title,
            started_at,
            folder: &folder_rel,
        },
    )?;
    let abort = |message: String| {
        let _ = store::delete_meeting(&conn, &id);
        let _ = std::fs::remove_dir_all(&folder);
        Err(message)
    };

    let clock = Arc::new(RecordingClock::started());
    let (err_tx, err_rx) = crossbeam_channel::unbounded::<String>();
    let (mic_rx, device) = match state
        .audio
        .start_capture(settings.audio_device_selection.clone(), Some(err_tx))
    {
        Ok(v) => v,
        Err(e) => return abort(format!("Couldn't start the microphone: {e}")),
    };
    log::info!("[recorder] {id}: microphone {device:?}");
    let err_app = app.clone();
    std::thread::spawn(move || {
        while let Ok(message) = err_rx.recv() {
            log::error!("[recorder] {message}");
            let _ = err_app.emit("recorder-error", message);
        }
    });
    let mic = match recorder::spawn_track(
        "mic",
        mic_rx.into_iter().map(|c| c.samples),
        folder.join(store::MIC_FILE),
        clock.clone(),
    ) {
        Ok(t) => t,
        Err(e) => {
            let _ = state.audio.stop_capture();
            return abort(format!("Couldn't write the recording: {e}"));
        }
    };

    let mut warning = None;
    let mut system = None;
    let mut system_capture = None;
    if settings.capture_system_audio {
        if system_audio::permission_status_fresh() == system_audio::PermissionStatus::Denied {
            warning = Some(
                "System audio recording is turned off in System Settings, so only your mic is recorded."
                    .to_string(),
            );
        }
        let (tx, rx) = crossbeam_channel::unbounded::<Vec<i16>>();
        match SystemAudioCapture::start(tx) {
            Ok(capture) => match recorder::spawn_track(
                "system",
                rx.into_iter(),
                folder.join(store::SYSTEM_FILE),
                clock.clone(),
            ) {
                Ok(track) => {
                    system = Some(track);
                    system_capture = Some(capture);
                }
                Err(e) => warning = Some(format!("Call audio isn't recorded: {e}")),
            },
            Err(e) => {
                log::warn!("[recorder] system audio unavailable: {e}");
                warning = Some(format!("Call audio isn't recorded: {e}"));
            }
        }
    } else {
        warning =
            Some("Call audio recording is off in Settings; only your mic is recorded.".to_string());
    }
    if let Some(w) = &warning {
        let _ = store::set_capture_warning(&conn, &id, Some(w));
    }
    *state.session.lock().unwrap() = Some(Session {
        meeting_id: id.clone(),
        title,
        started_at,
        folder,
        clock,
        mic,
        system,
        system_capture,
        warning,
        marker_count: 0,
    });
    log::info!("[recorder] {id}: recording");
    Ok(())
}

/// Stop capture, finalize the files and hand the meeting to transcription.
fn stop_session(state: &AppState) -> Result<String, String> {
    let session = state
        .session
        .lock()
        .unwrap()
        .take()
        .ok_or_else(|| "Nothing is being recorded".to_string())?;
    session.clock.freeze();
    if let Err(e) = state.audio.stop_capture() {
        log::warn!("[recorder] stopping the microphone: {e}");
    }
    if let Some(capture) = session.system_capture {
        capture.stop();
    }
    let join = |t: TrackHandle| match t.thread.join() {
        Ok(Ok(n)) => n,
        Ok(Err(e)) => {
            log::error!("[recorder] writing {}: {e}", t.path.display());
            0
        }
        Err(_) => {
            log::error!("[recorder] writer for {} panicked", t.path.display());
            0
        }
    };
    let samples = join(session.mic).max(session.system.map(join).unwrap_or(0));
    let duration_ms = (samples * 1000 / recorder::SAMPLE_RATE as u64) as i64;
    if let Err(e) = library::write_mixed(&session.folder) {
        log::warn!("[recorder] mixing: {e}");
    }
    let conn = db()?;
    let ended = Utc::now();
    store::finish_recording(&conn, &session.meeting_id, ended, duration_ms)?;
    log::info!(
        "[recorder] {}: stopped after {}s (started {})",
        session.meeting_id,
        duration_ms / 1000,
        session.started_at
    );
    Ok(session.meeting_id)
}

fn spawn_status_ticker(app: AppHandle, state: AppState) {
    tauri::async_runtime::spawn(async move {
        loop {
            let status = state.status();
            let idle = status.state == RecorderState::Idle;
            let _ = app.emit("recorder-status", status);
            if idle {
                break;
            }
            tokio::time::sleep(STATUS_TICK).await;
        }
    });
}

async fn start_recording_inner(
    app: &AppHandle,
    state: &AppState,
    title: Option<String>,
) -> Result<RecorderStatus, String> {
    let (a, s) = (app.clone(), state.clone());
    tokio::task::spawn_blocking(move || start_session(&a, &s, title))
        .await
        .map_err(|e| e.to_string())??;
    show_widget(app);
    apply_shortcut(app);
    emit_status(app, state);
    emit_meetings_changed(app, state.recording_id().as_deref());
    spawn_status_ticker(app.clone(), state.clone());
    Ok(state.status())
}

async fn stop_recording_inner(app: &AppHandle, state: &AppState) -> Result<RecorderStatus, String> {
    let s = state.clone();
    let id = tokio::task::spawn_blocking(move || stop_session(&s))
        .await
        .map_err(|e| e.to_string())??;
    apply_shortcut(app);
    emit_status(app, state);
    emit_meetings_changed(app, Some(&id));
    queue_transcription(app.clone(), state.clone(), id, None)?;
    Ok(state.status())
}

async fn toggle_recording(app: &AppHandle) {
    let state = app.state::<AppState>().inner().clone();
    let result = if state.recording_id().is_some() {
        stop_recording_inner(app, &state).await
    } else {
        start_recording_inner(app, &state, None).await
    };
    if let Err(e) = result {
        log::error!("[recorder] {e}");
        let _ = app.emit("recorder-error", e);
    }
}

#[tauri::command]
fn get_recorder_status(state: State<'_, AppState>) -> RecorderStatus {
    state.status()
}

#[tauri::command]
async fn start_recording(
    title: Option<String>,
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<RecorderStatus, String> {
    start_recording_inner(&app, &state, title).await
}

#[tauri::command]
async fn stop_recording(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<RecorderStatus, String> {
    stop_recording_inner(&app, &state).await
}

fn set_paused(app: &AppHandle, state: &AppState, paused: bool) -> Result<RecorderStatus, String> {
    {
        let guard = state.session.lock().unwrap();
        let session = guard.as_ref().ok_or("Nothing is being recorded")?;
        if paused {
            session.clock.pause();
        } else {
            session.clock.resume();
        }
    }
    emit_status(app, state);
    Ok(state.status())
}

#[tauri::command]
fn pause_recording(app: AppHandle, state: State<'_, AppState>) -> Result<RecorderStatus, String> {
    set_paused(&app, &state, true)
}

#[tauri::command]
fn resume_recording(app: AppHandle, state: State<'_, AppState>) -> Result<RecorderStatus, String> {
    set_paused(&app, &state, false)
}

// --- Highlights & notes ---

/// The live meeting and the current moment on its recording clock.
#[derive(Debug, Clone, Serialize)]
struct LiveMoment {
    meeting_id: String,
    at_ms: i64,
}

fn live_moment(state: &AppState) -> Result<LiveMoment, String> {
    let guard = state.session.lock().unwrap();
    let session = guard.as_ref().ok_or("Nothing is being recorded")?;
    Ok(LiveMoment {
        meeting_id: session.meeting_id.clone(),
        at_ms: session.clock.elapsed_ms(),
    })
}

/// After a marker was added, edited or deleted: refresh the live count (and
/// tell the recorder), or rewrite a finished meeting's transcript.md.
fn markers_changed(
    app: &AppHandle,
    state: &AppState,
    conn: &rusqlite::Connection,
    meeting_id: &str,
    added: Option<&Marker>,
) {
    let live = {
        let mut guard = state.session.lock().unwrap();
        match guard.as_mut() {
            Some(s) if s.meeting_id == meeting_id => {
                s.marker_count = store::count_markers(conn, meeting_id).unwrap_or(s.marker_count);
                true
            }
            _ => false,
        }
    };
    if live {
        if let Some(marker) = added {
            let _ = app.emit("marker-added", marker);
        }
        emit_status(app, state);
    } else if store::get_meeting(conn, meeting_id)
        .ok()
        .flatten()
        .is_some_and(|m| m.status == MeetingStatus::Ready)
    {
        if let Err(e) = library::write_transcript(&paths::data_dir(), conn, meeting_id) {
            log::warn!("[markers] rewriting transcript.md for {meeting_id}: {e}");
        }
    }
    emit_meetings_changed(app, Some(meeting_id));
}

/// Mark a highlight now, in the meeting being recorded.
fn mark_now(app: &AppHandle, state: &AppState) -> Result<Marker, String> {
    let moment = live_moment(state)?;
    let conn = db()?;
    let marker = store::insert_marker(
        &conn,
        &moment.meeting_id,
        MarkerKind::Highlight,
        moment.at_ms,
        None,
    )?;
    markers_changed(app, state, &conn, &moment.meeting_id, Some(&marker));
    Ok(marker)
}

/// The note shortcut: capture the moment now, then open the recorder's field.
fn open_note_from_shortcut(app: &AppHandle) {
    let state = app.state::<AppState>().inner().clone();
    match live_moment(&state) {
        Ok(moment) => {
            show_widget(app);
            let _ = app.emit("widget-note", moment);
        }
        Err(e) => log::warn!("[shortcut] note: {e}"),
    }
}

#[tauri::command]
fn mark_moment(app: AppHandle, state: State<'_, AppState>) -> Result<Marker, String> {
    mark_now(&app, &state)
}

/// The note's moment is when its field opens, not when it is saved.
#[tauri::command]
fn begin_note(state: State<'_, AppState>) -> Result<LiveMoment, String> {
    live_moment(&state)
}

#[tauri::command]
fn add_marker(
    meeting_id: String,
    kind: MarkerKind,
    at_ms: i64,
    text: Option<String>,
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<Marker, String> {
    let conn = db()?;
    let marker = store::insert_marker(&conn, &meeting_id, kind, at_ms, text.as_deref())?;
    markers_changed(&app, &state, &conn, &meeting_id, Some(&marker));
    Ok(marker)
}

#[tauri::command]
fn update_marker(
    id: i64,
    text: Option<String>,
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<Marker, String> {
    let conn = db()?;
    let marker = store::update_marker_text(&conn, id, text.as_deref())?;
    markers_changed(&app, &state, &conn, &marker.meeting_id, None);
    Ok(marker)
}

#[tauri::command]
fn delete_marker(id: i64, app: AppHandle, state: State<'_, AppState>) -> Result<(), String> {
    let conn = db()?;
    let meeting_id = store::delete_marker(&conn, id)?;
    markers_changed(&app, &state, &conn, &meeting_id, None);
    Ok(())
}

/// The recorder takes the keyboard only while its title or note field is in use.
#[tauri::command]
fn set_widget_input(mode: widget_window::InputMode, app: AppHandle) -> Result<(), String> {
    widget_window::set_input(&app, mode)
}

// --- Transcription queue ---

fn queue_transcription(
    app: AppHandle,
    state: AppState,
    meeting_id: String,
    profile_id: Option<String>,
) -> Result<(), String> {
    if state.recording_id().as_deref() == Some(meeting_id.as_str()) {
        return Err("Stop the recording before transcribing it".to_string());
    }
    let profile = match profile_id {
        Some(id) => config::profile_by_id(&id)?,
        None => config::active_profile()?,
    };
    if !state.jobs.lock().unwrap().insert(meeting_id.clone()) {
        return Err("This meeting is already being transcribed".to_string());
    }
    // Show "transcribing" right away, even while queued behind another job.
    if let Ok(conn) = db() {
        let _ = store::set_status(&conn, &meeting_id, MeetingStatus::Transcribing, None);
    }
    emit_meetings_changed(&app, Some(&meeting_id));
    tauri::async_runtime::spawn(async move {
        let _turn = state.job_lock.lock().await;
        let data_dir = paths::data_dir();
        let progress_app = app.clone();
        let progress = move |p: transcribe::Progress| {
            let _ = progress_app.emit("transcription-progress", p);
        };
        let result = transcribe::run(
            transcribe::Job {
                data_dir: &data_dir,
                meeting_id: &meeting_id,
                profile,
                languages: config::languages().unwrap_or_default(),
                vocabulary: config::get_custom_vocabulary().unwrap_or_default(),
            },
            &progress,
        )
        .await;
        state.jobs.lock().unwrap().remove(&meeting_id);
        if let Err(e) = &result {
            let _ = app.emit(
                "transcription-error",
                serde_json::json!({ "meeting_id": meeting_id, "error": e }),
            );
        }
        emit_meetings_changed(&app, Some(&meeting_id));
    });
    Ok(())
}

#[tauri::command]
fn retranscribe_meeting(
    id: String,
    profile_id: Option<String>,
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<(), String> {
    queue_transcription(app, state.inner().clone(), id, profile_id)
}

// --- Library ---

#[derive(Serialize)]
struct MeetingDetail {
    meeting: Meeting,
    segments: Vec<Segment>,
    markers: Vec<Marker>,
    files: library::MeetingFiles,
}

#[tauri::command]
fn list_meetings(
    query: Option<String>,
    from: Option<String>,
    to: Option<String>,
    limit: Option<u32>,
) -> Result<Vec<Meeting>, String> {
    if !paths::db_path().exists() {
        return Ok(Vec::new());
    }
    store::list_meetings(
        &db()?,
        &store::ListQuery {
            from,
            to,
            query,
            limit: Some(limit.unwrap_or(500)),
            offset: None,
        },
    )
}

#[tauri::command]
fn get_meeting(id: String) -> Result<MeetingDetail, String> {
    let conn = db()?;
    let meeting = store::require_meeting(&conn, &id)?;
    let segments = store::segments(&conn, &id)?;
    let markers = store::markers(&conn, &id)?;
    let files = library::files(&paths::data_dir(), &meeting)?;
    Ok(MeetingDetail {
        meeting,
        segments,
        markers,
        files,
    })
}

#[tauri::command]
fn get_meeting_markdown(id: String) -> Result<String, String> {
    let conn = db()?;
    let meeting = store::require_meeting(&conn, &id)?;
    Ok(markdown::render(
        &meeting,
        &store::segments(&conn, &id)?,
        &store::markers(&conn, &id)?,
    ))
}

#[tauri::command]
fn rename_meeting(
    id: String,
    title: String,
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<Meeting, String> {
    let conn = db()?;
    store::rename_meeting(&conn, &id, &title)?;
    let meeting = store::require_meeting(&conn, &id)?;
    if let Some(session) = state.session.lock().unwrap().as_mut() {
        if session.meeting_id == id {
            session.title = meeting.title.clone();
        }
    }
    if meeting.status == MeetingStatus::Ready {
        library::write_transcript(&paths::data_dir(), &conn, &id)?;
    }
    emit_meetings_changed(&app, Some(&id));
    Ok(meeting)
}

#[tauri::command]
fn delete_meeting(id: String, app: AppHandle, state: State<'_, AppState>) -> Result<(), String> {
    if state.recording_id().as_deref() == Some(id.as_str()) {
        return Err("Stop the recording before deleting it".to_string());
    }
    if state.jobs.lock().unwrap().contains(&id) {
        return Err("Wait for the transcription to finish before deleting".to_string());
    }
    library::delete(&paths::data_dir(), &db()?, &id)?;
    emit_meetings_changed(&app, None);
    Ok(())
}

#[tauri::command]
fn reveal_meeting(id: String) -> Result<(), String> {
    let conn = db()?;
    let meeting = store::require_meeting(&conn, &id)?;
    let files = library::files(&paths::data_dir(), &meeting)?;
    let target = files.transcript.or(files.mixed).unwrap_or(files.folder);
    reveal(&target)
}

fn reveal(path: &Path) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .arg("-R")
            .arg(path)
            .spawn()
            .map(|_| ())
            .map_err(|e| e.to_string())
    }
    #[cfg(not(target_os = "macos"))]
    {
        open::that_detached(path.parent().unwrap_or(path)).map_err(|e| e.to_string())
    }
}

// --- Profiles ---

#[tauri::command]
fn list_profiles() -> Result<ProfilesState, String> {
    config::profiles_state()
}

fn profiles_changed(
    app: &AppHandle,
    result: Result<ProfilesState, String>,
) -> Result<ProfilesState, String> {
    if let Ok(state) = &result {
        let _ = app.emit("profiles-changed", state.clone());
    }
    result
}

#[tauri::command]
fn create_profile(profile: NewProfile, app: AppHandle) -> Result<ProfilesState, String> {
    profiles_changed(&app, config::create_profile(profile))
}

#[tauri::command]
fn update_profile(profile: Profile, app: AppHandle) -> Result<ProfilesState, String> {
    profiles_changed(&app, config::update_profile(profile))
}

#[tauri::command]
fn delete_profile(profile_id: String, app: AppHandle) -> Result<ProfilesState, String> {
    profiles_changed(&app, config::delete_profile(&profile_id))
}

#[tauri::command]
fn set_active_profile(profile_id: String, app: AppHandle) -> Result<ProfilesState, String> {
    profiles_changed(&app, config::set_active_profile(&profile_id))
}

#[tauri::command]
async fn test_profile_connection(profile_id: String) -> Result<String, String> {
    let profile = config::profile_by_id(&profile_id)?;
    match &profile.settings {
        ProfileSettings::Gladia { api_key, .. } => {
            gladia::GladiaClient::default()
                .test_connection(api_key)
                .await
        }
        ProfileSettings::OpenAiCompat { .. } => {
            let settings = profile
                .settings
                .batch_settings()
                .expect("OpenAI-compatible profiles have batch settings");
            batch_stt::test_connection(&settings).await
        }
        ProfileSettings::WhisperLocal { model_path } => whisper_local::test_setup(model_path).await,
    }
}

#[tauri::command]
fn get_languages() -> Result<Vec<String>, String> {
    config::languages()
}

#[tauri::command]
fn save_languages(languages: Vec<String>) -> Result<(), String> {
    config::save_languages(languages)
}

// --- Recording settings & shortcut ---

#[tauri::command]
fn get_recording_settings() -> Result<RecordingSettings, String> {
    config::recording_settings()
}

#[tauri::command]
fn save_recording_settings(settings: RecordingSettings, app: AppHandle) -> Result<(), String> {
    settings.validate()?;
    let mut parsed: Vec<Shortcut> = Vec::new();
    for (_, accelerator) in settings.enabled_shortcuts() {
        let shortcut = accelerator
            .parse::<Shortcut>()
            .map_err(|e| format!("Invalid shortcut \"{accelerator}\": {e}"))?;
        if parsed.contains(&shortcut) {
            return Err(format!("{accelerator} is used for two shortcuts"));
        }
        parsed.push(shortcut);
    }
    config::save_recording_settings(settings)?;
    apply_shortcut(&app);
    Ok(())
}

/// Register the global shortcuts: record always, mark and note only while a
/// recording runs (so they don't take those keys from other apps otherwise).
fn apply_shortcut(app: &AppHandle) {
    let gs = app.global_shortcut();
    let _ = gs.unregister_all();
    let Ok(settings) = config::recording_settings() else {
        return;
    };
    let recording = app.state::<AppState>().recording_id().is_some();
    for (action, accelerator) in settings.enabled_shortcuts() {
        if action.only_while_recording() && !recording {
            continue;
        }
        let result = gs.on_shortcut(accelerator, move |app, _shortcut, event| {
            if event.state != ShortcutState::Pressed {
                return;
            }
            let app = app.clone();
            match action {
                ShortcutAction::Record => {
                    tauri::async_runtime::spawn(async move { toggle_recording(&app).await });
                }
                ShortcutAction::Mark => {
                    tauri::async_runtime::spawn_blocking(move || {
                        let state = app.state::<AppState>().inner().clone();
                        if let Err(e) = mark_now(&app, &state) {
                            log::warn!("[shortcut] mark: {e}");
                        }
                    });
                }
                ShortcutAction::Note => open_note_from_shortcut(&app),
            }
        });
        if let Err(e) = result {
            log::warn!("[shortcut] could not register {action:?} {accelerator:?}: {e}");
        }
    }
}

#[tauri::command]
fn list_audio_device_info() -> Vec<AudioDeviceInfo> {
    audio::list_input_device_info()
}

// --- Storage ---

#[derive(Serialize)]
struct StorageInfo {
    data_dir: PathBuf,
    db_path: PathBuf,
    default_dir: PathBuf,
    is_default: bool,
}

#[tauri::command]
fn get_storage_info() -> StorageInfo {
    let data_dir = paths::data_dir();
    let default_dir = paths::config_dir();
    StorageInfo {
        db_path: paths::db_path_in(&data_dir),
        is_default: data_dir == default_dir,
        data_dir,
        default_dir,
    }
}

#[tauri::command]
async fn choose_storage_dir(app: AppHandle) -> Result<Option<String>, String> {
    use tauri_plugin_dialog::DialogExt;
    let picked = app
        .dialog()
        .file()
        .set_title("Choose where Parley keeps meetings")
        .blocking_pick_folder();
    match picked {
        Some(path) => Ok(Some(
            path.into_path()
                .map_err(|e| e.to_string())?
                .to_string_lossy()
                .into_owned(),
        )),
        None => Ok(None),
    }
}

#[tauri::command]
fn move_storage(
    dir: Option<String>,
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<StorageInfo, String> {
    if state.recording_id().is_some() || !state.jobs.lock().unwrap().is_empty() {
        return Err("Wait until nothing is recording or transcribing".to_string());
    }
    let from = paths::data_dir();
    let to = match dir.as_deref().map(str::trim).filter(|d| !d.is_empty()) {
        Some(d) => PathBuf::from(d),
        None => paths::config_dir(),
    };
    if !to.is_absolute() {
        return Err("Choose an absolute folder path".to_string());
    }
    library::move_data(&from, &to)?;
    let saved = (to != paths::config_dir()).then(|| to.to_string_lossy().into_owned());
    config::set_storage_dir(saved)?;
    let _ = app.asset_protocol_scope().allow_directory(&to, true);
    emit_meetings_changed(&app, None);
    Ok(get_storage_info())
}

// --- Claude / MCP ---

#[derive(Serialize)]
struct McpInfo {
    binary_path: PathBuf,
    binary_exists: bool,
    db_path: PathBuf,
    claude_code_command: String,
    desktop_config: String,
}

fn shell_quote(path: &str) -> String {
    if path
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || "/._-+".contains(c))
    {
        path.to_string()
    } else {
        format!("'{}'", path.replace('\'', "'\\''"))
    }
}

#[tauri::command]
fn get_mcp_info() -> Result<McpInfo, String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let binary_path = exe
        .parent()
        .map(|dir| dir.join("parley-mcp"))
        .ok_or("Can't locate the app")?;
    let path_str = binary_path.to_string_lossy().into_owned();
    let desktop_config = serde_json::to_string_pretty(&serde_json::json!({
        "mcpServers": { "parley": { "command": path_str, "args": [] } }
    }))
    .map_err(|e| e.to_string())?;
    Ok(McpInfo {
        binary_exists: binary_path.is_file(),
        claude_code_command: format!("claude mcp add parley -- {}", shell_quote(&path_str)),
        desktop_config,
        db_path: paths::db_path(),
        binary_path,
    })
}

// --- Permissions ---

#[derive(Serialize)]
struct PermissionsState {
    microphone: String,
    system_audio: system_audio::PermissionStatus,
}

#[tauri::command]
async fn get_permissions() -> Result<PermissionsState, String> {
    let microphone = tokio::task::spawn_blocking(permissions::microphone::check_status_fresh)
        .await
        .map_err(|e| e.to_string())?;
    let system_audio = tokio::task::spawn_blocking(system_audio::permission_status_fresh)
        .await
        .map_err(|e| e.to_string())?;
    Ok(PermissionsState {
        microphone,
        system_audio,
    })
}

#[tauri::command]
async fn request_microphone_permission() -> Result<bool, String> {
    tokio::task::spawn_blocking(permissions::microphone::request_access)
        .await
        .map_err(|e| e.to_string())
}

/// Start a tap for a moment: the first tap is what makes macOS ask for
/// "System Audio Recording" access.
#[tauri::command]
async fn request_system_audio_permission(
    state: State<'_, AppState>,
) -> Result<system_audio::PermissionStatus, String> {
    if state.recording_id().is_some() {
        return Ok(system_audio::permission_status_fresh());
    }
    tokio::task::spawn_blocking(|| {
        let (tx, _rx) = crossbeam_channel::unbounded();
        let capture = SystemAudioCapture::start(tx)?;
        std::thread::sleep(Duration::from_millis(300));
        capture.stop();
        Ok::<_, String>(system_audio::permission_status_fresh())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
fn open_system_settings(panel: String) -> Result<(), String> {
    permissions::open_settings_pane(&panel)
}

// --- Windows & tray ---

fn show_main_window(app: &AppHandle) {
    #[cfg(target_os = "macos")]
    let _ = app.set_activation_policy(tauri::ActivationPolicy::Regular);
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}

/// Show the floating widget without taking focus from the call app.
fn show_widget(app: &AppHandle) {
    let Some(window) = app.get_webview_window("widget") else {
        return;
    };
    #[cfg(target_os = "macos")]
    {
        if let Ok(ns_window) = window.ns_window() {
            let ns_window = ns_window as usize;
            let _ = window.run_on_main_thread(move || unsafe {
                let ns_window = ns_window as *mut objc2::runtime::AnyObject;
                let _: () = objc2::msg_send![ns_window, orderFrontRegardless];
            });
            return;
        }
    }
    let _ = window.show();
}

#[tauri::command]
fn show_recorder_widget(app: AppHandle) {
    show_widget(&app);
}

#[tauri::command]
fn hide_recorder_widget(app: AppHandle) {
    if let Some(window) = app.get_webview_window("widget") {
        let _ = window.hide();
    }
}

#[tauri::command]
fn open_library(app: AppHandle, meeting_id: Option<String>) {
    show_main_window(&app);
    if let Some(id) = meeting_id {
        let _ = app.emit("open-meeting", id);
    }
}

fn build_tray_menu(app: &AppHandle, status: &RecorderStatus) -> tauri::Result<Menu<tauri::Wry>> {
    let recording = status.state != RecorderState::Idle;
    let toggle = MenuItem::with_id(
        app,
        "toggle",
        if recording {
            "Stop recording"
        } else {
            "Start recording"
        },
        true,
        None::<&str>,
    )?;
    let pause = MenuItem::with_id(
        app,
        "pause",
        if status.state == RecorderState::Paused {
            "Resume"
        } else {
            "Pause"
        },
        recording,
        None::<&str>,
    )?;
    let widget = MenuItem::with_id(app, "widget", "Show recorder", true, None::<&str>)?;
    let library = MenuItem::with_id(app, "library", "Open library", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit Parley", true, None::<&str>)?;
    let sep1 = PredefinedMenuItem::separator(app)?;
    let sep2 = PredefinedMenuItem::separator(app)?;
    Menu::with_items(
        app,
        &[&toggle, &pause, &sep1, &widget, &library, &sep2, &quit],
    )
}

fn refresh_tray(app: &AppHandle, status: &RecorderStatus) {
    let Some(tray) = app.tray_by_id(TRAY_ID) else {
        return;
    };
    let recording = status.state != RecorderState::Idle;
    if let Ok(menu) = build_tray_menu(app, status) {
        let _ = tray.set_menu(Some(menu));
    }
    let icon = if recording {
        TRAY_ICON_RECORDING
    } else {
        TRAY_ICON_IDLE
    };
    if let Ok(image) = tauri::image::Image::from_bytes(icon) {
        let _ = tray.set_icon(Some(image));
        // The idle icon follows the menu bar's light/dark style; the red
        // recording dot must keep its colour.
        let _ = tray.set_icon_as_template(!recording);
    }
    let _ = tray.set_tooltip(Some(match status.state {
        RecorderState::Idle => "Parley",
        RecorderState::Recording => "Parley — Recording",
        RecorderState::Paused => "Parley — Paused",
    }));
}

fn on_tray_menu(app: &AppHandle, id: &str) {
    match id {
        "toggle" => {
            let app = app.clone();
            tauri::async_runtime::spawn(async move { toggle_recording(&app).await });
        }
        "pause" => {
            let state = app.state::<AppState>().inner().clone();
            let paused = state.status().state == RecorderState::Paused;
            if let Err(e) = set_paused(app, &state, !paused) {
                log::warn!("[tray] {e}");
            }
        }
        "widget" => show_widget(app),
        "library" => show_main_window(app),
        "quit" => {
            let state = app.state::<AppState>().inner().clone();
            if state.recording_id().is_some() {
                // Never lose a meeting on quit: finalize the files first. It is
                // transcribed (via Retry) after the next launch.
                if let Err(e) = stop_session(&state) {
                    log::error!("[recorder] finalizing on quit: {e}");
                }
            }
            app.exit(0);
        }
        _ => {}
    }
}

fn main() {
    // Fresh microphone status probe (see `permissions::microphone::check_status_fresh`).
    if std::env::args().any(|a| a == "--check-mic") {
        print!("{}", permissions::microphone::check_status());
        std::process::exit(0);
    }
    // Fresh System Audio Recording status probe (see `system_audio::permission_status_fresh`).
    if std::env::args().any(|a| a == "--check-system-audio") {
        print!("{}", system_audio::permission_status().as_str());
        std::process::exit(0);
    }

    tauri::Builder::default()
        .plugin(
            tauri_plugin_log::Builder::new()
                .targets([
                    tauri_plugin_log::Target::new(tauri_plugin_log::TargetKind::LogDir {
                        file_name: Some("parley".into()),
                    }),
                    tauri_plugin_log::Target::new(tauri_plugin_log::TargetKind::Stdout),
                ])
                .level(log::LevelFilter::Info)
                .build(),
        )
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            let state = AppState {
                audio: Arc::new(AudioCapture::new()),
                session: Arc::new(Mutex::new(None)),
                job_lock: Arc::new(TokioMutex::new(())),
                jobs: Arc::new(Mutex::new(HashSet::new())),
            };
            app.manage(state);
            widget_window::init(app.handle());

            let data_dir = paths::data_dir();
            let _ = app.asset_protocol_scope().allow_directory(&data_dir, true);
            match db().and_then(|conn| library::recover(&data_dir, &conn)) {
                Ok(ids) if !ids.is_empty() => log::warn!("[recover] recovered meetings: {ids:?}"),
                Ok(_) => {}
                Err(e) => log::error!("[recover] {e}"),
            }

            let menu = build_tray_menu(app.handle(), &RecorderStatus::idle())?;
            TrayIconBuilder::with_id(TRAY_ID)
                .icon(tauri::image::Image::from_bytes(TRAY_ICON_IDLE)?)
                .icon_as_template(true)
                .tooltip("Parley")
                .menu(&menu)
                .show_menu_on_left_click(true)
                .on_menu_event(|app, event| on_tray_menu(app, event.id.as_ref()))
                .build(app)?;

            apply_shortcut(app.handle());

            if let Some(window) = app.get_webview_window("main") {
                let w = window.clone();
                window.on_window_event(move |event| {
                    if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                        api.prevent_close();
                        let _ = w.hide();
                        #[cfg(target_os = "macos")]
                        let _ = w
                            .app_handle()
                            .set_activation_policy(tauri::ActivationPolicy::Accessory);
                    }
                });
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            config::reset_corrupted_config,
            get_recorder_status,
            start_recording,
            stop_recording,
            pause_recording,
            resume_recording,
            retranscribe_meeting,
            mark_moment,
            begin_note,
            add_marker,
            update_marker,
            delete_marker,
            set_widget_input,
            list_meetings,
            get_meeting,
            get_meeting_markdown,
            rename_meeting,
            delete_meeting,
            reveal_meeting,
            list_profiles,
            create_profile,
            update_profile,
            delete_profile,
            set_active_profile,
            test_profile_connection,
            get_languages,
            save_languages,
            vocabulary::save_custom_vocabulary,
            vocabulary::get_custom_vocabulary,
            vocabulary::export_vocabulary_csv,
            vocabulary::import_vocabulary_csv,
            get_recording_settings,
            save_recording_settings,
            list_audio_device_info,
            get_storage_info,
            choose_storage_dir,
            move_storage,
            get_mcp_info,
            get_permissions,
            request_microphone_permission,
            request_system_audio_permission,
            open_system_settings,
            show_recorder_widget,
            hide_recorder_widget,
            open_library,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_quote_only_quotes_when_needed() {
        assert_eq!(
            shell_quote("/Applications/Parley.app/Contents/MacOS/parley-mcp"),
            "/Applications/Parley.app/Contents/MacOS/parley-mcp"
        );
        assert_eq!(
            shell_quote("/Users/a b/parley-mcp"),
            "'/Users/a b/parley-mcp'"
        );
        assert_eq!(shell_quote("/x/it's"), "'/x/it'\\''s'");
    }

    #[test]
    fn idle_status_is_empty() {
        let s = RecorderStatus::idle();
        assert_eq!(s.state, RecorderState::Idle);
        assert!(s.meeting_id.is_none());
        let v = serde_json::to_value(&s).unwrap();
        assert_eq!(v["state"], "idle");
        assert_eq!(v["marker_count"], 0);
    }
}
