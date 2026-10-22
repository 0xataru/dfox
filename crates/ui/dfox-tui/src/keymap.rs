//! Turns key presses into [`Action`]s, from a preset plus overrides from the
//! config file.

use std::collections::{BTreeMap, HashMap};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use serde::{Deserialize, Serialize};

use crate::action::Action;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Preset {
    #[default]
    Standard,
    /// Standard, plus `hjkl`, `g`/`G` and `Ctrl+D`/`Ctrl+U` outside text fields.
    Vim,
}

/// A key with its modifiers. Shift is part of the character for plain
/// character keys, so it is dropped from them; with Ctrl or Alt it is kept
/// and the letter is lower-cased, so `ctrl+shift+z` matches however the
/// terminal reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Key {
    pub code: KeyCode,
    pub modifiers: KeyModifiers,
}

impl Key {
    pub fn new(code: KeyCode, modifiers: KeyModifiers) -> Self {
        let chord = modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT);
        match code {
            KeyCode::Char(c) if chord && c.is_ascii_uppercase() => Self {
                code: KeyCode::Char(c.to_ascii_lowercase()),
                modifiers: modifiers | KeyModifiers::SHIFT,
            },
            KeyCode::Char(_) if !chord => Self {
                code,
                modifiers: modifiers - KeyModifiers::SHIFT,
            },
            KeyCode::BackTab => Self {
                code,
                modifiers: modifiers - KeyModifiers::SHIFT,
            },
            _ => Self { code, modifiers },
        }
    }

    /// Parses `ctrl+e`, `f5`, `shift+tab`, `G`, `?`, `space`, `pagedown`, ….
    pub fn parse(text: &str) -> Option<Key> {
        let mut modifiers = KeyModifiers::NONE;
        let mut parts: Vec<&str> = text.split('+').collect();
        // `ctrl++` or `+`: the key itself is a plus sign.
        if text.ends_with('+') {
            parts.retain(|p| !p.is_empty());
            parts.push("+");
        }
        let key = parts.pop()?;
        for modifier in parts {
            modifiers |= match modifier.to_lowercase().as_str() {
                "ctrl" | "control" => KeyModifiers::CONTROL,
                "alt" => KeyModifiers::ALT,
                "shift" => KeyModifiers::SHIFT,
                _ => return None,
            };
        }
        let lower = key.to_lowercase();
        let code = match lower.as_str() {
            "enter" | "return" => KeyCode::Enter,
            "esc" | "escape" => KeyCode::Esc,
            "tab" if modifiers.contains(KeyModifiers::SHIFT) => KeyCode::BackTab,
            "tab" => KeyCode::Tab,
            "backtab" => KeyCode::BackTab,
            "backspace" => KeyCode::Backspace,
            "delete" | "del" => KeyCode::Delete,
            "space" => KeyCode::Char(' '),
            "up" => KeyCode::Up,
            "down" => KeyCode::Down,
            "left" => KeyCode::Left,
            "right" => KeyCode::Right,
            "pageup" | "pgup" => KeyCode::PageUp,
            "pagedown" | "pgdn" => KeyCode::PageDown,
            "home" => KeyCode::Home,
            "end" => KeyCode::End,
            f if f.len() > 1 && f.starts_with('f') => KeyCode::F(f[1..].parse().ok()?),
            _ => {
                let mut chars = key.chars();
                let c = chars.next()?;
                if chars.next().is_some() {
                    return None;
                }
                KeyCode::Char(c)
            }
        };
        Some(Key::new(code, modifiers))
    }

    /// How the key is written in help: `Ctrl+E`, `F5`, `Shift+Tab`, `G`.
    pub fn label(&self) -> String {
        let mut label = String::new();
        if self.modifiers.contains(KeyModifiers::CONTROL) {
            label.push_str("Ctrl+");
        }
        if self.modifiers.contains(KeyModifiers::ALT) {
            label.push_str("Alt+");
        }
        if self.modifiers.contains(KeyModifiers::SHIFT) {
            label.push_str("Shift+");
        }
        let key = match self.code {
            KeyCode::Char(' ') => "Space".to_string(),
            KeyCode::Char(c) if !self.modifiers.is_empty() => c.to_ascii_uppercase().to_string(),
            KeyCode::Char(c) => c.to_string(),
            KeyCode::F(n) => format!("F{n}"),
            KeyCode::BackTab => "Shift+Tab".to_string(),
            KeyCode::PageUp => "PgUp".to_string(),
            KeyCode::PageDown => "PgDn".to_string(),
            KeyCode::Up => "↑".to_string(),
            KeyCode::Down => "↓".to_string(),
            KeyCode::Left => "←".to_string(),
            KeyCode::Right => "→".to_string(),
            other => format!("{other:?}"),
        };
        label.push_str(&key);
        label
    }
}

#[derive(Debug, Clone)]
pub struct Keymap {
    bindings: HashMap<Key, Action>,
}

