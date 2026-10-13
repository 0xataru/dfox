//! `config.toml` in the dfox config directory: `$DFOX_CONFIG_DIR`, else
//! `$XDG_CONFIG_HOME/dfox` or `~/.config/dfox` (`%APPDATA%\dfox` on Windows).
//! User themes live in its `themes/` subdirectory.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use crate::{icons::IconStyle, keymap::Preset};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// A built-in theme, or the name of a file in `themes/` without `.toml`.
    pub theme: String,
    /// `standard` or `vim`.
    pub keymap: Preset,
    /// Spinners and fades. Off draws static symbols instead.
    pub animations: bool,
    /// `auto`, `nerd` or `ascii`.
    pub icons: IconStyle,
    /// Clicks and the wheel. While on, selecting text with the mouse needs
    /// Shift (Option in some macOS terminals).
    pub mouse: bool,
    /// Rows kept from a query's result; the rest are read and dropped.
    pub row_limit: usize,
    /// Replaces the keys of an action, for example `execute = ["f5", "ctrl+r"]`.
    pub keys: BTreeMap<String, Vec<String>>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            theme: "dfox".to_string(),
            keymap: Preset::Standard,
            animations: true,
            icons: IconStyle::Auto,
            mouse: true,
            row_limit: 100_000,
            keys: BTreeMap::new(),
        }
    }
}

impl Config {
    /// The dfox config directory, if one can be found.
    pub fn dir() -> Option<PathBuf> {
        if let Some(dir) = std::env::var_os("DFOX_CONFIG_DIR") {
            return Some(PathBuf::from(dir));
        }
        if cfg!(windows) {
            return directories::BaseDirs::new().map(|dirs| dirs.config_dir().join("dfox"));
        }
        let base = match std::env::var_os("XDG_CONFIG_HOME") {
            Some(dir) if !dir.is_empty() => PathBuf::from(dir),
            _ => directories::BaseDirs::new()?.home_dir().join(".config"),
        };
        Some(base.join("dfox"))
    }

    /// Reads `config.toml` from `dir`. A missing file gives the defaults; a
    /// broken one gives the defaults and a message saying what is wrong.
    pub fn load(dir: Option<&Path>) -> (Self, Option<String>) {
        let Some(path) = dir.map(|dir| dir.join("config.toml")) else {
            return (Self::default(), None);
        };
        match std::fs::read_to_string(&path) {
            Ok(text) => match toml::from_str(&text) {
                Ok(config) => (config, None),
                Err(err) => (
                    Self::default(),
                    Some(format!("{}: {}", path.display(), err.message())),
                ),
            },
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => (Self::default(), None),
            Err(err) => (
                Self::default(),
                Some(format!("can't read {}: {err}", path.display())),
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config() {
        let text = toml::to_string(&Config::default()).expect("serialize");
        insta::assert_snapshot!(text);
        assert_eq!(
            toml::from_str::<Config>(&text).expect("parse"),
            Config::default()
        );
    }

    #[test]
    fn missing_settings_keep_their_defaults() {
        let config: Config =
            toml::from_str("keymap = \"vim\"\n[keys]\nexecute = [\"ctrl+r\"]").expect("parse");
        assert_eq!(config.keymap, Preset::Vim);
        assert_eq!(config.keys["execute"], ["ctrl+r"]);
        assert_eq!(config.theme, "dfox");
    }

    #[test]
    fn a_broken_file_falls_back_to_the_defaults() {
        let dir = std::env::temp_dir().join(format!("dfox-config-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("dir");
        std::fs::write(dir.join("config.toml"), "animation = false").expect("write");

        let (config, problem) = Config::load(Some(&dir));
        assert_eq!(config, Config::default());
        let problem = problem.expect("a problem");
        assert!(problem.contains("unknown field `animation`"), "{problem}");

        assert_eq!(
            Config::load(Some(&dir.join("missing"))),
            (Config::default(), None)
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
