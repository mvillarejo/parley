//! Where Parley keeps its files. Shared by the app and `parley-mcp` so both
//! always agree on the database location.
//!
//! - `config.json` lives in the Parley config directory
//!   (`~/Library/Application Support/parley/` on macOS).
//! - The data directory holds `parley.db` and `meetings/<id>/`. It defaults to
//!   the config directory and can be moved with the `storage_dir` setting.
//! - `PARLEY_HOME` overrides the config directory (tests, screenshots, a
//!   separate dev profile).

use std::path::{Path, PathBuf};

pub const APP_DIR_NAME: &str = "parley";
pub const DB_FILE_NAME: &str = "parley.db";
pub const MEETINGS_DIR_NAME: &str = "meetings";
pub const HOME_ENV: &str = "PARLEY_HOME";

/// Directory that holds `config.json` (and, by default, the data).
pub fn config_dir() -> PathBuf {
    if let Some(home) = std::env::var_os(HOME_ENV).filter(|v| !v.is_empty()) {
        return PathBuf::from(home);
    }
    let base = dirs::config_dir().unwrap_or_else(|| {
        std::env::var("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| std::env::temp_dir())
    });
    base.join(APP_DIR_NAME)
}

pub fn config_path() -> PathBuf {
    config_dir().join("config.json")
}

/// The `storage_dir` saved in `config.json`, if any. Read leniently: a missing
/// or unreadable config means "use the default".
pub fn storage_dir_from_config(config_path: &Path) -> Option<PathBuf> {
    let text = std::fs::read_to_string(config_path).ok()?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    value
        .get("storage_dir")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

/// Data directory currently in use.
pub fn data_dir() -> PathBuf {
    storage_dir_from_config(&config_path()).unwrap_or_else(config_dir)
}

pub fn db_path_in(data_dir: &Path) -> PathBuf {
    data_dir.join(DB_FILE_NAME)
}

pub fn db_path() -> PathBuf {
    db_path_in(&data_dir())
}

/// Absolute folder of a meeting, from the folder stored in the database
/// (relative to the data directory, e.g. `meetings/20261008-101500-ab12`).
pub fn meeting_dir(data_dir: &Path, folder: &str) -> PathBuf {
    data_dir.join(folder)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn storage_dir_is_read_from_config_and_blank_is_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        assert_eq!(storage_dir_from_config(&path), None);
        std::fs::write(&path, r#"{"storage_dir":"/Volumes/Data/parley"}"#).unwrap();
        assert_eq!(
            storage_dir_from_config(&path),
            Some(PathBuf::from("/Volumes/Data/parley"))
        );
        std::fs::write(&path, r#"{"storage_dir":"  "}"#).unwrap();
        assert_eq!(storage_dir_from_config(&path), None);
        std::fs::write(&path, r#"{"storage_dir":null}"#).unwrap();
        assert_eq!(storage_dir_from_config(&path), None);
        std::fs::write(&path, "not json").unwrap();
        assert_eq!(storage_dir_from_config(&path), None);
    }

    #[test]
    fn meeting_dir_joins_relative_folder() {
        assert_eq!(
            meeting_dir(Path::new("/data"), "meetings/abc"),
            PathBuf::from("/data/meetings/abc")
        );
    }
}
