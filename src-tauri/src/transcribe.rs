//! Batch transcription of a finished meeting: both tracks go through the
//! chosen profile's provider, the results are merged into one time-ordered
//! transcript ("Me" + "Them"), stored, and written to `transcript.md`.

use crate::batch_stt::{self, RequestOptions};
use crate::config::{Profile, ProfileSettings};
use crate::gladia::{GladiaClient, GladiaOptions};
use crate::library;
use crate::vocabulary::CustomVocabEntry;
use parley_lib::merge::{self, TrackSegment};
use parley_lib::store::{self, MeetingStatus};
use parley_lib::wav;
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
            let options = RequestOptions::from_session(&job.languages, job.vocabulary.clone());
            let client = batch_stt::http_client();
            let me = batch_stt::transcribe_track(
                &client,
                &settings,
                &options,
                &mic.0,
                mic.1,
                report("me"),
            )
            .await?;
            let them = batch_stt::transcribe_track(
                &client,
                &settings,
                &options,
                &system.0,
                system.1,
                report("them"),
            )
            .await?;
            Ok((me, them))
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
    let (me, them) = transcribe_tracks(job, &mic, &system, progress).await?;
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
}
