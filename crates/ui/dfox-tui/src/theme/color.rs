//! Hex colours, and how they are drawn on terminals with fewer colours.

use ratatui::style::Color;

/// How many colours the terminal can show.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorDepth {
    TrueColor,
    Ansi256,
    Ansi16,
}

impl ColorDepth {
    /// The depth `color` was made for: a theme's colours tell what the
    /// terminal can show.
    pub fn of(color: Color) -> Self {
        match color {
            Color::Rgb(..) => ColorDepth::TrueColor,
            Color::Indexed(_) => ColorDepth::Ansi256,
            _ => ColorDepth::Ansi16,
        }
    }

    /// Reads the terminal's colour support from the environment.
    pub fn detect() -> Self {
        let var = |name| std::env::var(name).unwrap_or_default().to_lowercase();
        let colorterm = var("COLORTERM");
        if colorterm.contains("truecolor")
            || colorterm.contains("24bit")
            || std::env::var_os("WT_SESSION").is_some()
        {
            ColorDepth::TrueColor
        } else if var("TERM").contains("256") {
            ColorDepth::Ansi256
        } else {
            ColorDepth::Ansi16
        }
    }
}

/// A colour as red, green and blue.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgb(pub u8, pub u8, pub u8);

impl Rgb {
    /// Parses `#rrggbb` or `rrggbb`.
    pub fn parse(hex: &str) -> Option<Self> {
        let hex = hex.trim().trim_start_matches('#');
        if hex.len() != 6 || !hex.is_ascii() {
            return None;
        }
        let byte = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).ok();
        Some(Rgb(byte(0)?, byte(2)?, byte(4)?))
    }

    /// The closest colour the terminal can draw.
    pub fn to_color(self, depth: ColorDepth) -> Color {
        match depth {
            ColorDepth::TrueColor => Color::Rgb(self.0, self.1, self.2),
            ColorDepth::Ansi256 => Color::Indexed(self.to_ansi256()),
            ColorDepth::Ansi16 => self.to_ansi16(),
        }
    }

    /// The nearest colour of the xterm 6×6×6 cube or grey ramp.
    fn to_ansi256(self) -> u8 {
        const STEPS: [u8; 6] = [0, 95, 135, 175, 215, 255];
        let nearest_step = |v: u8| {
            (0..6)
                .min_by_key(|&i| (STEPS[i] as i32 - v as i32).abs())
                .unwrap_or(0)
        };
        let (r, g, b) = (
            nearest_step(self.0),
            nearest_step(self.1),
            nearest_step(self.2),
        );
        let cube = Rgb(STEPS[r], STEPS[g], STEPS[b]);
        let cube_index = 16 + 36 * r as u8 + 6 * g as u8 + b as u8;

        let average = (self.0 as u32 + self.1 as u32 + self.2 as u32) / 3;
        let grey_step = (average.saturating_sub(8) / 10).min(23) as u8;
        let grey_value = 8 + 10 * grey_step;
        let grey = Rgb(grey_value, grey_value, grey_value);

        if self.distance(grey) < self.distance(cube) {
            232 + grey_step
        } else {
            cube_index
        }
    }

    /// The nearest of the 16 standard colours, using xterm's values for them.
    fn to_ansi16(self) -> Color {
        const ANSI: [(Rgb, Color); 16] = [
            (Rgb(0, 0, 0), Color::Black),
            (Rgb(205, 0, 0), Color::Red),
            (Rgb(0, 205, 0), Color::Green),
            (Rgb(205, 205, 0), Color::Yellow),
            (Rgb(0, 0, 238), Color::Blue),
            (Rgb(205, 0, 205), Color::Magenta),
            (Rgb(0, 205, 205), Color::Cyan),
            (Rgb(229, 229, 229), Color::Gray),
            (Rgb(127, 127, 127), Color::DarkGray),
            (Rgb(255, 0, 0), Color::LightRed),
            (Rgb(0, 255, 0), Color::LightGreen),
            (Rgb(255, 255, 0), Color::LightYellow),
            (Rgb(92, 92, 255), Color::LightBlue),
            (Rgb(255, 0, 255), Color::LightMagenta),
            (Rgb(0, 255, 255), Color::LightCyan),
            (Rgb(255, 255, 255), Color::White),
        ];
        ANSI.iter()
            .min_by_key(|(rgb, _)| self.distance(*rgb))
            .map_or(Color::Reset, |(_, color)| *color)
    }

    fn distance(self, other: Rgb) -> u32 {
        let d = |a: u8, b: u8| (a as i32 - b as i32).unsigned_abs();
        let (r, g, b) = (d(self.0, other.0), d(self.1, other.1), d(self.2, other.2));
        // Weighted for how sensitive the eye is to each channel.
        2 * r * r + 4 * g * g + 3 * b * b
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_parses_with_or_without_hash() {
        assert_eq!(Rgb::parse("#ff9e3b"), Some(Rgb(255, 158, 59)));
        assert_eq!(Rgb::parse("1A1B26"), Some(Rgb(26, 27, 38)));
        assert_eq!(Rgb::parse("#fff"), None);
        assert_eq!(Rgb::parse("#gggggg"), None);
        assert_eq!(Rgb::parse("#ééé"), None);
    }

    #[test]
    fn colours_fall_back_to_what_the_terminal_has() {
        let orange = Rgb(255, 158, 59);
        assert_eq!(
            orange.to_color(ColorDepth::TrueColor),
            Color::Rgb(255, 158, 59)
        );
        assert_eq!(orange.to_color(ColorDepth::Ansi256), Color::Indexed(215));
        assert_eq!(orange.to_color(ColorDepth::Ansi16), Color::Yellow);

        let grey = Rgb(40, 40, 40);
        assert_eq!(grey.to_color(ColorDepth::Ansi256), Color::Indexed(235));
        assert_eq!(Rgb(0, 0, 0).to_color(ColorDepth::Ansi16), Color::Black);
        assert_eq!(
            Rgb(250, 250, 250).to_color(ColorDepth::Ansi16),
            Color::White
        );
    }

    #[test]
    fn depth_is_read_back_from_a_colour() {
        for depth in [
            ColorDepth::TrueColor,
            ColorDepth::Ansi256,
            ColorDepth::Ansi16,
        ] {
            assert_eq!(ColorDepth::of(Rgb(255, 158, 59).to_color(depth)), depth);
        }
    }
}
