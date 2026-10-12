//! SQLite storage for meetings, transcript segments (with FTS5 search) and the
//! highlights and notes the user marks during or after a meeting.
//!
//! The app opens the database read-write in WAL mode; `parley-mcp` opens it
//! read-only, so Claude can read while a meeting is being written.

use chrono::{DateTime, Local, NaiveDate, TimeZone, Utc};
use rusqlite::{params, Connection, OpenFlags, OptionalExtension, Row};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::time::Duration;

const SCHEMA_VERSION: i32 = 2;

pub const MIC_FILE: &str = "mic.wav";
pub const SYSTEM_FILE: &str = "system.wav";
pub const MIXED_FILE: &str = "mixed.wav";
pub const TRANSCRIPT_FILE: &str = "transcript.md";

pub const SPEAKER_ME: &str = "Me";
pub const SPEAKER_THEM: &str = "Them";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MeetingStatus {
    Recording,
    Transcribing,
    Ready,
    Failed,
}

impl MeetingStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            MeetingStatus::Recording => "recording",
            MeetingStatus::Transcribing => "transcribing",
            MeetingStatus::Ready => "ready",
            MeetingStatus::Failed => "failed",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "recording" => Some(MeetingStatus::Recording),
            "transcribing" => Some(MeetingStatus::Transcribing),
            "ready" => Some(MeetingStatus::Ready),
            "failed" => Some(MeetingStatus::Failed),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Meeting {
    pub id: String,
    pub title: String,
    /// RFC 3339, UTC.
    pub started_at: String,
    pub ended_at: Option<String>,
    pub duration_ms: i64,
    pub status: MeetingStatus,
    pub error: Option<String>,
    /// Non-fatal capture problem, e.g. system audio was unavailable.
    pub capture_warning: Option<String>,
    pub profile_name: Option<String>,
    pub provider: Option<String>,
    pub language: Option<String>,
    /// Relative to the data directory, e.g. `meetings/20261008-101500-ab12`.
    pub folder: String,
    pub speakers: Vec<String>,
    pub segment_count: i64,
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Segment {
    pub start_ms: i64,
    pub end_ms: i64,
    pub speaker: String,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SearchHit {
    pub meeting_id: String,
    pub meeting_title: String,
    pub started_at: String,
    pub start_ms: i64,
    pub end_ms: i64,
    pub speaker: String,
    pub snippet: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MarkerKind {
    Highlight,
    Note,
}

impl MarkerKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            MarkerKind::Highlight => "highlight",
            MarkerKind::Note => "note",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "highlight" => Some(MarkerKind::Highlight),
            "note" => Some(MarkerKind::Note),
            _ => None,
        }
    }
}

/// A moment the user flagged: a highlight, or a note with text. `at_ms` is on
/// the recording clock, the same one as `Segment::start_ms`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Marker {
    pub id: i64,
    pub meeting_id: String,
    pub at_ms: i64,
    pub kind: MarkerKind,
    pub text: Option<String>,
    pub created_at: String,
}

/// A marker with the meeting it belongs to, for cross-meeting listings.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MarkerHit {
    pub marker: Marker,
    pub meeting_title: String,
    pub started_at: String,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct MarkerQuery {
    pub meeting_id: Option<String>,
    /// Bounds on the meeting's `started_at`, like `ListQuery`.
    pub from: Option<String>,
    pub to: Option<String>,
    pub kind: Option<MarkerKind>,
    pub limit: Option<u32>,
}

pub const MAX_MARKER_TEXT: usize = 2000;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ListQuery {
    /// Inclusive lower bound on `started_at` (RFC 3339 or `YYYY-MM-DD`, local day).
    pub from: Option<String>,
    /// Inclusive upper bound (a bare date covers the whole local day).
    pub to: Option<String>,
    /// Matches the title or the transcript text.
    pub query: Option<String>,
    pub limit: Option<u32>,
    pub offset: Option<u32>,
}

pub const DEFAULT_LIST_LIMIT: u32 = 50;
pub const MAX_LIST_LIMIT: u32 = 500;

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

fn now() -> String {
    Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

pub fn open_rw(path: &Path) -> Result<Connection, String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(err)?;
    }
    let conn = Connection::open(path).map_err(err)?;
    conn.busy_timeout(Duration::from_secs(5)).map_err(err)?;
    conn.pragma_update(None, "journal_mode", "WAL")
        .map_err(err)?;
    conn.pragma_update(None, "foreign_keys", "ON")
        .map_err(err)?;
    conn.pragma_update(None, "synchronous", "NORMAL")
        .map_err(err)?;
    migrate(&conn)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).ok();
    }
    Ok(conn)
}

/// Read-only connection for the MCP server. Fails if the database does not
/// exist (nothing recorded yet) instead of creating it.
pub fn open_ro(path: &Path) -> Result<Connection, String> {
    if !path.exists() {
        return Err(format!(
            "No Parley database at {} — record a meeting first, or pass --db",
            path.display()
        ));
    }
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(err)?;
    conn.busy_timeout(Duration::from_secs(5)).map_err(err)?;
    Ok(conn)
}

pub fn migrate(conn: &Connection) -> Result<(), String> {
    let version: i32 = conn
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .map_err(err)?;
    if version >= SCHEMA_VERSION {
        return Ok(());
    }
    if version < 1 {
        migrate_v1(conn)?;
    }
    if version < 2 {
        migrate_v2(conn)?;
    }
    Ok(())
}

