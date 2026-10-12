//! Colours. Every colour the UI draws comes from a [`Theme`]; nothing outside
//! this module names a colour (`scripts/check.sh` enforces it).

mod color;
mod palettes;

use std::path::Path;

use dfox_core::result::Cell;
use ratatui::style::{Color, Modifier, Style};

pub use color::{ColorDepth, Rgb};
pub use palettes::BUILTIN;

/// Declares the colour tokens once, for both the hex [`Palette`] and the
/// terminal-ready [`Theme`].
macro_rules! tokens {
    ($($token:ident),* $(,)?) => {
        /// Theme colours as RGB, before they are fitted to the terminal.
        #[derive(Debug, Clone, PartialEq, Eq)]
        pub struct Palette {
            $(pub $token: Rgb,)*
        }

        /// Colours ready to draw on this terminal.
        #[derive(Debug, Clone, PartialEq)]
        pub struct Theme {
            pub name: String,
            $(pub $token: Color,)*
        }

        /// Token names, in declaration order, as used in theme files.
        pub const TOKENS: &[&str] = &[$(stringify!($token)),*];

        impl Palette {
            /// Builds a palette from one hex colour per token, in [`TOKENS`] order.
            fn from_hex(hex: &[&str]) -> Option<Self> {
                let mut values = hex.iter().map(|h| Rgb::parse(h));
                Some(Self { $($token: values.next()??,)* })
            }

            /// Sets one token. Returns `false` for an unknown name.
            fn set(&mut self, token: &str, rgb: Rgb) -> bool {
                match token {
                    $(stringify!($token) => self.$token = rgb,)*
                    _ => return false,
                }
                true
            }

            pub fn to_theme(&self, name: &str, depth: ColorDepth) -> Theme {
                Theme {
                    name: name.to_string(),
                    $($token: self.$token.to_color(depth),)*
                }
            }
        }
    };
}

tokens!(
    bg,
    surface,
    surface_alt,
    border,
    border_focus,
    text,
    text_dim,
    accent,
    accent_alt,
    success,
    warning,
    danger,
    null,
    number,
    string,
    keyword,
    comment,
    selection,
);

/// The palette of a built-in theme.
pub fn builtin(name: &str) -> Option<Palette> {
    BUILTIN
        .iter()
        .find(|(builtin, _)| *builtin == name)
        .and_then(|(_, hex)| Palette::from_hex(hex))
}

/// Loads `name`: a built-in theme, or `<themes_dir>/<name>.toml`.
///
/// A theme file sets any tokens it wants as `"#rrggbb"`; the rest come from
/// the theme named by `extends`, or from `dfox`.
pub fn load(name: &str, themes_dir: Option<&Path>, depth: ColorDepth) -> Result<Theme, String> {
    if let Some(palette) = builtin(name) {
        return Ok(palette.to_theme(name, depth));
    }
    let dir = themes_dir.ok_or_else(|| format!("unknown theme {name:?}"))?;
    let path = dir.join(format!("{name}.toml"));
    let text = std::fs::read_to_string(&path)
        .map_err(|err| format!("theme {name:?}: can't read {}: {err}", path.display()))?;
    parse_theme_file(&text)
        .map(|palette| palette.to_theme(name, depth))
        .map_err(|err| format!("theme {name:?}: {err}"))
}

fn parse_theme_file(text: &str) -> Result<Palette, String> {
    let table: toml::Table = text.parse().map_err(|err| format!("{err}"))?;
    let base = match table.get("extends") {
        Some(toml::Value::String(base)) => base.as_str(),
        Some(_) => return Err("`extends` must be a theme name".to_string()),
        None => "dfox",
    };
    let mut palette = builtin(base).ok_or_else(|| format!("unknown base theme {base:?}"))?;
    for (key, value) in table.iter().filter(|(key, _)| *key != "extends") {
        let hex = value
            .as_str()
            .ok_or_else(|| format!("`{key}` must be a \"#rrggbb\" string"))?;
        let rgb = Rgb::parse(hex).ok_or_else(|| format!("`{key}`: {hex:?} is not #rrggbb"))?;
        if !palette.set(key, rgb) {
            return Err(format!(
                "unknown colour `{key}`; known: {}",
                TOKENS.join(", ")
            ));
        }
    }
    Ok(palette)
}

impl Default for Theme {
    fn default() -> Self {
        builtin("dfox")
            .map(|palette| palette.to_theme("dfox", ColorDepth::TrueColor))
            .unwrap_or_else(|| Theme::plain("dfox"))
    }
}

impl Theme {
    /// The terminal's own colours, if a palette is ever missing.
    fn plain(name: &str) -> Self {
        let c = Color::Reset;
        Theme {
            name: name.to_string(),
            bg: c,
            surface: c,
            surface_alt: c,
            border: c,
            border_focus: c,
            text: c,
            text_dim: c,
            accent: c,
            accent_alt: c,
            success: c,
            warning: c,
            danger: c,
            null: c,
            number: c,
            string: c,
            keyword: c,
            comment: c,
            selection: c,
        }
    }

