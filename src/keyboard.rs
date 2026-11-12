use anyhow::{Context, Result};
use log::debug;

use std::collections::HashMap;
use std::{thread, time};

use evdev::{uinput::VirtualDevice, AttributeSet, EventType as EvdevEventType, InputEvent, KeyCode as EvdevKey};

/// Keys that type characters on a US layout: the character each key produces on its own,
/// followed by the one it produces with shift (if any).
const CHARACTER_KEYS: &[(EvdevKey, &str)] = &[
    (EvdevKey::KEY_A, "aA"),
    (EvdevKey::KEY_B, "bB"),
    (EvdevKey::KEY_C, "cC"),
    (EvdevKey::KEY_D, "dD"),
    (EvdevKey::KEY_E, "eE"),
    (EvdevKey::KEY_F, "fF"),
    (EvdevKey::KEY_G, "gG"),
    (EvdevKey::KEY_H, "hH"),
    (EvdevKey::KEY_I, "iI"),
    (EvdevKey::KEY_J, "jJ"),
    (EvdevKey::KEY_K, "kK"),
    (EvdevKey::KEY_L, "lL"),
    (EvdevKey::KEY_M, "mM"),
    (EvdevKey::KEY_N, "nN"),
    (EvdevKey::KEY_O, "oO"),
    (EvdevKey::KEY_P, "pP"),
    (EvdevKey::KEY_Q, "qQ"),
    (EvdevKey::KEY_R, "rR"),
    (EvdevKey::KEY_S, "sS"),
    (EvdevKey::KEY_T, "tT"),
    (EvdevKey::KEY_U, "uU"),
    (EvdevKey::KEY_V, "vV"),
    (EvdevKey::KEY_W, "wW"),
    (EvdevKey::KEY_X, "xX"),
    (EvdevKey::KEY_Y, "yY"),
    (EvdevKey::KEY_Z, "zZ"),
    (EvdevKey::KEY_1, "1!"),
    (EvdevKey::KEY_2, "2@"),
    (EvdevKey::KEY_3, "3#"),
    (EvdevKey::KEY_4, "4$"),
    (EvdevKey::KEY_5, "5%"),
    (EvdevKey::KEY_6, "6^"),
    (EvdevKey::KEY_7, "7&"),
    (EvdevKey::KEY_8, "8*"),
    (EvdevKey::KEY_9, "9("),
    (EvdevKey::KEY_0, "0)"),
    (EvdevKey::KEY_MINUS, "-_"),
    (EvdevKey::KEY_EQUAL, "=+"),
    (EvdevKey::KEY_LEFTBRACE, "[{"),
    (EvdevKey::KEY_RIGHTBRACE, "]}"),
    (EvdevKey::KEY_BACKSLASH, "\\|"),
    (EvdevKey::KEY_SEMICOLON, ";:"),
    (EvdevKey::KEY_APOSTROPHE, "'\""),
    (EvdevKey::KEY_GRAVE, "`~"),
    (EvdevKey::KEY_COMMA, ",<"),
    (EvdevKey::KEY_DOT, ".>"),
    (EvdevKey::KEY_SLASH, "/?"),
    (EvdevKey::KEY_SPACE, " "),
    (EvdevKey::KEY_TAB, "\t"),
    (EvdevKey::KEY_ENTER, "\n"),
    (EvdevKey::KEY_BACKSPACE, "\x08"),
    (EvdevKey::KEY_ESC, "\x1b"),
];

const MODIFIER_KEYS: &[EvdevKey] = &[EvdevKey::KEY_LEFTSHIFT, EvdevKey::KEY_LEFTCTRL, EvdevKey::KEY_LEFTALT];

