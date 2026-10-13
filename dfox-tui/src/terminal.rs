//! Taking over the terminal and giving it back, also after a panic.

use std::{
    io::{self, stdout},
    sync::atomic::{AtomicBool, Ordering},
};

use crossterm::{
    event::{
        DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
        KeyboardEnhancementFlags, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
    },
    execute,
    terminal::supports_keyboard_enhancement,
};
use ratatui::DefaultTerminal;

static MOUSE: AtomicBool = AtomicBool::new(false);
static ENHANCED: AtomicBool = AtomicBool::new(false);

/// Enters the alternate screen and raw mode, with bracketed paste, the mouse
/// when `mouse` is set, and keys like `Ctrl+Enter` where the terminal can
/// tell them apart.
pub fn init(mouse: bool) -> io::Result<DefaultTerminal> {
    // Restores the screen and raw mode on panic.
    let terminal = ratatui::try_init()?;
    let enhanced = matches!(supports_keyboard_enhancement(), Ok(true));
    execute!(stdout(), EnableBracketedPaste)?;
    if mouse {
        execute!(stdout(), EnableMouseCapture)?;
    }
    if enhanced {
        execute!(
            stdout(),
            PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
        )?;
    }
    MOUSE.store(mouse, Ordering::Relaxed);
    ENHANCED.store(enhanced, Ordering::Relaxed);

    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        leave();
        hook(info);
    }));
    Ok(terminal)
}

pub fn restore() {
    leave();
    ratatui::restore();
}

/// Turns off what [`init`] turned on, besides what ratatui restores.
fn leave() {
    let mut out = stdout();
    if ENHANCED.swap(false, Ordering::Relaxed) {
        let _ = execute!(out, PopKeyboardEnhancementFlags);
    }
    if MOUSE.swap(false, Ordering::Relaxed) {
        let _ = execute!(out, DisableMouseCapture);
    }
    let _ = execute!(out, DisableBracketedPaste);
}
