//! Meeting files on disk: folder layout, transcript.md, deletion, crash
//! recovery and moving the data directory.

use chrono::Utc;
use parley_lib::store::{self, Meeting, MeetingStatus};
use parley_lib::{markdown, paths, wav};
use std::fs;
use std::path::{Component, Path, PathBuf};

#[derive(Debug, Clone, serde::Serialize)]
pub struct MeetingFiles {
    pub folder: PathBuf,
    pub mic: Option<PathBuf>,
    pub system: Option<PathBuf>,
    pub mixed: Option<PathBuf>,
    pub transcript: Option<PathBuf>,
}

/// The folder recorded in the database, refusing anything that could escape
/// `<data>/meetings/` (a hand-edited or corrupted row must never make Parley
/// delete files elsewhere).
pub fn meeting_folder(data_dir: &Path, meeting: &Meeting) -> Result<PathBuf, String> {
    let rel = Path::new(&meeting.folder);
    let mut components = rel.components();
    let safe = matches!(components.next(), Some(Component::Normal(first)) if first == paths::MEETINGS_DIR_NAME)
        && matches!(components.next(), Some(Component::Normal(_)))
        && components.next().is_none();
    if !safe {
        return Err(format!(
            "Refusing to use unexpected meeting folder {:?}",
            meeting.folder
        ));
    }
    Ok(data_dir.join(rel))
}

pub fn files(data_dir: &Path, meeting: &Meeting) -> Result<MeetingFiles, String> {
    let folder = meeting_folder(data_dir, meeting)?;
    let existing = |name: &str| {
        let p = folder.join(name);
        p.is_file().then_some(p)
    };
    Ok(MeetingFiles {
        mic: existing(store::MIC_FILE),
        system: existing(store::SYSTEM_FILE),
        mixed: existing(store::MIXED_FILE),
        transcript: existing(store::TRANSCRIPT_FILE),
        folder,
    })
}

/// (Re)write `transcript.md` from the database.
pub fn write_transcript(
    data_dir: &Path,
    conn: &rusqlite::Connection,
    id: &str,
) -> Result<PathBuf, String> {
    let meeting = store::require_meeting(conn, id)?;
    let segments = store::segments(conn, id)?;
    let markers = store::markers(conn, id)?;
    let folder = meeting_folder(data_dir, &meeting)?;
    fs::create_dir_all(&folder).map_err(|e| e.to_string())?;
    let path = folder.join(store::TRANSCRIPT_FILE);
    let tmp = folder.join(".transcript.md.tmp");
    fs::write(&tmp, markdown::render(&meeting, &segments, &markers)).map_err(|e| e.to_string())?;
    fs::rename(&tmp, &path).map_err(|e| e.to_string())?;
    Ok(path)
}

/// Mix the two tracks into `mixed.wav` for playback.
pub fn write_mixed(folder: &Path) -> Result<(), String> {
    let read = |name: &str| {
        let p = folder.join(name);
        if p.is_file() {
            wav::read_wav(&p)
                .map(|w| w.samples)
                .map_err(|e| e.to_string())
        } else {
            Ok(Vec::new())
        }
    };
    let mixed = wav::mix(&read(store::MIC_FILE)?, &read(store::SYSTEM_FILE)?);
    wav::write_wav(
        &folder.join(store::MIXED_FILE),
        &mixed,
        crate::recorder::SAMPLE_RATE,
    )
    .map_err(|e| e.to_string())
}

pub fn delete(data_dir: &Path, conn: &rusqlite::Connection, id: &str) -> Result<(), String> {
    let meeting = store::require_meeting(conn, id)?;
    let folder = meeting_folder(data_dir, &meeting)?;
    store::delete_meeting(conn, id)?;
    if folder.exists() {
        fs::remove_dir_all(&folder).map_err(|e| {
            format!(
                "The meeting was removed, but its files could not be deleted ({e}): {}",
                folder.display()
            )
        })?;
    }
    Ok(())
}

pub const INTERRUPTED_RECORDING: &str =
    "Parley quit while recording. The audio up to that point was recovered — use Retry to transcribe it.";
pub const INTERRUPTED_TRANSCRIPTION: &str =
    "Parley quit while transcribing — use Retry to transcribe again.";

