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

/// The two kinds of list the tablet formats by itself while text is typed.
#[derive(Clone, Copy, PartialEq, Debug)]
enum ListKind {
    Numbered,
    Bulleted,
}

/// The kind of list item a line is written as, and the line without its marker.
///
/// A dash or bullet character counts as "- " because that is what gets typed for it.
fn list_item(line: &str) -> Option<(ListKind, &str)> {
    const BULLETS: [&str; 6] = ["- ", "* ", "• ", "– ", "— ", "− "];
    if let Some(rest) = BULLETS.iter().find_map(|bullet| line.strip_prefix(bullet)) {
        return Some((ListKind::Bulleted, rest));
    }
    let digits = line.bytes().take_while(u8::is_ascii_digit).count();
    let rest = line[digits..].strip_prefix(". ")?;
    (digits > 0).then_some((ListKind::Numbered, rest))
}

/// The list the tablet starts when this line is typed outside of a list.
/// Bullets always start one; of the numbers, only "1. " does.
fn list_started_by(line: &str) -> Option<ListKind> {
    match list_item(line)? {
        (ListKind::Numbered, _) if !line.starts_with("1. ") => None,
        (kind, _) => Some(kind),
    }
}

/// Rewrite `text` so that its lists come out right when typed on the tablet.
///
/// reMarkable formats lists as they are typed: a line that starts with "1. " becomes a
/// numbered list and one that starts with "- " or "* " a bulleted list. Enter then goes on
/// with the next number or bullet, and Enter on an empty item ends the list. A model
/// writes every marker out itself, so typed as it is, each item after the first would get
/// two ("2. 2. ...").
///
/// So only the first item keeps its marker, blank lines between items are left out (they
/// would end the list), and a list is ended with an extra Enter when the text goes on
/// with something that is not an item of it.
pub fn text_for_tablet_lists(text: &str) -> String {
    let lines: Vec<&str> = text.split('\n').collect();
    let mut typed: Vec<&str> = Vec::new();
    // The list the tablet is in once the lines so far have been typed
    let mut open: Option<ListKind> = None;

    let mut index = 0;
    while index < lines.len() {
        let line = lines[index];

        if let Some(kind) = open {
            if let Some((_, rest)) = list_item(line).filter(|&(item_kind, _)| item_kind == kind) {
                typed.push(rest);
                index += 1;
                continue;
            }

            if line.trim().is_empty() {
                let Some(next) = lines[index..].iter().position(|line| !line.trim().is_empty()) else {
                    // Only blank lines are left: typing them would add an empty item
                    break;
                };
                if list_item(lines[index + next]).is_some_and(|(item_kind, _)| item_kind == kind) {
                    // The list goes on after the gap
                    index += next;
                    continue;
                }
                // This blank line is the Enter that ends the list
                typed.push("");
                open = None;
                index += 1;
                continue;
            }

            // Something else follows straight away: end the list first
            typed.push("");
        }

        open = list_started_by(line);
        typed.push(line);
        index += 1;
    }

    typed.join("\n")
}

