//! Batch transcription of a finished meeting: both tracks go through the
//! chosen profile's provider, the results are merged into one time-ordered
//! transcript ("Me" + "Them"), stored, and written to `transcript.md`.

use crate::batch_stt::{self, RequestOptions};
use crate::config::{Profile, ProfileSettings};
use crate::gladia::{GladiaClient, GladiaOptions};
use crate::library;
use crate::whisper_local::WhisperServer;
use crate::vocabulary::CustomVocabEntry;
use parley_lib::merge::{self, TrackSegment};
use parley_lib::store::{self, MeetingStatus};
use parley_lib::wav;
use std::io::{BufRead, Write};
use std::path::Path;

#[derive(Debug, Clone, serde::Serialize, PartialEq)]
pub struct Progress {
    pub meeting_id: String,
    /// "me" or "them".
    pub track: &'static str,
    pub done: usize,
    pub total: usize,
}

pub struct Job<'a> {
    pub data_dir: &'a Path,
    pub meeting_id: &'a str,
    pub profile: Profile,
    pub languages: Vec<String>,
    pub vocabulary: Vec<CustomVocabEntry>,
}

/// Utterances finished by an interrupted run, so a retry resumes instead of
/// starting over. One JSON object per line: a header naming the settings that
/// produced them, then `{"track","start","end","text"}` per utterance. Removed
/// once the transcript is saved.
pub const CHECKPOINT_FILE: &str = "transcription.partial.jsonl";