/// After a crash or forced quit: repair the WAV headers of meetings left in
/// `recording`, close them, and mark interrupted transcriptions as failed so
/// they can be retried. Returns the ids that were recovered.
pub fn recover(data_dir: &Path, conn: &rusqlite::Connection) -> Result<Vec<String>, String> {
    let mut recovered = Vec::new();
    for meeting in store::meetings_with_status(conn, MeetingStatus::Recording)? {
        let folder = match meeting_folder(data_dir, &meeting) {
            Ok(f) => f,
            Err(e) => {
                log::warn!("[recover] {e}");
                continue;
            }
        };
        let mut samples = 0u64;
        for name in [store::MIC_FILE, store::SYSTEM_FILE] {
            let path = folder.join(name);
            if path.is_file() {
                match wav::repair_header(&path) {
                    Ok(n) => samples = samples.max(n),
                    Err(e) => log::warn!("[recover] {}: {e}", path.display()),
                }
            }
        }
        let duration_ms = (samples * 1000 / crate::recorder::SAMPLE_RATE as u64) as i64;
        let ended = chrono::DateTime::parse_from_rfc3339(&meeting.started_at)
            .map(|s| s.with_timezone(&Utc) + chrono::Duration::milliseconds(duration_ms))
            .unwrap_or_else(|_| Utc::now());
        store::finish_recording(conn, &meeting.id, ended, duration_ms)?;
        if let Err(e) = write_mixed(&folder) {
            log::warn!("[recover] mixing {}: {e}", meeting.id);
        }
        store::set_status(
            conn,
            &meeting.id,
            MeetingStatus::Failed,
            Some(INTERRUPTED_RECORDING),
        )?;
        recovered.push(meeting.id);
    }
    for meeting in store::meetings_with_status(conn, MeetingStatus::Transcribing)? {
        store::set_status(
            conn,
            &meeting.id,
            MeetingStatus::Failed,
            Some(INTERRUPTED_TRANSCRIPTION),
        )?;
        recovered.push(meeting.id);
    }
    Ok(recovered)
}

fn copy_dir(from: &Path, to: &Path) -> std::io::Result<()> {
    fs::create_dir_all(to)?;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir(&entry.path(), &target)?;
        } else {
            fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}

fn move_path(from: &Path, to: &Path) -> std::io::Result<()> {
    match fs::rename(from, to) {
        Ok(()) => Ok(()),
        // Different volume: copy, then remove the original.
        Err(_) => {
            if from.is_dir() {
                copy_dir(from, to)?;
                fs::remove_dir_all(from)
            } else {
                fs::copy(from, to)?;
                fs::remove_file(from)
            }
        }
    }
}

