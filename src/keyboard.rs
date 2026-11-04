use anyhow::Result;
use log::debug;

use std::collections::HashMap;
use std::{thread, time};

use evdev::{uinput::VirtualDevice, AttributeSet, EventType as EvdevEventType, InputEvent, KeyCode as EvdevKey};

/// The virtual keyboard uses the US layout and has no dead keys, so accented and
/// typographic characters are replaced by the closest ASCII text before typing.
fn fold_char(c: char) -> String {
    let s = match c {
        'à' | 'â' | 'ä' | 'á' => "a",
        'À' | 'Â' | 'Ä' | 'Á' => "A",
        'é' | 'è' | 'ê' | 'ë' => "e",
        'É' | 'È' | 'Ê' | 'Ë' => "E",
        'î' | 'ï' | 'í' => "i",
        'Î' | 'Ï' => "I",
        'ô' | 'ö' | 'ó' => "o",
        'Ô' | 'Ö' => "O",
        'ù' | 'û' | 'ü' | 'ú' => "u",
        'Ù' | 'Û' | 'Ü' => "U",
        'ç' => "c",
        'Ç' => "C",
        'ñ' => "n",
        'œ' => "oe",
        'Œ' => "OE",
        'æ' => "ae",
        '’' | '‘' => "'",
        '«' | '»' | '“' | '”' => "\"",
        '–' | '—' | '−' => "-",
        '…' => "...",
        '×' => "x",
        '²' => "^2",
        '³' => "^3",
        '÷' => "/",
        '≈' => "~",
        '≤' => "<=",
        '≥' => ">=",
        '≠' => "!=",
        '→' => "->",
        '°' => " deg",
        '\u{a0}' | '\u{202f}' => " ",
        _ => return c.to_string(),
    };
    s.to_string()
}

pub struct Keyboard {
    device: Option<VirtualDevice>,
    key_map: HashMap<char, (EvdevKey, bool)>,
    progress_count: u32,
    no_draw_progress: bool,
}

impl Keyboard {
    pub fn new(no_draw: bool, no_draw_progress: bool) -> Self {
        let device = if no_draw { None } else { Some(Self::create_virtual_device()) };

        Self {
            device,
            key_map: Self::create_key_map(),
            progress_count: 0,
            no_draw_progress,
        }
    }

    fn create_virtual_device() -> VirtualDevice {
        debug!("Creating virtual keyboard");
        let mut keys = AttributeSet::new();

        keys.insert(EvdevKey::KEY_A);
        keys.insert(EvdevKey::KEY_B);
        keys.insert(EvdevKey::KEY_C);
        keys.insert(EvdevKey::KEY_D);
        keys.insert(EvdevKey::KEY_E);
        keys.insert(EvdevKey::KEY_F);
        keys.insert(EvdevKey::KEY_G);
        keys.insert(EvdevKey::KEY_H);
        keys.insert(EvdevKey::KEY_I);
        keys.insert(EvdevKey::KEY_J);
        keys.insert(EvdevKey::KEY_K);
        keys.insert(EvdevKey::KEY_L);
        keys.insert(EvdevKey::KEY_M);
        keys.insert(EvdevKey::KEY_N);
        keys.insert(EvdevKey::KEY_O);
        keys.insert(EvdevKey::KEY_P);
        keys.insert(EvdevKey::KEY_Q);
        keys.insert(EvdevKey::KEY_R);
        keys.insert(EvdevKey::KEY_S);
        keys.insert(EvdevKey::KEY_T);
        keys.insert(EvdevKey::KEY_U);
        keys.insert(EvdevKey::KEY_V);
        keys.insert(EvdevKey::KEY_W);
        keys.insert(EvdevKey::KEY_X);
        keys.insert(EvdevKey::KEY_Y);
        keys.insert(EvdevKey::KEY_Z);

        keys.insert(EvdevKey::KEY_1);
        keys.insert(EvdevKey::KEY_2);
        keys.insert(EvdevKey::KEY_3);
        keys.insert(EvdevKey::KEY_4);
        keys.insert(EvdevKey::KEY_5);
        keys.insert(EvdevKey::KEY_6);
        keys.insert(EvdevKey::KEY_7);
        keys.insert(EvdevKey::KEY_8);
        keys.insert(EvdevKey::KEY_9);
        keys.insert(EvdevKey::KEY_0);

        // Add punctuation and special keys
        keys.insert(EvdevKey::KEY_SPACE);
        keys.insert(EvdevKey::KEY_ENTER);
        keys.insert(EvdevKey::KEY_TAB);
        keys.insert(EvdevKey::KEY_LEFTSHIFT);
        keys.insert(EvdevKey::KEY_MINUS);
        keys.insert(EvdevKey::KEY_EQUAL);
        keys.insert(EvdevKey::KEY_LEFTBRACE);
        keys.insert(EvdevKey::KEY_RIGHTBRACE);
        keys.insert(EvdevKey::KEY_BACKSLASH);
        keys.insert(EvdevKey::KEY_SEMICOLON);
        keys.insert(EvdevKey::KEY_APOSTROPHE);
        keys.insert(EvdevKey::KEY_GRAVE);
        keys.insert(EvdevKey::KEY_COMMA);
        keys.insert(EvdevKey::KEY_DOT);
        keys.insert(EvdevKey::KEY_SLASH);

        keys.insert(EvdevKey::KEY_BACKSPACE);
        keys.insert(EvdevKey::KEY_ESC);

        keys.insert(EvdevKey::KEY_LEFTCTRL);
        keys.insert(EvdevKey::KEY_LEFTALT);

        VirtualDevice::builder()
            .unwrap()
            .name("Virtual Keyboard")
            .with_keys(&keys)
            .unwrap()
            .build()
            .unwrap()
    }

