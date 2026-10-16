//! The Atlas visual system: warm paper in light mode, graphite in dark mode,
//! copper for actions and selection. Tokens mirror `styles/base.css` of the
//! original Helix Atlas design so both apps paint the same way.

use gpui::{App, Global, Hsla, Rgba, rgb};

/// Interface typography (compact, humanist).
pub const FONT_UI: &str = "Avenir Next";
/// Workspace and welcome titles.
pub const FONT_DISPLAY: &str = "Iowan Old Style";
/// Code, metadata and compact labels.
pub const FONT_MONO: &str = "Menlo";

/// Colour scheme.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Light,
    Dark,
}

impl Mode {
    /// The opposite scheme.
    #[must_use]
    pub fn toggled(self) -> Self {
        match self {
            Self::Light => Self::Dark,
            Self::Dark => Self::Light,
        }
    }

    /// Serialised name used in the settings file.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Light => "light",
            Self::Dark => "dark",
        }
    }
}

/// Colours of the animated query preview and the editor highlighter.
#[derive(Clone, Copy, Debug)]
pub struct SyntaxColors {
    pub keyword: Hsla,
    pub entity: Hsla,
    pub label: Hsla,
    pub property: Hsla,
    pub direction: Hsla,
    pub operator: Hsla,
    pub literal: Hsla,
    pub number: Hsla,
    pub punctuation: Hsla,
    pub comment: Hsla,
    pub param: Hsla,
}

/// Every colour token of the design system.
#[derive(Clone, Copy, Debug)]
pub struct Theme {
    pub mode: Mode,
    pub window: Hsla,
    pub surface0: Hsla,
    pub surface1: Hsla,
    pub surface2: Hsla,
    pub elevated: Hsla,
    pub toolbar: Hsla,
    pub control: Hsla,
    pub ink: Hsla,
    pub ink_secondary: Hsla,
    pub ink_muted: Hsla,
    pub hairline: Hsla,
    pub border: Hsla,
    pub accent: Hsla,
    pub accent_ink: Hsla,
    pub danger: Hsla,
    pub good: Hsla,
    pub warning: Hsla,
    pub overlay: Hsla,
    pub syntax: SyntaxColors,
}

fn c(hex: u32) -> Hsla {
    rgb(hex).into()
}

fn ca(r: u8, g: u8, b: u8, a: f32) -> Hsla {
    Rgba {
        r: f32::from(r) / 255.,
        g: f32::from(g) / 255.,
        b: f32::from(b) / 255.,
        a,
    }
    .into()
}

impl Theme {
    /// Graphite surfaces.
    #[must_use]
    pub fn dark() -> Self {
        let accent = c(0xe6ac77);
        Self {
            mode: Mode::Dark,
            window: c(0x17191b),
            surface0: c(0x1d2022),
            surface1: c(0x222629),
            surface2: c(0x2c3134),
            elevated: c(0x303639),
            toolbar: c(0x191c1e),
            control: c(0x282d30),
            ink: c(0xf2eee5),
            ink_secondary: c(0xbcbdb9),
            ink_muted: c(0x9caaa9),
            hairline: ca(226, 233, 230, 0.09),
            border: ca(226, 233, 230, 0.18),
            accent,
            accent_ink: c(0x2e2015),
            danger: c(0xff817c),
            good: c(0x75c69b),
            warning: c(0xf6ca69),
            overlay: ca(4, 9, 6, 0.7),
            syntax: SyntaxColors {
                keyword: accent,
                entity: c(0xeec485),
                label: c(0x93c9ed),
                property: c(0xbcbdb9),
                direction: c(0xc2aaec),
                operator: accent,
                literal: c(0x95d4a7),
                number: c(0xeec485),
                punctuation: c(0x9caaa9),
                comment: c(0x7d8887),
                param: c(0xc2aaec),
            },
        }
    }

