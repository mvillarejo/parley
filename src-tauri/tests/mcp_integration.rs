//! Spawns the real `parley-mcp` binary against a temporary database and
//! drives it over stdio like Claude would.

use chrono::{DateTime, Utc};
use parley_lib::store::{self, MarkerKind, NewMeeting, Segment};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

struct Mcp {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: i64,
}

impl Mcp {
    fn spawn(db: &Path) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_parley-mcp"))
            .arg("--db")
            .arg(db)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn parley-mcp");
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        let mut mcp = Self {
            child,
            stdin,
            stdout,
            next_id: 1,
        };
        let init = mcp.request(
            "initialize",
            json!({
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": { "name": "test", "version": "0" }
            }),
        );
        assert_eq!(init["result"]["serverInfo"]["name"], "parley");
        mcp.notify("notifications/initialized");
        mcp
    }

    fn send(&mut self, msg: &Value) {
        writeln!(self.stdin, "{msg}").unwrap();
        self.stdin.flush().unwrap();
    }

    fn notify(&mut self, method: &str) {
        self.send(&json!({ "jsonrpc": "2.0", "method": method }));
    }

    fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        self.send(&json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }));
        let mut line = String::new();
        self.stdout.read_line(&mut line).unwrap();
        let reply: Value = serde_json::from_str(&line).expect("one JSON reply per line");
        assert_eq!(reply["id"], id, "reply matches request");
        reply
    }

    fn call(&mut self, tool: &str, args: Value) -> Value {
        let reply = self.request("tools/call", json!({ "name": tool, "arguments": args }));
        reply["result"].clone()
    }
}

impl Drop for Mcp {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn at(s: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
}

fn seg(start: i64, end: i64, speaker: &str, text: &str) -> Segment {
    Segment {
        start_ms: start,
        end_ms: end,
        speaker: speaker.into(),
        text: text.into(),
    }
}

fn seed(db: &Path) {
    let mut conn = store::open_rw(db).unwrap();
    for (id, title, started) in [
        ("m-old", "Hiring sync", "2026-09-01T09:00:00Z"),
        ("m-new", "Budget review", "2026-10-07T14:00:00Z"),
        ("m-live", "Still talking", "2026-10-08T10:00:00Z"),
    ] {
        store::insert_meeting(
            &conn,
            &NewMeeting {
                id,
                title,
                started_at: at(started),
                folder: &format!("meetings/{id}"),
            },
        )
        .unwrap();
    }
    store::finish_recording(&conn, "m-new", at("2026-10-07T14:45:00Z"), 2_700_000).unwrap();
    store::save_transcript(
        &mut conn,
        "m-new",
        &[
            seg(1_000, 4_000, "Me", "Can we close the Q4 budget today?"),
            seg(5_000, 9_000, "Them 1", "Yes, marketing spend is approved."),
            seg(
                70_000,
                74_000,
                "Them 2",
                "Infrastructure needs another review.",
            ),
        ],
    )
    .unwrap();
    store::save_transcript(
        &mut conn,
        "m-old",
        &[seg(0, 2_000, "Them", "We should hire two engineers")],
    )
    .unwrap();
    store::insert_marker(&conn, "m-new", MarkerKind::Highlight, 6_000, None).unwrap();
    store::insert_marker(
        &conn,
        "m-new",
        MarkerKind::Note,
        71_000,
        Some("Book the infra review"),
    )
    .unwrap();
    store::insert_marker(&conn, "m-old", MarkerKind::Note, 1_000, Some("Two hires")).unwrap();
    store::insert_marker(&conn, "m-live", MarkerKind::Highlight, 3_000, None).unwrap();
}

#[test]
fn every_tool_and_resource_works_against_a_real_database() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("parley.db");
    seed(&db);
    let mut mcp = Mcp::spawn(&db);

