//! The Screen tab's "send keys" mode: PC/Mac key presses held on the MEGA65's
//! keyboard matrix through the virtual keyboard registers.

use egui::Key;
use mega65::machine::{matrix_code, KEY_LSHIFT, KEY_RSHIFT};

/// Matrix keys for one PC key. `shift` is whether the PC's shift is held; it is
/// passed on for letters, digits and keys like cursor/F-keys, while symbols use
/// whatever the MEGA65 needs to type the same character.
pub fn map_key(key: Key, shift: bool) -> Option<Vec<u8>> {
    let sh = |codes: &[u8]| {
        let mut v = codes.to_vec();
        if shift {
            v.insert(0, KEY_LSHIFT);
        }
        Some(v)
    };
    let ch = |c: u8| matrix_code(c).map(|(code, s)| if s { vec![KEY_LSHIFT, code] } else { vec![code] });
    match key {
        Key::ArrowDown => sh(&[0x07]),
        Key::ArrowRight => sh(&[0x02]),
        // The C65 matrix has no up/left: they are right shift + down/right.
        Key::ArrowUp => Some(vec![KEY_RSHIFT, 0x07]),
        Key::ArrowLeft => Some(vec![KEY_RSHIFT, 0x02]),
        Key::Enter => sh(&[0x01]),
        Key::Space => sh(&[0x3c]),
        Key::Tab => sh(&[0x41]),
        Key::Backspace | Key::Delete => sh(&[0x00]),
        Key::Insert => Some(vec![KEY_LSHIFT, 0x00]),
        Key::Escape => sh(&[0x3f]), // RUN/STOP
        Key::Home => sh(&[0x33]),
        Key::F1 => sh(&[0x04]),
        Key::F3 => sh(&[0x05]),
        Key::F5 => sh(&[0x06]),
        Key::F7 => sh(&[0x03]),
        Key::F9 => sh(&[0x44]),
        Key::F11 => sh(&[0x45]),
        Key::F13 => sh(&[0x46]),
        Key::F2 => Some(vec![KEY_LSHIFT, 0x04]),
        Key::F4 => Some(vec![KEY_LSHIFT, 0x05]),
        Key::F6 => Some(vec![KEY_LSHIFT, 0x06]),
        Key::F8 => Some(vec![KEY_LSHIFT, 0x03]),
        Key::F10 => Some(vec![KEY_LSHIFT, 0x44]),
        Key::F12 => Some(vec![KEY_LSHIFT, 0x45]),
        Key::F14 => Some(vec![KEY_LSHIFT, 0x46]),
        _ => {
            let name = key.name();
            if name.len() == 1 && name.as_bytes()[0].is_ascii_alphanumeric() {
                // Letters and digits: same position, PC shift passed on.
                let (code, _) = matrix_code(name.as_bytes()[0].to_ascii_lowercase())?;
                return sh(&[code]);
            }
            let c = match (key, shift) {
                (Key::Comma, true) => b'<',
                (Key::Period, true) => b'>',
                (Key::Slash, true) | (Key::Questionmark, _) => b'?',
                (Key::Semicolon, true) | (Key::Colon, _) => b':',
                (Key::Comma, _) => b',',
                (Key::Period, _) => b'.',
                (Key::Slash, _) => b'/',
                (Key::Semicolon, _) => b';',
                (Key::Minus, _) => b'-',
                (Key::Plus, _) => b'+',
                (Key::Equals, true) => b'+',
                (Key::Equals, _) => b'=',
                (Key::Quote, true) => b'"',
                (Key::Quote, _) => b'\'',
                (Key::OpenBracket, _) => b'[',
                (Key::CloseBracket, _) => b']',
                (Key::Backslash, _) => b'}', // pound
                (Key::Backtick, _) => b'_', // left arrow
                _ => return None,
            };
            ch(c)
        }
    }
}

/// PC keys currently held, by the key that was pressed, and the matrix keys each holds.
#[derive(Default)]
pub struct KeyState {
    held: Vec<(Key, Vec<u8>)>,
    sent: Vec<u8>,
}

impl KeyState {
    /// Feed one egui event. Returns true if it was a key event this mode handles
    /// (so the caller can keep it from the rest of the UI).
    pub fn event(&mut self, e: &egui::Event) -> bool {
        let egui::Event::Key { key, physical_key, pressed, repeat, modifiers } = e else {
            return false;
        };
        // ⌘/Ctrl shortcuts stay with Studio.
        if modifiers.command || modifiers.ctrl || modifiers.mac_cmd {
            return false;
        }
        let id = physical_key.unwrap_or(*key);
        if !pressed {
            let before = self.held.len();
            self.held.retain(|(k, _)| *k != id);
            return before != self.held.len() || map_key(*key, modifiers.shift).is_some();
        }
        let Some(codes) = map_key(*key, modifiers.shift) else { return false };
        if !repeat && !self.held.iter().any(|(k, _)| *k == id) {
            self.held.push((id, codes));
        }
        true
    }

    pub fn release_all(&mut self) {
        self.held.clear();
    }

    pub fn is_empty(&self) -> bool {
        self.held.is_empty() && self.sent.is_empty()
    }

    /// The matrix keys to hold now (shifts first, at most three), if that differs
    /// from what was last returned.
    pub fn changed(&mut self) -> Option<Vec<u8>> {
        let mut keys: Vec<u8> = Vec::new();
        let all = self.held.iter().flat_map(|(_, c)| c.iter().copied());
        let (shifts, others): (Vec<u8>, Vec<u8>) = all.partition(|&k| k == KEY_LSHIFT || k == KEY_RSHIFT);
        for k in shifts.into_iter().chain(others) {
            if !keys.contains(&k) && keys.len() < 3 {
                keys.push(k);
            }
        }
        if keys == self.sent {
            return None;
        }
        self.sent = keys.clone();
        Some(keys)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mapping() {
        assert_eq!(map_key(Key::ArrowUp, false), Some(vec![KEY_RSHIFT, 0x07]));
        assert_eq!(map_key(Key::Enter, false), Some(vec![0x01]));
        assert_eq!(map_key(Key::Space, false), Some(vec![0x3c]));
        assert_eq!(map_key(Key::Tab, false), Some(vec![0x41]));
        assert_eq!(map_key(Key::P, false), Some(vec![0x29]));
        assert_eq!(map_key(Key::P, true), Some(vec![KEY_LSHIFT, 0x29]));
        assert_eq!(map_key(Key::Plus, true), Some(vec![0x28]));
        assert_eq!(map_key(Key::Equals, true), Some(vec![0x28]));
        assert_eq!(map_key(Key::Comma, true), Some(vec![KEY_LSHIFT, 0x2f]));
    }

    #[test]
    fn hold_and_release() {
        let ev = |key, pressed, shift| egui::Event::Key {
            key,
            physical_key: None,
            pressed,
            repeat: false,
            modifiers: egui::Modifiers { shift, ..Default::default() },
        };
        let mut s = KeyState::default();
        assert!(s.event(&ev(Key::ArrowLeft, true, false)));
        assert!(s.event(&ev(Key::Space, true, false)));
        assert_eq!(s.changed(), Some(vec![KEY_RSHIFT, 0x02, 0x3c]));
        assert_eq!(s.changed(), None);
        s.event(&ev(Key::ArrowLeft, false, false));
        assert_eq!(s.changed(), Some(vec![0x3c]));
        s.event(&ev(Key::Space, false, false));
        assert_eq!(s.changed(), Some(vec![]));
    }
}