    /// Warm paper surfaces.
    #[must_use]
    pub fn light() -> Self {
        let accent = c(0x955126);
        Self {
            mode: Mode::Light,
            window: c(0xeae8e1),
            surface0: c(0xfaf9f5),
            surface1: c(0xf1f0e9),
            surface2: c(0xe9e8df),
            elevated: c(0xffffff),
            toolbar: c(0xfaf9f5),
            control: c(0xf7f6f0),
            ink: c(0x292f30),
            ink_secondary: c(0x545f60),
            ink_muted: c(0x697272),
            hairline: ca(39, 51, 51, 0.10),
            border: ca(39, 51, 51, 0.19),
            accent,
            accent_ink: c(0xffffff),
            danger: c(0xa9342e),
            good: c(0x276e4c),
            warning: c(0x8d5a08),
            overlay: ca(15, 25, 17, 0.36),
            syntax: SyntaxColors {
                keyword: accent,
                entity: c(0x87591c),
                label: c(0x2569a8),
                property: c(0x545f60),
                direction: c(0x6654a5),
                operator: accent,
                literal: c(0x276e35),
                number: c(0x87591c),
                punctuation: c(0x697272),
                comment: c(0x8a9290),
                param: c(0x6654a5),
            },
        }
    }

    /// The theme for a scheme.
    #[must_use]
    pub fn for_mode(mode: Mode) -> Self {
        match mode {
            Mode::Light => Self::light(),
            Mode::Dark => Self::dark(),
        }
    }

    /// `color-mix(in srgb, a <pct>%, b)`.
    #[must_use]
    pub fn mix(&self, a: Hsla, b: Hsla, pct_a: f32) -> Hsla {
        mix(a, b, pct_a)
    }
}

/// `color-mix(in srgb, a pct_a, b)` including the CSS alpha semantics, so mixing
/// with `transparent` yields a translucent `a`.
#[must_use]
pub fn mix(a: Hsla, b: Hsla, pct_a: f32) -> Hsla {
    let t = pct_a.clamp(0., 1.);
    let a: Rgba = a.into();
    let b: Rgba = b.into();
    let alpha = a.a * t + b.a * (1. - t);
    if alpha <= 0. {
        return Rgba {
            r: 0.,
            g: 0.,
            b: 0.,
            a: 0.,
        }
        .into();
    }
    let wa = a.a * t / alpha;
    let wb = b.a * (1. - t) / alpha;
    Rgba {
        r: a.r * wa + b.r * wb,
        g: a.g * wa + b.g * wb,
        b: a.b * wa + b.b * wb,
        a: alpha,
    }
    .into()
}

/// Fully transparent.
#[must_use]
pub fn transparent() -> Hsla {
    gpui::transparent_black()
}

/// The active theme, stored as a GPUI global.
#[derive(Clone, Copy, Debug)]
pub struct ThemeState(pub Theme);

impl Global for ThemeState {}

static DARK: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

/// Installs the theme as the GPUI global.
pub fn install(cx: &mut App, mode: Mode) {
    DARK.store(mode == Mode::Dark, std::sync::atomic::Ordering::Relaxed);
    cx.set_global(ThemeState(Theme::for_mode(mode)));
}

/// Default icon tint. GPUI paints an SVG only with the icon's own text
/// colour, so icons cannot inherit it from their parent.
#[must_use]
pub fn icon_default() -> Hsla {
    let mode = if DARK.load(std::sync::atomic::Ordering::Relaxed) {
        Mode::Dark
    } else {
        Mode::Light
    };
    Theme::for_mode(mode).ink_secondary
}

/// The active theme.
pub fn theme(cx: &App) -> &Theme {
    &cx.global::<ThemeState>().0
}

/// Replaces the active theme.
pub fn set_mode(cx: &mut App, mode: Mode) {
    install(cx, mode);
    cx.refresh_windows();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mixing_with_transparent_keeps_the_hue_and_scales_alpha() {
        let accent = Theme::dark().accent;
        let mixed: Rgba = mix(accent, transparent(), 0.12).into();
        let base: Rgba = accent.into();
        assert!((mixed.a - 0.12).abs() < 1e-5);
        assert!((mixed.r - base.r).abs() < 1e-4);
    }
}
