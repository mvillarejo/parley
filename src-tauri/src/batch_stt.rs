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
use std::collections::HashMap;
use std::time::Duration;

/// Gemma 4 accepts at most 30 s of audio per request; keep a safety margin.
pub const MAX_CHUNK_SECONDS: u32 = 28;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(120);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// Retries for rate limits (429) and server errors (5xx).
const MAX_RETRIES: u32 = 3;
const MAX_RETRY_WAIT: Duration = Duration::from_secs(30);
/// Text stored for an utterance the model could not transcribe.
pub const UNCLEAR: &str = "[unclear]";
/// This many unclear utterances in a row means the server is stuck, not the audio.
const MAX_CONSECUTIVE_UNCLEAR: usize = 3;
/// A phrase repeated more than this many times in a row is a decoding loop.
const MAX_REPEATS: usize = 3;
/// Longest phrase (in words) checked for loops.
const MAX_REPEAT_WORDS: usize = 8;

/// How a track is cut into requests.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Chunking {
    /// One request per utterance (at most `MAX_CHUNK_SECONDS`), plain `text`
    /// response. For LLM-based speech models with a short audio limit.
    Utterances,
    /// Runs of utterances up to `max_ms`, broken at pauses longer than
    /// `max_gap_ms`, with a `verbose_json` response carrying timed segments.
    /// For Whisper, which windows long audio itself and is slow on tiny clips.
    Long { max_ms: u32, max_gap_ms: u32 },
}

/// Chunking for a local whisper.cpp server.
pub const WHISPER_CHUNKING: Chunking = Chunking::Long {
    max_ms: 180_000,
    max_gap_ms: 8_000,
};

