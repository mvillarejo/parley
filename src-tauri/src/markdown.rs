//! `transcript.md`: the Claude-friendly artifact for one meeting. YAML front
//! matter with the metadata, a "Highlights & notes" list, then the
//! conversation grouped into speaker turns with the markers inline:
//!
//! ```text
//! > ⭐ **Highlight** [00:12:34]
//! > 📝 **Note** [00:12:34]: the note's text
//! ```

use crate::store::{Marker, MarkerKind, Meeting, Segment};
use chrono::{DateTime, Local};

/// `HH:MM:SS` for a millisecond offset.
pub fn timestamp(ms: i64) -> String {
    let total = ms.max(0) / 1000;
    format!(
        "{:02}:{:02}:{:02}",
        total / 3600,
        (total % 3600) / 60,
        total % 60
    )
}

/// Same speaker, and less than this between segments: one paragraph.
const TURN_GAP_MS: i64 = 4_000;

#[derive(Debug, Clone, PartialEq)]
pub struct Turn {
    pub speaker: String,
    pub start_ms: i64,
    pub end_ms: i64,
    pub text: String,
}

/// Group consecutive segments of the same speaker into turns.
pub fn turns(segments: &[Segment]) -> Vec<Turn> {
    let mut out: Vec<Turn> = Vec::new();
    for s in segments {
        match out.last_mut() {
            Some(t) if t.speaker == s.speaker && s.start_ms - t.end_ms < TURN_GAP_MS => {
                t.text.push(' ');
                t.text.push_str(&s.text);
                t.end_ms = t.end_ms.max(s.end_ms);
            }
            _ => out.push(Turn {
                speaker: s.speaker.clone(),
                start_ms: s.start_ms,
                end_ms: s.end_ms,
                text: s.text.clone(),
            }),
        }
    }
    out
}

/// A turn of conversation or a marker, in transcript order.
#[derive(Debug, Clone, PartialEq)]
pub enum Entry<'a> {
    Turn(Turn),
    Marker(&'a Marker),
}

/// Interleave turns and markers by time. A marker goes before the first
/// segment that starts at or after it, and splits a turn it falls inside, so it
/// sits exactly where it was made.
pub fn timeline<'a>(segments: &[Segment], markers: &'a [Marker]) -> Vec<Entry<'a>> {
    let mut sorted: Vec<&Marker> = markers.iter().collect();
    sorted.sort_by_key(|m| (m.at_ms, m.id));
    let mut pending = sorted.into_iter().peekable();
    let mut out = Vec::new();
    let mut chunk: Vec<Segment> = Vec::new();
    for s in segments {
        if pending.peek().is_some_and(|m| m.at_ms <= s.start_ms) {
            out.extend(turns(&chunk).into_iter().map(Entry::Turn));
            chunk.clear();
            while let Some(m) = pending.next_if(|m| m.at_ms <= s.start_ms) {
                out.push(Entry::Marker(m));
            }
        }
        chunk.push(s.clone());
    }
    out.extend(turns(&chunk).into_iter().map(Entry::Turn));
    out.extend(pending.map(Entry::Marker));
    out
}

/// `⭐ **Highlight** [00:12:34]` or `📝 **Note** [00:12:34]: text`.
pub fn marker_line(m: &Marker) -> String {
    let (icon, label) = match m.kind {
        MarkerKind::Highlight => ("⭐", "Highlight"),
        MarkerKind::Note => ("📝", "Note"),
    };
    let mut line = format!("{icon} **{label}** [{}]", timestamp(m.at_ms));
    if let Some(text) = m.text.as_deref().filter(|t| !t.is_empty()) {
        line.push_str(": ");
        line.push_str(text);
    }
    line
}

fn yaml_str(value: &str) -> String {
    // A JSON string is a valid YAML double-quoted scalar.
    serde_json::to_string(value).unwrap_or_else(|_| "\"\"".to_string())
}

fn local_time(rfc3339: &str) -> Option<DateTime<Local>> {
    DateTime::parse_from_rfc3339(rfc3339)
        .ok()
        .map(|dt| dt.with_timezone(&Local))
}

fn human_duration(ms: i64) -> String {
    let minutes = (ms.max(0) + 30_000) / 60_000;
    if minutes < 1 {
        "under a minute".to_string()
    } else if minutes < 60 {
        format!("{minutes} min")
    } else {
        format!("{} h {:02} min", minutes / 60, minutes % 60)
    }
}

