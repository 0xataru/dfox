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
    /// Ask before running `DELETE` or `UPDATE` without `WHERE`, `DROP`,
    /// `TRUNCATE` and `ALTER … DROP`.
    pub confirm_destructive: bool,
    /// Replaces the keys of an action, for example `execute = ["f5", "ctrl+r"]`.
    pub keys: BTreeMap<String, Vec<String>>,
    /// Ask GitHub at each start whether a newer dfox is out.
    pub update_check: bool,
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
            confirm_destructive: true,
            keys: BTreeMap::new(),
            update_check: true,
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

    /// Sets `action`'s keys in `dir`'s `config.toml`, or with `None` drops
    /// them so the defaults apply. The rest of the file, comments included,
    /// stays as it is.
    pub fn save_keys(dir: &Path, action: &str, keys: Option<&[String]>) -> Result<(), String> {
        let path = dir.join("config.toml");
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(err) => return Err(format!("can't read {}: {err}", path.display())),
        };
        let mut doc: toml_edit::DocumentMut =
            text.parse().map_err(|err: toml_edit::TomlError| {
                format!("{}: {}", path.display(), err.message())
            })?;
        let table = doc
            .entry("keys")
            .or_insert(toml_edit::table())
            .as_table_like_mut()
            .ok_or_else(|| format!("{}: `keys` isn't a table", path.display()))?;
        match keys {
            Some(keys) => {
                let keys: toml_edit::Array = keys.iter().map(String::as_str).collect();
                table.insert(action, toml_edit::value(keys));
            }
            None => {
                table.remove(action);
            }
        }
        // Written aside, then moved over, so a failed write never leaves half a file.
        let written = path.with_extension("toml.new");
        std::fs::create_dir_all(dir)
            .and_then(|()| std::fs::write(&written, doc.to_string()))
            .and_then(|()| std::fs::rename(&written, &path))
            .map_err(|err| format!("can't write {}: {err}", path.display()))
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
    fn keys_are_saved_and_dropped_keeping_the_rest() {
        let dir = std::env::temp_dir().join(format!("dfox-keys-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        Config::save_keys(&dir, "execute", Some(&["f6".into()])).expect("into no file");
        assert_eq!(Config::load(Some(&dir)).0.keys["execute"], ["f6"]);

        let path = dir.join("config.toml");
        std::fs::write(&path, "# mine\ntheme = \"nord\" # dark\n").expect("write");
        Config::save_keys(&dir, "execute", Some(&["f6".into(), "ctrl+r".into()])).expect("save");
        Config::save_keys(&dir, "history", Some(&[])).expect("save");
        let text = std::fs::read_to_string(&path).expect("read");
        assert!(
            text.starts_with("# mine\ntheme = \"nord\" # dark\n"),
            "{text}"
        );
        let config = Config::load(Some(&dir)).0;
        assert_eq!(config.keys["execute"], ["f6", "ctrl+r"]);
        assert!(config.keys["history"].is_empty());

        Config::save_keys(&dir, "execute", None).expect("drop");
        let config = Config::load(Some(&dir)).0;
        assert!(!config.keys.contains_key("execute"));
        assert_eq!(config.theme, "nord");

        // A file it can't read is left alone.
        std::fs::write(&path, "theme = ").expect("write");
        assert!(Config::save_keys(&dir, "execute", None).is_err());
        assert_eq!(std::fs::read_to_string(&path).expect("read"), "theme = ");
        let _ = std::fs::remove_dir_all(&dir);
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