#[derive(serde::Serialize, serde::Deserialize)]
struct CheckpointHeader {
    settings: String,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct CheckpointEntry {
    track: String,
    start: usize,
    end: usize,
    segments: Vec<CheckpointSegment>,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct CheckpointSegment {
    start_ms: i64,
    end_ms: i64,
    text: String,
}

struct Checkpoint {
    file: Option<std::fs::File>,
}

impl Checkpoint {
    /// Open the checkpoint for `settings`, returning what it already holds per
    /// track. A checkpoint written with other settings is discarded.
    fn open(folder: &Path, settings: &str) -> (Self, batch_stt::Resume, batch_stt::Resume) {
        let path = folder.join(CHECKPOINT_FILE);
        let (mut me, mut them) = (batch_stt::Resume::new(), batch_stt::Resume::new());
        let mut matches = false;
        if let Ok(file) = std::fs::File::open(&path) {
            let mut lines = std::io::BufReader::new(file).lines().map_while(Result::ok);
            matches = lines
                .next()
                .and_then(|l| serde_json::from_str::<CheckpointHeader>(&l).ok())
                .is_some_and(|h| h.settings == settings);
            if matches {
                // A torn last line from a crash is skipped.
                for entry in lines.filter_map(|l| serde_json::from_str::<CheckpointEntry>(&l).ok()) {
                    let target = if entry.track == "me" { &mut me } else { &mut them };
                    let segments = entry
                        .segments
                        .into_iter()
                        .map(|s| TrackSegment {
                            start_ms: s.start_ms,
                            end_ms: s.end_ms,
                            text: s.text,
                            speaker: None,
                        })
                        .collect();
                    target.insert((entry.start, entry.end), segments);
                }
            }
        }
        let file = if matches {
            std::fs::OpenOptions::new().append(true).open(&path).ok()
        } else {
            std::fs::File::create(&path).ok().and_then(|mut f| {
                let header = serde_json::to_string(&CheckpointHeader {
                    settings: settings.to_string(),
                })
                .ok()?;
                writeln!(f, "{header}").ok()?;
                Some(f)
            })
        };
        if file.is_none() {
            log::warn!("[transcribe] can't write {}; progress won't be resumable", path.display());
        }
        (Self { file }, me, them)
    }

    fn record(&mut self, track: &str, start: usize, end: usize, segments: &[TrackSegment]) {
        let Some(file) = self.file.as_mut() else {
            return;
        };
        let entry = CheckpointEntry {
            track: track.to_string(),
            start,
            end,
            segments: segments
                .iter()
                .map(|s| CheckpointSegment {
                    start_ms: s.start_ms,
                    end_ms: s.end_ms,
                    text: s.text.clone(),
                })
                .collect(),
        };
        if let Ok(line) = serde_json::to_string(&entry) {
            let _ = writeln!(file, "{line}");
        }
    }

}

/// Keeps the Mac from idle-sleeping while a long meeting transcribes.
struct KeepAwake(Option<std::process::Child>);

impl KeepAwake {
    fn start() -> Self {
        #[cfg(target_os = "macos")]
        {
            let pid = std::process::id().to_string();
            match std::process::Command::new("/usr/bin/caffeinate")
                .args(["-i", "-w", &pid])
                .spawn()
            {
                Ok(child) => return Self(Some(child)),
                Err(e) => log::warn!("[transcribe] can't start caffeinate: {e}"),
            }
        }
        Self(None)
    }
}

impl Drop for KeepAwake {
    fn drop(&mut self) {
        if let Some(child) = self.0.as_mut() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

fn read_track(path: Option<std::path::PathBuf>) -> Result<(Vec<i16>, u32), String> {
    match path {
        Some(p) => wav::read_wav(&p)
            .map(|w| (w.samples, w.sample_rate))
            .map_err(|e| format!("Can't read {}: {e}", p.display())),
        None => Ok((Vec::new(), crate::recorder::SAMPLE_RATE)),
    }
}

async fn transcribe_tracks(
    job: &Job<'_>,
    folder: &Path,
    mic: &(Vec<i16>, u32),
    system: &(Vec<i16>, u32),
    progress: &(dyn Fn(Progress) + Send + Sync),
) -> Result<(Vec<TrackSegment>, Vec<TrackSegment>), String> {
    let report = |track: &'static str| {
        move |done, total| {
            progress(Progress {
                meeting_id: job.meeting_id.to_string(),
                track,
                done,
                total,
            })
        }
    };
    match &job.profile.settings {
        ProfileSettings::OpenAiCompat { .. } => {
            let settings = job
                .profile
                .settings
                .batch_settings()
                .expect("OpenAI-compatible profiles have batch settings");
            transcribe_batch(job, folder, &settings, mic, system, &report).await
        }
        ProfileSettings::WhisperLocal { model_path } => {
            let language = match job.languages.as_slice() {
                [only] => Some(only.as_str()),
                _ => None,
            };
            // Stopped when this arm returns.
            let server = WhisperServer::start(model_path, language).await?;
            transcribe_batch(job, folder, &server.settings, mic, system, &report).await
        }
        ProfileSettings::Gladia {
            api_key,
            code_switching,
        } => {
            let client = GladiaClient::default();
            let base = GladiaOptions {
                languages: job.languages.clone(),
                code_switching: *code_switching,
                vocabulary: job.vocabulary.clone(),
                diarization: false,
            };
            let me = client
                .transcribe_track(api_key, &base, &mic.0, mic.1, report("me"))
                .await?;
            let them_options = GladiaOptions {
                diarization: true,
                ..base
            };
            let them = client
                .transcribe_track(api_key, &them_options, &system.0, system.1, report("them"))
                .await?;
            Ok((me, them))
        }
    }
}

/// Both tracks through an OpenAI-compatible server, checkpointing each chunk
/// so an interrupted job resumes where it stopped.
async fn transcribe_batch<R, F>(
    job: &Job<'_>,
    folder: &Path,
    settings: &batch_stt::BatchSettings,
    mic: &(Vec<i16>, u32),
    system: &(Vec<i16>, u32),
    report: &R,
) -> Result<(Vec<TrackSegment>, Vec<TrackSegment>), String>
where
    R: Fn(&'static str) -> F,
    F: FnMut(usize, usize),
{
    let options = RequestOptions::from_session(&job.languages, job.vocabulary.clone());
    let client = batch_stt::http_client();
    // What shapes the text: a checkpoint from another model is not reused.
    let key = format!(
        "{}|{:?}|{:?}|{:?}",
        settings.model, settings.chunking, options.language, options.prompt
    );
    let (mut checkpoint, resume_me, resume_them) = Checkpoint::open(folder, &key);
    if !resume_me.is_empty() || !resume_them.is_empty() {
        log::info!(
            "[transcribe] {}: resuming with {} chunks already done",
            job.meeting_id,
            resume_me.len() + resume_them.len()
        );
    }
    let me = batch_stt::transcribe_track(
        &client,
        settings,
        &options,
        &mic.0,
        mic.1,
        &resume_me,
        |start, end, segments| checkpoint.record("me", start, end, segments),
        report("me"),
    )
    .await?;
    let them = batch_stt::transcribe_track(
        &client,
        settings,
        &options,
        &system.0,
        system.1,
        &resume_them,
        |start, end, segments| checkpoint.record("them", start, end, segments),
        report("them"),
    )
    .await?;
    Ok((me, them))
}

/// Run one transcription job. On failure the meeting is marked `failed` with
/// the error (and can be retried); the error is also returned.
pub async fn run(job: Job<'_>, progress: &(dyn Fn(Progress) + Send + Sync)) -> Result<(), String> {
    {
        // Problems with the request itself leave the meeting untouched.
        let conn = store::open_rw(&parley_lib::paths::db_path_in(job.data_dir))?;
        let meeting = store::require_meeting(&conn, job.meeting_id)?;
        if meeting.status == MeetingStatus::Recording {
            return Err("This meeting is still recording".to_string());
        }
    }
    let result = run_inner(&job, progress).await;
    if let Err(error) = &result {
        log::error!("[transcribe] {}: {error}", job.meeting_id);
        if let Ok(conn) = store::open_rw(&parley_lib::paths::db_path_in(job.data_dir)) {
            let _ = store::set_status(&conn, job.meeting_id, MeetingStatus::Failed, Some(error));
        }
    }
    result
}

async fn run_inner(
    job: &Job<'_>,
    progress: &(dyn Fn(Progress) + Send + Sync),
) -> Result<(), String> {
    let db = parley_lib::paths::db_path_in(job.data_dir);
    let (mic_path, system_path, folder) = {
        let conn = store::open_rw(&db)?;
        let meeting = store::require_meeting(&conn, job.meeting_id)?;
        store::set_status(&conn, job.meeting_id, MeetingStatus::Transcribing, None)?;
        let language = match job.languages.as_slice() {
            [only] => Some(only.as_str()),
            _ => None,
        };
        store::set_transcription_meta(
            &conn,
            job.meeting_id,
            &job.profile.name,
            job.profile.settings.kind(),
            language,
        )?;
        let files = library::files(job.data_dir, &meeting)?;
        (files.mic, files.system, files.folder)
    };
    if mic_path.is_none() && system_path.is_none() {
        return Err("This meeting has no audio files".to_string());
    }
    if !folder.join(store::MIXED_FILE).exists() {
        let f = folder.clone();
        if let Err(e) = tokio::task::spawn_blocking(move || library::write_mixed(&f))
            .await
            .map_err(|e| e.to_string())?
        {
            log::warn!("[transcribe] could not write mixed.wav: {e}");
        }
    }
    let (mic, system) = tokio::task::spawn_blocking(move || {
        Ok::<_, String>((read_track(mic_path)?, read_track(system_path)?))
    })
    .await
    .map_err(|e| e.to_string())??;

    let started = std::time::Instant::now();
    let _awake = KeepAwake::start();
    let (me, them) = transcribe_tracks(job, &folder, &mic, &system, progress).await?;
    let segments = merge::merge(merge::label_me(me), merge::label_them(them));
    log::info!(
        "[transcribe] {}: {} segments in {}s with {}",
        job.meeting_id,
        segments.len(),
        started.elapsed().as_secs(),
        job.profile.name
    );
    let mut conn = store::open_rw(&db)?;
    store::save_transcript(&mut conn, job.meeting_id, &segments)?;
    library::write_transcript(job.data_dir, &conn, job.meeting_id)?;
    // Only now is every utterance safely stored.
    let _ = std::fs::remove_file(folder.join(CHECKPOINT_FILE));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use parley_lib::store::NewMeeting;

    fn speech(ms: u32) -> Vec<i16> {
        (0..(16 * ms) as usize)
            .map(|i| ((i as f32 * 0.2).sin() * 9000.0) as i16)
            .collect()
    }

    fn silence(ms: u32) -> Vec<i16> {
        vec![0; (16 * ms) as usize]
    }

    fn profile(base_url: &str) -> Profile {
        Profile {
            id: "p".into(),
            name: "Mock".into(),
            settings: ProfileSettings::OpenAiCompat {
                base_url: base_url.into(),
                model: "m".into(),
                api_key: String::new(),
            },
        }
    }

    fn meeting(dir: &Path, id: &str) -> std::path::PathBuf {
        let conn = store::open_rw(&parley_lib::paths::db_path_in(dir)).unwrap();
        let folder = format!("meetings/{id}");
        store::insert_meeting(
            &conn,
            &NewMeeting {
                id,
                title: "Call",
                started_at: Utc::now(),
                folder: &folder,
            },
        )
        .unwrap();
        store::finish_recording(&conn, id, Utc::now(), 6000).unwrap();
        let abs = dir.join(folder);
        std::fs::create_dir_all(&abs).unwrap();
        abs
    }

    #[tokio::test]
    async fn both_tracks_are_merged_labelled_and_written() {
        let dir = tempfile::tempdir().unwrap();
        let folder = meeting(dir.path(), "m1");
        // Me speaks at 0.5 s, Them at 3 s.
        wav::write_wav(
            &folder.join("mic.wav"),
            &[silence(500), speech(1000), silence(4500)].concat(),
            16_000,
        )
        .unwrap();
        wav::write_wav(
            &folder.join("system.wav"),
            &[silence(3000), speech(1500), silence(1500)].concat(),
            16_000,
        )
        .unwrap();
        let (base, _) = crate::test_http::mock_server(vec![
            (200, r#"{"text":"Hi, it's me."}"#),
            (200, r#"{"text":"Hello from the call."}"#),
        ])
        .await;
        let seen = std::sync::Mutex::new(Vec::new());
        run(
            Job {
                data_dir: dir.path(),
                meeting_id: "m1",
                profile: profile(&base),
                languages: vec!["en".into()],
                vocabulary: vec![],
            },
            &|p| seen.lock().unwrap().push((p.track, p.done, p.total)),
        )
        .await
        .unwrap();
        assert_eq!(*seen.lock().unwrap(), vec![("me", 1, 1), ("them", 1, 1)]);
        let conn = store::open_rw(&parley_lib::paths::db_path_in(dir.path())).unwrap();
        let m = store::require_meeting(&conn, "m1").unwrap();
        assert_eq!(m.status, MeetingStatus::Ready);
        assert_eq!(m.profile_name.as_deref(), Some("Mock"));
        assert_eq!(m.language.as_deref(), Some("en"));
        let segs = store::segments(&conn, "m1").unwrap();
        assert_eq!(
            segs.iter()
                .map(|s| (s.speaker.as_str(), s.text.as_str()))
                .collect::<Vec<_>>(),
            vec![("Me", "Hi, it's me."), ("Them", "Hello from the call.")]
        );
        let md = std::fs::read_to_string(folder.join("transcript.md")).unwrap();
        assert!(md.contains("participants: [\"Me\", \"Them\"]"));
        assert!(folder.join("mixed.wav").exists());
    }

    #[tokio::test]
    async fn provider_failure_marks_the_meeting_failed() {
        let dir = tempfile::tempdir().unwrap();
        let folder = meeting(dir.path(), "m1");
        wav::write_wav(&folder.join("mic.wav"), &speech(1000), 16_000).unwrap();
        let err = run(
            Job {
                data_dir: dir.path(),
                meeting_id: "m1",
                profile: profile("http://127.0.0.1:9/v1"),
                languages: vec![],
                vocabulary: vec![],
            },
            &|_| {},
        )
        .await
        .unwrap_err();
        assert!(err.contains("Can't reach"), "{err}");
        let conn = store::open_rw(&parley_lib::paths::db_path_in(dir.path())).unwrap();
        let m = store::require_meeting(&conn, "m1").unwrap();
        assert_eq!(m.status, MeetingStatus::Failed);
        assert_eq!(m.error.as_deref(), Some(err.as_str()));
    }

    #[tokio::test]
    async fn meetings_without_audio_or_still_recording_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        meeting(dir.path(), "empty");
        let job = |id: &'static str| Job {
            data_dir: dir.path(),
            meeting_id: id,
            profile: profile("http://127.0.0.1:9/v1"),
            languages: vec![],
            vocabulary: vec![],
        };
        assert_eq!(
            run(job("empty"), &|_| {}).await,
            Err("This meeting has no audio files".to_string())
        );
        let conn = store::open_rw(&parley_lib::paths::db_path_in(dir.path())).unwrap();
        store::insert_meeting(
            &conn,
            &NewMeeting {
                id: "live",
                title: "Live",
                started_at: Utc::now(),
                folder: "meetings/live",
            },
        )
        .unwrap();
        assert!(run(job("live"), &|_| {}).await.is_err());
        assert_eq!(
            store::require_meeting(&conn, "live").unwrap().status,
            MeetingStatus::Recording,
            "a live recording is never marked failed"
        );
        assert!(run(job("missing"), &|_| {}).await.is_err());
    }

    #[tokio::test]
    async fn a_retry_resumes_from_the_checkpoint_and_then_removes_it() {
        let dir = tempfile::tempdir().unwrap();
        let folder = meeting(dir.path(), "m1");
        wav::write_wav(
            &folder.join("mic.wav"),
            &[silence(500), speech(1000), silence(4500)].concat(),
            16_000,
        )
        .unwrap();
        wav::write_wav(
            &folder.join("system.wav"),
            &[silence(3000), speech(1500), silence(1500)].concat(),
            16_000,
        )
        .unwrap();
        let job = |base: &str| Job {
            data_dir: dir.path(),
            meeting_id: "m1",
            profile: profile(base),
            languages: vec![],
            vocabulary: vec![],
        };

        // First run: "Me" succeeds, then the server breaks on "Them".
        let (base, _) = crate::test_http::mock_server(vec![
            (200, r#"{"text":"Kept from run one."}"#),
            (404, r#"{"error":"model 'm' not found"}"#),
        ])
        .await;
        assert!(run(job(&base), &|_| {}).await.is_err());
        assert!(folder.join(CHECKPOINT_FILE).exists());

        // Retry: only "Them" is sent.
        let (base, seen) =
            crate::test_http::mock_server(vec![(200, r#"{"text":"From run two."}"#)]).await;
        run(job(&base), &|_| {}).await.unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(seen.lock().unwrap().len(), 1);
        assert!(!folder.join(CHECKPOINT_FILE).exists());
        let md = std::fs::read_to_string(folder.join(store::TRANSCRIPT_FILE)).unwrap();
        assert!(md.contains("Kept from run one.") && md.contains("From run two."), "{md}");
    }

    /// Full pipeline on a real meeting with local Whisper. Run with:
    /// `PARLEY_MEETING_DIR=/path/to/meeting PARLEY_WHISPER_MODEL=/path/ggml.bin PARLEY_OUT=/tmp/out.md cargo test --manifest-path src-tauri/Cargo.toml -- --ignored whisper_real --nocapture`
    #[tokio::test]
    #[ignore]
    async fn whisper_real_meeting() {
        let src = std::path::PathBuf::from(std::env::var("PARLEY_MEETING_DIR").unwrap());
        let dir = tempfile::tempdir().unwrap();
        let folder = meeting(dir.path(), "real");
        for f in ["mic.wav", "system.wav"] {
            std::fs::copy(src.join(f), folder.join(f)).unwrap();
        }
        let started = std::time::Instant::now();
        run(
            Job {
                data_dir: dir.path(),
                meeting_id: "real",
                profile: Profile {
                    id: "w".into(),
                    name: "Local Whisper".into(),
                    settings: ProfileSettings::WhisperLocal {
                        model_path: std::env::var("PARLEY_WHISPER_MODEL").unwrap(),
                    },
                },
                languages: vec!["en".into(), "es".into()],
                vocabulary: vec![],
            },
            &|p| println!("{} {}/{}", p.track, p.done, p.total),
        )
        .await
        .unwrap();
        println!("total: {}s", started.elapsed().as_secs());
        let md = std::fs::read_to_string(folder.join(store::TRANSCRIPT_FILE)).unwrap();
        std::fs::write(std::env::var("PARLEY_OUT").unwrap(), md).unwrap();
    }
}
