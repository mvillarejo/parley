//! Microphone permission (AVFoundation) and the System Audio Recording status.

#[cfg(target_os = "macos")]
pub mod microphone {
    use objc2::msg_send;
    use objc2::runtime::{AnyClass, AnyObject, Bool};

    #[link(name = "AVFoundation", kind = "framework")]
    extern "C" {
        static AVMediaTypeAudio: *const AnyObject;
    }

    pub fn check_status() -> String {
        unsafe {
            let cls = match AnyClass::get("AVCaptureDevice") {
                Some(c) => c,
                None => return "unknown".into(),
            };
            let status: i64 = msg_send![cls, authorizationStatusForMediaType: AVMediaTypeAudio];
            match status {
                0 => "not_determined",
                1 => "restricted",
                2 => "denied",
                3 => "authorized",
                _ => "unknown",
            }
            .into()
        }
    }

    /// `authorizationStatusForMediaType:` is cached for the process lifetime, so a
    /// grant the user just made isn't seen until restart. Spawn our own binary
    /// with `--check-mic` to read a fresh status; fall back to the in-process
    /// check if the subprocess can't be launched.
    pub fn check_status_fresh() -> String {
        if let Ok(exe) = std::env::current_exe() {
            if let Ok(output) = std::process::Command::new(&exe).arg("--check-mic").output() {
                if output.status.success() {
                    let status = String::from_utf8_lossy(&output.stdout).trim().to_string();
                    if !status.is_empty() {
                        return status;
                    }
                }
            }
        }
        check_status()
    }

    pub fn request_access() -> bool {
        use std::sync::mpsc;
        unsafe {
            let cls = match AnyClass::get("AVCaptureDevice") {
                Some(c) => c,
                None => return false,
            };
            let (tx, rx) = mpsc::channel::<bool>();
            let handler = block2::RcBlock::new(move |granted: Bool| {
                let _ = tx.send(granted.as_bool());
            });
            let _: () = msg_send![
                cls,
                requestAccessForMediaType: AVMediaTypeAudio
                completionHandler: &*handler
            ];
            match rx.recv_timeout(std::time::Duration::from_secs(120)) {
                Ok(granted) => granted,
                Err(_) => check_status_fresh() == "authorized",
            }
        }
    }
}

#[cfg(not(target_os = "macos"))]
pub mod microphone {
    pub fn check_status() -> String {
        "authorized".into()
    }
    pub fn check_status_fresh() -> String {
        "authorized".into()
    }
    pub fn request_access() -> bool {
        true
    }
}

/// Open a Privacy & Security pane in System Settings.
pub fn open_settings_pane(panel: &str) -> Result<(), String> {
    let url = match panel {
        "microphone" => {
            "x-apple.systempreferences:com.apple.preference.security?Privacy_Microphone"
        }
        "system_audio" => {
            "x-apple.systempreferences:com.apple.preference.security?Privacy_AudioCapture"
        }
        _ => return Err(format!("Unknown settings panel: {panel}")),
    };
    open::that_detached(url).map_err(|e| e.to_string())
}