/// The closest thing the keyboard can type for a character it has no key for.
/// Models often reply with accented letters and typographic punctuation, and the
/// virtual keyboard is a US layout without dead keys.
fn ascii_fallback(c: char) -> Option<&'static str> {
    Some(match c {
        'à' | 'â' | 'ä' | 'á' | 'ã' | 'å' => "a",
        'À' | 'Â' | 'Ä' | 'Á' | 'Ã' | 'Å' => "A",
        'é' | 'è' | 'ê' | 'ë' => "e",
        'É' | 'È' | 'Ê' | 'Ë' => "E",
        'î' | 'ï' | 'í' | 'ì' => "i",
        'Î' | 'Ï' | 'Í' | 'Ì' => "I",
        'ô' | 'ö' | 'ó' | 'ò' | 'õ' => "o",
        'Ô' | 'Ö' | 'Ó' | 'Ò' | 'Õ' => "O",
        'ù' | 'û' | 'ü' | 'ú' => "u",
        'Ù' | 'Û' | 'Ü' | 'Ú' => "U",
        'ç' => "c",
        'Ç' => "C",
        'ñ' => "n",
        'Ñ' => "N",
        'ß' => "ss",
        'œ' => "oe",
        'Œ' => "OE",
        'æ' => "ae",
        'Æ' => "AE",
        '’' | '‘' => "'",
        '“' | '”' | '«' | '»' => "\"",
        '–' | '—' | '−' => "-",
        '…' => "...",
        '•' => "*",
        '×' => "x",
        '÷' => "/",
        '²' => "^2",
        '³' => "^3",
        '≈' => "~",
        '≤' => "<=",
        '≥' => ">=",
        '≠' => "!=",
        '→' => "->",
        '←' => "<-",
        '°' => " deg",
        '\u{a0}' | '\u{202f}' => " ",
        _ => return None,
    })
}

pub struct Keyboard {
    device: Option<VirtualDevice>,
    /// The key for each character, and whether it needs shift
    key_map: HashMap<char, (EvdevKey, bool)>,
    progress_count: u32,
    no_draw_progress: bool,
    /// How long to wait after each key. xochitl on a Paper Pro (firmware 3.29) takes
    /// every key at 7 ms apart and loses about one in twenty at 5 ms.
    key_delay: time::Duration,
}

impl Keyboard {
    pub fn new(no_draw: bool, no_draw_progress: bool) -> Result<Self> {
        let device = if no_draw { None } else { Some(Self::create_virtual_device()?) };

        Ok(Self {
            device,
            key_map: Self::create_key_map(),
            progress_count: 0,
            no_draw_progress,
            key_delay: time::Duration::from_millis(10),
        })
    }

    /// Change how long to wait after each key.
    pub fn set_key_delay(&mut self, key_delay: time::Duration) {
        self.key_delay = key_delay;
    }

    fn create_virtual_device() -> Result<VirtualDevice> {
        debug!("Creating virtual keyboard");
        let mut keys = AttributeSet::new();
        for &(key, _) in CHARACTER_KEYS {
            keys.insert(key);
        }
        for &key in MODIFIER_KEYS {
            keys.insert(key);
        }

        VirtualDevice::builder()
            .and_then(|builder| builder.name("Virtual Keyboard").with_keys(&keys))
            .and_then(|builder| builder.build())
            .context("Failed to create the virtual keyboard (is /dev/uinput available?)")
    }

    fn create_key_map() -> HashMap<char, (EvdevKey, bool)> {
        let mut key_map = HashMap::new();
        for &(key, characters) in CHARACTER_KEYS {
            let mut characters = characters.chars();
            if let Some(plain) = characters.next() {
                key_map.insert(plain, (key, false));
            }
            if let Some(shifted) = characters.next() {
                key_map.insert(shifted, (key, true));
            }
        }
        key_map
    }

    pub fn key_down(&mut self, key: EvdevKey) -> Result<()> {
        if let Some(device) = &mut self.device {
            device.emit(&[(InputEvent::new(EvdevEventType::KEY.0, key.code(), 1))])?;
            device.emit(&[InputEvent::new(EvdevEventType::SYNCHRONIZATION.0, 0, 0)])?;
            thread::sleep(time::Duration::from_millis(1));
        }
        Ok(())
    }

    pub fn key_up(&mut self, key: EvdevKey) -> Result<()> {
        if let Some(device) = &mut self.device {
            device.emit(&[(InputEvent::new(EvdevEventType::KEY.0, key.code(), 0))])?;
            device.emit(&[InputEvent::new(EvdevEventType::SYNCHRONIZATION.0, 0, 0)])?;
            thread::sleep(time::Duration::from_millis(1));
        }
        Ok(())
    }

    /// The keys to press to type a string, and whether each needs shift.
    /// A character without a key becomes its ASCII fallback, or is skipped if it has none.
    fn keys_for(&self, input: &str) -> Vec<(EvdevKey, bool)> {
        let mut keys = Vec::new();
        for c in input.chars() {
            match self.key_map.get(&c) {
                Some(&key) => keys.push(key),
                None => keys.extend(ascii_fallback(c).unwrap_or_default().chars().filter_map(|c| self.key_map.get(&c))),
            }
        }
        keys
    }