    /// The default text on the background.
    pub fn base(&self) -> Style {
        Style::default().fg(self.text).bg(self.bg)
    }

    pub fn surface(&self) -> Style {
        Style::default().fg(self.text).bg(self.surface)
    }

    pub fn dim(&self) -> Style {
        Style::default().fg(self.text_dim)
    }

    pub fn border(&self, focused: bool) -> Style {
        Style::default().fg(if focused {
            self.border_focus
        } else {
            self.border
        })
    }

    pub fn title(&self, focused: bool) -> Style {
        let style = Style::default().add_modifier(Modifier::BOLD);
        if focused {
            style.fg(self.accent)
        } else {
            style.fg(self.text)
        }
    }

    /// The selected item of a focused list.
    pub fn highlight(&self) -> Style {
        Style::default()
            .fg(self.bg)
            .bg(self.accent)
            .add_modifier(Modifier::BOLD)
    }

    /// The selected item of a list that isn't focused, or a selected row.
    pub fn selection(&self) -> Style {
        Style::default().fg(self.text).bg(self.selection)
    }

    pub fn key(&self) -> Style {
        Style::default()
            .fg(self.accent)
            .add_modifier(Modifier::BOLD)
    }

    pub fn header(&self) -> Style {
        Style::default()
            .fg(self.accent_alt)
            .add_modifier(Modifier::BOLD)
    }

    pub fn success(&self) -> Style {
        Style::default().fg(self.success)
    }

    pub fn warning(&self) -> Style {
        Style::default().fg(self.warning)
    }

    pub fn danger(&self) -> Style {
        Style::default().fg(self.danger)
    }

    /// A result value, coloured by its kind.
    pub fn cell(&self, cell: &Cell<'_>) -> Style {
        let style = Style::default();
        match cell {
            Cell::Null => style.fg(self.null).add_modifier(Modifier::ITALIC),
            Cell::Bool(_) => style.fg(self.keyword),
            Cell::Int(_) | Cell::UInt(_) | Cell::Float(_) | Cell::Decimal(_) => {
                style.fg(self.number)
            }
            Cell::Json(_) => style.fg(self.string),
            Cell::Bytes(_) => style.fg(self.comment),
            Cell::Undecodable(_) => style.fg(self.danger),
            _ => style.fg(self.text),
        }
    }

    /// A colour between `from` and `to`, `at` from 0.0 to 1.0. Only blends
    /// true colours; on other terminals it switches halfway.
    pub fn blend(from: Color, to: Color, at: f64) -> Color {
        match (from, to) {
            (Color::Rgb(r1, g1, b1), Color::Rgb(r2, g2, b2)) => {
                let mix = |a: u8, b: u8| {
                    (a as f64 + (b as f64 - a as f64) * at.clamp(0.0, 1.0)).round() as u8
                };
                Color::Rgb(mix(r1, r2), mix(g1, g2), mix(b1, b2))
            }
            _ if at < 0.5 => from,
            _ => to,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_builtin_theme_is_complete() {
        for (name, hex) in BUILTIN {
            assert_eq!(
                hex.len(),
                TOKENS.len(),
                "{name} has the wrong number of colours"
            );
            assert!(Palette::from_hex(hex).is_some(), "{name} has a bad colour");
        }
    }

    #[test]
    fn theme_files_extend_a_builtin() {
        let palette = parse_theme_file("extends = \"nord\"\naccent = \"#ff0000\"").expect("theme");
        let nord = builtin("nord").expect("nord");
        assert_eq!(palette.accent, Rgb(255, 0, 0));
        assert_eq!(palette.bg, nord.bg);
    }

    #[test]
    fn theme_file_mistakes_are_explained() {
        let unknown = parse_theme_file("acent = \"#ff0000\"").unwrap_err();
        assert!(unknown.starts_with("unknown colour `acent`"), "{unknown}");
        assert!(parse_theme_file("accent = \"red\"")
            .unwrap_err()
            .contains("not #rrggbb"));
        assert!(parse_theme_file("extends = \"nope\"")
            .unwrap_err()
            .contains("nope"));
    }

    #[test]
    fn user_themes_load_from_the_themes_directory() {
        let dir = std::env::temp_dir().join(format!("dfox-themes-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("dir");
        std::fs::write(dir.join("mine.toml"), "text = \"#010203\"").expect("write");

        let theme = load("mine", Some(&dir), ColorDepth::TrueColor).expect("theme");
        assert_eq!(theme.name, "mine");
        assert_eq!(theme.text, Color::Rgb(1, 2, 3));
        assert!(load("missing", Some(&dir), ColorDepth::TrueColor).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn blend_mixes_true_colours() {
        let mid = Theme::blend(Color::Rgb(0, 0, 0), Color::Rgb(200, 100, 50), 0.5);
        assert_eq!(mid, Color::Rgb(100, 50, 25));
        assert_eq!(
            Theme::blend(Color::Indexed(1), Color::Indexed(2), 0.7),
            Color::Indexed(2)
        );
    }
}
