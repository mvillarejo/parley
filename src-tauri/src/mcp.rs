//! A small, dependency-free MCP server (JSON-RPC 2.0 over stdio, one JSON
//! message per line) that exposes Parley's meetings to Claude, read-only.
//!
//! Tools: `list_meetings`, `get_meeting`, `get_transcript`, `search_transcripts`,
//! `list_highlights`.
//! Resources: `parley://meeting/{id}` (the meeting's markdown transcript).

use crate::store::{self, ListQuery, Marker, MarkerKind, MarkerQuery, Meeting};
use serde::Deserialize;
use serde_json::{json, Value};
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

pub const SERVER_NAME: &str = "parley";
pub const LATEST_PROTOCOL: &str = "2025-06-18";
const SUPPORTED_PROTOCOLS: [&str; 3] = [LATEST_PROTOCOL, "2025-03-26", "2024-11-05"];
const RESOURCE_PREFIX: &str = "parley://meeting/";
const RESOURCE_LIST_LIMIT: u32 = 200;
/// Transcript context returned around each highlight or note.
pub const MARKER_CONTEXT_MS: i64 = 30_000;

const PARSE_ERROR: i64 = -32700;
const INVALID_REQUEST: i64 = -32600;
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;
const RESOURCE_NOT_FOUND: i64 = -32002;

const INSTRUCTIONS: &str = "Parley records meetings on this Mac and transcribes them. \
Speaker labels come from the audio source: \"Me\" is the user's microphone, \"Them\" \
(or \"Them 1\", \"Them 2\"…) is the call audio. Use search_transcripts to find where \
something was discussed, list_meetings to browse by date, and get_transcript to read a \
meeting. Timestamps are offsets from the start of the recording. \
Highlights and notes are moments the user flagged on purpose, during the call or \
afterwards: they are the user's explicit signal of what matters. Prioritise them in \
summaries, decisions and action items, and use the discussion around them. \
list_highlights returns them across meetings with the transcript within 30 seconds \
either side; get_meeting and get_transcript include them for one meeting (in \
transcript.md as \"> ⭐ **Highlight** [HH:MM:SS]\" and \"> 📝 **Note** [HH:MM:SS]: \
text\").";

pub struct Server {
    /// Fixed database, or `None` to follow the app's storage setting on every
    /// call (so moving the storage folder doesn't strand the server).
    db_path: Option<PathBuf>,
}

struct RpcError {
    code: i64,
    message: String,
}

fn rpc_err(code: i64, message: impl Into<String>) -> RpcError {
    RpcError {
        code,
        message: message.into(),
    }
}

/// Result of a tool call: text for the model plus optional structured data.
struct ToolOutput {
    text: String,
    structured: Option<Value>,
}

impl Server {
    pub fn new(db_path: PathBuf) -> Self {
        Self {
            db_path: Some(db_path),
        }
    }

    /// Resolve the database like the app does, on every call.
    pub fn following_app_settings() -> Self {
        Self { db_path: None }
    }

    pub fn db_path(&self) -> PathBuf {
        self.db_path.clone().unwrap_or_else(crate::paths::db_path)
    }

