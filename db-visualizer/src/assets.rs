//! Icons are compiled into the binary so the app runs from a single file.

use std::borrow::Cow;

use gpui::{AssetSource, Result, SharedString};

macro_rules! icons {
    ($($name:literal),* $(,)?) => {
        const ICONS: &[(&str, &[u8])] = &[
            $((concat!("icons/", $name, ".svg"), include_bytes!(concat!("../assets/icons/", $name, ".svg"))),)*
        ];
    };
}

icons!(
    "compass",
    "terminal-square",
    "database",
    "network",
    "plug",
    "plug-zap",
    "sun",
    "moon",
    "monitor",
    "cloud",
    "refresh-cw",
    "play",
    "loader-circle",
    "lock-keyhole",
    "lock-keyhole-open",
    "arrow-up-right",
    "arrow-right",
    "arrow-left",
    "mouse-pointer-2",
    "search",
    "chevrons-up-down",
    "chevron-right",
    "chevron-down",
    "circle-dot",
    "git-fork",
    "zap",
    "plus",
    "minus",
    "maximize",
    "rotate-ccw",
    "x",
    "check",
    "trash-2",
    "pencil",
    "circle-alert",
    "braces",
);

/// Serves the embedded icon set to GPUI's `svg()` element.
#[derive(Debug, Clone, Copy)]
pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        Ok(ICONS
            .iter()
            .find(|(name, _)| *name == path)
            .map(|(_, bytes)| Cow::Borrowed(*bytes)))
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        Ok(ICONS
            .iter()
            .filter(|(name, _)| name.starts_with(path))
            .map(|(name, _)| SharedString::from(*name))
            .collect())
    }
}
