//! Where NexaDB keeps things, following each platform's conventions.
//!
//! | | macOS | Linux |
//! |---|---|---|
//! | data | `~/Library/Application Support/NexaDB` | `$XDG_DATA_HOME/nexadb` (`~/.local/share/nexadb`) |
//! | logs | `~/Library/Logs/NexaDB` | `<data>/logs` |
//!
//! `NEXA_DATA` overrides the data directory everywhere.

use std::path::{Path, PathBuf};

fn home() -> PathBuf {
    std::env::var_os("HOME").map_or_else(|| PathBuf::from("."), PathBuf::from)
}

pub fn default_data_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("NEXA_DATA") {
        return PathBuf::from(dir);
    }
    if cfg!(target_os = "macos") {
        return home().join("Library/Application Support/NexaDB");
    }
    std::env::var_os("XDG_DATA_HOME")
        .map_or_else(|| home().join(".local/share"), PathBuf::from)
        .join("nexadb")
}

pub fn log_file(data: &Path) -> PathBuf {
    if cfg!(target_os = "macos")
        && std::env::var_os("NEXA_DATA").is_none()
        && data == default_data_dir()
    {
        return home().join("Library/Logs/NexaDB/nexa.log");
    }
    data.join("logs/nexa.log")
}

/// Written by a running server: its pid, URL and data directory.
pub fn state_file(data: &Path) -> PathBuf {
    data.join("server.json")
}

/// launchd agent (macOS) / systemd user unit (Linux).
pub fn service_file() -> PathBuf {
    if cfg!(target_os = "macos") {
        home().join("Library/LaunchAgents/com.nexadb.nexa.plist")
    } else {
        std::env::var_os("XDG_CONFIG_HOME")
            .map_or_else(|| home().join(".config"), PathBuf::from)
            .join("systemd/user/nexadb.service")
    }
}

pub fn history_file() -> PathBuf {
    home().join(".nexa_history")
}
