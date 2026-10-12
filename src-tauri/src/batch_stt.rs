//! OpenAI-compatible batch speech-to-text provider (`POST {base}/audio/transcriptions`).
//!
//! A recorded track is split into utterances by voice activity detection;
//! each utterance (at most `MAX_CHUNK_SECONDS`) is sent as a mono 16-bit WAV
//! and becomes one timestamped segment. Silence is never sent. Defaults target
//! local Ollama; Groq or OpenAI work by changing base URL, key and model.

use crate::vocabulary::{normalize_vocabulary, CustomVocabEntry};
use parley_lib::merge::TrackSegment;
use parley_lib::vad::{self, VadConfig};
use parley_lib::wav::wav_bytes;
use std::time::Duration;

/// Gemma 4 accepts at most 30 s of audio per request; keep a safety margin.
pub const MAX_CHUNK_SECONDS: u32 = 28;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(120);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// Retries for rate limits (429) and server errors (5xx).
const MAX_RETRIES: u32 = 3;
const MAX_RETRY_WAIT: Duration = Duration::from_secs(30);

/// Endpoint settings for one session.
#[derive(Debug, Clone, PartialEq)]
pub struct BatchSettings {
    pub base_url: String,
    pub model: String,
    pub api_key: Option<String>,
}

/// Per-session request options derived from the user's transcription settings.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RequestOptions {
    pub language: Option<String>,
    pub prompt: Option<String>,
}

impl RequestOptions {
    /// `language` only when exactly one is configured (otherwise let the model
    /// auto-detect); `prompt` is the vocabulary terms joined with commas.
    pub fn from_session(languages: &[String], vocabulary: Vec<CustomVocabEntry>) -> Self {
        let languages: Vec<&String> = languages.iter().filter(|l| !l.trim().is_empty()).collect();
        let language = match languages.as_slice() {
            [only] => Some(only.trim().to_string()),
            _ => None,
        };
        let terms: Vec<String> = normalize_vocabulary(vocabulary)
            .into_iter()
            .map(|entry| entry.value.trim().to_string())
            .collect();
        let prompt = (!terms.is_empty()).then(|| terms.join(", "));
        Self { language, prompt }
    }
}

/// Everything needed to send one transcription request, minus the audio.
#[derive(Debug, Clone, PartialEq)]
pub struct RequestParts {
    pub url: String,
    /// Text multipart fields, in order (the `file` part is added separately).
    pub fields: Vec<(&'static str, String)>,
    pub bearer: Option<String>,
}

pub fn build_request_parts(settings: &BatchSettings, options: &RequestOptions) -> RequestParts {
    let mut fields = vec![("model", settings.model.clone())];
    if let Some(language) = &options.language {
        fields.push(("language", language.clone()));
    }
    if let Some(prompt) = &options.prompt {
        fields.push(("prompt", prompt.clone()));
    }
    RequestParts {
        url: format!("{}/audio/transcriptions", trim_base_url(&settings.base_url)),
        fields,
        bearer: settings
            .api_key
            .as_ref()
            .map(|key| key.trim().to_string())
            .filter(|key| !key.is_empty()),
    }
}

fn trim_base_url(base_url: &str) -> &str {
    base_url.trim().trim_end_matches('/')
}

pub fn build_request(
    client: &reqwest::Client,
    parts: &RequestParts,
    wav: Vec<u8>,
) -> reqwest::RequestBuilder {
    let file = reqwest::multipart::Part::bytes(wav)
        .file_name("audio.wav")
        .mime_str("audio/wav")
        .expect("static mime type is valid");
    let mut form = reqwest::multipart::Form::new().part("file", file);
    for (name, value) in &parts.fields {
        form = form.text(*name, value.clone());
    }
    let mut request = client.post(&parts.url).multipart(form);
    if let Some(key) = &parts.bearer {
        request = request.bearer_auth(key);
    }
    request
}

/// Extract `text` from a transcription response. Empty text is `Ok("")`; the
/// caller decides whether the whole recording came back empty.
pub fn parse_transcription_response(body: &str) -> Result<String, String> {
    let value: serde_json::Value = serde_json::from_str(body).map_err(|_| {
        format!(
            "Unexpected response from transcription server: {}",
            snippet(body)
        )
    })?;
    match value.get("text").and_then(|text| text.as_str()) {
        Some(text) => Ok(text.trim().to_string()),
        None => Err(format!(
            "Transcription response had no text: {}",
            snippet(body)
        )),
    }
}

/// Whether `model` is listed in an OpenAI-style `GET /models` response.
pub fn models_response_contains(body: &str, model: &str) -> Result<bool, String> {
    let value: serde_json::Value = serde_json::from_str(body)
        .map_err(|_| format!("Unexpected /models response: {}", snippet(body)))?;
    let data = value
        .get("data")
        .and_then(|data| data.as_array())
        .ok_or_else(|| format!("Unexpected /models response: {}", snippet(body)))?;
    Ok(data
        .iter()
        .filter_map(|entry| entry.get("id").and_then(|id| id.as_str()))
        .any(|id| id == model))
}

/// "Can't reach Ollama at http://localhost:11434 — is it running?"
pub fn unreachable_message(base_url: &str) -> String {
    let (name, origin) = match url::Url::parse(trim_base_url(base_url)) {
        Ok(url) => {
            let name = if url.port() == Some(11434) {
                "Ollama"
            } else {
                "the transcription server"
            };
            (name, url.origin().ascii_serialization())
        }
        Err(_) => ("the transcription server", base_url.to_string()),
    };
    format!("Can't reach {name} at {origin} — is it running?")
}

fn http_error_message(status: reqwest::StatusCode, body: &str) -> String {
    let detail = serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|value| error_detail(&value))
        .unwrap_or_else(|| snippet(body));
    format!("Transcription server returned {status}: {detail}")
}