impl Keymap {
    /// The preset's bindings with `overrides` applied. An override replaces
    /// every key of its action. Returns the keymap and a message per override
    /// that couldn't be used.
    pub fn new(preset: Preset, overrides: &BTreeMap<String, Vec<String>>) -> (Self, Vec<String>) {
        let mut bindings = HashMap::new();
        for (key, action) in standard().into_iter().chain(match preset {
            Preset::Standard => Vec::new(),
            Preset::Vim => vim(),
        }) {
            bindings.insert(key, action);
        }

        let mut problems = Vec::new();
        for (name, keys) in overrides {
            let Some(action) = Action::from_name(name) else {
                problems.push(format!("[keys]: unknown action `{name}`"));
                continue;
            };
            let parsed: Vec<Key> = keys
                .iter()
                .filter_map(|text| {
                    let key = Key::parse(text);
                    if key.is_none() {
                        problems.push(format!("[keys] {name}: can't read key {text:?}"));
                    }
                    key
                })
                .collect();
            bindings.retain(|_, bound| *bound != action);
            for key in parsed {
                bindings.insert(key, action);
            }
        }
        (Self { bindings }, problems)
    }

    pub fn action(&self, event: KeyEvent) -> Option<Action> {
        self.bindings
            .get(&Key::new(event.code, event.modifiers))
            .copied()
    }

    /// The keys bound to `action`, as labels, keys with fewer modifiers
    /// first.
    pub fn keys_for(&self, action: Action) -> Vec<String> {
        let mut keys: Vec<&Key> = self
            .bindings
            .iter()
            .filter(|(_, bound)| **bound == action)
            .map(|(key, _)| key)
            .collect();
        keys.sort_by_key(|key| (key.modifiers.bits().count_ones(), key.label()));
        keys.into_iter().map(Key::label).collect()
    }

    /// The first key of `action`, for short hints.
    pub fn hint(&self, action: Action) -> String {
        self.keys_for(action).into_iter().next().unwrap_or_default()
    }
}

fn bind(keys: &[&str], action: Action) -> Vec<(Key, Action)> {
    keys.iter()
        .filter_map(|text| Key::parse(text))
        .map(|key| (key, action))
        .collect()
}

fn standard() -> Vec<(Key, Action)> {
    [
        bind(&["ctrl+q", "q"], Action::Quit),
        bind(&["?", "f2"], Action::Help),
        bind(&["space"], Action::Menu),
        bind(&["esc"], Action::Back),
        bind(&["up"], Action::Up),
        bind(&["down"], Action::Down),
        bind(&["left"], Action::Left),
        bind(&["right"], Action::Right),
        bind(&["pageup"], Action::PageUp),
        bind(&["pagedown"], Action::PageDown),
        bind(&["home"], Action::Home),
        bind(&["end"], Action::End),
        bind(&["enter"], Action::Confirm),
        bind(&["tab"], Action::NextFocus),
        bind(&["shift+tab"], Action::PrevFocus),
        bind(&["backspace"], Action::Backspace),
        bind(&["f5", "ctrl+e", "ctrl+enter"], Action::Execute),
        bind(
            &["shift+f5", "ctrl+shift+enter", "ctrl+shift+e"],
            Action::ExecuteAll,
        ),
        bind(&["ctrl+r"], Action::History),
        bind(&["ctrl+n"], Action::NewTab),
        bind(&["ctrl+w"], Action::CloseTab),
        bind(&["ctrl+pagedown", "alt+]"], Action::NextTab),
        bind(&["ctrl+pageup", "alt+["], Action::PrevTab),
        bind(&["ctrl+c"], Action::Copy),
        bind(&["y"], Action::CopyAs),
        bind(&["ctrl+x"], Action::Cut),
        bind(&["ctrl+v"], Action::Paste),
        bind(&["ctrl+a"], Action::SelectAll),
        bind(&["V"], Action::SelectRow),
        bind(&["ctrl+z"], Action::Undo),
        bind(&["ctrl+y", "ctrl+shift+z"], Action::Redo),
        bind(&["ctrl+p"], Action::Palette),
        bind(&["ctrl+space"], Action::Complete),
        bind(&["i"], Action::Inspect),
        bind(&["J"], Action::JsonView),
        bind(&["o"], Action::AddRow),
        bind(&["x"], Action::Export),
        bind(&[">"], Action::Wider),
        bind(&["<"], Action::Narrower),
        bind(&["ctrl+left", "alt+left", "alt+b"], Action::WordLeft),
        bind(&["ctrl+right", "alt+right", "alt+f"], Action::WordRight),
        bind(&["shift+left"], Action::SelectLeft),
        bind(&["shift+right"], Action::SelectRight),
        bind(&["shift+up"], Action::SelectUp),
        bind(&["shift+down"], Action::SelectDown),
        bind(&["shift+home"], Action::SelectHome),
        bind(&["shift+end"], Action::SelectEnd),
        bind(&["alt+backspace", "ctrl+backspace"], Action::DeleteWord),
        bind(&["f1"], Action::Connections),
        bind(&["n"], Action::New),
        bind(&["e"], Action::Edit),
        bind(&["d", "delete"], Action::Delete),
        bind(&["ctrl+s"], Action::Save),
        bind(&["/"], Action::Filter),
        bind(&["R"], Action::Refresh),
        bind(&["f12"], Action::Debug),
        bind(&["ctrl+t"], Action::NextTheme),
    ]
    .concat()
}