    /// Serve until stdin closes.
    pub fn run<R: BufRead, W: Write>(&self, input: R, mut output: W) -> std::io::Result<()> {
        for line in input.lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            if let Some(reply) = self.handle_line(&line) {
                writeln!(output, "{reply}")?;
                output.flush()?;
            }
        }
        Ok(())
    }

    /// Handle one raw message. Returns the serialized reply, if any.
    pub fn handle_line(&self, line: &str) -> Option<String> {
        let value: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(e) => {
                return Some(
                    error_response(
                        Value::Null,
                        rpc_err(PARSE_ERROR, format!("Parse error: {e}")),
                    )
                    .to_string(),
                )
            }
        };
        if let Value::Array(batch) = &value {
            let replies: Vec<Value> = batch.iter().filter_map(|m| self.handle(m)).collect();
            return (!replies.is_empty()).then(|| Value::Array(replies).to_string());
        }
        self.handle(&value).map(|v| v.to_string())
    }

    pub fn handle(&self, msg: &Value) -> Option<Value> {
        let id = msg.get("id").cloned();
        let Some(method) = msg.get("method").and_then(Value::as_str) else {
            // A response from the client (we never send requests) or garbage.
            return id.map(|id| error_response(id, rpc_err(INVALID_REQUEST, "Missing method")));
        };
        let Some(id) = id else {
            // Notification (initialized, cancelled…): nothing to answer.
            return None;
        };
        let params = msg.get("params").cloned().unwrap_or(Value::Null);
        let result = match method {
            "initialize" => Ok(self.initialize(&params)),
            "ping" => Ok(json!({})),
            "tools/list" => Ok(json!({ "tools": tool_definitions() })),
            "tools/call" => self.call_tool(&params),
            "resources/list" => self.list_resources(),
            "resources/templates/list" => Ok(json!({
                "resourceTemplates": [{
                    "uriTemplate": format!("{RESOURCE_PREFIX}{{id}}"),
                    "name": "meeting-transcript",
                    "title": "Meeting transcript",
                    "description": "Markdown transcript of a Parley meeting, with YAML front matter.",
                    "mimeType": "text/markdown"
                }]
            })),
            "resources/read" => self.read_resource(&params),
            "prompts/list" => Ok(json!({ "prompts": [] })),
            other => Err(rpc_err(
                METHOD_NOT_FOUND,
                format!("Method not found: {other}"),
            )),
        };
        Some(match result {
            Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
            Err(e) => error_response(id, e),
        })
    }

    fn initialize(&self, params: &Value) -> Value {
        let requested = params
            .get("protocolVersion")
            .and_then(Value::as_str)
            .unwrap_or(LATEST_PROTOCOL);
        let version = if SUPPORTED_PROTOCOLS.contains(&requested) {
            requested
        } else {
            LATEST_PROTOCOL
        };
        json!({
            "protocolVersion": version,
            "capabilities": { "tools": {}, "resources": {} },
            "serverInfo": {
                "name": SERVER_NAME,
                "title": "Parley meeting transcripts",
                "version": env!("CARGO_PKG_VERSION")
            },
            "instructions": INSTRUCTIONS
        })
    }

    fn connection(&self) -> Result<rusqlite::Connection, String> {
        store::open_ro(&self.db_path())
    }

    fn data_dir(&self) -> PathBuf {
        self.db_path()
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_default()
    }

    fn call_tool(&self, params: &Value) -> Result<Value, RpcError> {
        let name = params
            .get("name")
            .and_then(Value::as_str)
            .ok_or_else(|| rpc_err(INVALID_PARAMS, "tools/call needs a tool name"))?;
        let args = match params.get("arguments") {
            None | Some(Value::Null) => json!({}),
            Some(v) => v.clone(),
        };
        let outcome = match name {
            "list_meetings" => self.tool_list_meetings(args),
            "get_meeting" => self.tool_get_meeting(args),
            "get_transcript" => self.tool_get_transcript(args),
            "search_transcripts" => self.tool_search(args),
            "list_highlights" => self.tool_list_highlights(args),
            other => return Err(rpc_err(INVALID_PARAMS, format!("Unknown tool: {other}"))),
        };
        Ok(match outcome {
            Ok(out) => {
                let mut result = json!({
                    "content": [{ "type": "text", "text": out.text }],
                    "isError": false
                });
                if let Some(structured) = out.structured {
                    result["structuredContent"] = structured;
                }
                result
            }
            Err(message) => json!({
                "content": [{ "type": "text", "text": message }],
                "isError": true
            }),
        })
    }

    fn meeting_json(&self, m: &Meeting) -> Value {
        let folder = crate::paths::meeting_dir(&self.data_dir(), &m.folder);
        json!({
            "id": m.id,
            "title": m.title,
            "started_at": m.started_at,
            "ended_at": m.ended_at,
            "duration_seconds": m.duration_ms / 1000,
            "duration": crate::markdown::timestamp(m.duration_ms),
            "status": m.status.as_str(),
            "error": m.error,
            "speakers": m.speakers,
            "segment_count": m.segment_count,
            "transcribed_with": m.profile_name,
            "resource_uri": format!("{RESOURCE_PREFIX}{}", m.id),
            "transcript_path": folder.join(store::TRANSCRIPT_FILE),
            "folder": folder,
        })
    }

    fn tool_list_meetings(&self, args: Value) -> Result<ToolOutput, String> {
        #[derive(Deserialize)]
        struct Args {
            from: Option<String>,
            to: Option<String>,
            query: Option<String>,
            limit: Option<u32>,
        }
        let args: Args = parse_args(args)?;
        let conn = self.connection()?;
        let meetings = store::list_meetings(
            &conn,
            &ListQuery {
                from: args.from,
                to: args.to,
                query: args.query,
                limit: Some(args.limit.unwrap_or(20)),
                offset: None,
            },
        )?;
        let list: Vec<Value> = meetings.iter().map(|m| self.meeting_json(m)).collect();
        let structured = json!({ "meetings": list });
        Ok(ToolOutput {
            text: if meetings.is_empty() {
                "No meetings match.".to_string()
            } else {
                serde_json::to_string_pretty(&structured).unwrap_or_default()
            },
            structured: Some(structured),
        })
    }

    fn tool_get_meeting(&self, args: Value) -> Result<ToolOutput, String> {
        #[derive(Deserialize)]
        struct Args {
            id: String,
        }
        let args: Args = parse_args(args)?;
        let conn = self.connection()?;
        let meeting = store::require_meeting(&conn, args.id.trim())?;
        let mut value = self.meeting_json(&meeting);
        value["markers"] = store::markers(&conn, &meeting.id)?
            .iter()
            .map(marker_json)
            .collect();
        Ok(ToolOutput {
            text: serde_json::to_string_pretty(&value).unwrap_or_default(),
            structured: Some(value),
        })
    }

    fn tool_get_transcript(&self, args: Value) -> Result<ToolOutput, String> {
        #[derive(Deserialize)]
        #[serde(rename_all = "lowercase")]
        enum Format {
            Markdown,
            Segments,
        }
        #[derive(Deserialize)]
        struct Args {
            id: String,
            format: Option<Format>,
        }
        let args: Args = parse_args(args)?;
        let conn = self.connection()?;
        let meeting = store::require_meeting(&conn, args.id.trim())?;
        let segments = store::segments(&conn, &meeting.id)?;
        let markers = store::markers(&conn, &meeting.id)?;
        let pending = match meeting.status {
            store::MeetingStatus::Ready => None,
            store::MeetingStatus::Recording => Some("is still being recorded"),
            store::MeetingStatus::Transcribing => Some("is still being transcribed"),
            store::MeetingStatus::Failed => Some("failed to transcribe"),
        };
        if let (Some(why), true) = (pending, segments.is_empty()) {
            return Err(format!(
                "Meeting {} {why}; no transcript yet.{}{}",
                meeting.id,
                meeting
                    .error
                    .as_deref()
                    .map(|e| format!(" Error: {e}"))
                    .unwrap_or_default(),
                if markers.is_empty() {
                    String::new()
                } else {
                    format!(
                        " It has {} highlight(s)/note(s): see get_meeting or list_highlights.",
                        markers.len()
                    )
                }
            ));
        }
        match args.format.unwrap_or(Format::Markdown) {
            Format::Markdown => Ok(ToolOutput {
                text: crate::markdown::render(&meeting, &segments, &markers),
                structured: None,
            }),
            Format::Segments => {
                let value = json!({
                    "meeting_id": meeting.id,
                    "title": meeting.title,
                    "started_at": meeting.started_at,
                    "segments": segments.iter().map(|s| json!({
                        "start_ms": s.start_ms,
                        "end_ms": s.end_ms,
                        "start": crate::markdown::timestamp(s.start_ms),
                        "speaker": s.speaker,
                        "text": s.text,
                    })).collect::<Vec<_>>(),
                    "markers": markers.iter().map(marker_json).collect::<Vec<_>>()
                });
                Ok(ToolOutput {
                    text: serde_json::to_string_pretty(&value).unwrap_or_default(),
                    structured: Some(value),
                })
            }
        }
    }

    fn tool_search(&self, args: Value) -> Result<ToolOutput, String> {
        #[derive(Deserialize)]
        struct Args {
            query: String,
            limit: Option<u32>,
        }
        let args: Args = parse_args(args)?;
        if store::fts_query(&args.query).is_none() {
            return Err("query must contain at least one word".to_string());
        }
        let conn = self.connection()?;
        let hits = store::search(&conn, &args.query, args.limit.unwrap_or(20))?;
        let list: Vec<Value> = hits
            .iter()
            .map(|h| {
                json!({
                    "meeting_id": h.meeting_id,
                    "meeting_title": h.meeting_title,
                    "started_at": h.started_at,
                    "start_ms": h.start_ms,
                    "timestamp": crate::markdown::timestamp(h.start_ms),
                    "speaker": h.speaker,
                    "snippet": h.snippet,
                })
            })
            .collect();
        let structured = json!({ "results": list });
        Ok(ToolOutput {
            text: if hits.is_empty() {
                format!("No transcript mentions {:?}.", args.query)
            } else {
                serde_json::to_string_pretty(&structured).unwrap_or_default()
            },
            structured: Some(structured),
        })
    }

    fn tool_list_highlights(&self, args: Value) -> Result<ToolOutput, String> {
        #[derive(Deserialize)]
        struct Args {
            meeting_id: Option<String>,
            from: Option<String>,
            to: Option<String>,
            kind: Option<MarkerKind>,
            limit: Option<u32>,
        }
        let args: Args = parse_args(args)?;
        let conn = self.connection()?;
        let hits = store::list_markers(
            &conn,
            &MarkerQuery {
                meeting_id: args.meeting_id,
                from: args.from,
                to: args.to,
                kind: args.kind,
                limit: Some(args.limit.unwrap_or(50)),
            },
        )?;
        let mut list = Vec::with_capacity(hits.len());
        for h in &hits {
            let m = &h.marker;
            let context = store::segments_between(
                &conn,
                &m.meeting_id,
                m.at_ms - MARKER_CONTEXT_MS,
                m.at_ms + MARKER_CONTEXT_MS,
            )?;
            let mut value = marker_json(m);
            value["meeting_id"] = json!(m.meeting_id);
            value["meeting_title"] = json!(h.meeting_title);
            value["meeting_started_at"] = json!(h.started_at);
            value["resource_uri"] = json!(format!("{RESOURCE_PREFIX}{}", m.meeting_id));
            value["context"] = context
                .iter()
                .map(|s| {
                    json!({
                        "start_ms": s.start_ms,
                        "timestamp": crate::markdown::timestamp(s.start_ms),
                        "speaker": s.speaker,
                        "text": s.text,
                    })
                })
                .collect();
            list.push(value);
        }
        let structured = json!({ "highlights": list });
        Ok(ToolOutput {
            text: if hits.is_empty() {
                "No highlights or notes match.".to_string()
            } else {
                serde_json::to_string_pretty(&structured).unwrap_or_default()
            },
            structured: Some(structured),
        })
    }

    fn list_resources(&self) -> Result<Value, RpcError> {
        let resources: Vec<Value> = match self.connection() {
            // No database yet: nothing to list (not an error for the client).
            Err(_) => Vec::new(),
            Ok(conn) => store::list_meetings(
                &conn,
                &ListQuery {
                    limit: Some(RESOURCE_LIST_LIMIT),
                    ..ListQuery::default()
                },
            )
            .map_err(|e| rpc_err(-32603, e))?
            .into_iter()
            .map(|m| {
                json!({
                    "uri": format!("{RESOURCE_PREFIX}{}", m.id),
                    "name": m.id,
                    "title": m.title,
                    "description": format!(
                        "{} · {} · {}",
                        m.started_at,
                        crate::markdown::timestamp(m.duration_ms),
                        m.status.as_str()
                    ),
                    "mimeType": "text/markdown"
                })
            })
            .collect(),
        };
        Ok(json!({ "resources": resources }))
    }

    fn read_resource(&self, params: &Value) -> Result<Value, RpcError> {
        let uri = params
            .get("uri")
            .and_then(Value::as_str)
            .ok_or_else(|| rpc_err(INVALID_PARAMS, "resources/read needs a uri"))?;
        let id = uri
            .strip_prefix(RESOURCE_PREFIX)
            .filter(|id| !id.is_empty())
            .ok_or_else(|| rpc_err(RESOURCE_NOT_FOUND, format!("Resource not found: {uri}")))?;
        let conn = self
            .connection()
            .map_err(|e| rpc_err(RESOURCE_NOT_FOUND, e))?;
        let meeting = store::get_meeting(&conn, id)
            .map_err(|e| rpc_err(-32603, e))?
            .ok_or_else(|| rpc_err(RESOURCE_NOT_FOUND, format!("Resource not found: {uri}")))?;
        let segments = store::segments(&conn, &meeting.id).map_err(|e| rpc_err(-32603, e))?;
        let markers = store::markers(&conn, &meeting.id).map_err(|e| rpc_err(-32603, e))?;
        Ok(json!({
            "contents": [{
                "uri": uri,
                "mimeType": "text/markdown",
                "text": crate::markdown::render(&meeting, &segments, &markers)
            }]
        }))
    }
}