    /// Type a string.
    pub fn string_to_keypresses(&mut self, input: &str) -> Result<()> {
        let keys = self.keys_for(input);

        if let Some(device) = &mut self.device {
            // make sure we are synced before we start; this might be paranoia
            device.emit(&[InputEvent::new(EvdevEventType::SYNCHRONIZATION.0, 0, 0)])?;
            thread::sleep(time::Duration::from_millis(10));

            for (key, shift) in keys {
                if shift {
                    // Press Shift
                    device.emit(&[InputEvent::new(EvdevEventType::KEY.0, EvdevKey::KEY_LEFTSHIFT.code(), 1)])?;
                }

                // Press key
                device.emit(&[InputEvent::new(EvdevEventType::KEY.0, key.code(), 1)])?;

                // Release key
                device.emit(&[InputEvent::new(EvdevEventType::KEY.0, key.code(), 0)])?;

                if shift {
                    // Release Shift
                    device.emit(&[InputEvent::new(EvdevEventType::KEY.0, EvdevKey::KEY_LEFTSHIFT.code(), 0)])?;
                }

                // Sync event
                device.emit(&[InputEvent::new(EvdevEventType::SYNCHRONIZATION.0, 0, 0)])?;
                thread::sleep(self.key_delay);
            }
        }
        Ok(())
    }

    fn key_cmd(&mut self, button: &str, shift: bool) -> Result<()> {
        self.key_down(EvdevKey::KEY_LEFTCTRL)?;
        if shift {
            self.key_down(EvdevKey::KEY_LEFTSHIFT)?;
        }
        self.string_to_keypresses(button)?;
        if shift {
            self.key_up(EvdevKey::KEY_LEFTSHIFT)?;
        }
        self.key_up(EvdevKey::KEY_LEFTCTRL)?;
        Ok(())
    }

    pub fn key_cmd_title(&mut self) -> Result<()> {
        self.key_cmd("1", false)
    }

    pub fn key_cmd_subheading(&mut self) -> Result<()> {
        self.key_cmd("2", false)
    }

    pub fn key_cmd_body(&mut self) -> Result<()> {
        self.key_cmd("3", false)
    }

    pub fn key_cmd_bullet(&mut self) -> Result<()> {
        self.key_cmd("4", false)
    }

    /// Type a progress note, to be erased again by `progress_end`.
    pub fn progress(&mut self, note: &str) -> Result<()> {
        if self.no_draw_progress {
            return Ok(());
        }
        self.string_to_keypresses(note)?;
        self.progress_count += note.len() as u32;
        Ok(())
    }

    /// Erase the progress notes typed so far.
    pub fn progress_end(&mut self) -> Result<()> {
        if self.no_draw_progress {
            return Ok(());
        }
        // Send a backspace for each character of the notes
        self.string_to_keypresses(&"\x08".repeat(self.progress_count as usize))?;
        self.progress_count = 0;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_map_covers_printable_ascii() {
        let key_map = Keyboard::create_key_map();
        for c in (' '..='~').chain(['\n', '\t']) {
            assert!(key_map.contains_key(&c), "no key for {c:?}");
        }
    }

    #[test]
    fn key_map_knows_which_characters_need_shift() {
        let key_map = Keyboard::create_key_map();
        assert_eq!(key_map[&'a'], (EvdevKey::KEY_A, false));
        assert_eq!(key_map[&'A'], (EvdevKey::KEY_A, true));
        assert_eq!(key_map[&'?'], (EvdevKey::KEY_SLASH, true));
        assert_eq!(key_map[&'\''], (EvdevKey::KEY_APOSTROPHE, false));
        assert_eq!(key_map[&'"'], (EvdevKey::KEY_APOSTROPHE, true));
        assert_eq!(key_map[&'\n'], (EvdevKey::KEY_ENTER, false));
    }

    #[test]
    fn characters_without_a_key_are_typed_as_their_ascii_fallback() {
        let keyboard = Keyboard::new(true, true).unwrap();

        // Typographic punctuation and accents come out as plain ASCII
        assert_eq!(keyboard.keys_for("café… “ok” — 5×2"), keyboard.keys_for("cafe... \"ok\" - 5x2"));
        // A character with no fallback is skipped
        assert_eq!(keyboard.keys_for("a🙂b"), keyboard.keys_for("ab"));
    }
}
