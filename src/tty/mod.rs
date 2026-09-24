mod tline;
mod tline_builder;
mod trange;
mod tstring;

pub const CSI_RESET: &str = "\u{1b}[0m";
pub const CSI_BOLD: &str = "\u{1b}[1m";
pub const CSI_ITALIC: &str = "\u{1b}[3m";

pub const CSI_GREEN: &str = "\u{1b}[32m";

pub const CSI_RED: &str = "\u{1b}[31m";
pub const CSI_BOLD_RED: &str = "\u{1b}[1m\u{1b}[38;5;9m";
pub const CSI_BOLD_4BIT_RED: &str = "\u{1b}[1m\u{1b}[91m";
pub const CSI_BOLD_ORANGE: &str = "\u{1b}[1m\u{1b}[38;5;208m";
pub const CSI_BOLD_GREEN: &str = "\u{1b}[1m\u{1b}[38;5;34m";

/// Used for "Blocking"
pub const CSI_BLUE: &str = "\u{1b}[1m\u{1b}[36m";

#[cfg(windows)]
pub const CSI_BOLD_YELLOW: &str = "\u{1b}[1m\u{1b}[38;5;11m";
#[cfg(not(windows))]
pub const CSI_BOLD_YELLOW: &str = "\u{1b}[1m\u{1b}[33m";

#[cfg(windows)]
pub const CSI_BOLD_BLUE: &str = "\u{1b}[1m\u{1b}[38;5;14m";
#[cfg(not(windows))]
pub const CSI_BOLD_BLUE: &str = "\u{1b}[1m\u{1b}[38;5;12m";
pub const CSI_BOLD_4BIT_BLUE: &str = "\u{1b}[1m\u{1b}[94m";

// rustc doesn't always have the 256 colors available: depending on the
// terminal it may fall back to the bright 4 bit colors, which is what it
// does on Windows. They're the ones it then uses for the `-->` of a
// location, the `warning` of a title, and the body of a diagnostic.
pub const CSI_BOLD_4BIT_CYAN: &str = "\u{1b}[1m\u{1b}[96m";
#[cfg(windows)]
pub const CSI_BOLD_4BIT_BRIGHT_YELLOW: &str = "\u{1b}[1m\u{1b}[93m";
#[cfg(windows)]
pub const CSI_BOLD_4BIT_WHITE: &str = "\u{1b}[1m\u{1b}[97m";

#[cfg(windows)]
pub const CSI_BOLD_4BIT_YELLOW: &str = "\u{1b}[1m\u{1b}[33m";

#[cfg(windows)]
pub const CSI_BOLD_WHITE: &str = "\u{1b}[1m\u{1b}[38;5;15m";

static TAB_REPLACEMENT: &str = "    ";

use {
    crate::W,
    anyhow::Result,
    std::io::Write,
    termimad::crossterm::style::{
        Color,
        SetBackgroundColor,
        SetForegroundColor,
    },
};

pub use {
    tline::*,
    tline_builder::*,
    trange::*,
    tstring::*,
};

pub fn draw(
    w: &mut W,
    csi: &str,
    raw: &str,
) -> Result<()> {
    if csi.is_empty() {
        write!(w, "{raw}")?;
    } else {
        write!(w, "{csi}{raw}{CSI_RESET}")?;
    }
    Ok(())
}
/// CSI sequence for bold text with the given foreground and background colors
pub fn csi(
    fg: Color,
    bg: Color,
) -> String {
    format!(
        "{CSI_BOLD}{}{}",
        SetForegroundColor(fg),
        SetBackgroundColor(bg)
    )
}
/// CSI sequence for bold text with the given foreground color
pub fn csi_bold_fg(fg: Color) -> String {
    format!("{CSI_BOLD}{}", SetForegroundColor(fg))
}
