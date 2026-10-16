//! Persistence of the connection and the theme preference, in the platform
//! config directory. The token is stored in plain text, protected by `0600`
//! file permissions rather than a keychain, matching the original app.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::client::ConnectionSettings;

/// Everything remembered between launches.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Settings {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connection: Option<ConnectionSettings>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub theme: Option<String>,
}

/// `~/Library/Application Support/Ratik Atlas/settings.json` on macOS, the
/// XDG config directory elsewhere.
#[must_use]
pub fn settings_path() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from)?;
    let dir = if cfg!(target_os = "macos") {
        home.join("Library/Application Support/Ratik Atlas")
    } else if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME") {
        PathBuf::from(xdg).join("ratik-atlas")
    } else {
        home.join(".config/ratik-atlas")
    };
    Some(dir.join("settings.json"))
}

/// Reads the saved settings; a missing or unreadable file yields the default.
#[must_use]
pub fn load() -> Settings {
    let Some(path) = settings_path() else {
        return Settings::default();
    };
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

/// Writes the settings, creating the directory and narrowing permissions.
///
/// # Errors
/// I/O failures while creating the directory or writing the file.
pub fn save(settings: &Settings) -> std::io::Result<()> {
    let Some(path) = settings_path() else {
        return Err(std::io::Error::other("no home directory"));
    };
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let text = serde_json::to_string_pretty(settings).map_err(std::io::Error::other)?;
    std::fs::write(&path, text)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}