    fn create_key_map() -> HashMap<char, (EvdevKey, bool)> {
        let mut key_map = HashMap::new();

        // Lowercase letters
        key_map.insert('a', (EvdevKey::KEY_A, false));
        key_map.insert('b', (EvdevKey::KEY_B, false));
        key_map.insert('c', (EvdevKey::KEY_C, false));
        key_map.insert('d', (EvdevKey::KEY_D, false));
        key_map.insert('e', (EvdevKey::KEY_E, false));
        key_map.insert('f', (EvdevKey::KEY_F, false));
        key_map.insert('g', (EvdevKey::KEY_G, false));
        key_map.insert('h', (EvdevKey::KEY_H, false));
        key_map.insert('i', (EvdevKey::KEY_I, false));
        key_map.insert('j', (EvdevKey::KEY_J, false));
        key_map.insert('k', (EvdevKey::KEY_K, false));
        key_map.insert('l', (EvdevKey::KEY_L, false));
        key_map.insert('m', (EvdevKey::KEY_M, false));
        key_map.insert('n', (EvdevKey::KEY_N, false));
        key_map.insert('o', (EvdevKey::KEY_O, false));
        key_map.insert('p', (EvdevKey::KEY_P, false));
        key_map.insert('q', (EvdevKey::KEY_Q, false));
        key_map.insert('r', (EvdevKey::KEY_R, false));
        key_map.insert('s', (EvdevKey::KEY_S, false));
        key_map.insert('t', (EvdevKey::KEY_T, false));
        key_map.insert('u', (EvdevKey::KEY_U, false));
        key_map.insert('v', (EvdevKey::KEY_V, false));
        key_map.insert('w', (EvdevKey::KEY_W, false));
        key_map.insert('x', (EvdevKey::KEY_X, false));
        key_map.insert('y', (EvdevKey::KEY_Y, false));
        key_map.insert('z', (EvdevKey::KEY_Z, false));

        // Uppercase letters
        key_map.insert('A', (EvdevKey::KEY_A, true));
        key_map.insert('B', (EvdevKey::KEY_B, true));
        key_map.insert('C', (EvdevKey::KEY_C, true));
        key_map.insert('D', (EvdevKey::KEY_D, true));
        key_map.insert('E', (EvdevKey::KEY_E, true));
        key_map.insert('F', (EvdevKey::KEY_F, true));
        key_map.insert('G', (EvdevKey::KEY_G, true));
        key_map.insert('H', (EvdevKey::KEY_H, true));
        key_map.insert('I', (EvdevKey::KEY_I, true));
        key_map.insert('J', (EvdevKey::KEY_J, true));
        key_map.insert('K', (EvdevKey::KEY_K, true));
        key_map.insert('L', (EvdevKey::KEY_L, true));
        key_map.insert('M', (EvdevKey::KEY_M, true));
        key_map.insert('N', (EvdevKey::KEY_N, true));
        key_map.insert('O', (EvdevKey::KEY_O, true));
        key_map.insert('P', (EvdevKey::KEY_P, true));
        key_map.insert('Q', (EvdevKey::KEY_Q, true));
        key_map.insert('R', (EvdevKey::KEY_R, true));
        key_map.insert('S', (EvdevKey::KEY_S, true));
        key_map.insert('T', (EvdevKey::KEY_T, true));
        key_map.insert('U', (EvdevKey::KEY_U, true));
        key_map.insert('V', (EvdevKey::KEY_V, true));
        key_map.insert('W', (EvdevKey::KEY_W, true));
        key_map.insert('X', (EvdevKey::KEY_X, true));
        key_map.insert('Y', (EvdevKey::KEY_Y, true));
        key_map.insert('Z', (EvdevKey::KEY_Z, true));

        // Numbers
        key_map.insert('0', (EvdevKey::KEY_0, false));
        key_map.insert('1', (EvdevKey::KEY_1, false));
        key_map.insert('2', (EvdevKey::KEY_2, false));
        key_map.insert('3', (EvdevKey::KEY_3, false));
        key_map.insert('4', (EvdevKey::KEY_4, false));
        key_map.insert('5', (EvdevKey::KEY_5, false));
        key_map.insert('6', (EvdevKey::KEY_6, false));
        key_map.insert('7', (EvdevKey::KEY_7, false));
        key_map.insert('8', (EvdevKey::KEY_8, false));
        key_map.insert('9', (EvdevKey::KEY_9, false));

        // Special characters
        key_map.insert('!', (EvdevKey::KEY_1, true));
        key_map.insert('@', (EvdevKey::KEY_2, true));
        key_map.insert('#', (EvdevKey::KEY_3, true));
        key_map.insert('$', (EvdevKey::KEY_4, true));
        key_map.insert('%', (EvdevKey::KEY_5, true));
        key_map.insert('^', (EvdevKey::KEY_6, true));
        key_map.insert('&', (EvdevKey::KEY_7, true));
        key_map.insert('*', (EvdevKey::KEY_8, true));
        key_map.insert('(', (EvdevKey::KEY_9, true));
        key_map.insert(')', (EvdevKey::KEY_0, true));
        key_map.insert('_', (EvdevKey::KEY_MINUS, true));
        key_map.insert('+', (EvdevKey::KEY_EQUAL, true));
        key_map.insert('{', (EvdevKey::KEY_LEFTBRACE, true));
        key_map.insert('}', (EvdevKey::KEY_RIGHTBRACE, true));
        key_map.insert('|', (EvdevKey::KEY_BACKSLASH, true));
        key_map.insert(':', (EvdevKey::KEY_SEMICOLON, true));
        key_map.insert('"', (EvdevKey::KEY_APOSTROPHE, true));
        key_map.insert('<', (EvdevKey::KEY_COMMA, true));
        key_map.insert('>', (EvdevKey::KEY_DOT, true));
        key_map.insert('?', (EvdevKey::KEY_SLASH, true));
        key_map.insert('~', (EvdevKey::KEY_GRAVE, true));

        // Common punctuation
        key_map.insert('-', (EvdevKey::KEY_MINUS, false));
        key_map.insert('=', (EvdevKey::KEY_EQUAL, false));
        key_map.insert('[', (EvdevKey::KEY_LEFTBRACE, false));
        key_map.insert(']', (EvdevKey::KEY_RIGHTBRACE, false));
        key_map.insert('\\', (EvdevKey::KEY_BACKSLASH, false));
        key_map.insert(';', (EvdevKey::KEY_SEMICOLON, false));
        key_map.insert('\'', (EvdevKey::KEY_APOSTROPHE, false));
        key_map.insert(',', (EvdevKey::KEY_COMMA, false));
        key_map.insert('.', (EvdevKey::KEY_DOT, false));
        key_map.insert('/', (EvdevKey::KEY_SLASH, false));
        key_map.insert('`', (EvdevKey::KEY_GRAVE, false));

        // Whitespace
        key_map.insert(' ', (EvdevKey::KEY_SPACE, false));
        key_map.insert('\t', (EvdevKey::KEY_TAB, false));
        key_map.insert('\n', (EvdevKey::KEY_ENTER, false));

        // Action keys, such as backspace, escape, ctrl, alt
        key_map.insert('\x08', (EvdevKey::KEY_BACKSPACE, false));
        key_map.insert('\x1b', (EvdevKey::KEY_ESC, false));

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

    pub fn string_to_keypresses(&mut self, input: &str) -> Result<()> {
        if let Some(device) = &mut self.device {
            // make sure we are synced before we start; this might be paranoia
            device.emit(&[InputEvent::new(EvdevEventType::SYNCHRONIZATION.0, 0, 0)])?;
            thread::sleep(time::Duration::from_millis(10));

            let folded: String = input.chars().map(fold_char).collect();
            for c in folded.chars() {
                if let Some(&(key, shift)) = self.key_map.get(&c) {
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
                    thread::sleep(time::Duration::from_millis(10));
                }
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
        self.key_cmd("1", false)?;
        Ok(())
    }

    pub fn key_cmd_subheading(&mut self) -> Result<()> {
        self.key_cmd("2", false)?;
        Ok(())
    }

    pub fn key_cmd_body(&mut self) -> Result<()> {
        self.key_cmd("3", false)?;
        Ok(())
    }

    pub fn key_cmd_bullet(&mut self) -> Result<()> {
        self.key_cmd("4", false)?;
        Ok(())
    }

    pub fn progress(&mut self, note: &str) -> Result<()> {
        if self.no_draw_progress {
            return Ok(());
        }
        self.string_to_keypresses(note)?;
        self.progress_count += note.len() as u32;
        Ok(())
    }

    pub fn progress_end(&mut self) -> Result<()> {
        if self.no_draw_progress {
            return Ok(());
        }
        // Send a backspace for each progress
        for _ in 0..self.progress_count {
            self.string_to_keypresses("\x08")?;
        }
        self.progress_count = 0;
        Ok(())
    }
}