fn marker_json(m: &Marker) -> Value {
    json!({
        "id": m.id,
        "kind": m.kind.as_str(),
        "at_ms": m.at_ms,
        "timestamp": crate::markdown::timestamp(m.at_ms),
        "text": m.text,
        "created_at": m.created_at,
    })
}

fn parse_args<T: for<'de> Deserialize<'de>>(args: Value) -> Result<T, String> {
    serde_json::from_value(args).map_err(|e| format!("Invalid arguments: {e}"))
}

fn error_response(id: Value, e: RpcError) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": e.code, "message": e.message }
    })
}

pub fn tool_definitions() -> Value {
    json!([
        {
            "name": "list_meetings",
            "title": "List meetings",
            "description": "List recorded meetings, newest first. Filter by date range (YYYY-MM-DD in local time, or RFC 3339) and/or a text query that matches titles and transcript text.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "from": { "type": "string", "description": "Earliest start date, inclusive (e.g. 2026-10-01)." },
                    "to": { "type": "string", "description": "Latest start date, inclusive (e.g. 2026-10-08)." },
                    "query": { "type": "string", "description": "Words that must appear in the title or transcript." },
                    "limit": { "type": "integer", "minimum": 1, "maximum": 500, "default": 20 }
                }
            },
            "annotations": { "readOnlyHint": true }
        },
        {
            "name": "get_meeting",
            "title": "Get meeting",
            "description": "Metadata for one meeting: title, start/end time, duration, status, speakers, file paths, and the highlights and notes the user marked (with timestamps).",
            "inputSchema": {
                "type": "object",
                "properties": { "id": { "type": "string", "description": "Meeting id from list_meetings or search_transcripts." } },
                "required": ["id"]
            },
            "annotations": { "readOnlyHint": true }
        },
        {
            "name": "get_transcript",
            "title": "Get transcript",
            "description": "The transcript of one meeting. format=markdown (default) returns the transcript.md document with YAML front matter, a Highlights & notes list and speaker turns with the user's highlights and notes inline; format=segments returns timestamped segments and markers as JSON.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "id": { "type": "string" },
                    "format": { "type": "string", "enum": ["markdown", "segments"], "default": "markdown" }
                },
                "required": ["id"]
            },
            "annotations": { "readOnlyHint": true }
        },
        {
            "name": "search_transcripts",
            "title": "Search transcripts",
            "description": "Full-text search across all transcripts. Every word must match (the last one as a prefix). Returns snippets with meeting id, speaker and timestamp.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "query": { "type": "string" },
                    "limit": { "type": "integer", "minimum": 1, "maximum": 500, "default": 20 }
                },
                "required": ["query"]
            },
            "annotations": { "readOnlyHint": true }
        },
        {
            "name": "list_highlights",
            "title": "List highlights and notes",
            "description": "The moments the user flagged on purpose, during a call or afterwards: highlights and notes, across meetings (newest meeting first, then in time order). Each has its meeting id and title, timestamp, note text, and the transcript segments within 30 seconds either side. They are the user's explicit signal of what matters: lead with them in summaries and action items.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "meeting_id": { "type": "string", "description": "Only this meeting." },
                    "from": { "type": "string", "description": "Earliest meeting start date, inclusive (e.g. 2026-10-01)." },
                    "to": { "type": "string", "description": "Latest meeting start date, inclusive (e.g. 2026-10-08)." },
                    "kind": { "type": "string", "enum": ["highlight", "note"], "description": "Only highlights or only notes." },
                    "limit": { "type": "integer", "minimum": 1, "maximum": 500, "default": 50 }
                }
            },
            "annotations": { "readOnlyHint": true }
        }
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn server() -> (tempfile::TempDir, Server) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("parley.db");
        (dir, Server::new(path))
    }

    fn req(method: &str, params: Value) -> Value {
        json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": params })
    }

    #[test]
    fn initialize_negotiates_the_protocol_version() {
        let (_d, s) = server();
        let r = s
            .handle(&req(
                "initialize",
                json!({ "protocolVersion": "2025-03-26" }),
            ))
            .unwrap();
        assert_eq!(r["result"]["protocolVersion"], "2025-03-26");
        assert_eq!(r["result"]["serverInfo"]["name"], "parley");
        assert!(r["result"]["capabilities"]["tools"].is_object());
        let r = s
            .handle(&req(
                "initialize",
                json!({ "protocolVersion": "1999-01-01" }),
            ))
            .unwrap();
        assert_eq!(r["result"]["protocolVersion"], LATEST_PROTOCOL);
    }

    #[test]
    fn notifications_get_no_reply_and_unknown_methods_error() {
        let (_d, s) = server();
        assert!(s
            .handle(&json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }))
            .is_none());
        let r = s.handle(&req("bogus/method", json!({}))).unwrap();
        assert_eq!(r["error"]["code"], METHOD_NOT_FOUND);
        let r = s.handle_line("{not json").unwrap();
        assert!(r.contains("-32700"));
    }

    #[test]
    fn tools_list_declares_the_five_tools() {
        let (_d, s) = server();
        let r = s.handle(&req("tools/list", json!({}))).unwrap();
        let names: Vec<_> = r["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(
            names,
            vec![
                "list_meetings",
                "get_meeting",
                "get_transcript",
                "search_transcripts",
                "list_highlights"
            ]
        );
    }

    #[test]
    fn missing_database_is_a_tool_error_not_a_crash() {
        let (_d, s) = server();
        let r = s
            .handle(&req("tools/call", json!({ "name": "list_meetings" })))
            .unwrap();
        assert_eq!(r["result"]["isError"], true);
        assert!(r["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("No Parley database"));
        let r = s.handle(&req("resources/list", json!({}))).unwrap();
        assert_eq!(r["result"]["resources"], json!([]));
    }

    #[test]
    fn bad_arguments_are_reported_as_tool_errors() {
        let (_d, s) = server();
        for (name, args) in [
            ("get_meeting", json!({})),
            ("get_meeting", json!({ "id": 5 })),
            ("get_meeting", json!({ "id": null })),
            ("get_transcript", json!({ "id": "x", "format": "pdf" })),
            ("list_meetings", json!({ "limit": "ten" })),
            ("search_transcripts", json!({ "query": "  " })),
            ("list_highlights", json!({ "kind": "star" })),
            ("list_highlights", json!({ "kind": 3 })),
            ("list_highlights", json!({ "limit": "ten" })),
            ("list_highlights", json!({ "meeting_id": 5 })),
            ("list_highlights", json!({ "from": "last week" })),
        ] {
            let r = s
                .handle(&req(
                    "tools/call",
                    json!({ "name": name, "arguments": args }),
                ))
                .unwrap();
            assert_eq!(r["result"]["isError"], true, "{name} {args}");
        }
        let r = s
            .handle(&req("tools/call", json!({ "name": "drop_tables" })))
            .unwrap();
        assert_eq!(r["error"]["code"], INVALID_PARAMS);
    }
}