/// Endpoint settings for one session.
#[derive(Debug, Clone, PartialEq)]
pub struct BatchSettings {
    pub base_url: String,
    pub model: String,
    pub api_key: Option<String>,
    pub chunking: Chunking,
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
    if matches!(settings.chunking, Chunking::Long { .. }) {
        fields.push(("response_format", "verbose_json".to_string()));
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
        request_timeout_message().to_string()
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

/// Why one clip failed. Only a timeout is recoverable per clip: local models
/// sometimes loop on an utterance and never finish, while every other error
/// (server down, bad model, bad key) would fail every clip the same way.
#[derive(Debug, Clone, PartialEq)]
pub enum ClipError {
    TimedOut,
    Failed(String),
}

impl ClipError {
    fn from_reqwest(base_url: &str, error: &reqwest::Error) -> Self {
        if error.is_timeout() {
            ClipError::TimedOut
        } else {
            ClipError::Failed(request_error_message(base_url, error))
        }
    }

    #[cfg(test)]
    pub fn message(&self) -> String {
        match self {
            ClipError::TimedOut => request_timeout_message().to_string(),
            ClipError::Failed(message) => message.clone(),
        }
    }
}

fn request_timeout_message() -> &'static str {
    "The transcription server took too long to respond"
}

/// Time allowed for one clip. A 28 s utterance takes 2-15 s on a local model;
/// one that loops never finishes, so give up long before the client timeout.
fn clip_timeout(chunking: Chunking, samples: usize, sample_rate: u32) -> Duration {
    // Mock-server tests hang on purpose; the real-server tests set one of these.
    let real_server_test = ["PARLEY_STT_WAV", "PARLEY_MEETING_DIR"]
        .iter()
        .any(|v| std::env::var_os(v).is_some());
    if cfg!(test) && !real_server_test {
        return Duration::from_millis(300);
    }
    let audio = Duration::from_millis(samples as u64 * 1000 / sample_rate.max(1) as u64);
    match chunking {
        Chunking::Utterances => (Duration::from_secs(15) + audio * 2).min(REQUEST_TIMEOUT),
        // whisper.cpp runs about 8x real time on Apple silicon.
        Chunking::Long { .. } => Duration::from_secs(30) + audio / 2,
    }
}

/// Transcribe one short clip. Empty text is `Ok("")`.
#[cfg(test)]
pub async fn transcribe_clip(
    client: &reqwest::Client,
    settings: &BatchSettings,
    parts: &RequestParts,
    samples: &[i16],
    sample_rate: u32,
) -> Result<String, String> {
    send_span(client, settings, parts, samples, sample_rate, 0)
        .await
        .map(|segments| {
            segments
                .into_iter()
                .map(|s| s.text)
                .collect::<Vec<_>>()
                .join(" ")
        })
        .map_err(|e| e.message())
}

/// Send one span of audio that starts `offset_ms` into the track; returns its
/// segments with track timestamps.
async fn send_span(
    client: &reqwest::Client,
    settings: &BatchSettings,
    parts: &RequestParts,
    samples: &[i16],
    sample_rate: u32,
    offset_ms: i64,
) -> Result<Vec<TrackSegment>, ClipError> {
    let wav = wav_bytes(samples, sample_rate);
    let timeout = clip_timeout(settings.chunking, samples.len(), sample_rate);
    let mut attempt = 0;
    loop {
        let response = build_request(client, parts, wav.clone())
            .timeout(timeout)
            .send()
            .await
            .map_err(|e| ClipError::from_reqwest(&settings.base_url, &e))?;
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
            .map_err(|e| ClipError::from_reqwest(&settings.base_url, &e))?;
        if !status.is_success() {
            return Err(ClipError::Failed(http_error_message(status, &body)));
        }
        return match settings.chunking {
            Chunking::Utterances => {
                let text = parse_transcription_response(&body).map_err(ClipError::Failed)?;
                let text = collapse_repeats(&text);
                let end_ms = offset_ms + (samples.len() as u64 * 1000 / sample_rate.max(1) as u64) as i64;
                Ok(if text.is_empty() {
                    vec![]
                } else {
                    vec![segment(offset_ms, end_ms, text)]
                })
            }
            Chunking::Long { .. } => {
                parse_verbose_response(&body, offset_ms).map_err(ClipError::Failed)
            }
        };
    }
}

fn segment(start_ms: i64, end_ms: i64, text: String) -> TrackSegment {
    TrackSegment {
        start_ms,
        end_ms,
        text,
        speaker: None,
    }
}

/// Phrases Whisper is known to produce on silence or noise (it was trained on
/// subtitled video). Compared after lowercasing and dropping punctuation.
const HALLUCINATIONS: &[&str] = &[
    "suscríbete al canal",
    "subscribe to the channel",
    "thanks for watching",
    "thank you for watching",
    "gracias por ver el video",
    "gracias por ver",
    "subtítulos realizados por la comunidad de amaraorg",
    "subtítulos por la comunidad de amaraorg",
    "amaraorg",
    "música",
    "music",
];

fn normalized_phrase(text: &str) -> String {
    text.chars()
        .filter(|c| c.is_alphanumeric() || c.is_whitespace())
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// Segments from a Whisper `verbose_json` response, shifted by `offset_ms`.
/// Drops what Whisper itself flags as non-speech, known hallucinations, and a
/// sentence repeated back to back (a decoding loop).
pub fn parse_verbose_response(body: &str, offset_ms: i64) -> Result<Vec<TrackSegment>, String> {
    let value: serde_json::Value = serde_json::from_str(body).map_err(|_| {
        format!(
            "Unexpected response from transcription server: {}",
            snippet(body)
        )
    })?;
    let Some(segments) = value.get("segments").and_then(|s| s.as_array()) else {
        // No segments: fall back to the plain text over the whole span.
        let text = parse_transcription_response(body)?;
        return Ok(if text.is_empty() {
            vec![]
        } else {
            vec![segment(offset_ms, offset_ms, collapse_repeats(&text))]
        });
    };
    let mut out: Vec<TrackSegment> = Vec::new();
    for s in segments {
        let text = s.get("text").and_then(|t| t.as_str()).unwrap_or("").trim();
        let number = |key: &str| s.get(key).and_then(|v| v.as_f64());
        let no_speech = number("no_speech_prob").unwrap_or(0.0);
        let logprob = number("avg_logprob").unwrap_or(0.0);
        // Whisper's own silence rule.
        if text.is_empty() || (no_speech > 0.6 && logprob < -1.0) {
            continue;
        }
        let phrase = normalized_phrase(text);
        if phrase.is_empty() || HALLUCINATIONS.contains(&phrase.as_str()) {
            continue;
        }
        if phrase.split(' ').count() >= 3
            && out.last().is_some_and(|prev| normalized_phrase(&prev.text) == phrase)
        {
            continue;
        }
        let ms = |secs: f64| offset_ms + (secs * 1000.0).round() as i64;
        out.push(segment(
            ms(number("start").unwrap_or(0.0)),
            ms(number("end").unwrap_or(0.0)),
            collapse_repeats(text),
        ));
    }
    Ok(out)
}

/// Everything `transcribe_span` needs about the track being transcribed.
struct Track<'a> {
    client: &'a reqwest::Client,
    settings: &'a BatchSettings,
    parts: &'a RequestParts,
    plain: &'a RequestParts,
    samples: &'a [i16],
    sample_rate: u32,
}

impl Track<'_> {
    fn ms(&self, sample: usize) -> i64 {
        (sample as u64 * 1000 / self.sample_rate.max(1) as u64) as i64
    }