/// Move the database and meeting folders from `from` to `to`. The target must
/// not already contain Parley data. Other files in `from` (config.json, logs)
/// stay where they are.
pub fn move_data(from: &Path, to: &Path) -> Result<(), String> {
    if from == to {
        return Ok(());
    }
    if to.starts_with(from.join(paths::MEETINGS_DIR_NAME)) {
        return Err("The new location can't be inside the current meetings folder".to_string());
    }
    let db = paths::db_path_in(to);
    if db.exists() || to.join(paths::MEETINGS_DIR_NAME).exists() {
        return Err(format!(
            "{} already contains Parley data; choose an empty folder",
            to.display()
        ));
    }
    fs::create_dir_all(to).map_err(|e| format!("Can't create {}: {e}", to.display()))?;
    let names = [
        paths::DB_FILE_NAME.to_string(),
        format!("{}-wal", paths::DB_FILE_NAME),
        format!("{}-shm", paths::DB_FILE_NAME),
        paths::MEETINGS_DIR_NAME.to_string(),
    ];
    for name in names {
        let src = from.join(&name);
        if src.exists() {
            move_path(&src, &to.join(&name))
                .map_err(|e| format!("Moving {} failed: {e}", src.display()))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use parley_lib::store::{NewMeeting, Segment};

    fn setup() -> (tempfile::TempDir, rusqlite::Connection) {
        let dir = tempfile::tempdir().unwrap();
        let conn = store::open_rw(&paths::db_path_in(dir.path())).unwrap();
        (dir, conn)
    }

    fn add(conn: &rusqlite::Connection, data: &Path, id: &str) -> PathBuf {
        let folder = format!("meetings/{id}");
        store::insert_meeting(
            conn,
            &NewMeeting {
                id,
                title: "Sync",
                started_at: Utc::now(),
                folder: &folder,
            },
        )
        .unwrap();
        let abs = data.join(&folder);
        fs::create_dir_all(&abs).unwrap();
        abs
    }

    #[test]
    fn folders_outside_meetings_are_refused() {
        let (dir, conn) = setup();
        add(&conn, dir.path(), "ok");
        let mut m = store::require_meeting(&conn, "ok").unwrap();
        assert!(meeting_folder(dir.path(), &m).is_ok());
        for bad in [
            "../etc",
            "meetings/../../x",
            "/tmp/x",
            "meetings",
            "other/x",
            "meetings/a/b",
            "",
        ] {
            m.folder = bad.to_string();
            assert!(meeting_folder(dir.path(), &m).is_err(), "{bad}");
        }
    }

    #[test]
    fn delete_removes_row_and_folder() {
        let (dir, conn) = setup();
        let folder = add(&conn, dir.path(), "m1");
        fs::write(folder.join("mic.wav"), b"x").unwrap();
        delete(dir.path(), &conn, "m1").unwrap();
        assert!(!folder.exists());
        assert!(store::get_meeting(&conn, "m1").unwrap().is_none());
        assert!(delete(dir.path(), &conn, "m1").is_err());
    }

    #[test]
    fn transcript_and_files_are_written_and_listed() {
        let (dir, mut conn) = setup();
        let folder = add(&conn, dir.path(), "m1");
        store::save_transcript(
            &mut conn,
            "m1",
            &[Segment {
                start_ms: 0,
                end_ms: 1000,
                speaker: "Me".into(),
                text: "Hello".into(),
            }],
        )
        .unwrap();
        store::insert_marker(&conn, "m1", store::MarkerKind::Note, 500, Some("Key point")).unwrap();
        let path = write_transcript(dir.path(), &conn, "m1").unwrap();
        let md = fs::read_to_string(&path).unwrap();
        assert!(md.contains("**Me** [00:00:00]\nHello"));
        assert!(md.contains("> 📝 **Note** [00:00:00]: Key point"), "{md}");
        wav::write_wav(&folder.join("mic.wav"), &[1000; 1600], 16_000).unwrap();
        wav::write_wav(&folder.join("system.wav"), &[-500; 800], 16_000).unwrap();
        write_mixed(&folder).unwrap();
        let mixed = wav::read_wav(&folder.join("mixed.wav")).unwrap();
        assert_eq!(mixed.samples.len(), 1600);
        assert_eq!(mixed.samples[0], 500);
        assert_eq!(mixed.samples[1599], 1000);
        let f = files(dir.path(), &store::require_meeting(&conn, "m1").unwrap()).unwrap();
        assert!(
            f.mic.is_some() && f.system.is_some() && f.mixed.is_some() && f.transcript.is_some()
        );
    }

    #[test]
    fn recovery_repairs_interrupted_recordings_and_transcriptions() {
        let (dir, conn) = setup();
        let folder = add(&conn, dir.path(), "crashed");
        // A WAV whose header was never finalized: 2 s of audio.
        let mut bytes = wav::wav_bytes(&vec![700i16; 32_000], 16_000);
        bytes[40..44].copy_from_slice(&0u32.to_le_bytes());
        fs::write(folder.join("mic.wav"), &bytes).unwrap();
        add(&conn, dir.path(), "busy");
        store::finish_recording(&conn, "busy", Utc::now(), 1000).unwrap();
        add(&conn, dir.path(), "fine");
        store::set_status(&conn, "fine", MeetingStatus::Ready, None).unwrap();

        let mut ids = recover(dir.path(), &conn).unwrap();
        ids.sort();
        assert_eq!(ids, vec!["busy", "crashed"]);
        let crashed = store::require_meeting(&conn, "crashed").unwrap();
        assert_eq!(crashed.status, MeetingStatus::Failed);
        assert_eq!(crashed.duration_ms, 2000);
        assert_eq!(crashed.error.as_deref(), Some(INTERRUPTED_RECORDING));
        assert_eq!(
            wav::read_wav(&folder.join("mic.wav"))
                .unwrap()
                .samples
                .len(),
            32_000
        );
        assert!(folder.join("mixed.wav").exists());
        let busy = store::require_meeting(&conn, "busy").unwrap();
        assert_eq!(busy.error.as_deref(), Some(INTERRUPTED_TRANSCRIPTION));
        assert_eq!(
            store::require_meeting(&conn, "fine").unwrap().status,
            MeetingStatus::Ready
        );
        // Idempotent.
        assert!(recover(dir.path(), &conn).unwrap().is_empty());
    }

    #[test]
    fn move_data_moves_db_and_meetings_but_not_config() {
        let (from, conn) = setup();
        add(&conn, from.path(), "m1");
        fs::write(from.path().join("meetings/m1/mic.wav"), b"audio").unwrap();
        fs::write(from.path().join("config.json"), b"{}").unwrap();
        drop(conn);
        let target = tempfile::tempdir().unwrap();
        let to = target.path().join("Parley data");
        move_data(from.path(), &to).unwrap();
        assert!(paths::db_path_in(&to).exists());
        assert_eq!(fs::read(to.join("meetings/m1/mic.wav")).unwrap(), b"audio");
        assert!(!from.path().join("meetings").exists());
        assert!(from.path().join("config.json").exists());
        let conn = store::open_rw(&paths::db_path_in(&to)).unwrap();
        assert!(store::get_meeting(&conn, "m1").unwrap().is_some());
        // A folder that already holds Parley data is refused.
        assert!(move_data(from.path(), &to).is_err());
        // Same place is a no-op; inside meetings/ is refused.
        assert!(move_data(&to, &to).is_ok());
        assert!(move_data(&to, &to.join("meetings/x")).is_err());
    }
}