/// `{"error":"msg"}` or `{"error":{"message":"msg"}}`. Ollama may wrap the
/// upstream error as a JSON string inside `error`, so unwrap one level of that.
fn error_detail(value: &serde_json::Value) -> Option<String> {
    let error = value.get("error")?;
    if let Some(message) = error.get("message").and_then(|m| m.as_str()) {
        return Some(message.to_string());
    }
    let text = error.as_str()?;
    Some(
        serde_json::from_str::<serde_json::Value>(text)
            .ok()
            .and_then(|inner| error_detail(&inner))
            .unwrap_or_else(|| text.to_string()),
    )
}

fn request_error_message(base_url: &str, error: &reqwest::Error) -> String {
    if error.is_connect() {
        unreachable_message(base_url)
    } else if error.is_timeout() {
        "The transcription server took too long to respond".to_string()
    } else {
        format!("Transcription request failed: {error}")
    }
}

fn snippet(body: &str) -> String {
    let trimmed = body.trim();
    if trimmed.chars().count() > 200 {
        format!("{}…", trimmed.chars().take(200).collect::<String>())
    } else {
        trimmed.to_string()
    }
}

pub fn http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(REQUEST_TIMEOUT)
        .build()
        .unwrap_or_else(|_| reqwest::Client::new())
}

fn retry_wait(attempt: u32, headers: &reqwest::header::HeaderMap) -> Duration {
    let from_header = headers
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse::<u64>().ok())
        .map(Duration::from_secs);
    from_header
        .unwrap_or_else(|| Duration::from_secs(2u64.pow(attempt)))
        .min(MAX_RETRY_WAIT)
}

/// Transcribe one short clip. Empty text is `Ok("")`.
pub async fn transcribe_clip(
    client: &reqwest::Client,
    settings: &BatchSettings,
    parts: &RequestParts,
    samples: &[i16],
    sample_rate: u32,
) -> Result<String, String> {
    let wav = wav_bytes(samples, sample_rate);
    let mut attempt = 0;
    loop {
        let response = build_request(client, parts, wav.clone())
            .send()
            .await
            .map_err(|e| request_error_message(&settings.base_url, &e))?;
        let status = response.status();
        if (status.as_u16() == 429 || status.is_server_error()) && attempt < MAX_RETRIES {
            attempt += 1;
            let wait = retry_wait(attempt, response.headers());
            log::warn!("[batch-stt] {status}; retrying in {}s", wait.as_secs());
            tokio::time::sleep(wait).await;
            continue;
        }
        let body = response
            .text()
            .await
            .map_err(|e| request_error_message(&settings.base_url, &e))?;
        if !status.is_success() {
            return Err(http_error_message(status, &body));
        }
        return parse_transcription_response(&body);
    }
}

/// VAD settings for a provider: utterances never exceed its request limit.
pub fn vad_config() -> VadConfig {
    VadConfig {
        max_utterance_ms: MAX_CHUNK_SECONDS * 1000,
        ..VadConfig::default()
    }
}

