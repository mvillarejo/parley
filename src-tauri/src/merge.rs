//! Turn per-track transcription results into one time-ordered transcript with
//! speaker labels: the mic track is "Me", the system track is "Them" (or
//! "Them 1", "Them 2"… when the provider diarized several voices).

use crate::store::{Segment, SPEAKER_ME, SPEAKER_THEM};
use std::collections::{BTreeSet, HashSet};

/// One utterance as returned for a single track.
#[derive(Debug, Clone, PartialEq)]
pub struct TrackSegment {
    pub start_ms: i64,
    pub end_ms: i64,
    pub text: String,
    /// Diarization speaker index within the track, when available.
    pub speaker: Option<u32>,
}

fn clean_text(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn has_words(text: &str) -> bool {
    text.chars().any(char::is_alphanumeric)
}

pub fn label_me(track: Vec<TrackSegment>) -> Vec<Segment> {
    label(track, |_| SPEAKER_ME.to_string())
}

/// "Them" for one voice; "Them 1", "Them 2"… (numbered by first appearance)
/// when the provider found several.
pub fn label_them(track: Vec<TrackSegment>) -> Vec<Segment> {
    let mut order: Vec<u32> = Vec::new();
    let mut sorted = track.clone();
    sorted.sort_by_key(|s| s.start_ms);
    for s in &sorted {
        if let Some(idx) = s.speaker {
            if !order.contains(&idx) {
                order.push(idx);
            }
        }
    }
    let multiple = order.len() > 1;
    label(track, move |speaker| match (multiple, speaker) {
        (true, Some(idx)) => {
            let n = order.iter().position(|o| *o == idx).unwrap_or(0) + 1;
            format!("{SPEAKER_THEM} {n}")
        }
        _ => SPEAKER_THEM.to_string(),
    })
}

fn label(track: Vec<TrackSegment>, name: impl Fn(Option<u32>) -> String) -> Vec<Segment> {
    track
        .into_iter()
        .filter_map(|s| {
            let text = clean_text(&s.text);
            has_words(&text).then(|| Segment {
                start_ms: s.start_ms.max(0),
                end_ms: s.end_ms.max(s.start_ms).max(0),
                speaker: name(s.speaker),
                text,
            })
        })
        .collect()
}

fn words(text: &str) -> HashSet<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_lowercase)
        .collect()
}

fn similarity(a: &str, b: &str) -> f64 {
    let (a, b) = (words(a), words(b));
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let inter = a.intersection(&b).count() as f64;
    let union = a.union(&b).count() as f64;
    inter / union
}

fn overlap_ms(a: &Segment, b: &Segment) -> i64 {
    (a.end_ms.min(b.end_ms) - a.start_ms.max(b.start_ms)).max(0)
}

/// Without headphones the mic also hears the call. A "Me" segment that
/// overlaps a "Them" segment in time and says nearly the same words is that
/// echo, so it is dropped.
pub fn is_echo(me: &Segment, them: &[Segment]) -> bool {
    let me_len = (me.end_ms - me.start_ms).max(1);
    them.iter().any(|t| {
        overlap_ms(me, t) * 2 >= me_len.min((t.end_ms - t.start_ms).max(1))
            && similarity(&me.text, &t.text) >= 0.6
    })
}

/// Merge both tracks into one transcript ordered by start time.
pub fn merge(me: Vec<Segment>, them: Vec<Segment>) -> Vec<Segment> {
    let mut out: Vec<Segment> = me.into_iter().filter(|m| !is_echo(m, &them)).collect();
    out.extend(them);
    out.sort_by(|a, b| {
        a.start_ms
            .cmp(&b.start_ms)
            .then(a.end_ms.cmp(&b.end_ms))
            .then(a.speaker.cmp(&b.speaker))
    });
    out
}

/// Speakers in order of first appearance.
pub fn speakers(segments: &[Segment]) -> Vec<String> {
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for s in segments {
        if seen.insert(s.speaker.clone()) {
            out.push(s.speaker.clone());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ts(start: i64, end: i64, text: &str, speaker: Option<u32>) -> TrackSegment {
        TrackSegment {
            start_ms: start,
            end_ms: end,
            text: text.to_string(),
            speaker,
        }
    }

    #[test]
    fn me_track_is_labelled_and_cleaned() {
        let out = label_me(vec![
            ts(0, 1000, "  hello   there ", None),
            ts(2000, 3000, " ... ", None),
            ts(4000, 3500, "bad times", None),
        ]);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].speaker, "Me");
        assert_eq!(out[0].text, "hello there");
        // end is never before start
        assert_eq!(out[1].end_ms, 4000);
    }

    #[test]
    fn single_or_missing_diarization_is_just_them() {
        let out = label_them(vec![ts(0, 1, "a", Some(3)), ts(2, 3, "b", Some(3))]);
        assert!(out.iter().all(|s| s.speaker == "Them"));
        let out = label_them(vec![ts(0, 1, "a", None)]);
        assert_eq!(out[0].speaker, "Them");
    }

    #[test]
    fn several_voices_are_numbered_by_first_appearance() {
        let out = label_them(vec![
            ts(5000, 6000, "second voice", Some(0)),
            ts(0, 1000, "first voice", Some(2)),
            ts(7000, 8000, "first again", Some(2)),
        ]);
        let names: Vec<_> = out
            .iter()
            .map(|s| (s.text.as_str(), s.speaker.as_str()))
            .collect();
        assert_eq!(
            names,
            vec![
                ("second voice", "Them 2"),
                ("first voice", "Them 1"),
                ("first again", "Them 1")
            ]
        );
    }

    #[test]
    fn merge_orders_by_time_across_tracks() {
        let me = label_me(vec![
            ts(0, 2000, "hi all", None),
            ts(9000, 10000, "thanks", None),
        ]);
        let them = label_them(vec![ts(3000, 8000, "hello, shall we start", None)]);
        let merged = merge(me, them);
        let order: Vec<_> = merged.iter().map(|s| s.speaker.as_str()).collect();
        assert_eq!(order, vec!["Me", "Them", "Me"]);
        assert_eq!(speakers(&merged), vec!["Me", "Them"]);
    }

    #[test]
    fn mic_echo_of_the_call_is_dropped_but_real_overlap_is_kept() {
        let them = label_them(vec![ts(1000, 5000, "The launch moves to November", None)]);
        let me = label_me(vec![
            // Speaker bleed picked up by the mic, slightly later and garbled.
            ts(1200, 5100, "the launch moves to november", None),
            // Me talking over them with different words.
            ts(3000, 4000, "wait, which launch?", None),
            // Same words but at a different time: not an echo.
            ts(20_000, 24_000, "The launch moves to November", None),
        ]);
        let merged = merge(me, them);
        let mine: Vec<_> = merged
            .iter()
            .filter(|s| s.speaker == "Me")
            .map(|s| s.start_ms)
            .collect();
        assert_eq!(mine, vec![3000, 20_000]);
    }

    #[test]
    fn empty_inputs_merge_to_empty() {
        assert!(merge(vec![], vec![]).is_empty());
        assert!(speakers(&[]).is_empty());
    }
}
