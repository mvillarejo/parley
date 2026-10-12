//! Gladia pre-recorded transcription: upload a track, start a job, poll until
//! it is done. The call ("Them") track is diarized so several remote voices
//! come back as separate speakers.
//!
//! API: `POST /v2/upload` (multipart `audio`) → `audio_url`;
//! `POST /v2/pre-recorded` → `id`; `GET /v2/pre-recorded/{id}` until
//! `status` is `done` (`result.transcription.utterances`) or `error`.

use crate::vocabulary::{normalize_vocabulary, CustomVocabEntry};
use parley_lib::merge::TrackSegment;
use parley_lib::vad;
use parley_lib::wav::wav_bytes;
use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};

pub const DEFAULT_BASE_URL: &str = "https://api.gladia.io";
/// Long recordings are sent in pieces of at most this length.
pub const MAX_CHUNK_SECONDS: usize = 60 * 60;
const POLL_INTERVAL: Duration = Duration::from_secs(3);
const MIN_JOB_TIMEOUT: Duration = Duration::from_secs(15 * 60);

#[derive(Debug, Clone, Default, PartialEq)]
pub struct GladiaOptions {
    pub languages: Vec<String>,
    pub code_switching: bool,
    pub vocabulary: Vec<CustomVocabEntry>,
    pub diarization: bool,
}

#[derive(Debug, Serialize, PartialEq)]
struct LanguageConfig {
    languages: Vec<String>,
    code_switching: bool,
}

#[derive(Debug, Serialize, PartialEq)]
struct VocabularyConfig {
    vocabulary: Vec<CustomVocabEntry>,
}

#[derive(Debug, Serialize, PartialEq)]
struct JobRequest {
    audio_url: String,
    diarization: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    language_config: Option<LanguageConfig>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    custom_vocabulary: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    custom_vocabulary_config: Option<VocabularyConfig>,
}

fn job_request(audio_url: String, options: &GladiaOptions) -> JobRequest {
    let languages: Vec<String> = options
        .languages
        .iter()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect();
    let vocabulary = normalize_vocabulary(options.vocabulary.clone());
    JobRequest {
        audio_url,
        diarization: options.diarization,
        language_config: (!languages.is_empty() || options.code_switching).then_some(
            LanguageConfig {
                code_switching: options.code_switching && languages.len() != 1,
                languages,
            },
        ),
        custom_vocabulary: !vocabulary.is_empty(),
        custom_vocabulary_config: (!vocabulary.is_empty())
            .then_some(VocabularyConfig { vocabulary }),
    }
}

#[derive(Deserialize)]
struct UploadResponse {
    audio_url: String,
}

#[derive(Deserialize)]
struct JobCreated {
    id: String,
}

#[derive(Deserialize)]
struct Utterance {
    start: f64,
    end: f64,
    text: String,
    #[serde(default)]
    speaker: Option<u32>,
}

#[derive(Debug, PartialEq)]
enum JobState {
    Pending,
    Done(Vec<TrackSegment>),
    Failed(String),
}

fn parse_job(body: &str) -> Result<JobState, String> {
    let value: serde_json::Value = serde_json::from_str(body)
        .map_err(|_| format!("Unexpected Gladia response: {}", snippet(body)))?;
    match value.get("status").and_then(|s| s.as_str()) {
        Some("queued") | Some("processing") => Ok(JobState::Pending),
        Some("error") => Ok(JobState::Failed(format!(
            "Gladia could not transcribe this audio{}",
            value
                .get("error_code")
                .and_then(|c| c.as_i64())
                .map(|c| format!(" (error {c})"))
                .unwrap_or_default()
        ))),
        Some("done") => {
            let utterances = value
                .pointer("/result/transcription/utterances")
                .cloned()
                .unwrap_or(serde_json::Value::Array(vec![]));
            let utterances: Vec<Utterance> = serde_json::from_value(utterances)
                .map_err(|e| format!("Unexpected Gladia transcript shape: {e}"))?;
            Ok(JobState::Done(
                utterances
                    .into_iter()
                    .map(|u| TrackSegment {
                        start_ms: (u.start * 1000.0).round() as i64,
                        end_ms: (u.end * 1000.0).round() as i64,
                        text: u.text,
                        speaker: u.speaker,
                    })
                    .collect(),
            ))
        }
        other => Err(format!("Unexpected Gladia job status: {other:?}")),
    }
}

