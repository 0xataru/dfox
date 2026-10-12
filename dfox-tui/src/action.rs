//! What a key press means, independent of which key it was.

/// A user intent. The keymap turns key presses into these, and components
/// handle them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Action {
    Quit,
    Help,
    Menu,
    Back,
    Up,
    Down,
    Left,
    Right,
    PageUp,
    PageDown,
    Home,
    End,
    Confirm,
    NextFocus,
    PrevFocus,
    Backspace,
    Execute,
    CopyRow,
    CopyAll,
    Connections,
    Debug,
    NextTheme,
    New,
    Edit,
    Delete,
    Save,
    Filter,
    Refresh,
    /// A character typed into a text field. Not bindable.
    Input(char),
}

impl Action {
    /// Every bindable action, in the order the help screen lists them.
    pub const BINDABLE: &'static [Action] = &[
        Action::Execute,
        Action::CopyRow,
        Action::CopyAll,
        Action::NextFocus,
        Action::PrevFocus,
        Action::Filter,
        Action::Refresh,
        Action::New,
        Action::Edit,
        Action::Delete,
        Action::Save,
        Action::Connections,
        Action::NextTheme,
        Action::Debug,
        Action::Menu,
        Action::Help,
        Action::Back,
        Action::Quit,
        Action::Confirm,
        Action::Up,
        Action::Down,
        Action::Left,
        Action::Right,
        Action::PageUp,
        Action::PageDown,
        Action::Home,
        Action::End,
        Action::Backspace,
    ];

    /// The name used for this action in `[keys]` in the config file.
    pub fn name(self) -> &'static str {
        match self {
            Action::Quit => "quit",
            Action::Help => "help",
            Action::Menu => "menu",
            Action::Back => "back",
            Action::Up => "up",
            Action::Down => "down",
            Action::Left => "left",
            Action::Right => "right",
            Action::PageUp => "page_up",
            Action::PageDown => "page_down",
            Action::Home => "home",
            Action::End => "end",
            Action::Confirm => "confirm",
            Action::NextFocus => "next_focus",
            Action::PrevFocus => "prev_focus",
            Action::Backspace => "backspace",
            Action::Execute => "execute",
            Action::CopyRow => "copy_row",
            Action::CopyAll => "copy_all",
            Action::Connections => "connections",
            Action::New => "new",
            Action::Edit => "edit",
            Action::Delete => "delete",
            Action::Save => "save",
            Action::Filter => "filter",
            Action::Refresh => "refresh",
            Action::Debug => "debug",
            Action::NextTheme => "next_theme",
            Action::Input(_) => "input",
        }
    }

    pub fn from_name(name: &str) -> Option<Action> {
        Action::BINDABLE.iter().copied().find(|a| a.name() == name)
    }

    /// What the action does, for the help screen.
    pub fn description(self) -> &'static str {
        match self {
            Action::Quit => "Quit",
            Action::Help => "Show keys",
            Action::Menu => "Command menu",
            Action::Back => "Back / close",
            Action::Up => "Up",
            Action::Down => "Down",
            Action::Left => "Left / previous column",
            Action::Right => "Right / next column",
            Action::PageUp => "Page up",
            Action::PageDown => "Page down",
            Action::Home => "First row / line start",
            Action::End => "Last row / line end",
            Action::Confirm => "Select / new line",
            Action::NextFocus => "Next panel",
            Action::PrevFocus => "Previous panel",
            Action::Backspace => "Delete a character",
            Action::Execute => "Run the query",
            Action::CopyRow => "Copy row / cancel query",
            Action::CopyAll => "Copy all rows",
            Action::Connections => "Connection list",
            Action::New => "New connection",
            Action::Edit => "Edit connection",
            Action::Delete => "Delete connection",
            Action::Save => "Save",
            Action::Filter => "Filter",
            Action::Refresh => "Refresh",
            Action::Debug => "Event log",
            Action::NextTheme => "Next theme",
            Action::Input(_) => "Type",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn descriptions_fit_the_help_columns() {
        for action in Action::BINDABLE {
            assert!(action.description().len() <= 24, "{action:?}");
        }
    }

    #[test]
    fn names_round_trip() {
        for action in Action::BINDABLE {
            assert_eq!(Action::from_name(action.name()), Some(*action));
        }
        assert_eq!(Action::from_name("input"), None);
    }
}