fn vim() -> Vec<(Key, Action)> {
    [
        bind(&["j"], Action::Down),
        bind(&["k"], Action::Up),
        bind(&["h"], Action::Left),
        bind(&["l"], Action::Right),
        bind(&["g"], Action::Home),
        bind(&["G"], Action::End),
        bind(&["ctrl+d"], Action::PageDown),
        bind(&["ctrl+u"], Action::PageUp),
    ]
    .concat()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn press(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, modifiers)
    }

    #[test]
    fn keys_parse_and_label() {
        let cases = [
            ("ctrl+e", "Ctrl+E"),
            ("F5", "F5"),
            ("shift+tab", "Shift+Tab"),
            ("G", "G"),
            ("?", "?"),
            ("space", "Space"),
            ("pagedown", "PgDn"),
            ("ctrl++", "Ctrl++"),
            ("ctrl+shift+z", "Ctrl+Shift+Z"),
            ("ctrl+enter", "Ctrl+Enter"),
        ];
        for (text, label) in cases {
            let key = Key::parse(text).unwrap_or_else(|| panic!("{text} didn't parse"));
            assert_eq!(key.label(), label);
        }
        assert_eq!(Key::parse("hyper+x"), None);
        assert_eq!(Key::parse("ab"), None);
    }

    #[test]
    fn shift_is_part_of_the_character() {
        let (keymap, _) = Keymap::new(Preset::Vim, &BTreeMap::new());
        assert_eq!(
            keymap.action(press(KeyCode::Char('G'), KeyModifiers::SHIFT)),
            Some(Action::End)
        );
        assert_eq!(
            keymap.action(press(KeyCode::BackTab, KeyModifiers::SHIFT)),
            Some(Action::PrevFocus)
        );
    }

    #[test]
    fn shift_counts_in_chords_however_it_is_reported() {
        let (keymap, _) = Keymap::new(Preset::Standard, &BTreeMap::new());
        let ctrl_shift = KeyModifiers::CONTROL | KeyModifiers::SHIFT;
        for code in [KeyCode::Char('z'), KeyCode::Char('Z')] {
            assert_eq!(keymap.action(press(code, ctrl_shift)), Some(Action::Redo));
        }
        assert_eq!(
            keymap.action(press(KeyCode::Char('z'), KeyModifiers::CONTROL)),
            Some(Action::Undo)
        );
        assert_eq!(
            keymap.action(press(KeyCode::Enter, ctrl_shift)),
            Some(Action::ExecuteAll)
        );
    }

    #[test]
    fn presets_differ_only_where_they_should() {
        let (standard, _) = Keymap::new(Preset::Standard, &BTreeMap::new());
        let (vim, _) = Keymap::new(Preset::Vim, &BTreeMap::new());
        let j = press(KeyCode::Char('j'), KeyModifiers::NONE);
        assert_eq!(standard.action(j), None);
        assert_eq!(vim.action(j), Some(Action::Down));
        let f5 = press(KeyCode::F(5), KeyModifiers::NONE);
        assert_eq!(standard.action(f5), Some(Action::Execute));
        assert_eq!(vim.action(f5), Some(Action::Execute));
    }

    #[test]
    fn overrides_replace_an_actions_keys() {
        let overrides = BTreeMap::from([
            ("execute".to_string(), vec!["ctrl+r".to_string()]),
            ("nope".to_string(), vec!["x".to_string()]),
            ("help".to_string(), vec!["hyper+h".to_string()]),
        ]);
        let (keymap, problems) = Keymap::new(Preset::Standard, &overrides);

        assert_eq!(
            keymap.action(press(KeyCode::Char('r'), KeyModifiers::CONTROL)),
            Some(Action::Execute)
        );
        assert_eq!(
            keymap.action(press(KeyCode::F(5), KeyModifiers::NONE)),
            None
        );
        assert_eq!(keymap.keys_for(Action::Execute), ["Ctrl+R"]);
        assert_eq!(
            problems,
            [
                "[keys] help: can't read key \"hyper+h\"",
                "[keys]: unknown action `nope`"
            ]
        );
    }

    #[test]
    fn plain_keys_are_listed_first() {
        let (keymap, _) = Keymap::new(Preset::Standard, &BTreeMap::new());
        assert_eq!(
            keymap.keys_for(Action::Execute),
            ["F5", "Ctrl+E", "Ctrl+Enter"]
        );
        assert_eq!(keymap.hint(Action::Help), "?");
    }
}