pub fn render(meeting: &Meeting, segments: &[Segment], markers: &[Marker]) -> String {
    let speakers = crate::merge::speakers(segments);
    let date = local_time(&meeting.started_at)
        .map(|dt| dt.to_rfc3339_opts(chrono::SecondsFormat::Secs, false))
        .unwrap_or_else(|| meeting.started_at.clone());
    let mut out = String::new();
    out.push_str("---\n");
    out.push_str(&format!("title: {}\n", yaml_str(&meeting.title)));
    out.push_str(&format!("id: {}\n", yaml_str(&meeting.id)));
    out.push_str(&format!("date: {date}\n"));
    out.push_str(&format!(
        "duration: {}\n",
        yaml_str(&timestamp(meeting.duration_ms))
    ));
    out.push_str(&format!(
        "duration_seconds: {}\n",
        meeting.duration_ms / 1000
    ));
    out.push_str(&format!(
        "participants: [{}]\n",
        speakers
            .iter()
            .map(|s| yaml_str(s))
            .collect::<Vec<_>>()
            .join(", ")
    ));
    out.push_str(&format!("status: {}\n", meeting.status.as_str()));
    out.push_str(&format!("markers: {}\n", markers.len()));
    if let Some(profile) = &meeting.profile_name {
        out.push_str(&format!("transcribed_with: {}\n", yaml_str(profile)));
    }
    if let Some(lang) = &meeting.language {
        out.push_str(&format!("language: {}\n", yaml_str(lang)));
    }
    out.push_str("source: Parley\n");
    out.push_str("---\n\n");
    out.push_str(&format!("# {}\n\n", meeting.title));
    let when = local_time(&meeting.started_at)
        .map(|dt| dt.format("%A, %-d %B %Y, %H:%M").to_string())
        .unwrap_or_else(|| meeting.started_at.clone());
    let mut subtitle = vec![when, human_duration(meeting.duration_ms)];
    if !speakers.is_empty() {
        subtitle.push(speakers.join(", "));
    }
    out.push_str(&format!("_{}_\n\n", subtitle.join(" · ")));
    let mut sorted: Vec<&Marker> = markers.iter().collect();
    sorted.sort_by_key(|m| (m.at_ms, m.id));
    if !sorted.is_empty() {
        out.push_str("## Highlights & notes\n\n");
        for m in &sorted {
            out.push_str(&format!("- {}\n", marker_line(m)));
        }
        out.push('\n');
    }
    out.push_str(
        "> Speaker labels come from the audio source: **Me** is the local microphone, **Them** is the call audio.\n\n",
    );
    if !sorted.is_empty() {
        out.push_str("## Transcript\n\n");
    }
    if segments.is_empty() {
        out.push_str("_No speech was transcribed._\n");
        if sorted.is_empty() {
            return out;
        }
        out.push('\n');
    }
    for entry in timeline(segments, markers) {
        match entry {
            Entry::Turn(turn) => out.push_str(&format!(
                "**{}** [{}]\n{}\n\n",
                turn.speaker,
                timestamp(turn.start_ms),
                turn.text
            )),
            Entry::Marker(m) => out.push_str(&format!("> {}\n\n", marker_line(m))),
        }
    }
    out.truncate(out.trim_end().len());
    out.push('\n');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{MarkerKind, MeetingStatus};

    fn meeting() -> Meeting {
        Meeting {
            id: "20261008-101500-ab12".into(),
            title: "Q4 \"planning\": budget".into(),
            started_at: "2026-10-08T08:15:00.000Z".into(),
            ended_at: Some("2026-10-08T09:05:30.000Z".into()),
            duration_ms: 3_030_000,
            status: MeetingStatus::Ready,
            error: None,
            capture_warning: None,
            profile_name: Some("Local Ollama".into()),
            provider: Some("openai_compat".into()),
            language: None,
            folder: "meetings/20261008-101500-ab12".into(),
            speakers: vec![],
            segment_count: 0,
            updated_at: "2026-10-08T09:06:00.000Z".into(),
        }
    }

    fn seg(start: i64, end: i64, speaker: &str, text: &str) -> Segment {
        Segment {
            start_ms: start,
            end_ms: end,
            speaker: speaker.into(),
            text: text.into(),
        }
    }

    #[test]
    fn timestamps_are_zero_padded() {
        assert_eq!(timestamp(0), "00:00:00");
        assert_eq!(timestamp(61_999), "00:01:01");
        assert_eq!(timestamp(3_723_000), "01:02:03");
        assert_eq!(timestamp(-5), "00:00:00");
    }

    #[test]
    fn consecutive_segments_of_one_speaker_form_a_turn() {
        let t = turns(&[
            seg(0, 1000, "Me", "Hi."),
            seg(1500, 3000, "Me", "Ready?"),
            seg(3500, 5000, "Them", "Yes."),
            seg(20_000, 21_000, "Them", "Later point."),
        ]);
        assert_eq!(t.len(), 3);
        assert_eq!(t[0].text, "Hi. Ready?");
        assert_eq!(t[0].end_ms, 3000);
        assert_eq!(t[2].start_ms, 20_000);
    }

    #[test]
    fn renders_front_matter_and_turns() {
        let md = render(
            &meeting(),
            &[
                seg(1000, 2000, "Me", "Let's start."),
                seg(65_000, 70_000, "Them 1", "Agreed."),
            ],
            &[],
        );
        assert!(
            md.starts_with("---\ntitle: \"Q4 \\\"planning\\\": budget\"\n"),
            "{md}"
        );
        assert!(md.contains("\nduration: \"00:50:30\"\n"));
        assert!(md.contains("\nduration_seconds: 3030\n"));
        assert!(md.contains("\nparticipants: [\"Me\", \"Them 1\"]\n"));
        assert!(md.contains("\ntranscribed_with: \"Local Ollama\"\n"));
        assert!(md.contains("\n# Q4 \"planning\": budget\n"));
        assert!(md.contains("**Me** [00:00:01]\nLet's start.\n\n**Them 1** [00:01:05]\nAgreed.\n"));
        assert!(md.ends_with("Agreed.\n"));
        assert!(md.contains("51 min"), "{md}");
        assert!(md.contains("\nmarkers: 0\n"));
        assert!(!md.contains("Highlights & notes"));
        assert!(!md.contains("## Transcript"));
    }

    #[test]
    fn empty_transcript_says_so() {
        let md = render(&meeting(), &[], &[]);
        assert!(md.contains("participants: []"));
        assert!(md.contains("_No speech was transcribed._"));
    }

    fn marker(id: i64, at_ms: i64, kind: MarkerKind, text: Option<&str>) -> Marker {
        Marker {
            id,
            meeting_id: "m".into(),
            at_ms,
            kind,
            text: text.map(String::from),
            created_at: "2026-10-08T08:20:00.000Z".into(),
        }
    }

    #[test]
    fn marker_lines_follow_the_documented_format() {
        assert_eq!(
            marker_line(&marker(1, 754_000, MarkerKind::Highlight, None)),
            "⭐ **Highlight** [00:12:34]"
        );
        assert_eq!(
            marker_line(&marker(
                2,
                754_000,
                MarkerKind::Note,
                Some("Ask for the numbers")
            )),
            "📝 **Note** [00:12:34]: Ask for the numbers"
        );
        assert_eq!(
            marker_line(&marker(3, 0, MarkerKind::Highlight, Some("label"))),
            "⭐ **Highlight** [00:00:00]: label"
        );
    }

    #[test]
    fn markers_split_turns_at_their_moment() {
        let segs = [
            seg(0, 1000, "Me", "One."),
            seg(1500, 2500, "Me", "Two."),
            seg(3000, 4000, "Them", "Three."),
        ];
        let marks = [
            marker(2, 5000, MarkerKind::Note, Some("after")),
            marker(1, 1500, MarkerKind::Highlight, None),
        ];
        let entries = timeline(&segs, &marks);
        let shape: Vec<String> = entries
            .iter()
            .map(|e| match e {
                Entry::Turn(t) => format!("{}:{}", t.speaker, t.text),
                Entry::Marker(m) => format!("marker@{}", m.at_ms),
            })
            .collect();
        assert_eq!(
            shape,
            vec![
                "Me:One.",
                "marker@1500",
                "Me:Two.",
                "Them:Three.",
                "marker@5000"
            ]
        );
        // Without markers it is just the turns.
        assert_eq!(timeline(&segs, &[]).len(), turns(&segs).len());
        // A marker before any speech comes first.
        assert!(matches!(
            timeline(&segs, &[marker(9, 0, MarkerKind::Highlight, None)])[0],
            Entry::Marker(_)
        ));
    }

    #[test]
    fn renders_markers_in_front_matter_list_and_inline() {
        let md = render(
            &meeting(),
            &[
                seg(1000, 2000, "Me", "Let's start."),
                seg(65_000, 70_000, "Them 1", "Agreed."),
            ],
            &[
                marker(7, 66_000, MarkerKind::Note, Some("Finance agreed")),
                marker(3, 30_000, MarkerKind::Highlight, None),
            ],
        );
        assert!(md.contains("\nmarkers: 2\n"), "{md}");
        let list = "## Highlights & notes\n\n- ⭐ **Highlight** [00:00:30]\n- 📝 **Note** [00:01:06]: Finance agreed\n\n";
        assert!(md.contains(list), "{md}");
        // The list comes right after the title block, before the transcript.
        assert!(md.find(list).unwrap() < md.find("## Transcript").unwrap());
        assert!(md.contains(
            "**Me** [00:00:01]\nLet's start.\n\n> ⭐ **Highlight** [00:00:30]\n\n**Them 1** [00:01:05]\nAgreed.\n\n> 📝 **Note** [00:01:06]: Finance agreed\n"
        ), "{md}");
        assert!(md.ends_with("Finance agreed\n"), "{md}");
    }

    #[test]
    fn markers_show_even_without_transcribed_speech() {
        let md = render(
            &meeting(),
            &[],
            &[marker(1, 1000, MarkerKind::Highlight, None)],
        );
        assert!(md.contains("_No speech was transcribed._"));
        assert!(md.contains("> ⭐ **Highlight** [00:00:01]\n"), "{md}");
    }
}