fn migrate_v1(conn: &Connection) -> Result<(), String> {
    conn.execute_batch(
        r#"
        BEGIN;
        CREATE TABLE IF NOT EXISTS meetings (
            id              TEXT PRIMARY KEY,
            title           TEXT NOT NULL,
            started_at      TEXT NOT NULL,
            ended_at        TEXT,
            duration_ms     INTEGER NOT NULL DEFAULT 0,
            status          TEXT NOT NULL,
            error           TEXT,
            capture_warning TEXT,
            profile_name    TEXT,
            provider        TEXT,
            language        TEXT,
            folder          TEXT NOT NULL,
            created_at      TEXT NOT NULL,
            updated_at      TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_meetings_started_at ON meetings(started_at DESC);
        CREATE TABLE IF NOT EXISTS segments (
            id         INTEGER PRIMARY KEY AUTOINCREMENT,
            meeting_id TEXT NOT NULL REFERENCES meetings(id) ON DELETE CASCADE,
            start_ms   INTEGER NOT NULL,
            end_ms     INTEGER NOT NULL,
            speaker    TEXT NOT NULL,
            text       TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_segments_meeting ON segments(meeting_id, start_ms);
        CREATE VIRTUAL TABLE IF NOT EXISTS segments_fts USING fts5(
            text,
            content='segments',
            content_rowid='id',
            tokenize='unicode61 remove_diacritics 2'
        );
        CREATE TRIGGER IF NOT EXISTS segments_ai AFTER INSERT ON segments BEGIN
            INSERT INTO segments_fts(rowid, text) VALUES (new.id, new.text);
        END;
        CREATE TRIGGER IF NOT EXISTS segments_ad AFTER DELETE ON segments BEGIN
            INSERT INTO segments_fts(segments_fts, rowid, text) VALUES ('delete', old.id, old.text);
        END;
        CREATE TRIGGER IF NOT EXISTS segments_au AFTER UPDATE ON segments BEGIN
            INSERT INTO segments_fts(segments_fts, rowid, text) VALUES ('delete', old.id, old.text);
            INSERT INTO segments_fts(rowid, text) VALUES (new.id, new.text);
        END;
        PRAGMA user_version = 1;
        COMMIT;
        "#,
    )
    .map_err(err)
}

/// v2: highlights and notes. They belong to the meeting, not the transcript,
/// so re-transcribing keeps them.
fn migrate_v2(conn: &Connection) -> Result<(), String> {
    conn.execute_batch(
        r#"
        BEGIN;
        CREATE TABLE IF NOT EXISTS markers (
            id         INTEGER PRIMARY KEY AUTOINCREMENT,
            meeting_id TEXT NOT NULL REFERENCES meetings(id) ON DELETE CASCADE,
            at_ms      INTEGER NOT NULL CHECK (at_ms >= 0),
            kind       TEXT NOT NULL CHECK (kind IN ('highlight', 'note')),
            text       TEXT,
            created_at TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_markers_meeting ON markers(meeting_id, at_ms);
        PRAGMA user_version = 2;
        COMMIT;
        "#,
    )
    .map_err(err)
}

/// A short, sortable, filesystem-safe id: `20261008-101500-ab12`.
pub fn new_meeting_id(started: DateTime<Utc>) -> String {
    use std::sync::atomic::{AtomicU32, Ordering};
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or_default();
    let salt = (nanos ^ COUNTER.fetch_add(1, Ordering::Relaxed).wrapping_mul(0x9E37)) & 0xffff;
    format!(
        "{}-{salt:04x}",
        started.with_timezone(&Local).format("%Y%m%d-%H%M%S")
    )
}

/// "Meeting · Oct 8, 10:15" — the title a new recording starts with.
pub fn default_title(started: DateTime<Utc>) -> String {
    format!(
        "Meeting · {}",
        started.with_timezone(&Local).format("%b %-d, %H:%M")
    )
}

pub fn normalize_title(title: &str) -> Result<String, String> {
    let title = title.split_whitespace().collect::<Vec<_>>().join(" ");
    if title.is_empty() {
        return Err("Title can't be empty".to_string());
    }
    if title.chars().count() > 200 {
        return Err("Title must be at most 200 characters".to_string());
    }
    Ok(title)
}

pub struct NewMeeting<'a> {
    pub id: &'a str,
    pub title: &'a str,
    pub started_at: DateTime<Utc>,
    pub folder: &'a str,
}

pub fn insert_meeting(conn: &Connection, new: &NewMeeting) -> Result<(), String> {
    let title = normalize_title(new.title)?;
    let ts = now();
    conn.execute(
        "INSERT INTO meetings (id, title, started_at, status, folder, created_at, updated_at)
         VALUES (?1, ?2, ?3, 'recording', ?4, ?5, ?5)",
        params![
            new.id,
            title,
            new.started_at
                .to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            new.folder,
            ts
        ],
    )
    .map_err(err)?;
    Ok(())
}

const MEETING_COLUMNS: &str =
    "m.id, m.title, m.started_at, m.ended_at, m.duration_ms, m.status, m.error,
     m.capture_warning, m.profile_name, m.provider, m.language, m.folder, m.updated_at,
     (SELECT COUNT(*) FROM segments s WHERE s.meeting_id = m.id),
     (SELECT GROUP_CONCAT(speaker, char(31)) FROM
        (SELECT speaker, MIN(start_ms) AS first FROM segments s WHERE s.meeting_id = m.id
         GROUP BY speaker ORDER BY first))";

fn meeting_from_row(row: &Row) -> rusqlite::Result<Meeting> {
    let status: String = row.get(5)?;
    let speakers: Option<String> = row.get(14)?;
    Ok(Meeting {
        id: row.get(0)?,
        title: row.get(1)?,
        started_at: row.get(2)?,
        ended_at: row.get(3)?,
        duration_ms: row.get(4)?,
        status: MeetingStatus::parse(&status).unwrap_or(MeetingStatus::Failed),
        error: row.get(6)?,
        capture_warning: row.get(7)?,
        profile_name: row.get(8)?,
        provider: row.get(9)?,
        language: row.get(10)?,
        folder: row.get(11)?,
        updated_at: row.get(12)?,
        segment_count: row.get(13)?,
        speakers: speakers
            .map(|s| s.split('\u{1f}').map(str::to_string).collect())
            .unwrap_or_default(),
    })
}

pub fn get_meeting(conn: &Connection, id: &str) -> Result<Option<Meeting>, String> {
    conn.query_row(
        &format!("SELECT {MEETING_COLUMNS} FROM meetings m WHERE m.id = ?1"),
        params![id],
        meeting_from_row,
    )
    .optional()
    .map_err(err)
}

pub fn require_meeting(conn: &Connection, id: &str) -> Result<Meeting, String> {
    get_meeting(conn, id)?.ok_or_else(|| format!("No meeting with id {id}"))
}

/// Turn free text into a safe FTS5 query: every word must match, the last one
/// as a prefix. Returns `None` when there is nothing to search for.
pub fn fts_query(input: &str) -> Option<String> {
    let words: Vec<String> = input
        .split(|c: char| !c.is_alphanumeric() && c != '\'')
        .map(|w| w.trim_matches('\''))
        .filter(|w| !w.is_empty())
        .map(|w| format!("\"{}\"", w.replace('"', "")))
        .collect();
    if words.is_empty() {
        return None;
    }
    let mut query = words.join(" ");
    query.push('*');
    Some(query)
}

/// Normalize a date bound to RFC 3339 UTC. A bare `YYYY-MM-DD` means local
/// midnight; as an upper bound it covers the whole day (exclusive next day).
pub fn normalize_bound(value: &str, upper: bool) -> Result<(String, bool), String> {
    let value = value.trim();
    if let Ok(dt) = DateTime::parse_from_rfc3339(value) {
        return Ok((
            dt.with_timezone(&Utc)
                .to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            false,
        ));
    }
    let date = NaiveDate::parse_from_str(value, "%Y-%m-%d")
        .map_err(|_| format!("Invalid date {value:?}: use YYYY-MM-DD or RFC 3339"))?;
    let date = if upper {
        date.succ_opt().ok_or("Date out of range")?
    } else {
        date
    };
    let local = Local
        .from_local_datetime(&date.and_hms_opt(0, 0, 0).expect("midnight is valid"))
        .earliest()
        .ok_or("Date out of range")?;
    Ok((
        local
            .with_timezone(&Utc)
            .to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        upper,
    ))
}

pub fn list_meetings(conn: &Connection, q: &ListQuery) -> Result<Vec<Meeting>, String> {
    let mut sql = format!("SELECT {MEETING_COLUMNS} FROM meetings m WHERE 1=1");
    let mut args: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
    if let Some(from) = q.from.as_deref().filter(|v| !v.trim().is_empty()) {
        let (bound, _) = normalize_bound(from, false)?;
        sql.push_str(&format!(" AND m.started_at >= ?{}", args.len() + 1));
        args.push(Box::new(bound));
    }
    if let Some(to) = q.to.as_deref().filter(|v| !v.trim().is_empty()) {
        let (bound, exclusive) = normalize_bound(to, true)?;
        let op = if exclusive { "<" } else { "<=" };
        sql.push_str(&format!(" AND m.started_at {op} ?{}", args.len() + 1));
        args.push(Box::new(bound));
    }
    if let Some(text) = q.query.as_deref().map(str::trim).filter(|v| !v.is_empty()) {
        let like = format!("%{}%", text.replace('%', "\\%").replace('_', "\\_"));
        let n = args.len() + 1;
        match fts_query(text) {
            Some(fts) => {
                sql.push_str(&format!(
                    " AND (m.title LIKE ?{n} ESCAPE '\\' OR m.id IN (
                        SELECT s.meeting_id FROM segments s
                        JOIN segments_fts f ON f.rowid = s.id
                        WHERE segments_fts MATCH ?{}))",
                    n + 1
                ));
                args.push(Box::new(like));
                args.push(Box::new(fts));
            }
            None => {
                sql.push_str(&format!(" AND m.title LIKE ?{n} ESCAPE '\\'"));
                args.push(Box::new(like));
            }
        }
    }
    let limit = q
        .limit
        .unwrap_or(DEFAULT_LIST_LIMIT)
        .clamp(1, MAX_LIST_LIMIT);
    sql.push_str(&format!(
        " ORDER BY m.started_at DESC LIMIT {limit} OFFSET {}",
        q.offset.unwrap_or(0)
    ));
    let mut stmt = conn.prepare(&sql).map_err(err)?;
    let rows = stmt
        .query_map(
            rusqlite::params_from_iter(args.iter().map(|a| a.as_ref())),
            meeting_from_row,
        )
        .map_err(err)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(err)?;
    Ok(rows)
}