fn snippet(body: &str) -> String {
    body.trim().chars().take(200).collect()
}

fn error_message(status: reqwest::StatusCode, body: &str) -> String {
    let detail = serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|v| {
            v.get("message")
                .and_then(|m| m.as_str())
                .map(str::to_string)
        })
        .unwrap_or_else(|| snippet(body));
    match status.as_u16() {
        401 | 403 => "Gladia rejected the API key".to_string(),
        _ => format!("Gladia returned {status}: {detail}"),
    }
}

#[derive(Clone)]
pub struct GladiaClient {
    base_url: String,
    http: reqwest::Client,
    poll_interval: Duration,
}

impl Default for GladiaClient {
    fn default() -> Self {
        Self::new(DEFAULT_BASE_URL)
    }
}

impl GladiaClient {
    pub fn new(base_url: &str) -> Self {
        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            http: reqwest::Client::builder()
                .connect_timeout(Duration::from_secs(10))
                .timeout(Duration::from_secs(15 * 60))
                .build()
                .unwrap_or_else(|_| reqwest::Client::new()),
            poll_interval: POLL_INTERVAL,
        }
    }

    fn request(
        &self,
        method: reqwest::Method,
        path: &str,
        api_key: &str,
    ) -> reqwest::RequestBuilder {
        self.http
            .request(method, format!("{}{path}", self.base_url))
            .header("x-gladia-key", api_key.trim())
            .header(
                "x-gladia-version",
                format!("Parley/{}", env!("CARGO_PKG_VERSION")),
            )
    }

    async fn send(&self, builder: reqwest::RequestBuilder) -> Result<String, String> {
        let response = builder
            .send()
            .await
            .map_err(|e| format!("Could not reach Gladia: {e}"))?;
        let status = response.status();
        let body = response
            .text()
            .await
            .map_err(|e| format!("Could not read Gladia's response: {e}"))?;
        if !status.is_success() {
            return Err(error_message(status, &body));
        }
        Ok(body)
    }

    pub async fn test_connection(&self, api_key: &str) -> Result<String, String> {
        if api_key.trim().is_empty() {
            return Err("Add a Gladia API key first".to_string());
        }
        self.send(self.request(reqwest::Method::GET, "/v2/pre-recorded?limit=1", api_key))
            .await
            .map(|_| "Connected. Gladia accepted the API key.".to_string())
    }

    async fn upload(&self, api_key: &str, wav: Vec<u8>) -> Result<String, String> {
        let part = reqwest::multipart::Part::bytes(wav)
            .file_name("track.wav")
            .mime_str("audio/wav")
            .expect("static mime type is valid");
        let body = self
            .send(
                self.request(reqwest::Method::POST, "/v2/upload", api_key)
                    .multipart(reqwest::multipart::Form::new().part("audio", part)),
            )
            .await?;
        serde_json::from_str::<UploadResponse>(&body)
            .map(|r| r.audio_url)
            .map_err(|_| format!("Unexpected Gladia upload response: {}", snippet(&body)))
    }

    async fn transcribe_clip(
        &self,
        api_key: &str,
        options: &GladiaOptions,
        samples: &[i16],
        sample_rate: u32,
    ) -> Result<Vec<TrackSegment>, String> {
        let audio_url = self
            .upload(api_key, wav_bytes(samples, sample_rate))
            .await?;
        let body = self
            .send(
                self.request(reqwest::Method::POST, "/v2/pre-recorded", api_key)
                    .json(&job_request(audio_url, options)),
            )
            .await?;
        let job: JobCreated = serde_json::from_str(&body)
            .map_err(|_| format!("Unexpected Gladia response: {}", snippet(&body)))?;
        let audio_secs = samples.len() as u64 / sample_rate.max(1) as u64;
        let deadline = Instant::now() + MIN_JOB_TIMEOUT.max(Duration::from_secs(audio_secs * 2));
        loop {
            let body = self
                .send(self.request(
                    reqwest::Method::GET,
                    &format!("/v2/pre-recorded/{}", job.id),
                    api_key,
                ))
                .await?;
            match parse_job(&body)? {
                JobState::Done(segments) => return Ok(segments),
                JobState::Failed(message) => return Err(message),
                JobState::Pending if Instant::now() > deadline => {
                    return Err("Gladia is taking too long; try again later".to_string())
                }
                JobState::Pending => tokio::time::sleep(self.poll_interval).await,
            }
        }
    }

    /// Transcribe a track. Silent tracks make no request; long tracks are
    /// split into `MAX_CHUNK_SECONDS` pieces and the timestamps re-based.
    pub async fn transcribe_track(
        &self,
        api_key: &str,
        options: &GladiaOptions,
        samples: &[i16],
        sample_rate: u32,
        mut progress: impl FnMut(usize, usize),
    ) -> Result<Vec<TrackSegment>, String> {
        if api_key.trim().is_empty() {
            return Err("Add a Gladia API key to this profile first".to_string());
        }
        let chunk = (MAX_CHUNK_SECONDS * sample_rate.max(1) as usize).max(1);
        let pieces: Vec<(usize, &[i16])> = samples
            .chunks(chunk)
            .enumerate()
            .map(|(i, c)| (i * chunk, c))
            .filter(|(_, c)| vad::has_speech(c, sample_rate, &vad::VadConfig::default()))
            .collect();
        let total = pieces.len();
        let mut out = Vec::new();
        for (i, (offset, piece)) in pieces.into_iter().enumerate() {
            let offset_ms = (offset as u64 * 1000 / sample_rate.max(1) as u64) as i64;
            let segments = self
                .transcribe_clip(api_key, options, piece, sample_rate)
                .await?;
            out.extend(segments.into_iter().map(|mut s| {
                s.start_ms += offset_ms;
                s.end_ms += offset_ms;
                s
            }));
            progress(i + 1, total);
        }
        Ok(out)
    }

    #[cfg(test)]
    fn with_poll_interval(mut self, interval: Duration) -> Self {
        self.poll_interval = interval;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_http::mock_server;

    fn speech(ms: u32) -> Vec<i16> {
        (0..(16 * ms) as usize)
            .map(|i| ((i as f32 * 0.2).sin() * 9000.0) as i16)
            .collect()
    }

    #[test]
    fn job_request_includes_only_configured_options() {
        let minimal =
            serde_json::to_value(job_request("u".into(), &GladiaOptions::default())).unwrap();
        assert_eq!(
            minimal,
            serde_json::json!({ "audio_url": "u", "diarization": false })
        );

        let full = serde_json::to_value(job_request(
            "u".into(),
            &GladiaOptions {
                languages: vec!["en".into(), " ".into(), "es".into()],
                code_switching: true,
                vocabulary: vec![
                    CustomVocabEntry {
                        value: "Parley".into(),
                        pronunciations: None,
                        language: None,
                        intensity: 0.4,
                    },
                    CustomVocabEntry {
                        value: "  ".into(),
                        pronunciations: None,
                        language: None,
                        intensity: 0.5,
                    },
                ],
                diarization: true,
            },
        ))
        .unwrap();
        assert_eq!(full["diarization"], true);
        assert_eq!(
            full["language_config"]["languages"],
            serde_json::json!(["en", "es"])
        );
        assert_eq!(full["language_config"]["code_switching"], true);
        assert_eq!(full["custom_vocabulary"], true);
        assert_eq!(
            full["custom_vocabulary_config"]["vocabulary"],
            serde_json::json!([{ "value": "Parley", "intensity": 0.4 }])
        );

        // One language: code switching is meaningless and off.
        let single = job_request(
            "u".into(),
            &GladiaOptions {
                languages: vec!["fr".into()],
                code_switching: true,
                ..GladiaOptions::default()
            },
        );
        assert!(!single.language_config.unwrap().code_switching);
    }

    #[test]
    fn job_states_parse() {
        assert_eq!(parse_job(r#"{"status":"queued"}"#), Ok(JobState::Pending));
        assert_eq!(
            parse_job(r#"{"status":"processing"}"#),
            Ok(JobState::Pending)
        );
        assert_eq!(
            parse_job(r#"{"status":"error","error_code":422}"#),
            Ok(JobState::Failed(
                "Gladia could not transcribe this audio (error 422)".into()
            ))
        );
        let done = parse_job(
            r#"{"status":"done","result":{"transcription":{"utterances":[
                {"start":1.25,"end":2.5,"text":"Hi","speaker":1,"channel":0,"language":"en","confidence":0.9,"words":[]},
                {"start":3.0,"end":4.0,"text":"No speaker field"}
            ]}}}"#,
        )
        .unwrap();
        assert_eq!(
            done,
            JobState::Done(vec![
                TrackSegment {
                    start_ms: 1250,
                    end_ms: 2500,
                    text: "Hi".into(),
                    speaker: Some(1)
                },
                TrackSegment {
                    start_ms: 3000,
                    end_ms: 4000,
                    text: "No speaker field".into(),
                    speaker: None
                },
            ])
        );
        assert_eq!(
            parse_job(r#"{"status":"done","result":{}}"#),
            Ok(JobState::Done(vec![]))
        );
        assert!(parse_job("<html>").is_err());
        assert!(parse_job(r#"{"status":"weird"}"#).is_err());
    }

    #[tokio::test]
    async fn uploads_starts_and_polls_a_job() {
        let (base, seen) = mock_server(vec![
            (
                200,
                r#"{"audio_url":"https://api.gladia.io/file/abc","audio_metadata":{}}"#,
            ),
            (
                201,
                r#"{"id":"job-1","result_url":"https://api.gladia.io/v2/transcription/job-1"}"#,
            ),
            (200, r#"{"status":"processing"}"#),
            (
                200,
                r#"{"status":"done","result":{"transcription":{"utterances":[{"start":0.5,"end":1.0,"text":"Hello","speaker":0}]}}}"#,
            ),
        ])
        .await;
        let client = GladiaClient::new(base.trim_end_matches("/v1"))
            .with_poll_interval(Duration::from_millis(10));
        let mut calls = Vec::new();
        let segments = client
            .transcribe_track(
                "key",
                &GladiaOptions {
                    diarization: true,
                    ..GladiaOptions::default()
                },
                &speech(1000),
                16_000,
                |d, t| calls.push((d, t)),
            )
            .await
            .unwrap();
        assert_eq!(segments.len(), 1);
        assert_eq!(segments[0].start_ms, 500);
        assert_eq!(segments[0].speaker, Some(0));
        assert_eq!(calls, vec![(1, 1)]);
        let requests: Vec<String> = seen
            .lock()
            .unwrap()
            .iter()
            .map(|r| String::from_utf8_lossy(r).to_string())
            .collect();
        assert!(requests[0].starts_with("POST /v2/upload"));
        assert!(requests[0].contains("name=\"audio\""));
        assert!(requests[0].to_lowercase().contains("x-gladia-key: key"));
        assert!(requests[1].starts_with("POST /v2/pre-recorded"));
        assert!(requests[1].contains(r#""audio_url":"https://api.gladia.io/file/abc""#));
        assert!(requests[1].contains(r#""diarization":true"#));
        assert!(requests[2].starts_with("GET /v2/pre-recorded/job-1"));
    }

    #[tokio::test]
    async fn bad_key_and_silent_tracks() {
        let (base, _) = mock_server(vec![(401, r#"{"message":"Unauthorized"}"#)]).await;
        let client = GladiaClient::new(base.trim_end_matches("/v1"));
        assert_eq!(
            client.test_connection("bad").await,
            Err("Gladia rejected the API key".to_string())
        );
        assert!(client.test_connection("  ").await.is_err());
        // Silence never reaches the network (the mock has no replies left).
        let segments = client
            .transcribe_track(
                "k",
                &GladiaOptions::default(),
                &vec![0; 32_000],
                16_000,
                |_, _| {},
            )
            .await
            .unwrap();
        assert!(segments.is_empty());
        assert!(client
            .transcribe_track(
                " ",
                &GladiaOptions::default(),
                &speech(500),
                16_000,
                |_, _| {}
            )
            .await
            .is_err());
    }
}
