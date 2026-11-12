//! Making use of a tool call while it is still arriving.
//!
//! A model writes a drawing out over several seconds. Streamed, the arguments of its
//! tool call arrive as the text of a JSON object a piece at a time, and the SVG in them
//! an element at a time. These pick out what can already be drawn.

/// The value so far of a string property of a JSON object whose text is still arriving.
///
/// `partial` is the start of the object's text, cut off anywhere. Returns None until
/// the property's value has begun. An escape that has only half arrived is left off.
pub fn string_property_so_far(partial: &str, key: &str) -> Option<String> {
    let mut chars = partial.chars().peekable();
    let mut depth = 0;
    // Whether the next string at the top level of the object is a property's name
    let mut name_next = false;

    while let Some(c) = chars.next() {
        match c {
            '{' | '[' => {
                depth += 1;
                name_next = depth == 1 && c == '{';
            }
            '}' | ']' => depth -= 1,
            ',' if depth == 1 => name_next = true,
            '"' => {
                let (string, finished) = read_string(&mut chars);
                if !(finished && depth == 1 && name_next) {
                    continue;
                }
                name_next = false;
                if string != key {
                    continue;
                }
                // The name, a colon, and then the value
                while chars.next_if(|c| c.is_whitespace()).is_some() {}
                chars.next_if_eq(&':')?;
                while chars.next_if(|c| c.is_whitespace()).is_some() {}
                chars.next_if_eq(&'"')?;
                return Some(read_string(&mut chars).0);
            }
            _ => {}
        }
    }
    None
}

/// Read the rest of a JSON string, after its opening quote: what it says, and whether
/// its closing quote was reached.
fn read_string(chars: &mut std::iter::Peekable<std::str::Chars>) -> (String, bool) {
    let mut string = String::new();
    while let Some(c) = chars.next() {
        match c {
            '"' => return (string, true),
            '\\' => {
                let Some(escaped) = chars.next() else {
                    break;
                };
                match escaped {
                    'n' => string.push('\n'),
                    't' => string.push('\t'),
                    'r' => string.push('\r'),
                    'b' => string.push('\u{8}'),
                    'f' => string.push('\u{c}'),
                    'u' => {
                        let Some(unit) = read_hex(chars) else {
                            break;
                        };
                        // A character outside the basic plane is written as two escapes
                        let code = if (0xD800..0xDC00).contains(&unit) {
                            let second = (chars.next() == Some('\\') && chars.next() == Some('u')).then(|| read_hex(chars)).flatten();
                            let Some(low) = second.filter(|low| (0xDC00..0xE000).contains(low)) else {
                                break;
                            };
                            0x10000 + ((unit - 0xD800) << 10) + (low - 0xDC00)
                        } else {
                            unit
                        };
                        string.push(char::from_u32(code).unwrap_or('\u{fffd}'));
                    }
                    other => string.push(other),
                }
            }
            other => string.push(other),
        }
    }
    (string, false)
}

fn read_hex(chars: &mut std::iter::Peekable<std::str::Chars>) -> Option<u32> {
    let mut value = 0;
    for _ in 0..4 {
        value = value * 16 + chars.next()?.to_digit(16)?;
    }
    Some(value)
}

/// As much of an SVG whose text is still arriving as can be drawn: the elements that
/// are complete so far, with the document closed off after them. None until the first
/// element is complete.
///
/// An element counts once it has closed, and so does one inside a group that is still
/// open: the group is closed off too. Anything else that is still open is left out
/// until it closes, since half a line of text is not where the whole line will be.
pub fn drawable_so_far(partial_svg: &str) -> Option<String> {
    // The elements that are open, outermost first, and whether each is a group
    let mut open: Vec<(&str, bool)> = Vec::new();
    // Where the last complete element ended, and the groups that were open around it
    let mut complete: Option<(usize, Vec<&str>)> = None;

    let mut at = 0;
    while let Some(start) = partial_svg[at..].find('<').map(|found| at + found) {
        let rest = &partial_svg[start..];
        // Comments, processing instructions and declarations are skipped whole
        let skipped = [("<!--", "-->"), ("<![CDATA[", "]]>"), ("<?", "?>"), ("<!", ">")]
            .iter()
            .find(|(opening, _)| rest.starts_with(opening))
            .map(|(opening, closing)| rest[opening.len()..].find(closing).map(|found| start + opening.len() + found + closing.len()));
        if let Some(end) = skipped {
            // One that has not all arrived is where what can be drawn stops
            let Some(end) = end else {
                break;
            };
            at = end;
            continue;
        }

        let Some(end) = tag_end(rest).map(|length| start + length) else {
            break;
        };
        let tag = &partial_svg[start + 1..end - 1];
        let name = tag.trim_start_matches('/').split(|c: char| c.is_whitespace() || c == '/').next().unwrap_or("");
        let closes = tag.starts_with('/');
        let closes_itself = tag.ends_with('/');

        if closes {
            open.pop();
            if open.is_empty() {
                // The document itself has closed: all of it can be drawn
                return Some(partial_svg[..end].to_string());
            }
        } else if !closes_itself {
            open.push((name, name == "g" || open.is_empty()));
        }
        // An element has just ended if this tag closed one or closed itself. It counts
        // if everything still open around it is a group (or the document).
        if (closes || closes_itself) && !open.is_empty() && open.iter().all(|&(_, is_group)| is_group) {
            complete = Some((end, open.iter().skip(1).map(|&(name, _)| name).collect()));
        }
        at = end;
    }

    let (end, groups) = complete?;
    let root = partial_svg[..end].find('<').and_then(|_| open_root_name(partial_svg))?;
    let mut drawable = partial_svg[..end].to_string();
    for group in groups.iter().rev() {
        drawable.push_str(&format!("</{group}>"));
    }
    drawable.push_str(&format!("</{root}>"));
    Some(drawable)
}