pub fn search(conn: &Connection, text: &str, limit: u32) -> Result<Vec<SearchHit>, String> {
    let Some(fts) = fts_query(text) else {
        return Ok(Vec::new());
    };
    let limit = limit.clamp(1, MAX_LIST_LIMIT);
    let mut stmt = conn
        .prepare(&format!(
            "SELECT s.meeting_id, m.title, m.started_at, s.start_ms, s.end_ms, s.speaker,
                    snippet(segments_fts, 0, '**', '**', '…', 16)
             FROM segments_fts f
             JOIN segments s ON s.id = f.rowid
             JOIN meetings m ON m.id = s.meeting_id
             WHERE segments_fts MATCH ?1
             ORDER BY bm25(segments_fts), m.started_at DESC, s.start_ms
             LIMIT {limit}"
        ))
        .map_err(err)?;
    let hits = stmt
        .query_map(params![fts], |row| {
            Ok(SearchHit {
                meeting_id: row.get(0)?,
                meeting_title: row.get(1)?,
                started_at: row.get(2)?,
                start_ms: row.get(3)?,
                end_ms: row.get(4)?,
                speaker: row.get(5)?,
                snippet: row.get(6)?,
            })
        })
        .map_err(err)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(err)?;
    Ok(hits)
}

pub fn segments(conn: &Connection, meeting_id: &str) -> Result<Vec<Segment>, String> {
    let mut stmt = conn
        .prepare(
            "SELECT start_ms, end_ms, speaker, text FROM segments
             WHERE meeting_id = ?1 ORDER BY start_ms, id",
        )
        .map_err(err)?;
    let rows = stmt
        .query_map(params![meeting_id], |row| {
            Ok(Segment {
                start_ms: row.get(0)?,
                end_ms: row.get(1)?,
                speaker: row.get(2)?,
                text: row.get(3)?,
            })
        })
        .map_err(err)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(err)?;
    Ok(rows)
}

fn touch(
    conn: &Connection,
    id: &str,
    sql: &str,
    args: &[&dyn rusqlite::ToSql],
) -> Result<(), String> {
    let changed = conn.execute(sql, args).map_err(err)?;
    if changed == 0 {
        return Err(format!("No meeting with id {id}"));
    }
    Ok(())
}

pub fn rename_meeting(conn: &Connection, id: &str, title: &str) -> Result<(), String> {
    let title = normalize_title(title)?;
    touch(
        conn,
        id,
        "UPDATE meetings SET title = ?2, updated_at = ?3 WHERE id = ?1",
        &[&id, &title, &now()],
    )
}

pub fn set_capture_warning(
    conn: &Connection,
    id: &str,
    warning: Option<&str>,
) -> Result<(), String> {
    touch(
        conn,
        id,
        "UPDATE meetings SET capture_warning = ?2, updated_at = ?3 WHERE id = ?1",
        &[&id, &warning, &now()],
    )
}

pub fn finish_recording(
    conn: &Connection,
    id: &str,
    ended_at: DateTime<Utc>,
    duration_ms: i64,
) -> Result<(), String> {
    touch(
        conn,
        id,
        "UPDATE meetings SET ended_at = ?2, duration_ms = ?3, status = 'transcribing', error = NULL,
         updated_at = ?4 WHERE id = ?1",
        &[
            &id,
            &ended_at.to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            &duration_ms,
            &now(),
        ],
    )
}

pub fn set_status(
    conn: &Connection,
    id: &str,
    status: MeetingStatus,
    error: Option<&str>,
) -> Result<(), String> {
    touch(
        conn,
        id,
        "UPDATE meetings SET status = ?2, error = ?3, updated_at = ?4 WHERE id = ?1",
        &[&id, &status.as_str(), &error, &now()],
    )
}

