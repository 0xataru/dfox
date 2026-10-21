//! Glyphs, chosen to suit the terminal's font.

use dfox_core::models::connections::DbType;
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
    /// A NULL in the result grid.
    pub null: &'static str,
    pub yes: &'static str,
    pub no: &'static str,
    /// Plain ASCII only: no block characters for the big logo.
    pub ascii: bool,
    /// Nerd Font glyphs: each database has its own.
    pub nerd: bool,
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
                null: "∅",
                yes: "✓",
                no: "✗",
                ascii: false,
                nerd: true,
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
            null: "∅",
            yes: "✓",
            no: "✗",
            ascii: false,
            nerd: false,
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
            null: "NULL",
            yes: "true",
            no: "false",
            ascii: true,
            nerd: false,
        }
    }

    /// The glyph of a database: its logo in a Nerd Font, the database
    /// icon otherwise, which the theme colours by database.
    pub fn engine(&self, driver: DbType) -> &'static str {
        if !self.nerd {
            return self.database;
        }
        match driver {
            DbType::Postgres => "\u{e76e}",
            DbType::MySql => "\u{e704}",
            DbType::Sqlite => "\u{e7c4}",
            DbType::Mongo => "\u{e7a4}",
            DbType::Redis => "\u{e76d}",
            // No logo in Nerd Fonts.
            DbType::Cassandra => self.database,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_database_has_its_own_logo_in_a_nerd_font() {
        let nerd = Icons::new(IconStyle::Nerd);
        let all = [
            DbType::Postgres,
            DbType::MySql,
            DbType::Sqlite,
            DbType::Mongo,
            DbType::Redis,
        ];
        let logos: std::collections::HashSet<_> = all.iter().map(|d| nerd.engine(*d)).collect();
        assert_eq!(logos.len(), all.len());
        assert_eq!(nerd.engine(DbType::Cassandra), nerd.database);
        let plain = Icons::unicode();
        assert!(all.iter().all(|d| plain.engine(*d) == plain.database));
    }
}
