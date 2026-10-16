//! The RatikDB look: an ultraviolet → cyan gradient, used sparingly.

use std::io::IsTerminal;
use std::sync::OnceLock;

/// Brand colors (RGB).
pub const ULTRAVIOLET: (u8, u8, u8) = (139, 92, 246);
pub const CYAN: (u8, u8, u8) = (34, 211, 238);
pub const VOLT: (u8, u8, u8) = (163, 230, 53);
pub const EMBER: (u8, u8, u8) = (251, 113, 133);
pub const AMBER: (u8, u8, u8) = (251, 191, 36);

pub const TAGLINE: &str = "Graphs at the speed of thought.";

const LOGO: [&str; 6] = [
    "██████╗  █████╗ ████████╗██╗██╗  ██╗██████╗ ██████╗ ",
    "██╔══██╗██╔══██╗╚══██╔══╝██║██║ ██╔╝██╔══██╗██╔══██╗",
    "██████╔╝███████║   ██║   ██║█████╔╝ ██║  ██║██████╔╝",
    "██╔══██╗██╔══██║   ██║   ██║██╔═██╗ ██║  ██║██╔══██╗",
    "██║  ██║██║  ██║   ██║   ██║██║  ██╗██████╔╝██████╔╝",
    "╚═╝  ╚═╝╚═╝  ╚═╝   ╚═╝   ╚═╝╚═╝  ╚═╝╚═════╝ ╚═════╝ ",
];

pub fn color() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("NO_COLOR").is_none() && std::io::stdout().is_terminal())
}

pub fn rgb(s: &str, (r, g, b): (u8, u8, u8)) -> String {
    if color() {
        format!("\x1b[38;2;{r};{g};{b}m{s}\x1b[0m")
    } else {
        s.to_owned()
    }
}

pub fn bold(s: &str) -> String {
    if color() {
        format!("\x1b[1m{s}\x1b[0m")
    } else {
        s.to_owned()
    }
}

pub fn dim(s: &str) -> String {
    if color() {
        format!("\x1b[2m{s}\x1b[0m")
    } else {
        s.to_owned()
    }
}

fn lerp((r1, g1, b1): (u8, u8, u8), (r2, g2, b2): (u8, u8, u8), t: f32) -> (u8, u8, u8) {
    let mix = |a: u8, b: u8| (f32::from(a) + (f32::from(b) - f32::from(a)) * t).round() as u8;
    (mix(r1, r2), mix(g1, g2), mix(b1, b2))
}

/// Paints `s` with a left-to-right ultraviolet → cyan gradient.
pub fn gradient(s: &str) -> String {
    if !color() {
        return s.to_owned();
    }
    let n = s.chars().count().max(2) - 1;
    s.chars()
        .enumerate()
        .map(|(i, c)| {
            if c == ' ' {
                return c.to_string();
            }
            let (r, g, b) = lerp(ULTRAVIOLET, CYAN, i as f32 / n as f32);
            format!("\x1b[38;2;{r};{g};{b}m{c}")
        })
        .collect::<String>()
        + "\x1b[0m"
}

/// The full logo, tagline and version.
pub fn banner() -> String {
    let mut out = String::from("\n");
    for line in LOGO {
        out.push_str("  ");
        out.push_str(&gradient(line));
        out.push('\n');
    }
    out.push_str(&format!(
        "  {}  {}\n",
        dim(TAGLINE),
        rgb(&format!("v{}", env!("CARGO_PKG_VERSION")), VOLT)
    ));
    out
}

/// `◆ RatikDB` — the compact mark used in one-line messages.
pub fn mark() -> String {
    format!("{} {}", gradient("◆"), bold("RatikDB"))
}

pub fn ok(msg: &str) -> String {
    format!("{} {msg}", rgb("✔", VOLT))
}

pub fn warn(msg: &str) -> String {
    format!("{} {msg}", rgb("!", AMBER))
}

pub fn err(msg: &str) -> String {
    format!("{} {msg}", rgb("✘", EMBER))
}

/// `label  value` aligned info line.
pub fn kv(label: &str, value: &str) -> String {
    format!("  {}  {value}", dim(&format!("{label:<9}")))
}
