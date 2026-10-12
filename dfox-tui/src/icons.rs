//! Glyphs, chosen to suit the terminal's font.

use serde::{Deserialize, Serialize};
use throbber_widgets_tui::symbols::throbber::{self, Set};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum IconStyle {
    /// Unicode symbols, or ASCII on the Linux console.
    #[default]
    Auto,
    /// Nerd Font glyphs.
    Nerd,
    Ascii,
}

#[derive(Debug, Clone)]
pub struct Icons {
    pub spinner: Set,
    pub database: &'static str,
    pub table: &'static str,
    pub column: &'static str,
    pub info: &'static str,
    pub success: &'static str,
    pub warning: &'static str,
    pub error: &'static str,
    pub selected: &'static str,
}

impl Icons {
    pub fn new(style: IconStyle) -> Self {
        match style {
            IconStyle::Auto => {
                let term = std::env::var("TERM").unwrap_or_default();
                if matches!(term.as_str(), "linux" | "dumb") {
                    Self::ascii()
                } else {
                    Self::unicode()
                }
            }
            IconStyle::Nerd => Self {
                spinner: throbber::BRAILLE_SIX,
                database: "\u{f1c0}",
                table: "\u{f0ce}",
                column: "\u{f0db}",
                info: "\u{f05a}",
                success: "\u{f00c}",
                warning: "\u{f071}",
                error: "\u{f00d}",
                selected: "\u{f054}",
            },
            IconStyle::Ascii => Self::ascii(),
        }
    }

    pub fn unicode() -> Self {
        Self {
            spinner: throbber::BRAILLE_SIX,
            database: "◆",
            table: "▤",
            column: "├",
            info: "●",
            success: "✔",
            warning: "▲",
            error: "✖",
            selected: "▸",
        }
    }

    fn ascii() -> Self {
        Self {
            spinner: throbber::ASCII,
            database: "#",
            table: "=",
            column: "|",
            info: "i",
            success: "+",
            warning: "!",
            error: "x",
            selected: ">",
        }
    }
}