/// How long a tag is, from its `<` to just past its `>`, allowing for a `>` inside a
/// quoted attribute. None if the tag has not all arrived.
fn tag_end(tag: &str) -> Option<usize> {
    let mut quote = None;
    for (index, c) in tag.char_indices() {
        match (quote, c) {
            (None, '"' | '\'') => quote = Some(c),
            (Some(open), c) if c == open => quote = None,
            (None, '>') => return Some(index + 1),
            _ => {}
        }
    }
    None
}

/// The name of a document's outermost element.
fn open_root_name(svg: &str) -> Option<&str> {
    let mut at = 0;
    loop {
        let start = at + svg[at..].find('<')?;
        let rest = &svg[start + 1..];
        if rest.starts_with('!') || rest.starts_with('?') {
            at = start + 1 + rest.find('>')?;
            continue;
        }
        return rest.split(|c: char| c.is_whitespace() || c == '>' || c == '/').next();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_string_property_is_read_as_far_as_it_has_arrived() {
        let whole = r#"{"input_description": "a \"quoted\" note, with a brace } in it", "input_features": [{"feature_description": "svg", "top_left_x_px": 1}], "svg": "<svg width=\"768\">\n<rect x=\"1\"/></svg>"}"#;
        let value = "<svg width=\"768\">\n<rect x=\"1\"/></svg>";

        // Nothing until the value begins, and then as much of it as there is
        let begins = whole.find(r#""svg": ""#).unwrap() + r#""svg": ""#.len();
        assert_eq!(string_property_so_far(&whole[..begins - 2], "svg"), None);
        assert_eq!(string_property_so_far(&whole[..begins], "svg"), Some(String::new()));
        let mut longest = 0;
        for end in begins..=whole.len() {
            let so_far = string_property_so_far(&whole[..end], "svg").unwrap();
            assert!(value.starts_with(&so_far), "{so_far:?} at {end}");
            assert!(so_far.len() >= longest);
            longest = so_far.len();
        }
        assert_eq!(string_property_so_far(whole, "svg").as_deref(), Some(value));
    }

    #[test]
    fn a_property_of_the_same_name_deeper_in_is_not_the_one() {
        let partial = r#"{"input_features": [{"svg": "not this"}], "note": "svg", "svg": "<svg"#;

        assert_eq!(string_property_so_far(partial, "svg").as_deref(), Some("<svg"));
    }

    #[test]
    fn escapes_are_read_and_half_an_escape_is_left_off() {
        assert_eq!(string_property_so_far(r#"{"svg": "aé😀b\\"#, "svg").as_deref(), Some("aé😀b\\"));
        assert_eq!(string_property_so_far(r#"{"svg": "a\"#, "svg").as_deref(), Some("a"));
        assert_eq!(string_property_so_far(r#"{"svg": "a\u00"#, "svg").as_deref(), Some("a"));
        assert_eq!(string_property_so_far(r#"{"svg": "a\ud83d"#, "svg").as_deref(), Some("a"));
    }

    #[test]
    fn an_svg_is_drawable_up_to_its_last_complete_element() {
        let svg = r#"<svg width="768" height="1024" xmlns="http://www.w3.org/2000/svg"><!-- a house --><rect x="1" y="2" width="3" height="4"/><circle cx="5" cy="5" r="2"/><text x="1" y="9">Hello there</text></svg>"#;
        let after = |element: &str| svg.find(element).unwrap() + element.len();

        // Nothing until the first element is complete
        assert_eq!(drawable_so_far(&svg[..after("<!-- a house -->")]), None);
        assert_eq!(drawable_so_far(&svg[..after(r#"height="4""#)]), None);

        let one = drawable_so_far(&svg[..after(r#"height="4"/>"#)]).unwrap();
        assert!(one.ends_with(r#"height="4"/></svg>"#), "{one}");
        // Half a circle, and half a line of text, wait until they are whole
        assert_eq!(drawable_so_far(&svg[..after("<circle cx")]).unwrap(), one);
        let two = drawable_so_far(&svg[..after("Hello th")]).unwrap();
        assert!(two.ends_with(r#"r="2"/></svg>"#), "{two}");

        assert_eq!(drawable_so_far(svg).unwrap(), svg);
    }

    #[test]
    fn elements_in_a_group_that_is_still_open_are_drawable() {
        let partial =
            r#"<svg xmlns="http://www.w3.org/2000/svg"><g transform="translate(10 10)"><g fill="none"><line x1="0" y1="0" x2="5" y2="5"/><path d="M 0 0 L"#;

        assert_eq!(
            drawable_so_far(partial).unwrap(),
            r#"<svg xmlns="http://www.w3.org/2000/svg"><g transform="translate(10 10)"><g fill="none"><line x1="0" y1="0" x2="5" y2="5"/></g></g></svg>"#
        );
    }

    #[test]
    fn what_is_inside_anything_but_a_group_waits_for_it_to_close() {
        let partial = r#"<svg xmlns="http://www.w3.org/2000/svg"><rect width="5" height="5"/><text x="1" y="1">One <tspan>two</tspan> thr"#;

        assert_eq!(
            drawable_so_far(partial).unwrap(),
            r#"<svg xmlns="http://www.w3.org/2000/svg"><rect width="5" height="5"/></svg>"#
        );
    }

    #[test]
    fn a_closing_bracket_in_an_attribute_does_not_end_the_tag() {
        let partial = r#"<svg xmlns="http://www.w3.org/2000/svg"><text x="1" y="1" data-note="a > b">hi</text><rect"#;

        assert!(drawable_so_far(partial).unwrap().ends_with("hi</text></svg>"));
    }
}