    async fn send(
        &self,
        parts: &RequestParts,
        start: usize,
        end: usize,
    ) -> Result<Vec<TrackSegment>, ClipError> {
        send_span(
            self.client,
            self.settings,
            parts,
            &self.samples[start..end],
            self.sample_rate,
            self.ms(start),
        )
        .await
    }
}

/// Transcribe a chunk (one or more utterances), recovering from a model that
/// hangs on it: retry without the vocabulary prompt (which makes loops more
/// likely), then split it in two and recurse; a single utterance that still
/// hangs is tried as two halves. `Ok(None)` means every attempt timed out.
fn transcribe_span<'a>(
    track: &'a Track<'a>,
    regions: &'a [vad::Region],
) -> std::pin::Pin<
    Box<dyn std::future::Future<Output = Result<Option<Vec<TrackSegment>>, String>> + Send + 'a>,
> {
    Box::pin(async move {
        let (start, end) = (regions[0].start, regions[regions.len() - 1].end);
        for parts in [track.parts, track.plain] {
            match track.send(parts, start, end).await {
                Ok(segments) => return Ok(Some(segments)),
                Err(ClipError::TimedOut) => {
                    log::warn!(
                        "[batch-stt] {} ms at {} ms timed out",
                        track.ms(end) - track.ms(start),
                        track.ms(start)
                    );
                }
                Err(ClipError::Failed(message)) => return Err(message),
            }
        }
        let mut out = Vec::new();
        let mut any = false;
        if regions.len() > 1 {
            let (first, second) = regions.split_at(regions.len() / 2);
            for half in [first, second] {
                match transcribe_span(track, half).await? {
                    Some(segments) => {
                        any = true;
                        out.extend(segments);
                    }
                    None => out.push(unclear(track, half[0].start, half[half.len() - 1].end)),
                }
            }
        } else {
            let mid = start + (end - start) / 2;
            for (a, b) in [(start, mid), (mid, end)] {
                match track.send(track.plain, a, b).await {
                    Ok(segments) => {
                        any = true;
                        out.extend(segments);
                    }
                    Err(ClipError::TimedOut) => out.push(unclear(track, a, b)),
                    Err(ClipError::Failed(message)) => return Err(message),
                }
            }
        }
        Ok(any.then_some(out))
    })
}

fn unclear(track: &Track<'_>, start: usize, end: usize) -> TrackSegment {
    segment(track.ms(start), track.ms(end), UNCLEAR.to_string())
}

/// Group utterances into the spans sent per request.
pub fn plan_chunks(regions: &[vad::Region], sample_rate: u32, chunking: Chunking) -> Vec<Vec<vad::Region>> {
    let Chunking::Long { max_ms, max_gap_ms } = chunking else {
        return regions.iter().map(|r| vec![*r]).collect();
    };
    let per_ms = sample_rate.max(1) as usize / 1000;
    let (max_len, max_gap) = (max_ms as usize * per_ms, max_gap_ms as usize * per_ms);
    let mut chunks: Vec<Vec<vad::Region>> = Vec::new();
    for region in regions {
        match chunks.last_mut() {
            Some(chunk)
                if region.start - chunk[chunk.len() - 1].end <= max_gap
                    && region.end - chunk[0].start <= max_len =>
            {
                chunk.push(*region)
            }
            _ => chunks.push(vec![*region]),
        }
    }
    chunks
}

/// Collapse a phrase repeated more than `MAX_REPEATS` times in a row to one
/// copy. Models sometimes loop on hard audio ("that, that, that, that, ...").
pub fn collapse_repeats(text: &str) -> String {
    let words: Vec<&str> = text.split_whitespace().collect();
    let key = |w: &str| {
        w.trim_matches(|c: char| !c.is_alphanumeric())
            .to_lowercase()
    };
    let keys: Vec<String> = words.iter().map(|w| key(w)).collect();
    let mut out: Vec<&str> = Vec::with_capacity(words.len());
    let mut i = 0;
    'outer: while i < words.len() {
        for n in 1..=MAX_REPEAT_WORDS.min(words.len() - i) {
            let mut reps = 1;
            while i + (reps + 1) * n <= words.len()
                && keys[i + reps * n..i + (reps + 1) * n] == keys[i..i + n]
            {
                reps += 1;
            }
            if reps > MAX_REPEATS && keys[i..i + n].iter().any(|k| !k.is_empty()) {
                out.extend_from_slice(&words[i..i + n]);
                i += reps * n;
                continue 'outer;
            }
        }
        out.push(words[i]);
        i += 1;
    }
    if out.len() == words.len() {
        text.to_string()
    } else {
        out.join(" ")
    }
}