/// As much of a text that is still arriving as can be typed already, with its lists
/// rewritten as `text_for_tablet_lists` does.
///
/// What a line turns into depends on how it starts, so the last line is held back
/// while it could still turn out to be a list item: while it is empty, or only the
/// beginning of a marker. Each result starts with the one before it.
pub fn text_for_tablet_lists_so_far(text: &str) -> String {
    let last_line_start = text.rfind('\n').map_or(0, |newline| newline + 1);
    let last_line = &text[last_line_start..];

    let digits = last_line.bytes().take_while(u8::is_ascii_digit).count();
    let could_be_a_number = digits > 0 && matches!(&last_line[digits..], "" | ".");
    let could_be_a_bullet = matches!(last_line, "-" | "*" | "•" | "–" | "—" | "−");

    if last_line.is_empty() || could_be_a_number || could_be_a_bullet {
        text_for_tablet_lists(&text[..last_line_start])
    } else {
        text_for_tablet_lists(text)
    }
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

    #[test]
    fn only_the_first_item_of_a_list_keeps_its_marker() {
        // The tablet numbers and bullets the following items itself
        assert_eq!(text_for_tablet_lists("1. a\n2. b\n3. c"), "1. a\nb\nc");
        assert_eq!(text_for_tablet_lists("- a\n- b"), "- a\nb");
        assert_eq!(text_for_tablet_lists("• a\n• b"), "• a\nb");
        // Text without lists is typed as it is
        assert_eq!(text_for_tablet_lists("one\n\ntwo 1. not a list"), "one\n\ntwo 1. not a list");
    }

    #[test]
    fn a_list_is_ended_before_the_text_that_follows_it() {
        // A blank line after the list is the Enter that ends it
        assert_eq!(text_for_tablet_lists("Intro:\n1. a\n2. b\n\nOutro"), "Intro:\n1. a\nb\n\nOutro");
        // Without one, that Enter is added
        assert_eq!(text_for_tablet_lists("1. a\n2. b\nOutro"), "1. a\nb\n\nOutro");
        // The same when another kind of list follows
        assert_eq!(text_for_tablet_lists("- a\n1. b\n2. c"), "- a\n\n1. b\nc");
        // Blank lines at the very end would leave an empty item behind
        assert_eq!(text_for_tablet_lists("1. a\n2. b\n\n"), "1. a\nb");
    }

    #[test]
    fn blank_lines_between_items_do_not_end_the_list() {
        assert_eq!(text_for_tablet_lists("1. a\n\n2. b\n\n3. c\n\nOutro"), "1. a\nb\nc\n\nOutro");
    }

    #[test]
    fn numbers_the_tablet_does_not_format_are_kept() {
        // Only "1. " starts a numbered list on the tablet
        assert_eq!(text_for_tablet_lists("3. a\n4. b"), "3. a\n4. b");
        assert_eq!(text_for_tablet_lists("1) a\n2) b"), "1) a\n2) b");
        // An indented line is not an item: it ends the list, and the next number stays
        assert_eq!(text_for_tablet_lists("1. a\n   note\n2. b"), "1. a\n\n   note\n2. b");
    }

    #[test]
    fn a_text_that_is_still_arriving_is_typed_in_step() {
        // Plain text is not held back
        assert_eq!(text_for_tablet_lists_so_far("Hello wor"), "Hello wor");
        // The start of a line is, while it could still be a marker
        assert_eq!(text_for_tablet_lists_so_far("1. a\n2"), "1. a");
        assert_eq!(text_for_tablet_lists_so_far("1. a\n2."), "1. a");
        assert_eq!(text_for_tablet_lists_so_far("1. a\n2. b"), "1. a\nb");
        assert_eq!(text_for_tablet_lists_so_far("1. a\n20 "), "1. a\n\n20 ");

        // However the text is cut, what can be typed only ever grows, up to the whole
        let texts = [
            "Intro:\n1. a\n2. b\n\nOutro",
            "1. a\n\n2. b\n\n3. c\n\nOutro\n",
            "- a\n- b\n1. c\n2. d\nEnd",
            "20 apples\n1. a\n   note\n2. b",
            "• один\n• два\n\n— и всё",
            "plain text\n\nwith two paragraphs",
        ];
        for text in texts {
            let mut typed = String::new();
            for end in (0..=text.len()).filter(|&end| text.is_char_boundary(end)) {
                let so_far = text_for_tablet_lists_so_far(&text[..end]);
                assert!(so_far.starts_with(&typed), "{so_far:?} does not carry on from {typed:?}");
                typed = so_far;
            }
            assert!(
                text_for_tablet_lists(text).starts_with(&typed),
                "{typed:?} is not the start of the whole of {text:?}"
            );
        }
    }
}