    // tools/list
    let tools = mcp.request("tools/list", json!({}));
    assert_eq!(tools["result"]["tools"].as_array().unwrap().len(), 5);
    let init = mcp.request("initialize", json!({ "protocolVersion": "2025-06-18" }));
    assert!(init["result"]["instructions"]
        .as_str()
        .unwrap()
        .contains("explicit signal of what matters"));

    // list_meetings: newest first, date range, query, limit.
    let all = mcp.call("list_meetings", json!({}));
    assert_eq!(all["isError"], false);
    let ids: Vec<_> = all["structuredContent"]["meetings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["id"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(ids, vec!["m-live", "m-new", "m-old"]);
    let ranged = mcp.call(
        "list_meetings",
        json!({ "from": "2026-10-01T00:00:00Z", "to": "2026-10-07T23:59:59Z" }),
    );
    let meetings = ranged["structuredContent"]["meetings"].as_array().unwrap();
    assert_eq!(meetings.len(), 1);
    assert_eq!(meetings[0]["id"], "m-new");
    assert_eq!(meetings[0]["duration_seconds"], 2700);
    assert_eq!(meetings[0]["speakers"], json!(["Me", "Them 1", "Them 2"]));
    assert!(meetings[0]["transcript_path"]
        .as_str()
        .unwrap()
        .ends_with("meetings/m-new/transcript.md"));
    let by_text = mcp.call("list_meetings", json!({ "query": "engineers" }));
    assert_eq!(by_text["structuredContent"]["meetings"][0]["id"], "m-old");
    let limited = mcp.call("list_meetings", json!({ "limit": 1 }));
    assert_eq!(
        limited["structuredContent"]["meetings"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let bad_date = mcp.call("list_meetings", json!({ "from": "last week" }));
    assert_eq!(bad_date["isError"], true);

    // get_meeting
    let meeting = mcp.call("get_meeting", json!({ "id": "m-new" }));
    assert_eq!(meeting["structuredContent"]["title"], "Budget review");
    assert_eq!(meeting["structuredContent"]["status"], "ready");
    let marks = meeting["structuredContent"]["markers"].as_array().unwrap();
    assert_eq!(marks.len(), 2);
    assert_eq!(marks[0]["kind"], "highlight");
    assert_eq!(marks[0]["timestamp"], "00:00:06");
    assert_eq!(marks[0]["text"], Value::Null);
    assert_eq!(marks[1]["kind"], "note");
    assert_eq!(marks[1]["at_ms"], 71_000);
    assert_eq!(marks[1]["text"], "Book the infra review");
    let missing = mcp.call("get_meeting", json!({ "id": "nope" }));
    assert_eq!(missing["isError"], true);

    // get_transcript, both formats
    let md = mcp.call("get_transcript", json!({ "id": "m-new" }));
    let text = md["content"][0]["text"].as_str().unwrap();
    assert!(text.starts_with("---\ntitle: \"Budget review\""), "{text}");
    assert!(text.contains("**Them 2** [00:01:10]\nInfrastructure needs another review."));
    assert!(text.contains("\nmarkers: 2\n"), "{text}");
    assert!(text.contains("## Highlights & notes\n\n- ⭐ **Highlight** [00:00:06]\n"));
    assert!(text.contains(
        "**Them 2** [00:01:10]\nInfrastructure needs another review.\n\n> 📝 **Note** [00:01:11]: Book the infra review"
    ), "{text}");
    let segs = mcp.call(
        "get_transcript",
        json!({ "id": "m-new", "format": "segments" }),
    );
    let list = segs["structuredContent"]["segments"].as_array().unwrap();
    assert_eq!(list.len(), 3);
    assert_eq!(list[1]["speaker"], "Them 1");
    assert_eq!(list[1]["start"], "00:00:05");
    assert_eq!(
        segs["structuredContent"]["markers"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    let live = mcp.call("get_transcript", json!({ "id": "m-live" }));
    assert_eq!(live["isError"], true);
    assert!(live["content"][0]["text"]
        .as_str()
        .unwrap()
        .contains("still being recorded"));
    assert!(live["content"][0]["text"]
        .as_str()
        .unwrap()
        .contains("1 highlight(s)/note(s)"));

    // search_transcripts
    let found = mcp.call(
        "search_transcripts",
        json!({ "query": "marketing approved" }),
    );
    let results = found["structuredContent"]["results"].as_array().unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0]["meeting_id"], "m-new");
    assert_eq!(results[0]["timestamp"], "00:00:05");
    assert_eq!(results[0]["speaker"], "Them 1");
    assert!(results[0]["snippet"]
        .as_str()
        .unwrap()
        .contains("**marketing**"));
    let none = mcp.call("search_transcripts", json!({ "query": "kubernetes" }));
    assert_eq!(none["isError"], false);
    assert_eq!(none["structuredContent"]["results"], json!([]));

    // resources
    let resources = mcp.request("resources/list", json!({}));
    let uris: Vec<_> = resources["result"]["resources"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["uri"].as_str().unwrap().to_string())
        .collect();
    assert!(uris.contains(&"parley://meeting/m-new".to_string()));
    let templates = mcp.request("resources/templates/list", json!({}));
    assert_eq!(
        templates["result"]["resourceTemplates"][0]["uriTemplate"],
        "parley://meeting/{id}"
    );
    let read = mcp.request("resources/read", json!({ "uri": "parley://meeting/m-new" }));
    let content = &read["result"]["contents"][0];
    assert_eq!(content["mimeType"], "text/markdown");
    assert!(content["text"]
        .as_str()
        .unwrap()
        .contains("# Budget review"));
    assert!(content["text"]
        .as_str()
        .unwrap()
        .contains("> ⭐ **Highlight** [00:00:06]"));
    let gone = mcp.request("resources/read", json!({ "uri": "parley://meeting/nope" }));
    assert_eq!(gone["error"]["code"], -32002);

    // list_highlights: across meetings, newest meeting first, with context.
    let all = mcp.call("list_highlights", json!({}));
    assert_eq!(all["isError"], false);
    let list = all["structuredContent"]["highlights"].as_array().unwrap();
    let order: Vec<_> = list
        .iter()
        .map(|h| {
            (
                h["meeting_id"].as_str().unwrap(),
                h["at_ms"].as_i64().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        order,
        vec![
            ("m-live", 3_000),
            ("m-new", 6_000),
            ("m-new", 71_000),
            ("m-old", 1_000)
        ]
    );
    let first_new = &list[1];
    assert_eq!(first_new["meeting_title"], "Budget review");
    assert_eq!(first_new["meeting_started_at"], "2026-10-07T14:00:00.000Z");
    assert_eq!(first_new["timestamp"], "00:00:06");
    assert_eq!(first_new["resource_uri"], "parley://meeting/m-new");
    // ±30 s around 00:00:06: the first two segments, not the one at 01:10.
    let context: Vec<_> = first_new["context"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["speaker"].as_str().unwrap())
        .collect();
    assert_eq!(context, vec!["Me", "Them 1"]);
    assert_eq!(first_new["context"][1]["timestamp"], "00:00:05");
    let note_ctx = list[2]["context"].as_array().unwrap();
    assert_eq!(note_ctx.len(), 1);
    assert_eq!(note_ctx[0]["speaker"], "Them 2");
    assert_eq!(list[0]["context"], json!([]));
    assert!(all["content"][0]["text"]
        .as_str()
        .unwrap()
        .contains("Book the infra review"));

    let notes = mcp.call("list_highlights", json!({ "kind": "note" }));
    let notes = notes["structuredContent"]["highlights"].as_array().unwrap();
    assert_eq!(notes.len(), 2);
    assert!(notes.iter().all(|n| n["kind"] == "note"));
    let one = mcp.call("list_highlights", json!({ "meeting_id": "m-old" }));
    assert_eq!(
        one["structuredContent"]["highlights"][0]["text"],
        "Two hires"
    );
    assert_eq!(
        one["structuredContent"]["highlights"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let ranged = mcp.call(
        "list_highlights",
        json!({ "from": "2026-10-01T00:00:00Z", "to": "2026-10-07T23:59:59Z" }),
    );
    let ranged = ranged["structuredContent"]["highlights"]
        .as_array()
        .unwrap();
    assert_eq!(ranged.len(), 2);
    assert!(ranged.iter().all(|h| h["meeting_id"] == "m-new"));
    let limited = mcp.call("list_highlights", json!({ "limit": 1 }));
    assert_eq!(
        limited["structuredContent"]["highlights"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let none = mcp.call("list_highlights", json!({ "meeting_id": "nope" }));
    assert_eq!(none["isError"], false);
    assert_eq!(none["content"][0]["text"], "No highlights or notes match.");
    for bad in [
        json!({ "kind": "star" }),
        json!({ "kind": 1 }),
        json!({ "from": "last week" }),
        json!({ "limit": -1 }),
        json!({ "meeting_id": ["m-new"] }),
    ] {
        assert_eq!(
            mcp.call("list_highlights", bad.clone())["isError"],
            true,
            "{bad}"
        );
    }
    // Explicit nulls mean "no filter".
    let nulls = mcp.call(
        "list_highlights",
        json!({ "meeting_id": null, "kind": null, "from": null, "to": null, "limit": null }),
    );
    assert_eq!(
        nulls["structuredContent"]["highlights"]
            .as_array()
            .unwrap()
            .len(),
        4
    );

    // ping
    assert_eq!(mcp.request("ping", json!({}))["result"], json!({}));
}

#[test]
fn server_sees_meetings_written_while_it_runs() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("parley.db");
    let mut writer = store::open_rw(&db).unwrap();
    let mut mcp = Mcp::spawn(&db);
    assert_eq!(
        mcp.call("list_meetings", json!({}))["structuredContent"]["meetings"],
        json!([])
    );
    store::insert_meeting(
        &writer,
        &NewMeeting {
            id: "fresh",
            title: "Just recorded",
            started_at: Utc::now(),
            folder: "meetings/fresh",
        },
    )
    .unwrap();
    store::save_transcript(&mut writer, "fresh", &[seg(0, 1, "Me", "brand new words")]).unwrap();
    let found = mcp.call("search_transcripts", json!({ "query": "brand new" }));
    assert_eq!(
        found["structuredContent"]["results"][0]["meeting_id"],
        "fresh"
    );
}

#[test]
fn missing_database_reports_a_helpful_tool_error() {
    let dir = tempfile::tempdir().unwrap();
    let mut mcp = Mcp::spawn(&dir.path().join("missing.db"));
    let r = mcp.call("list_meetings", json!({}));
    assert_eq!(r["isError"], true);
    assert!(r["content"][0]["text"]
        .as_str()
        .unwrap()
        .contains("No Parley database"));
    assert!(
        !dir.path().join("missing.db").exists(),
        "must not create the DB"
    );
}

#[test]
fn cli_flags_work() {
    let out = Command::new(env!("CARGO_BIN_EXE_parley-mcp"))
        .args(["--db", "/tmp/x/parley.db", "--print-db"])
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        "/tmp/x/parley.db"
    );
    let out = Command::new(env!("CARGO_BIN_EXE_parley-mcp"))
        .arg("--version")
        .output()
        .unwrap();
    assert!(String::from_utf8_lossy(&out.stdout).starts_with("parley-mcp "));
    let out = Command::new(env!("CARGO_BIN_EXE_parley-mcp"))
        .arg("--nope")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    let out = Command::new(env!("CARGO_BIN_EXE_parley-mcp"))
        .arg("--print-db")
        .env("PARLEY_DB", "/tmp/env/parley.db")
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        "/tmp/env/parley.db"
    );
}
