use std::io;

use crossterm::{
    event::DisableMouseCapture, 
    execute, 
    terminal::{
        disable_raw_mode, 
        Clear, 
        ClearType, 
        LeaveAlternateScreen,
    },
};

pub struct TerminalGuard;

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        if let Err(e) = disable_raw_mode() {
            log::error!("Error disabling raw mode: {}", e);
        }
        let mut stdout = io::stdout();
        if let Err(e) = execute!(
            stdout,
            LeaveAlternateScreen,
            DisableMouseCapture,
            Clear(ClearType::All)
        ) {
            log::error!("Error cleaning up terminal: {}", e);
        }
    }
}