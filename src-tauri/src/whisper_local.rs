//! Local whisper.cpp: Parley starts `whisper-server` (Homebrew `whisper-cpp`)
//! on a free loopback port for one transcription job and stops it afterwards,
//! so nothing has to be left running. The server speaks the OpenAI-compatible
//! `/v1/audio/transcriptions` API, so `batch_stt` drives it like any other.

use crate::batch_stt::{BatchSettings, WHISPER_CHUNKING};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const BINARY: &str = "whisper-server";
/// Loading a large model from a cold disk can take a while.
const STARTUP_TIMEOUT: Duration = Duration::from_secs(90);
const INSTALL_HINT: &str = "Install whisper.cpp with: brew install whisper-cpp";

/// `whisper-server` from Homebrew or `PATH`. Apps launched from Finder get a
/// minimal `PATH`, so the Homebrew prefixes are checked first.
pub fn find_binary() -> Option<PathBuf> {
    let fixed = ["/opt/homebrew/bin", "/usr/local/bin"].map(PathBuf::from);
    let from_path = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect::<Vec<_>>())
        .unwrap_or_default();
    fixed
        .into_iter()
        .chain(from_path)
        .map(|dir| dir.join(BINARY))
        .find(|candidate| candidate.is_file())
}

fn check_setup(model_path: &str) -> Result<PathBuf, String> {
    let binary = find_binary().ok_or_else(|| format!("whisper-server not found. {INSTALL_HINT}"))?;
    if !Path::new(model_path).is_file() {
        return Err(format!("Model file not found: {model_path}"));
    }
    Ok(binary)
}

fn model_name(model_path: &str) -> String {
    Path::new(model_path)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| model_path.to_string())
}

fn free_port() -> Result<u16, String> {
    std::net::TcpListener::bind("127.0.0.1:0")
        .and_then(|l| l.local_addr())
        .map(|a| a.port())
        .map_err(|e| format!("Can't find a free port for whisper-server: {e}"))
}

/// A running `whisper-server`, stopped on drop.
pub struct WhisperServer {
    child: Child,
    pub settings: BatchSettings,
}

impl WhisperServer {
    /// Start the server and wait until it answers. `language` is the meeting
    /// language when exactly one is configured; otherwise Whisper detects it.
    pub async fn start(model_path: &str, language: Option<&str>) -> Result<Self, String> {
        let binary = check_setup(model_path)?;
        let port = free_port()?;
        let threads = std::thread::available_parallelism()
            .map(|n| n.get().min(8))
            .unwrap_or(4)
            .to_string();
        let child = Command::new(&binary)
            .args(["-m", model_path, "--host", "127.0.0.1"])
            .args(["--port", &port.to_string()])
            .args(["--inference-path", "/v1/audio/transcriptions"])
            .args(["-l", language.unwrap_or("auto"), "-t", &threads])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("Can't start {}: {e}", binary.display()))?;
        let mut server = Self {
            child,
            settings: BatchSettings {
                base_url: format!("http://127.0.0.1:{port}/v1"),
                model: model_name(model_path),
                api_key: None,
                chunking: WHISPER_CHUNKING,
            },
        };
        server.wait_ready(port).await?;
        log::info!(
            "[whisper] server ready on port {port} with {}",
            server.settings.model
        );
        Ok(server)
    }

    async fn wait_ready(&mut self, port: u16) -> Result<(), String> {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(2))
            .build()
            .map_err(|e| e.to_string())?;
        let url = format!("http://127.0.0.1:{port}/health");
        let started = Instant::now();
        loop {
            if let Ok(Some(status)) = self.child.try_wait() {
                return Err(format!(
                    "whisper-server exited ({status}) while loading the model; check that the model file is a whisper.cpp ggml model"
                ));
            }
            if let Ok(response) = client.get(&url).send().await {
                if response.status().is_success() {
                    return Ok(());
                }
            }
            if started.elapsed() > STARTUP_TIMEOUT {
                return Err("whisper-server took too long to load the model".to_string());
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }
}

impl Drop for WhisperServer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Check the binary and model, and that the model actually loads.
pub async fn test_setup(model_path: &str) -> Result<String, String> {
    let server = WhisperServer::start(model_path, None).await?;
    Ok(format!(
        "Ready. whisper.cpp loaded {}.",
        server.settings.model
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_model_is_a_clear_error() {
        let err = check_setup("/nonexistent/ggml-model.bin").unwrap_err();
        assert!(
            err.contains("not found"),
            "{err}"
        );
    }

    #[test]
    fn model_name_is_the_file_name() {
        assert_eq!(
            model_name("/x/y/ggml-large-v3-turbo.bin"),
            "ggml-large-v3-turbo.bin"
        );
    }
}