pub fn set_transcription_meta(
    conn: &Connection,
    id: &str,
    profile_name: &str,
    provider: &str,
    language: Option<&str>,
) -> Result<(), String> {
    touch(
        conn,
        id,
        "UPDATE meetings SET profile_name = ?2, provider = ?3, language = ?4, updated_at = ?5
         WHERE id = ?1",
        &[&id, &profile_name, &provider, &language, &now()],
    )
}

/// Atomically replace a meeting's transcript and mark it ready.
pub fn save_transcript(conn: &mut Connection, id: &str, segs: &[Segment]) -> Result<(), String> {
    let tx = conn.transaction().map_err(err)?;
    tx.execute("DELETE FROM segments WHERE meeting_id = ?1", params![id])
        .map_err(err)?;
    {
        let mut insert = tx
            .prepare(
                "INSERT INTO segments (meeting_id, start_ms, end_ms, speaker, text)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
            )
            .map_err(err)?;
        for s in segs {
            insert
                .execute(params![id, s.start_ms, s.end_ms, s.speaker, s.text])
                .map_err(err)?;
        }
    }
    let changed = tx
        .execute(
            "UPDATE meetings SET status = 'ready', error = NULL, updated_at = ?2 WHERE id = ?1",
            params![id, now()],
        )
        .map_err(err)?;
    if changed == 0 {
        return Err(format!("No meeting with id {id}"));
    }
    tx.commit().map_err(err)
}

pub fn delete_meeting(conn: &Connection, id: &str) -> Result<(), String> {
    touch(conn, id, "DELETE FROM meetings WHERE id = ?1", &[&id])
}

pub fn meetings_with_status(
    conn: &Connection,
    status: MeetingStatus,
) -> Result<Vec<Meeting>, String> {
    let mut stmt = conn
        .prepare(&format!(
            "SELECT {MEETING_COLUMNS} FROM meetings m WHERE m.status = ?1 ORDER BY m.started_at"
        ))
        .map_err(err)?;
    let rows = stmt
        .query_map(params![status.as_str()], meeting_from_row)
        .map_err(err)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(err)?;
    Ok(rows)
}

/// Segments that overlap `[from_ms, to_ms]`, in order.
pub fn segments_between(
    conn: &Connection,
    meeting_id: &str,
    from_ms: i64,
    to_ms: i64,
) -> Result<Vec<Segment>, String> {
    let mut stmt = conn
        .prepare(
            "SELECT start_ms, end_ms, speaker, text FROM segments
             WHERE meeting_id = ?1 AND end_ms >= ?2 AND start_ms <= ?3
             ORDER BY start_ms, id",
        )
        .map_err(err)?;
    let rows = stmt
        .query_map(params![meeting_id, from_ms, to_ms], |row| {
            Ok(Segment {
                start_ms: row.get(0)?,
                end_ms: row.get(1)?,
                speaker: row.get(2)?,
                text: row.get(3)?,
            })
        })
        .map_err(err)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(err)?;
    Ok(rows)
}

// --- Markers ---

/// One line of text: whitespace collapsed, empty means none. Notes need text.
pub fn normalize_marker_text(
    kind: MarkerKind,
    text: Option<&str>,
) -> Result<Option<String>, String> {
    let text = text
        .map(|t| t.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|t| !t.is_empty());
    if let Some(t) = &text {
        if t.chars().count() > MAX_MARKER_TEXT {
            return Err(format!(
                "A note must be at most {MAX_MARKER_TEXT} characters"
            ));
        }
    }
    if kind == MarkerKind::Note && text.is_none() {
        return Err("A note needs some text".to_string());
    }
    Ok(text)
}

/// The read-only MCP connection may meet a database the app hasn't migrated
/// yet: it has no markers rather than an error.
fn has_markers_table(conn: &Connection) -> Result<bool, String> {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'markers')",
        [],
        |r| r.get(0),
    )
    .map_err(err)
}

const MARKER_COLUMNS: &str = "k.id, k.meeting_id, k.at_ms, k.kind, k.text, k.created_at";

fn marker_from_row(row: &Row) -> rusqlite::Result<Marker> {
    let kind: String = row.get(3)?;
    Ok(Marker {
        id: row.get(0)?,
        meeting_id: row.get(1)?,
        at_ms: row.get(2)?,
        kind: MarkerKind::parse(&kind).unwrap_or(MarkerKind::Highlight),
        text: row.get(4)?,
        created_at: row.get(5)?,
    })
}

pub fn get_marker(conn: &Connection, id: i64) -> Result<Option<Marker>, String> {
    if !has_markers_table(conn)? {
        return Ok(None);
    }
    conn.query_row(
        &format!("SELECT {MARKER_COLUMNS} FROM markers k WHERE k.id = ?1"),
        params![id],
        marker_from_row,
    )
    .optional()
    .map_err(err)
}

fn require_marker(conn: &Connection, id: i64) -> Result<Marker, String> {
    get_marker(conn, id)?.ok_or_else(|| format!("No highlight or note with id {id}"))
}

/// Save a marker right away. `at_ms` can't be negative; on a finished meeting
/// it is kept within the recording.
pub fn insert_marker(
    conn: &Connection,
    meeting_id: &str,
    kind: MarkerKind,
    at_ms: i64,
    text: Option<&str>,
) -> Result<Marker, String> {
    let meeting = require_meeting(conn, meeting_id)?;
    if at_ms < 0 {
        return Err("A highlight or note can't be before the recording started".to_string());
    }
    let at_ms = if meeting.status != MeetingStatus::Recording && meeting.duration_ms > 0 {
        at_ms.min(meeting.duration_ms)
    } else {
        at_ms
    };
    let text = normalize_marker_text(kind, text)?;
    conn.execute(
        "INSERT INTO markers (meeting_id, at_ms, kind, text, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![meeting_id, at_ms, kind.as_str(), text, now()],
    )
    .map_err(err)?;
    require_marker(conn, conn.last_insert_rowid())
}

/// Change a marker's text (a note can't be emptied).
pub fn update_marker_text(
    conn: &Connection,
    id: i64,
    text: Option<&str>,
) -> Result<Marker, String> {
    let marker = require_marker(conn, id)?;
    let text = normalize_marker_text(marker.kind, text)?;
    conn.execute(
        "UPDATE markers SET text = ?2 WHERE id = ?1",
        params![id, text],
    )
    .map_err(err)?;
    require_marker(conn, id)
}

/// Delete a marker; returns the meeting it belonged to.
pub fn delete_marker(conn: &Connection, id: i64) -> Result<String, String> {
    let marker = require_marker(conn, id)?;
    conn.execute("DELETE FROM markers WHERE id = ?1", params![id])
        .map_err(err)?;
    Ok(marker.meeting_id)
}

