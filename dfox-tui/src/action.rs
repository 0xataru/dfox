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
    ExecuteAll,
    Copy,
    CopyAs,
    SelectAll,
    Cut,
    Paste,
    Undo,
    Redo,
    WordLeft,
    WordRight,
    SelectLeft,
    SelectRight,
    SelectUp,
    SelectDown,
    SelectHome,
    SelectEnd,
    SelectRow,
    DeleteWord,
    NewTab,
    CloseTab,
    NextTab,
    PrevTab,
    History,
    Wider,
    Narrower,
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
        Action::ExecuteAll,
        Action::History,
        Action::NewTab,
        Action::CloseTab,
        Action::NextTab,
        Action::PrevTab,
        Action::Copy,
        Action::CopyAs,
        Action::Cut,
        Action::Paste,
        Action::SelectAll,
        Action::SelectRow,
        Action::Undo,
        Action::Redo,
        Action::Wider,
        Action::Narrower,
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
        Action::DeleteWord,
        Action::WordLeft,
        Action::WordRight,
        Action::SelectLeft,
        Action::SelectRight,
        Action::SelectUp,
        Action::SelectDown,
        Action::SelectHome,
        Action::SelectEnd,
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
            Action::ExecuteAll => "execute_all",
            Action::Copy => "copy",
            Action::CopyAs => "copy_as",
            Action::SelectAll => "select_all",
            Action::Cut => "cut",
            Action::Paste => "paste",
            Action::Undo => "undo",
            Action::Redo => "redo",
            Action::WordLeft => "word_left",
            Action::WordRight => "word_right",
            Action::SelectLeft => "select_left",
            Action::SelectRight => "select_right",
            Action::SelectUp => "select_up",
            Action::SelectDown => "select_down",
            Action::SelectHome => "select_home",
            Action::SelectEnd => "select_end",
            Action::SelectRow => "select_row",
            Action::DeleteWord => "delete_word",
            Action::NewTab => "new_tab",
            Action::CloseTab => "close_tab",
            Action::NextTab => "next_tab",
            Action::PrevTab => "prev_tab",
            Action::History => "history",
            Action::Wider => "wider",
            Action::Narrower => "narrower",
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
        // Names from before the grid had a selection.
        let name = match name {
            "copy_row" => "copy",
            "copy_all" => "select_all",
            name => name,
        };
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
            Action::Backspace => "Delete back",
            Action::Execute => "Run statement/selection",
            Action::ExecuteAll => "Run the whole script",
            Action::Copy => "Copy / cancel query",
            Action::CopyAs => "Copy as CSV, JSON, …",
            Action::SelectAll => "Select all",
            Action::Cut => "Cut",
            Action::Paste => "Paste",
            Action::Undo => "Undo",
            Action::Redo => "Redo",
            Action::WordLeft => "Previous word",
            Action::WordRight => "Next word",
            Action::SelectLeft => "Select left",
            Action::SelectRight => "Select right",
            Action::SelectUp => "Select up",
            Action::SelectDown => "Select down",
            Action::SelectHome => "Select to line start",
            Action::SelectEnd => "Select to line end",
            Action::SelectRow => "Select whole rows",
            Action::DeleteWord => "Delete a word",
            Action::NewTab => "New query tab",
            Action::CloseTab => "Close query tab",
            Action::NextTab => "Next query tab",
            Action::PrevTab => "Previous query tab",
            Action::History => "Query history",
            Action::Wider => "Widen column",
            Action::Narrower => "Narrow column",
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
        assert_eq!(Action::from_name("copy_row"), Some(Action::Copy));
        assert_eq!(Action::from_name("copy_all"), Some(Action::SelectAll));
    }
}