/// VAD settings for a provider: utterances never exceed its request limit.
pub fn vad_config() -> VadConfig {
    VadConfig {
        max_utterance_ms: MAX_CHUNK_SECONDS * 1000,
        ..VadConfig::default()
    }
}

/// Chunks already transcribed in an earlier, interrupted run, keyed by sample
/// range `(start, end)`. The VAD and chunk plan are deterministic, so the same
/// track yields the same ranges.
pub type Resume = HashMap<(usize, usize), Vec<TrackSegment>>;

/// Transcribe a whole track, one chunk per request (see `Chunking`). Chunks
/// found in `resume` are not sent again; `on_chunk(start, end, segments)` is
/// called for each newly transcribed one so the caller can checkpoint it.
/// `progress(done, total)` is called after each chunk. A chunk the model hangs
/// on becomes `[unclear]` instead of failing the meeting.
#[allow(clippy::too_many_arguments)]
pub async fn transcribe_track(
    client: &reqwest::Client,
    settings: &BatchSettings,
    options: &RequestOptions,
    samples: &[i16],
    sample_rate: u32,
    resume: &Resume,
    mut on_chunk: impl FnMut(usize, usize, &[TrackSegment]),
    mut progress: impl FnMut(usize, usize),
) -> Result<Vec<TrackSegment>, String> {
    let regions = vad::speech_regions(samples, sample_rate, &vad_config());
    let chunks = plan_chunks(&regions, sample_rate, settings.chunking);
    let parts = build_request_parts(settings, options);
    let plain = build_request_parts(
        settings,
        &RequestOptions {
            prompt: None,
            ..options.clone()
        },
    );
    let track = Track {
        client,
        settings,
        parts: &parts,
        plain: &plain,
        samples,
        sample_rate,
    };
    let total = chunks.len();
    let mut out = Vec::new();
    let mut unclear_run = 0;
    for (i, chunk) in chunks.iter().enumerate() {
        let key = (chunk[0].start, chunk[chunk.len() - 1].end);
        match resume.get(&key) {
            Some(segments) => out.extend(segments.iter().cloned()),
            None => match transcribe_span(&track, chunk).await? {
                Some(segments) => {
                    unclear_run = 0;
                    // Chunks with unclear parts are not checkpointed, so a
                    // retry tries them again.
                    if !segments.iter().any(|s| s.text == UNCLEAR) {
                        on_chunk(key.0, key.1, &segments);
                    }
                    out.extend(segments);
                }
                None => {
                    unclear_run += 1;
                    if unclear_run >= MAX_CONSECUTIVE_UNCLEAR {
                        return Err(format!(
                            "{} for {MAX_CONSECUTIVE_UNCLEAR} chunks in a row; finished chunks are kept and Retry continues from here",
                            request_timeout_message()
                        ));
                    }
                    log::warn!("[batch-stt] chunk at {} ms marked unclear", track.ms(key.0));
                    out.push(unclear(&track, key.0, key.1));
                }
            },
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
            chunking: Chunking::Utterances,
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
            chunking: Chunking::Utterances,
        };
        let client = reqwest::Client::new();
        let opts = RequestOptions::default();
        for track in [vec![], silence(3000)] {
            let result =
                transcribe_track(
                    &client,
                    &unreachable,
                    &opts,
                    &track,
                    RATE,
                    &Resume::new(),
                    |_, _, _| {},
                    |_, _| {},
                )
                .await;
            assert_eq!(result, Ok(vec![]));
        }
    }

    #[tokio::test]
    async fn connection_refused_becomes_a_clear_error() {
        let unreachable = BatchSettings {
            base_url: "http://127.0.0.1:9/v1".to_string(),
            model: "m".to_string(),
            api_key: None,
            chunking: Chunking::Utterances,
        };
        let result = transcribe_track(
            &reqwest::Client::new(),
            &unreachable,
            &RequestOptions::default(),
            &speech(1000),
            RATE,
            &Resume::new(),
            |_, _, _| {},
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
            chunking: Chunking::Utterances,
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
            &Resume::new(),
            |_, _, _| {},
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
            chunking: Chunking::Utterances,
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
            chunking: Chunking::Utterances,
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

    #[test]
    fn decoding_loops_are_collapsed_and_normal_text_is_kept() {
        assert_eq!(
            collapse_repeats("They are going. That, that, that, that, that, that. Done."),
            "They are going. That, Done."
        );
        assert_eq!(
            collapse_repeats("we can we can we can we can we can go"),
            "we can go"
        );
        // Three in a row is ordinary speech.
        let speech = "No, no, no. I mean it.";
        assert_eq!(collapse_repeats(speech), speech);
        assert_eq!(collapse_repeats(""), "");
    }

    fn settings_for(base_url: String) -> BatchSettings {
        BatchSettings {
            base_url,
            model: "m".to_string(),
            api_key: None,
            chunking: Chunking::Utterances,
        }
    }

    fn with_prompt() -> RequestOptions {
        RequestOptions {
            language: None,
            prompt: Some("Parley".to_string()),
        }
    }

    #[tokio::test]
    async fn a_hung_utterance_is_retried_without_the_prompt() {
        // Status 0 = the mock never answers, like a model stuck in a loop.
        let (base_url, seen) = crate::test_http::mock_server(vec![
            (0, ""),
            (200, r#"{"text":"Recovered."}"#),
        ])
        .await;
        let segments = transcribe_track(
            &reqwest::Client::new(),
            &settings_for(base_url),
            &with_prompt(),
            &speech(1000),
            RATE,
            &Resume::new(),
            |_, _, _| {},
            |_, _| {},
        )
        .await
        .unwrap();
        assert_eq!(segments.len(), 1);
        assert_eq!(segments[0].text, "Recovered.");
        let requests = seen.lock().unwrap();
        assert!(String::from_utf8_lossy(&requests[0]).contains("Parley"));
        assert!(!String::from_utf8_lossy(&requests[1]).contains("Parley"));
    }

    #[tokio::test]
    async fn an_utterance_that_always_hangs_becomes_unclear_and_the_track_continues() {
        let (base_url, _) = crate::test_http::mock_server(vec![
            // First utterance: prompt, plain, two halves all hang.
            (0, ""),
            (0, ""),
            (0, ""),
            (0, ""),
            // Second utterance is fine.
            (200, r#"{"text":"Next point."}"#),
        ])
        .await;
        let track = [speech(1000), silence(2000), speech(1000)].concat();
        let mut checkpointed = Vec::new();
        let segments = transcribe_track(
            &reqwest::Client::new(),
            &settings_for(base_url),
            &with_prompt(),
            &track,
            RATE,
            &Resume::new(),
            |_, _, segments| checkpointed.extend(segments.iter().map(|s| s.text.clone())),
            |_, _| {},
        )
        .await
        .unwrap();
        let texts: Vec<&str> = segments.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(texts, vec![UNCLEAR, "Next point."]);
        // Unclear utterances are not checkpointed, so a retry tries them again.
        assert_eq!(checkpointed, vec!["Next point."]);
    }

    #[tokio::test]
    async fn a_server_that_keeps_hanging_fails_the_track() {
        let (base_url, _) =
            crate::test_http::mock_server(vec![(0, ""); 4 * MAX_CONSECUTIVE_UNCLEAR]).await;
        let mut parts = Vec::new();
        for _ in 0..MAX_CONSECUTIVE_UNCLEAR {
            parts.push(speech(1000));
            parts.push(silence(2000));
        }
        let err = transcribe_track(
            &reqwest::Client::new(),
            &settings_for(base_url),
            &with_prompt(),
            &parts.concat(),
            RATE,
            &Resume::new(),
            |_, _, _| {},
            |_, _| {},
        )
        .await
        .unwrap_err();
        assert!(err.contains("in a row"), "{err}");
    }

    #[tokio::test]
    async fn resumed_utterances_are_not_sent_again() {
        let (base_url, seen) =
            crate::test_http::mock_server(vec![(200, r#"{"text":"Second."}"#)]).await;
        let track = [speech(1000), silence(2000), speech(1000)].concat();
        let first = vad::speech_regions(&track, RATE, &vad_config())[0];
        let resume = Resume::from([(
            (first.start, first.end),
            vec![segment(0, 0, "First.".to_string())],
        )]);
        let mut calls = Vec::new();
        let segments = transcribe_track(
            &reqwest::Client::new(),
            &settings_for(base_url),
            &RequestOptions::default(),
            &track,
            RATE,
            &resume,
            |_, _, _| {},
            |done, total| calls.push((done, total)),
        )
        .await
        .unwrap();
        let texts: Vec<&str> = segments.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(texts, vec!["First.", "Second."]);
        assert_eq!(calls, vec![(1, 2), (2, 2)]);
        assert_eq!(seen.lock().unwrap().len(), 1);
    }

    #[test]
    fn whisper_segments_are_offset_and_hallucinations_dropped() {
        let body = r#"{"text":"...","segments":[
            {"start":0.0,"end":2.0,"text":" Hola, ¿qué tal?","no_speech_prob":0.01,"avg_logprob":-0.2},
            {"start":2.0,"end":4.0,"text":" ¡Suscríbete al canal!","no_speech_prob":0.1,"avg_logprob":-0.3},
            {"start":4.0,"end":6.0,"text":" mmm","no_speech_prob":0.9,"avg_logprob":-1.4},
            {"start":6.0,"end":8.0,"text":" Vamos a empezar ya.","no_speech_prob":0.0,"avg_logprob":-0.1},
            {"start":8.0,"end":9.0,"text":" Vamos a empezar ya.","no_speech_prob":0.0,"avg_logprob":-0.1},
            {"start":9.0,"end":10.0,"text":" Sí.","no_speech_prob":0.0,"avg_logprob":-0.1}
        ]}"#;
        let segments = parse_verbose_response(body, 60_000).unwrap();
        let got: Vec<(i64, i64, &str)> = segments
            .iter()
            .map(|s| (s.start_ms, s.end_ms, s.text.as_str()))
            .collect();
        assert_eq!(
            got,
            vec![
                (60_000, 62_000, "Hola, ¿qué tal?"),
                (66_000, 68_000, "Vamos a empezar ya."),
                (69_000, 70_000, "Sí."),
            ]
        );
    }

    #[test]
    fn long_chunking_groups_utterances_until_a_pause_or_the_limit() {
        let r = |s_ms: usize, e_ms: usize| vad::Region {
            start: s_ms * 16,
            end: e_ms * 16,
        };
        let regions = [r(0, 5_000), r(6_000, 20_000), r(40_000, 50_000), r(51_000, 70_000)];
        let chunking = Chunking::Long {
            max_ms: 25_000,
            max_gap_ms: 8_000,
        };
        let chunks = plan_chunks(&regions, RATE, chunking);
        let spans: Vec<usize> = chunks.iter().map(|c| c.len()).collect();
        // The 20 s pause splits; then 40-70 s exceeds 25 s, so it splits too.
        assert_eq!(spans, vec![2, 1, 1]);
        assert_eq!(plan_chunks(&regions, RATE, Chunking::Utterances).len(), 4);
    }

    #[tokio::test]
    async fn long_chunks_ask_for_verbose_json_and_keep_whisper_timestamps() {
        let (base_url, seen) = crate::test_http::mock_server(vec![(
            200,
            r#"{"text":"a b","segments":[{"start":0.2,"end":1.0,"text":" First."},{"start":3.1,"end":4.0,"text":" Second."}]}"#,
        )])
        .await;
        let settings = BatchSettings {
            chunking: WHISPER_CHUNKING,
            ..settings_for(base_url)
        };
        let track = [silence(1000), speech(1000), silence(2000), speech(1000)].concat();
        let segments = transcribe_track(
            &reqwest::Client::new(),
            &settings,
            &RequestOptions::default(),
            &track,
            RATE,
            &Resume::new(),
            |_, _, _| {},
            |_, _| {},
        )
        .await
        .unwrap();
        assert_eq!(segments.len(), 2);
        let requests = seen.lock().unwrap();
        // Both utterances went in one request.
        assert_eq!(requests.len(), 1);
        assert!(String::from_utf8_lossy(&requests[0]).contains("verbose_json"));
        // Offsets are relative to the chunk start (the first utterance, ~0.8 s in).
        assert!((900..=1100).contains(&segments[0].start_ms), "{:?}", segments[0]);
        assert!(segments[1].start_ms > segments[0].start_ms + 2500);
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
            chunking: Chunking::Utterances,
        };
        println!("test_connection: {:?}", test_connection(&settings).await);
        let started = std::time::Instant::now();
        let segments = transcribe_track(
            &http_client(),
            &settings,
            &RequestOptions::from_session(&["en".to_string()], vocab(&["Parley"])),
            &wav.samples,
            wav.sample_rate,
            &Resume::new(),
            |_, _, _| {},
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