/// A meeting's markers in time order.
pub fn markers(conn: &Connection, meeting_id: &str) -> Result<Vec<Marker>, String> {
    if !has_markers_table(conn)? {
        return Ok(Vec::new());
    }
    let mut stmt = conn
        .prepare(&format!(
            "SELECT {MARKER_COLUMNS} FROM markers k WHERE k.meeting_id = ?1 ORDER BY k.at_ms, k.id"
        ))
        .map_err(err)?;
    let rows = stmt
        .query_map(params![meeting_id], marker_from_row)
        .map_err(err)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(err)?;
    Ok(rows)
}

pub fn count_markers(conn: &Connection, meeting_id: &str) -> Result<i64, String> {
    if !has_markers_table(conn)? {
        return Ok(0);
    }
    conn.query_row(
        "SELECT COUNT(*) FROM markers WHERE meeting_id = ?1",
        params![meeting_id],
        |r| r.get(0),
    )
    .map_err(err)
}

/// Markers across meetings: newest meeting first, then in time order.
pub fn list_markers(conn: &Connection, q: &MarkerQuery) -> Result<Vec<MarkerHit>, String> {
    if !has_markers_table(conn)? {
        return Ok(Vec::new());
    }
    let mut sql = format!(
        "SELECT {MARKER_COLUMNS}, m.title, m.started_at
         FROM markers k JOIN meetings m ON m.id = k.meeting_id WHERE 1=1"
    );
    let mut args: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
    if let Some(id) = q
        .meeting_id
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
    {
        sql.push_str(&format!(" AND k.meeting_id = ?{}", args.len() + 1));
        args.push(Box::new(id.to_string()));
    }
    if let Some(from) = q.from.as_deref().filter(|v| !v.trim().is_empty()) {
        let (bound, _) = normalize_bound(from, false)?;
        sql.push_str(&format!(" AND m.started_at >= ?{}", args.len() + 1));
        args.push(Box::new(bound));
    }
    if let Some(to) = q.to.as_deref().filter(|v| !v.trim().is_empty()) {
        let (bound, exclusive) = normalize_bound(to, true)?;
        let op = if exclusive { "<" } else { "<=" };
        sql.push_str(&format!(" AND m.started_at {op} ?{}", args.len() + 1));
        args.push(Box::new(bound));
    }
    if let Some(kind) = q.kind {
        sql.push_str(&format!(" AND k.kind = ?{}", args.len() + 1));
        args.push(Box::new(kind.as_str()));
    }
    let limit = q
        .limit
        .unwrap_or(DEFAULT_LIST_LIMIT)
        .clamp(1, MAX_LIST_LIMIT);
    sql.push_str(&format!(
        " ORDER BY m.started_at DESC, k.at_ms, k.id LIMIT {limit}"
    ));
    let mut stmt = conn.prepare(&sql).map_err(err)?;
    let rows = stmt
        .query_map(
            rusqlite::params_from_iter(args.iter().map(|a| a.as_ref())),
            |row| {
                Ok(MarkerHit {
                    marker: marker_from_row(row)?,
                    meeting_title: row.get(6)?,
                    started_at: row.get(7)?,
                })
            },
        )
        .map_err(err)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(err)?;
    Ok(rows)
}

#[cfg(test)]
pub mod tests {
    use super::*;

    pub fn seg(start: i64, end: i64, speaker: &str, text: &str) -> Segment {
        Segment {
            start_ms: start,
            end_ms: end,
            speaker: speaker.to_string(),
            text: text.to_string(),
        }
    }