/// Transcribe a whole track, one utterance per request. `progress(done, total)`
/// is called after each utterance.
pub async fn transcribe_track(
    client: &reqwest::Client,
    settings: &BatchSettings,
    options: &RequestOptions,
    samples: &[i16],
    sample_rate: u32,
    mut progress: impl FnMut(usize, usize),
) -> Result<Vec<TrackSegment>, String> {
    let regions = vad::speech_regions(samples, sample_rate, &vad_config());
    let parts = build_request_parts(settings, options);
    let total = regions.len();
    let mut out = Vec::with_capacity(total);
    for (i, region) in regions.into_iter().enumerate() {
        let text = transcribe_clip(
            client,
            settings,
            &parts,
            &samples[region.start..region.end],
            sample_rate,
        )
        .await?;
        if !text.is_empty() {
            out.push(TrackSegment {
                start_ms: region.start_ms(sample_rate) as i64,
                end_ms: region.end_ms(sample_rate) as i64,
                text,
                speaker: None,
            });
        }
        progress(i + 1, total);
    }
    Ok(out)
}

/// `GET {base}/models` and check the configured model is listed.
pub async fn test_connection(settings: &BatchSettings) -> Result<String, String> {
    let mut request = http_client().get(format!("{}/models", trim_base_url(&settings.base_url)));
    if let Some(key) = settings.api_key.as_ref().filter(|k| !k.trim().is_empty()) {
        request = request.bearer_auth(key.trim());
    }
    let response = request
        .send()
        .await
        .map_err(|e| request_error_message(&settings.base_url, &e))?;
    let status = response.status();
    let body = response
        .text()
        .await
        .map_err(|e| request_error_message(&settings.base_url, &e))?;
    if !status.is_success() {
        return Err(http_error_message(status, &body));
    }
    if models_response_contains(&body, &settings.model)? {
        Ok(format!("Connected. Model {} is available.", settings.model))
    } else {
        Err(format!(
            "Connected, but model {} isn't available. For Ollama run: ollama pull {}",
            settings.model, settings.model
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(api_key: Option<&str>) -> BatchSettings {
        BatchSettings {
            base_url: "http://localhost:11434/v1/".to_string(),
            model: "gemma4:e4b".to_string(),
            api_key: api_key.map(str::to_string),
        }
    }

    fn vocab(values: &[&str]) -> Vec<CustomVocabEntry> {
        values
            .iter()
            .map(|value| CustomVocabEntry {
                value: value.to_string(),
                pronunciations: None,
                language: None,
                intensity: 0.5,
            })
            .collect()
    }

    fn langs(values: &[&str]) -> Vec<String> {
        values.iter().map(|v| v.to_string()).collect()
    }

    #[test]
    fn language_is_sent_only_with_exactly_one_language() {
        assert_eq!(
            RequestOptions::from_session(&langs(&["es"]), vec![]).language,
            Some("es".to_string())
        );
        assert_eq!(
            RequestOptions::from_session(&langs(&["en", "es"]), vec![]).language,
            None
        );
        assert_eq!(RequestOptions::from_session(&[], vec![]).language, None);
        assert_eq!(
            RequestOptions::from_session(&langs(&["en", " "]), vec![]).language,
            Some("en".to_string())
        );
    }

    #[test]
    fn prompt_joins_vocabulary_terms_and_skips_blanks() {
        let options = RequestOptions::from_session(&[], vocab(&["Parley", " ", " Kubernetes "]));
        assert_eq!(options.prompt, Some("Parley, Kubernetes".to_string()));
        assert_eq!(
            RequestOptions::from_session(&[], vocab(&[" "])).prompt,
            None
        );
    }

    #[test]
    fn request_parts_include_only_configured_fields() {
        let minimal = build_request_parts(&settings(None), &RequestOptions::default());
        assert_eq!(
            minimal.url,
            "http://localhost:11434/v1/audio/transcriptions"
        );
        assert_eq!(minimal.fields, vec![("model", "gemma4:e4b".to_string())]);
        assert_eq!(minimal.bearer, None);

        let full = build_request_parts(
            &settings(Some(" sk-test ")),
            &RequestOptions {
                language: Some("en".to_string()),
                prompt: Some("Parley, Ollama".to_string()),
            },
        );
        assert_eq!(
            full.fields,
            vec![
                ("model", "gemma4:e4b".to_string()),
                ("language", "en".to_string()),
                ("prompt", "Parley, Ollama".to_string()),
            ]
        );
        assert_eq!(full.bearer, Some("sk-test".to_string()));
    }

    #[test]
    fn bearer_header_is_set_only_when_a_key_is_configured() {
        let client = reqwest::Client::new();
        for (key, expected) in [
            (None, None),
            (Some(""), None),
            (Some("   "), None),
            (Some("gsk_abc"), Some("Bearer gsk_abc")),
        ] {
            let parts = build_request_parts(&settings(key), &RequestOptions::default());
            let request = build_request(&client, &parts, wav_bytes(&[0, 0], RATE))
                .build()
                .unwrap();
            assert_eq!(request.method(), reqwest::Method::POST);
            assert_eq!(
                request
                    .headers()
                    .get(reqwest::header::AUTHORIZATION)
                    .map(|v| v.to_str().unwrap()),
                expected,
                "key={key:?}"
            );
            assert!(request
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .unwrap()
                .to_str()
                .unwrap()
                .starts_with("multipart/form-data"));
        }
    }

    #[test]
    fn parses_text_from_response() {
        assert_eq!(
            parse_transcription_response(r#"{"text":" Hello world. "}"#),
            Ok("Hello world.".to_string())
        );
        assert_eq!(
            parse_transcription_response(r#"{"text":""}"#),
            Ok(String::new())
        );
        assert!(parse_transcription_response(r#"{"error":"model not found"}"#).is_err());
        assert!(parse_transcription_response(r#"{"text":null}"#).is_err());
        assert!(parse_transcription_response("not json").is_err());
    }

    #[test]
    fn models_response_lookup() {
        let body = r#"{"object":"list","data":[{"id":"gemma4:latest"},{"id":"gemma4:e4b"}]}"#;
        assert_eq!(models_response_contains(body, "gemma4:e4b"), Ok(true));
        assert_eq!(
            models_response_contains(body, "whisper-large-v3"),
            Ok(false)
        );
        assert!(models_response_contains(r#"{"object":"list"}"#, "x").is_err());
        assert!(models_response_contains("<html>", "x").is_err());
    }

    #[test]
    fn unreachable_message_names_ollama_on_its_default_port() {
        assert_eq!(
            unreachable_message("http://localhost:11434/v1"),
            "Can't reach Ollama at http://localhost:11434 — is it running?"
        );
        assert_eq!(
            unreachable_message("https://api.groq.com/openai/v1"),
            "Can't reach the transcription server at https://api.groq.com — is it running?"
        );
    }

    #[test]
    fn http_error_message_prefers_the_server_error_field() {
        let status = reqwest::StatusCode::NOT_FOUND;
        assert_eq!(
            http_error_message(status, r#"{"error":"model 'x' not found"}"#),
            "Transcription server returned 404 Not Found: model 'x' not found"
        );
        assert_eq!(
            http_error_message(status, r#"{"error":{"message":"bad key"}}"#),
            "Transcription server returned 404 Not Found: bad key"
        );
        // Shape Ollama returns when its audio backend rejects the file.
        assert_eq!(
            http_error_message(
                reqwest::StatusCode::BAD_REQUEST,
                r#"{"error":"{\"error\":{\"code\":400,\"message\":\"Failed to load image or audio file\"}}"}"#
            ),
            "Transcription server returned 400 Bad Request: Failed to load image or audio file"
        );
    }

    const RATE: u32 = 16_000;

    fn speech(ms: u32) -> Vec<i16> {
        (0..(RATE * ms / 1000) as usize)
            .map(|i| ((i as f32 * 0.2).sin() * 9000.0) as i16)
            .collect()
    }

    fn silence(ms: u32) -> Vec<i16> {
        vec![0; (RATE * ms / 1000) as usize]
    }

    #[tokio::test]
    async fn silent_track_makes_no_request() {
        // Port 9 (discard) would fail to connect if a request were attempted.
        let unreachable = BatchSettings {
            base_url: "http://127.0.0.1:9/v1".to_string(),
            model: "m".to_string(),
            api_key: None,
        };
        let client = reqwest::Client::new();
        let opts = RequestOptions::default();
        for track in [vec![], silence(3000)] {
            let result =
                transcribe_track(&client, &unreachable, &opts, &track, RATE, |_, _| {}).await;
            assert_eq!(result, Ok(vec![]));
        }
    }

    #[tokio::test]
    async fn connection_refused_becomes_a_clear_error() {
        let unreachable = BatchSettings {
            base_url: "http://127.0.0.1:9/v1".to_string(),
            model: "m".to_string(),
            api_key: None,
        };
        let result = transcribe_track(
            &reqwest::Client::new(),
            &unreachable,
            &RequestOptions::default(),
            &speech(1000),
            RATE,
            |_, _| {},
        )
        .await;
        assert_eq!(
            result,
            Err(
                "Can't reach the transcription server at http://127.0.0.1:9 — is it running?"
                    .to_string()
            )
        );
    }

    #[tokio::test]
    async fn each_utterance_becomes_a_timestamped_segment() {
        let (base_url, seen) = crate::test_http::mock_server(vec![
            (200, r#"{"text":" Good morning. "}"#),
            (200, r#"{"text":""}"#),
            (200, r#"{"text":"Let's begin."}"#),
        ])
        .await;
        let settings = BatchSettings {
            base_url,
            model: "whisper".to_string(),
            api_key: Some("k".to_string()),
        };
        let track = [
            silence(1000),
            speech(1500),
            silence(2000),
            speech(800),
            silence(2000),
            speech(1200),
        ]
        .concat();
        let mut calls = Vec::new();
        let segments = transcribe_track(
            &reqwest::Client::new(),
            &settings,
            &RequestOptions::default(),
            &track,
            RATE,
            |done, total| calls.push((done, total)),
        )
        .await
        .unwrap();
        assert_eq!(calls, vec![(1, 3), (2, 3), (3, 3)]);
        // The empty middle reply is dropped; timestamps come from the VAD.
        assert_eq!(segments.len(), 2);
        assert_eq!(segments[0].text, "Good morning.");
        assert!(
            (780..=820).contains(&segments[0].start_ms),
            "{:?}",
            segments[0]
        );
        assert!(
            (7080..=7120).contains(&segments[1].start_ms),
            "{:?}",
            segments[1]
        );
        let requests = seen.lock().unwrap();
        assert_eq!(requests.len(), 3);
        let first = String::from_utf8_lossy(&requests[0]);
        assert!(first.starts_with("POST /v1/audio/transcriptions"));
        assert!(first.to_lowercase().contains("authorization: bearer k"));
        assert!(first.contains("RIFF"));
    }

    #[tokio::test]
    async fn rate_limits_are_retried_and_errors_surface() {
        let (base_url, seen) = crate::test_http::mock_server(vec![
            (429, r#"{"error":{"message":"slow down"}}"#),
            (200, r#"{"text":"ok"}"#),
        ])
        .await;
        let settings = BatchSettings {
            base_url,
            model: "m".to_string(),
            api_key: None,
        };
        let parts = build_request_parts(&settings, &RequestOptions::default());
        let text = transcribe_clip(
            &reqwest::Client::new(),
            &settings,
            &parts,
            &speech(500),
            RATE,
        )
        .await
        .unwrap();
        assert_eq!(text, "ok");
        assert_eq!(seen.lock().unwrap().len(), 2);

        let (base_url, _) =
            crate::test_http::mock_server(vec![(404, r#"{"error":"model 'm' not found"}"#)]).await;
        let settings = BatchSettings {
            base_url,
            model: "m".to_string(),
            api_key: None,
        };
        let parts = build_request_parts(&settings, &RequestOptions::default());
        let err = transcribe_clip(
            &reqwest::Client::new(),
            &settings,
            &parts,
            &speech(500),
            RATE,
        )
        .await
        .unwrap_err();
        assert!(err.contains("model 'm' not found"), "{err}");
    }

    /// Real end-to-end check against a local Ollama. Run with:
    /// `PARLEY_STT_WAV=/path/to/16k-mono.wav cargo test --manifest-path src-tauri/Cargo.toml -- --ignored ollama --nocapture`
    #[tokio::test]
    #[ignore]
    async fn ollama_transcribes_a_real_wav() {
        let path =
            std::env::var("PARLEY_STT_WAV").expect("set PARLEY_STT_WAV to a 16-bit mono WAV");
        let wav = parley_lib::wav::read_wav(std::path::Path::new(&path)).unwrap();
        let settings = BatchSettings {
            base_url: std::env::var("PARLEY_STT_BASE_URL")
                .unwrap_or_else(|_| "http://localhost:11434/v1".to_string()),
            model: std::env::var("PARLEY_STT_MODEL").unwrap_or_else(|_| "gemma4:e4b".to_string()),
            api_key: std::env::var("PARLEY_STT_API_KEY").ok(),
        };
        println!("test_connection: {:?}", test_connection(&settings).await);
        let started = std::time::Instant::now();
        let segments = transcribe_track(
            &http_client(),
            &settings,
            &RequestOptions::from_session(&["en".to_string()], vocab(&["Parley"])),
            &wav.samples,
            wav.sample_rate,
            |done, total| println!("{done}/{total}"),
        )
        .await
        .unwrap();
        println!(
            "segments: {segments:#?}\nlatency: {}ms for {}ms of audio",
            started.elapsed().as_millis(),
            wav.duration_ms()
        );
        assert!(!segments.is_empty());
    }
}
