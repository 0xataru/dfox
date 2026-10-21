//! Copying and pasting. Uses the system clipboard, and falls back to the
//! terminal's (OSC 52) where there is none, such as over SSH.

use std::io::Write;

/// How copied text reached a clipboard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Via {
    System,
    /// An OSC 52 sequence: the terminal decides whether to accept it.
    Terminal,
}

pub trait Clipboard {
    fn set(&mut self, text: String) -> Result<Via, String>;
    fn get(&mut self) -> Option<String>;
}

/// The system clipboard. It is kept open for the whole run: on X11 the copied
/// text is gone once the clipboard that set it is closed.
#[derive(Default)]
pub struct System {
    inner: Option<arboard::Clipboard>,
}

impl System {
    fn open(&mut self) -> Option<&mut arboard::Clipboard> {
        if self.inner.is_none() {
            self.inner = arboard::Clipboard::new().ok();
        }
        self.inner.as_mut()
    }
}

impl Clipboard for System {
    fn set(&mut self, text: String) -> Result<Via, String> {
        if let Some(clipboard) = self.open() {
            if clipboard.set_text(text.clone()).is_ok() {
                return Ok(Via::System);
            }
        }
        let mut out = std::io::stdout().lock();
        out.write_all(osc52(&text).as_bytes())
            .and_then(|()| out.flush())
            .map(|()| Via::Terminal)
            .map_err(|err| err.to_string())
    }

    fn get(&mut self) -> Option<String> {
        self.open()?.get_text().ok()
    }
}

/// A clipboard in memory, for tests.
#[cfg(test)]
#[derive(Debug, Default)]
pub struct Memory {
    pub text: Option<String>,
}

#[cfg(test)]
impl Clipboard for Memory {
    fn set(&mut self, text: String) -> Result<Via, String> {
        self.text = Some(text);
        Ok(Via::System)
    }

    fn get(&mut self) -> Option<String> {
        self.text.clone()
    }
}

/// The escape sequence that asks the terminal to put `text` on the clipboard.
pub fn osc52(text: &str) -> String {
    format!("\x1b]52;c;{}\x07", base64(text.as_bytes()))
}

fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |n, (i, b)| n | (u32::from(*b) << (16 - 8 * i)));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(ALPHABET[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_the_rfc_examples() {
        let cases = [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
        ];
        for (text, encoded) in cases {
            assert_eq!(base64(text.as_bytes()), encoded);
        }
    }

    #[test]
    fn osc52_wraps_the_text() {
        assert_eq!(osc52("hi"), "\x1b]52;c;aGk=\x07");
    }

    #[test]
    fn memory_clipboard_round_trips() {
        let mut clipboard = Memory::default();
        assert_eq!(clipboard.set("x".into()), Ok(Via::System));
        assert_eq!(clipboard.get().as_deref(), Some("x"));
    }
}