    fn at(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    fn db() -> (tempfile::TempDir, Connection) {
        let dir = tempfile::tempdir().unwrap();
        let conn = open_rw(&dir.path().join("parley.db")).unwrap();
        (dir, conn)
    }

    fn add(conn: &Connection, id: &str, title: &str, started: &str) {
        insert_meeting(
            conn,
            &NewMeeting {
                id,
                title,
                started_at: at(started),
                folder: &format!("meetings/{id}"),
            },
        )
        .unwrap();
    }

    #[test]
    fn fts5_is_available_in_the_bundled_sqlite() {
        let (_d, conn) = db();
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM segments_fts", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn meeting_lifecycle_recording_to_ready() {
        let (_d, mut conn) = db();
        add(&conn, "m1", "  Weekly   sync ", "2026-10-08T08:00:00Z");
        let m = require_meeting(&conn, "m1").unwrap();
        assert_eq!(m.title, "Weekly sync");
        assert_eq!(m.status, MeetingStatus::Recording);
        assert_eq!(m.segment_count, 0);
        assert!(m.speakers.is_empty());

        finish_recording(&conn, "m1", at("2026-10-08T08:30:00Z"), 1_800_000).unwrap();
        let m = require_meeting(&conn, "m1").unwrap();
        assert_eq!(m.status, MeetingStatus::Transcribing);
        assert_eq!(m.duration_ms, 1_800_000);
        assert_eq!(m.ended_at.as_deref(), Some("2026-10-08T08:30:00.000Z"));

        set_transcription_meta(&conn, "m1", "Local Ollama", "openai_compat", Some("en")).unwrap();
        save_transcript(
            &mut conn,
            "m1",
            &[
                seg(5_000, 7_000, "Them", "Can you hear me?"),
                seg(1_000, 3_000, "Me", "Hello everyone"),
            ],
        )
        .unwrap();
        let m = require_meeting(&conn, "m1").unwrap();
        assert_eq!(m.status, MeetingStatus::Ready);
        assert_eq!(m.segment_count, 2);
        assert_eq!(m.speakers, vec!["Me", "Them"]);
        assert_eq!(m.profile_name.as_deref(), Some("Local Ollama"));
        let s = segments(&conn, "m1").unwrap();
        assert_eq!(s[0].text, "Hello everyone");
    }

    #[test]
    fn failed_status_keeps_the_error_and_retry_clears_it() {
        let (_d, mut conn) = db();
        add(&conn, "m1", "T", "2026-10-08T08:00:00Z");
        set_status(&conn, "m1", MeetingStatus::Failed, Some("Ollama is down")).unwrap();
        let m = require_meeting(&conn, "m1").unwrap();
        assert_eq!(m.status, MeetingStatus::Failed);
        assert_eq!(m.error.as_deref(), Some("Ollama is down"));
        save_transcript(&mut conn, "m1", &[seg(0, 1, "Me", "ok")]).unwrap();
        assert_eq!(require_meeting(&conn, "m1").unwrap().error, None);
    }

    #[test]
    fn save_transcript_replaces_previous_segments_and_fts_rows() {
        let (_d, mut conn) = db();
        add(&conn, "m1", "T", "2026-10-08T08:00:00Z");
        save_transcript(&mut conn, "m1", &[seg(0, 1, "Me", "alpha budget")]).unwrap();
        save_transcript(&mut conn, "m1", &[seg(0, 1, "Me", "bravo roadmap")]).unwrap();
        assert!(search(&conn, "budget", 10).unwrap().is_empty());
        assert_eq!(search(&conn, "roadmap", 10).unwrap().len(), 1);
    }

    #[test]
    fn unknown_ids_are_errors() {
        let (_d, mut conn) = db();
        assert!(rename_meeting(&conn, "nope", "x").is_err());
        assert!(set_status(&conn, "nope", MeetingStatus::Ready, None).is_err());
        assert!(delete_meeting(&conn, "nope").is_err());
        assert!(save_transcript(&mut conn, "nope", &[]).is_err());
        assert!(finish_recording(&conn, "nope", Utc::now(), 0).is_err());
        assert_eq!(get_meeting(&conn, "nope").unwrap(), None);
        assert!(require_meeting(&conn, "nope").is_err());
    }

    #[test]
    fn titles_are_validated() {
        let (_d, conn) = db();
        add(&conn, "m1", "T", "2026-10-08T08:00:00Z");
        assert!(rename_meeting(&conn, "m1", "   ").is_err());
        assert!(rename_meeting(&conn, "m1", &"x".repeat(201)).is_err());
        rename_meeting(&conn, "m1", "Board prep").unwrap();
        assert_eq!(require_meeting(&conn, "m1").unwrap().title, "Board prep");
        // Duplicate ids are rejected.
        assert!(insert_meeting(
            &conn,
            &NewMeeting {
                id: "m1",
                title: "again",
                started_at: Utc::now(),
                folder: "meetings/m1"
            }
        )
        .is_err());
    }

    #[test]
    fn delete_cascades_to_segments_and_search() {
        let (_d, mut conn) = db();
        add(&conn, "m1", "T", "2026-10-08T08:00:00Z");
        save_transcript(&mut conn, "m1", &[seg(0, 1, "Me", "quarterly numbers")]).unwrap();
        delete_meeting(&conn, "m1").unwrap();
        assert!(segments(&conn, "m1").unwrap().is_empty());
        assert!(search(&conn, "quarterly", 10).unwrap().is_empty());
    }

    #[test]
    fn list_filters_by_date_query_and_limit() {
        let (_d, mut conn) = db();
        add(&conn, "a", "Hiring loop", "2026-10-01T09:00:00Z");
        add(&conn, "b", "Budget review", "2026-10-05T09:00:00Z");
        add(&conn, "c", "Vendor call", "2026-10-07T09:00:00Z");
        save_transcript(
            &mut conn,
            "c",
            &[seg(0, 1, "Them", "the budget is approved")],
        )
        .unwrap();

        let all = list_meetings(&conn, &ListQuery::default()).unwrap();
        assert_eq!(
            all.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
            vec!["c", "b", "a"]
        );

        let q = |from: Option<&str>, to: Option<&str>, query: Option<&str>| ListQuery {
            from: from.map(String::from),
            to: to.map(String::from),
            query: query.map(String::from),
            ..ListQuery::default()
        };
        let ids = |q: ListQuery| {
            list_meetings(&conn, &q)
                .unwrap()
                .into_iter()
                .map(|m| m.id)
                .collect::<Vec<_>>()
        };
        assert_eq!(
            ids(q(
                Some("2026-10-04T00:00:00Z"),
                Some("2026-10-06T00:00:00Z"),
                None
            )),
            vec!["b"]
        );
        // Title or transcript match.
        assert_eq!(ids(q(None, None, Some("budget"))), vec!["c", "b"]);
        assert_eq!(ids(q(None, None, Some("hiring"))), vec!["a"]);
        // Prefix match on the last word.
        assert_eq!(ids(q(None, None, Some("approv"))), vec!["c"]);
        // Punctuation-only queries match by title only and never error.
        assert!(ids(q(None, None, Some("\"*("))).is_empty());
        let limited = list_meetings(
            &conn,
            &ListQuery {
                limit: Some(1),
                offset: Some(1),
                ..ListQuery::default()
            },
        )
        .unwrap();
        assert_eq!(limited[0].id, "b");
        assert!(list_meetings(&conn, &q(Some("yesterday"), None, None)).is_err());
    }

    #[test]
    fn bare_dates_cover_the_whole_local_day() {
        let (lower, excl) = normalize_bound("2026-10-05", false).unwrap();
        assert!(!excl);
        let (upper, excl_u) = normalize_bound("2026-10-05", true).unwrap();
        assert!(excl_u);
        let lo = DateTime::parse_from_rfc3339(&lower).unwrap();
        let hi = DateTime::parse_from_rfc3339(&upper).unwrap();
        assert_eq!((hi - lo).num_hours(), 24);
        assert_eq!(
            normalize_bound("2026-10-05T10:00:00+02:00", false)
                .unwrap()
                .0,
            "2026-10-05T08:00:00.000Z"
        );
    }

    #[test]
    fn search_returns_snippets_with_meeting_and_time() {
        let (_d, mut conn) = db();
        add(&conn, "m1", "Roadmap", "2026-10-08T08:00:00Z");
        save_transcript(
            &mut conn,
            "m1",
            &[
                seg(0, 1000, "Me", "Let's talk about the migration plan"),
                seg(61_000, 64_000, "Them 1", "The migration starts in November"),
            ],
        )
        .unwrap();
        let hits = search(&conn, "migration november", 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].meeting_id, "m1");
        assert_eq!(hits[0].meeting_title, "Roadmap");
        assert_eq!(hits[0].start_ms, 61_000);
        assert_eq!(hits[0].speaker, "Them 1");
        assert!(
            hits[0].snippet.contains("**migration**"),
            "{}",
            hits[0].snippet
        );
        // Diacritics are folded.
        save_transcript(
            &mut conn,
            "m1",
            &[seg(0, 1, "Me", "reunión de presupuesto")],
        )
        .unwrap();
        assert_eq!(search(&conn, "reunion", 10).unwrap().len(), 1);
        assert!(search(&conn, "   ", 10).unwrap().is_empty());
        assert!(search(&conn, "AND OR NOT \"", 10).is_ok());
    }

    #[test]
    fn fts_query_quotes_words_and_prefixes_the_last() {
        assert_eq!(
            fts_query("budget plan").as_deref(),
            Some("\"budget\" \"plan\"*")
        );
        assert_eq!(fts_query("o'brien").as_deref(), Some("\"o'brien\"*"));
        assert_eq!(fts_query("  -- "), None);
    }

    #[test]
    fn read_only_connection_sees_writes_from_a_concurrent_writer() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("parley.db");
        assert!(open_ro(&path).is_err(), "must not create the database");
        let mut writer = open_rw(&path).unwrap();
        add(&writer, "m1", "Live", "2026-10-08T08:00:00Z");
        let reader = open_ro(&path).unwrap();
        assert_eq!(
            list_meetings(&reader, &ListQuery::default()).unwrap().len(),
            1
        );
        save_transcript(&mut writer, "m1", &[seg(0, 1, "Me", "fresh text")]).unwrap();
        assert_eq!(search(&reader, "fresh", 5).unwrap().len(), 1);
        // The reader can't write.
        assert!(reader.execute("DELETE FROM meetings", []).is_err());
    }

    #[test]
    fn statuses_round_trip_and_unknown_maps_to_none() {
        for s in [
            MeetingStatus::Recording,
            MeetingStatus::Transcribing,
            MeetingStatus::Ready,
            MeetingStatus::Failed,
        ] {
            assert_eq!(MeetingStatus::parse(s.as_str()), Some(s));
        }
        assert_eq!(MeetingStatus::parse("paused"), None);
    }

    #[test]
    fn ids_are_unique_and_folder_safe() {
        let t = Utc::now();
        let a = new_meeting_id(t);
        let b = new_meeting_id(t);
        assert_ne!(a, b);
        assert!(a.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'));
        assert!(default_title(t).starts_with("Meeting · "));
    }

    #[test]
    fn migration_upgrades_a_v1_database_and_keeps_its_data() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("parley.db");
        {
            // A database written by the first release: schema v1 only.
            let mut conn = Connection::open(&path).unwrap();
            migrate_v1(&conn).unwrap();
            add(&conn, "old", "Before markers", "2026-10-01T09:00:00Z");
            save_transcript(&mut conn, "old", &[seg(0, 1000, "Me", "legacy words")]).unwrap();
        }
        // parley-mcp may open it before the app migrates it: no markers, no error.
        let ro = open_ro(&path).unwrap();
        assert!(markers(&ro, "old").unwrap().is_empty());
        assert!(list_markers(&ro, &MarkerQuery::default())
            .unwrap()
            .is_empty());
        assert_eq!(count_markers(&ro, "old").unwrap(), 0);
        assert_eq!(get_marker(&ro, 1).unwrap(), None);
        drop(ro);

        let conn = open_rw(&path).unwrap();
        let version: i32 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);
        assert_eq!(require_meeting(&conn, "old").unwrap().segment_count, 1);
        assert_eq!(search(&conn, "legacy", 5).unwrap().len(), 1);
        let m = insert_marker(&conn, "old", MarkerKind::Highlight, 500, None).unwrap();
        assert_eq!(markers(&conn, "old").unwrap(), vec![m]);
        drop(conn);
        // Re-opening is a no-op.
        let conn = open_rw(&path).unwrap();
        assert_eq!(count_markers(&conn, "old").unwrap(), 1);
    }

    #[test]
    fn markers_are_validated_and_normalized() {
        let (_d, conn) = db();
        add(&conn, "m1", "T", "2026-10-08T08:00:00Z");
        let h = insert_marker(&conn, "m1", MarkerKind::Highlight, 754_000, None).unwrap();
        assert_eq!(h.kind, MarkerKind::Highlight);
        assert_eq!(h.at_ms, 754_000);
        assert_eq!(h.text, None);
        assert_eq!(h.meeting_id, "m1");
        let n = insert_marker(
            &conn,
            "m1",
            MarkerKind::Note,
            10_000,
            Some("  follow up\n with   Finance "),
        )
        .unwrap();
        assert_eq!(n.text.as_deref(), Some("follow up with Finance"));
        // Blank highlight text is no text.
        let blank = insert_marker(&conn, "m1", MarkerKind::Highlight, 1, Some("   ")).unwrap();
        assert_eq!(blank.text, None);

        for (kind, at, text) in [
            (MarkerKind::Note, 0, None),
            (MarkerKind::Note, 0, Some(" \t ")),
            (MarkerKind::Highlight, -1, None),
            (
                MarkerKind::Note,
                0,
                Some("x".repeat(MAX_MARKER_TEXT + 1).as_str()),
            ),
        ] {
            assert!(
                insert_marker(&conn, "m1", kind, at, text).is_err(),
                "{kind:?} {at} {text:?}"
            );
        }
        assert!(insert_marker(&conn, "nope", MarkerKind::Highlight, 0, None).is_err());
        assert!(insert_marker(
            &conn,
            "m1",
            MarkerKind::Note,
            0,
            Some("x".repeat(MAX_MARKER_TEXT).as_str())
        )
        .is_ok());
        // In time order.
        let at: Vec<i64> = markers(&conn, "m1")
            .unwrap()
            .iter()
            .map(|m| m.at_ms)
            .collect();
        assert_eq!(at, vec![0, 1, 10_000, 754_000]);
        // The database itself refuses bad rows written behind our back.
        assert!(conn
            .execute(
                "INSERT INTO markers (meeting_id, at_ms, kind, created_at) VALUES ('m1', 0, 'bogus', 'x')",
                [],
            )
            .is_err());
        assert!(conn
            .execute(
                "INSERT INTO markers (meeting_id, at_ms, kind, created_at) VALUES ('gone', 0, 'note', 'x')",
                [],
            )
            .is_err());
    }

    #[test]
    fn marker_time_is_kept_within_a_finished_recording() {
        let (_d, conn) = db();
        add(&conn, "m1", "T", "2026-10-08T08:00:00Z");
        // While recording the duration isn't known yet: no clamping.
        let live = insert_marker(&conn, "m1", MarkerKind::Highlight, 90_000, None).unwrap();
        assert_eq!(live.at_ms, 90_000);
        finish_recording(&conn, "m1", at("2026-10-08T08:01:00Z"), 60_000).unwrap();
        let late = insert_marker(&conn, "m1", MarkerKind::Note, 61_000, Some("late")).unwrap();
        assert_eq!(late.at_ms, 60_000);
    }

    #[test]
    fn notes_can_be_edited_and_markers_deleted() {
        let (_d, conn) = db();
        add(&conn, "m1", "T", "2026-10-08T08:00:00Z");
        let n = insert_marker(&conn, "m1", MarkerKind::Note, 5_000, Some("draft")).unwrap();
        let edited = update_marker_text(&conn, n.id, Some(" final  text ")).unwrap();
        assert_eq!(edited.text.as_deref(), Some("final text"));
        assert_eq!(edited.at_ms, 5_000);
        assert_eq!(edited.created_at, n.created_at);
        assert!(update_marker_text(&conn, n.id, Some("  ")).is_err());
        assert!(update_marker_text(&conn, n.id, None).is_err());
        assert_eq!(
            get_marker(&conn, n.id).unwrap().unwrap().text.as_deref(),
            Some("final text")
        );
        let h = insert_marker(&conn, "m1", MarkerKind::Highlight, 1_000, None).unwrap();
        assert_eq!(
            update_marker_text(&conn, h.id, Some("label"))
                .unwrap()
                .text
                .as_deref(),
            Some("label")
        );
        assert_eq!(update_marker_text(&conn, h.id, None).unwrap().text, None);
        assert_eq!(delete_marker(&conn, h.id).unwrap(), "m1");
        assert!(delete_marker(&conn, h.id).is_err());
        assert!(update_marker_text(&conn, 999, Some("x")).is_err());
        assert_eq!(count_markers(&conn, "m1").unwrap(), 1);
    }

    #[test]
    fn markers_survive_retranscription_and_go_with_the_meeting() {
        let (_d, mut conn) = db();
        add(&conn, "m1", "T", "2026-10-08T08:00:00Z");
        insert_marker(&conn, "m1", MarkerKind::Highlight, 1_000, None).unwrap();
        save_transcript(&mut conn, "m1", &[seg(0, 2_000, "Me", "first pass")]).unwrap();
        save_transcript(&mut conn, "m1", &[seg(0, 2_000, "Me", "second pass")]).unwrap();
        assert_eq!(count_markers(&conn, "m1").unwrap(), 1);
        delete_meeting(&conn, "m1").unwrap();
        assert_eq!(count_markers(&conn, "m1").unwrap(), 0);
        let left: i64 = conn
            .query_row("SELECT COUNT(*) FROM markers", [], |r| r.get(0))
            .unwrap();
        assert_eq!(left, 0);
    }

    #[test]
    fn a_marker_is_durable_as_soon_as_it_is_saved() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("parley.db");
        let writer = open_rw(&path).unwrap();
        add(&writer, "live", "Live", "2026-10-08T08:00:00Z");
        insert_marker(&writer, "live", MarkerKind::Highlight, 42_000, None).unwrap();
        // Another connection (parley-mcp, or the app after a crash) sees it
        // without the writer closing or committing anything else.
        let reader = open_ro(&path).unwrap();
        assert_eq!(markers(&reader, "live").unwrap()[0].at_ms, 42_000);
    }

    #[test]
    fn list_markers_filters_by_meeting_kind_and_date() {
        let (_d, conn) = db();
        add(&conn, "a", "Hiring loop", "2026-10-01T09:00:00Z");
        add(&conn, "b", "Budget review", "2026-10-05T09:00:00Z");
        insert_marker(
            &conn,
            "a",
            MarkerKind::Note,
            3_000,
            Some("strong candidate"),
        )
        .unwrap();
        insert_marker(&conn, "b", MarkerKind::Highlight, 9_000, None).unwrap();
        insert_marker(&conn, "b", MarkerKind::Note, 2_000, Some("cut travel")).unwrap();

        let ids = |q: MarkerQuery| {
            list_markers(&conn, &q)
                .unwrap()
                .into_iter()
                .map(|h| (h.marker.meeting_id, h.marker.at_ms))
                .collect::<Vec<_>>()
        };
        // Newest meeting first, then by time.
        assert_eq!(
            ids(MarkerQuery::default()),
            vec![
                ("b".into(), 2_000),
                ("b".into(), 9_000),
                ("a".into(), 3_000)
            ]
        );
        let all = list_markers(&conn, &MarkerQuery::default()).unwrap();
        assert_eq!(all[0].meeting_title, "Budget review");
        assert_eq!(all[0].started_at, "2026-10-05T09:00:00.000Z");
        assert_eq!(
            ids(MarkerQuery {
                meeting_id: Some(" a ".into()),
                ..MarkerQuery::default()
            }),
            vec![("a".into(), 3_000)]
        );
        assert_eq!(
            ids(MarkerQuery {
                kind: Some(MarkerKind::Highlight),
                ..MarkerQuery::default()
            }),
            vec![("b".into(), 9_000)]
        );
        assert_eq!(
            ids(MarkerQuery {
                from: Some("2026-09-30T00:00:00Z".into()),
                to: Some("2026-10-02T00:00:00Z".into()),
                ..MarkerQuery::default()
            }),
            vec![("a".into(), 3_000)]
        );
        assert_eq!(
            ids(MarkerQuery {
                limit: Some(1),
                ..MarkerQuery::default()
            })
            .len(),
            1
        );
        assert!(ids(MarkerQuery {
            meeting_id: Some("nope".into()),
            ..MarkerQuery::default()
        })
        .is_empty());
        assert!(list_markers(
            &conn,
            &MarkerQuery {
                from: Some("last week".into()),
                ..MarkerQuery::default()
            }
        )
        .is_err());
    }

    #[test]
    fn segments_between_returns_overlapping_segments() {
        let (_d, mut conn) = db();
        add(&conn, "m1", "T", "2026-10-08T08:00:00Z");
        save_transcript(
            &mut conn,
            "m1",
            &[
                seg(0, 10_000, "Me", "a"),
                seg(20_000, 40_000, "Them", "b"),
                seg(95_000, 99_000, "Me", "c"),
            ],
        )
        .unwrap();
        let texts = |from, to| {
            segments_between(&conn, "m1", from, to)
                .unwrap()
                .into_iter()
                .map(|s| s.text)
                .collect::<Vec<_>>()
        };
        assert_eq!(texts(30_000, 90_000), vec!["b"]);
        assert_eq!(texts(-30_000, 30_000), vec!["a", "b"]);
        assert_eq!(texts(10_000, 20_000), vec!["a", "b"]);
        assert!(texts(41_000, 94_000).is_empty());
    }

    #[test]
    fn marker_kinds_round_trip() {
        for k in [MarkerKind::Highlight, MarkerKind::Note] {
            assert_eq!(MarkerKind::parse(k.as_str()), Some(k));
            assert_eq!(
                serde_json::to_value(k).unwrap(),
                serde_json::json!(k.as_str())
            );
        }
        assert_eq!(MarkerKind::parse("star"), None);
        assert!(serde_json::from_value::<MarkerKind>(serde_json::json!("star")).is_err());
        assert!(serde_json::from_value::<MarkerKind>(serde_json::Value::Null).is_err());
    }
}
