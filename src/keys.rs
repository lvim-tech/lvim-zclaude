// The one place where a keystroke is turned from zellij's wire format into a `KeyWithModifier`,
// done here rather than by `zellij-tile` because the library gets it wrong.
//
// zellij sends a plugin the key as protobuf: `MainKey::Char(i32)`, the whole code point
// (`character as i32`, `zellij-utils/src/plugin_api/key.rs`). The *decoding* in the same file is
//
//     fn char_index_to_char(char_index: i32) -> char { char_index as u8 as char }
//
// so `б` (U+0431, 1073) comes out as `1` (1073 & 0xFF = 0x31). That decoding does not run in the
// zellij server — it runs **inside this plugin**, in the copy of `zellij-utils` 0.44.3 that
// `zellij-tile` links in, called from its `register_plugin!` macro. Measured 2026-09-20 with zellij
// 0.46.0 and the same line still present in 0.45.1: a Cyrillic letter typed on the board arrived
// as a digit. A patched zellij would not help this plugin at all; a plugin that decodes the key
// itself does not need one. Hence `host.rs`, and this.

use zellij_tile::prelude::*;
use zellij_tile::shim::plugin_api::event::{ProtobufEvent, ProtobufEventPayload, ProtobufEventType};
use zellij_tile::shim::plugin_api::key::{ProtobufKey, ProtobufMainKey};

/// The wire event → the `Event` the plugin handles. A key press is decoded here; every other event
/// goes through `zellij-tile` unchanged.
pub fn event_from_protobuf(event: ProtobufEvent) -> Result<Event, &'static str> {
    if event.name == ProtobufEventType::Key as i32 {
        if let Some(ProtobufEventPayload::KeyPayload(key)) = event.payload {
            return Ok(Event::Key(key_from_protobuf(key)?));
        }
    }
    Event::try_from(event)
}

/// The wire key → `KeyWithModifier`, with the whole code point.
///
/// The character is taken out before the library sees it: the modifiers and the named keys are
/// decoded by `zellij-tile` as ever (with an ASCII letter standing in for the character, which the
/// library cannot damage), and the real character is put back afterwards.
pub fn key_from_protobuf(key: ProtobufKey) -> Result<KeyWithModifier, &'static str> {
    let Some(ProtobufMainKey::Char(code)) = key.main_key else {
        return KeyWithModifier::try_from(key);
    };
    let character = u32::try_from(code)
        .ok()
        .and_then(char::from_u32)
        .ok_or("a key whose character is not a code point")?;
    let placeholder = ProtobufKey {
        main_key: Some(ProtobufMainKey::Char('a' as i32)),
        ..key
    };
    let mut decoded = KeyWithModifier::try_from(placeholder)?;
    decoded.bare_key = BareKey::Char(character);
    Ok(decoded)
}

/// **A key as the bytes a terminal would send for it** — what the preview writes into the
/// agent's pane when it acts as that pane's terminal (`v`). The legacy encoding, not kitty's:
/// it is what every program reads, and Claude Code's prompt reacts to `1`, `\r`, `\e`, `\e[A`.
///
/// `Ctrl` + a letter is the letter's low five bits (`Ctrl c` → 0x03), `Alt` + a key is `\e`
/// before the key, `Shift Tab` is `\e[Z`. `None` for a key that has no bytes (`CapsLock`, a
/// lone `Super`), which is then simply not sent.
pub fn bytes_of(key: &KeyWithModifier) -> Option<Vec<u8>> {
    let ctrl = key.key_modifiers.contains(&KeyModifier::Ctrl);
    let alt = key.key_modifiers.contains(&KeyModifier::Alt);
    let shift = key.key_modifiers.contains(&KeyModifier::Shift);
    let mut bytes: Vec<u8> = match key.bare_key {
        BareKey::Char(c) if ctrl => {
            // `Ctrl @`..`Ctrl _` are 0x00..0x1F; `Ctrl ?` is DEL. Anything else with Ctrl has no
            // legacy byte, so the character itself goes — better than swallowing the key.
            let upper = c.to_ascii_uppercase();
            match upper {
                '@'..='_' => vec![upper as u8 & 0x1f],
                '?' => vec![0x7f],
                _ => c.to_string().into_bytes(),
            }
        }
        BareKey::Char(c) => c.to_string().into_bytes(),
        BareKey::Enter => vec![b'\r'],
        BareKey::Esc => vec![0x1b],
        BareKey::Tab if shift => b"\x1b[Z".to_vec(),
        BareKey::Tab => vec![b'\t'],
        BareKey::Backspace => vec![0x7f],
        BareKey::Up => b"\x1b[A".to_vec(),
        BareKey::Down => b"\x1b[B".to_vec(),
        BareKey::Right => b"\x1b[C".to_vec(),
        BareKey::Left => b"\x1b[D".to_vec(),
        BareKey::Home => b"\x1b[H".to_vec(),
        BareKey::End => b"\x1b[F".to_vec(),
        BareKey::Insert => b"\x1b[2~".to_vec(),
        BareKey::Delete => b"\x1b[3~".to_vec(),
        BareKey::PageUp => b"\x1b[5~".to_vec(),
        BareKey::PageDown => b"\x1b[6~".to_vec(),
        BareKey::F(n @ 1..=4) => format!("\x1bO{}", (b'P' + n - 1) as char).into_bytes(),
        BareKey::F(n @ 5..=12) => {
            let code = match n {
                5 => 15,
                6 => 17,
                7 => 18,
                8 => 19,
                9 => 20,
                10 => 21,
                11 => 23,
                _ => 24,
            };
            format!("\x1b[{code}~").into_bytes()
        }
        _ => return None,
    };
    if alt {
        bytes.insert(0, 0x1b);
    }
    Some(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use zellij_tile::shim::plugin_api::key::{ProtobufKeyModifier, ProtobufNamedKey};

    /// The bytes a terminal sends: what makes the preview a terminal and not a picture.
    #[test]
    fn a_key_becomes_the_bytes_a_terminal_would_send() {
        let plain = |key: BareKey| KeyWithModifier::new(key);
        assert_eq!(bytes_of(&plain(BareKey::Char('1'))), Some(b"1".to_vec()), "an answer to a prompt");
        assert_eq!(bytes_of(&plain(BareKey::Char('ж'))), Some("ж".as_bytes().to_vec()), "UTF-8, whole");
        assert_eq!(bytes_of(&plain(BareKey::Enter)), Some(b"\r".to_vec()), "CR, as a terminal sends Enter");
        assert_eq!(bytes_of(&plain(BareKey::Esc)), Some(vec![0x1b]));
        assert_eq!(bytes_of(&plain(BareKey::Up)), Some(b"\x1b[A".to_vec()));
        assert_eq!(bytes_of(&plain(BareKey::Backspace)), Some(vec![0x7f]));
        assert_eq!(bytes_of(&plain(BareKey::Tab)), Some(b"\t".to_vec()));
        assert_eq!(bytes_of(&plain(BareKey::PageDown)), Some(b"\x1b[6~".to_vec()));
        assert_eq!(bytes_of(&plain(BareKey::F(1))), Some(b"\x1bOP".to_vec()));
        assert_eq!(bytes_of(&plain(BareKey::F(5))), Some(b"\x1b[15~".to_vec()));
        assert_eq!(bytes_of(&plain(BareKey::CapsLock)), None, "nothing to send");

        let with = |key: BareKey, m: KeyModifier| KeyWithModifier::new_with_modifiers(key, [m].into_iter().collect());
        assert_eq!(bytes_of(&with(BareKey::Char('c'), KeyModifier::Ctrl)), Some(vec![0x03]), "Ctrl c interrupts");
        assert_eq!(bytes_of(&with(BareKey::Char(']'), KeyModifier::Ctrl)), Some(vec![0x1d]));
        assert_eq!(bytes_of(&with(BareKey::Tab, KeyModifier::Shift)), Some(b"\x1b[Z".to_vec()), "Shift Tab, Claude's 'don't ask again'");
        assert_eq!(bytes_of(&with(BareKey::Char('x'), KeyModifier::Alt)), Some(b"\x1bx".to_vec()));
        assert_eq!(bytes_of(&with(BareKey::Enter, KeyModifier::Alt)), Some(b"\x1b\r".to_vec()));
    }

    fn char_key(character: char, modifier: Option<ProtobufKeyModifier>) -> ProtobufKey {
        ProtobufKey {
            modifier: modifier.map(|m| m as i32),
            additional_modifiers: vec![],
            main_key: Some(ProtobufMainKey::Char(character as i32)),
        }
    }

    fn key_event(key: ProtobufKey) -> ProtobufEvent {
        ProtobufEvent {
            name: ProtobufEventType::Key as i32,
            payload: Some(ProtobufEventPayload::KeyPayload(key)),
        }
    }

    #[test]
    fn a_cyrillic_letter_arrives_whole() {
        let key = key_from_protobuf(char_key('б', None)).unwrap();
        assert_eq!(key.bare_key, BareKey::Char('б'));
        assert!(key.key_modifiers.is_empty());
    }

    #[test]
    fn ascii_is_unchanged() {
        let key = key_from_protobuf(char_key('i', None)).unwrap();
        assert_eq!(key.bare_key, BareKey::Char('i'));
    }

    #[test]
    fn the_modifiers_survive_the_detour() {
        let key = key_from_protobuf(char_key('ж', Some(ProtobufKeyModifier::Ctrl))).unwrap();
        assert_eq!(key.bare_key, BareKey::Char('ж'));
        assert!(key.key_modifiers.contains(&KeyModifier::Ctrl));
        assert_eq!(key.key_modifiers.len(), 1);
    }

    #[test]
    fn a_named_key_goes_through_the_library() {
        let key = key_from_protobuf(ProtobufKey {
            modifier: None,
            additional_modifiers: vec![],
            main_key: Some(ProtobufMainKey::Key(ProtobufNamedKey::Enter as i32)),
        })
        .unwrap();
        assert_eq!(key.bare_key, BareKey::Enter);
    }

    #[test]
    fn a_code_that_is_no_character_is_refused_not_mangled() {
        let surrogate = ProtobufKey {
            modifier: None,
            additional_modifiers: vec![],
            main_key: Some(ProtobufMainKey::Char(0xD800)),
        };
        assert!(key_from_protobuf(surrogate).is_err());
        let negative = ProtobufKey {
            modifier: None,
            additional_modifiers: vec![],
            main_key: Some(ProtobufMainKey::Char(-1)),
        };
        assert!(key_from_protobuf(negative).is_err());
    }

    #[test]
    fn a_key_event_is_decoded_here_and_the_rest_by_the_library() {
        match event_from_protobuf(key_event(char_key('ю', None))).unwrap() {
            Event::Key(key) => assert_eq!(key.bare_key, BareKey::Char('ю')),
            other => panic!("not a key: {other:?}"),
        }
        let visible = ProtobufEvent {
            name: ProtobufEventType::Visible as i32,
            payload: Some(ProtobufEventPayload::VisiblePayload(true)),
        };
        assert_eq!(event_from_protobuf(visible).unwrap(), Event::Visible(true));
    }

    /// The reason this module exists. When this test fails, `zellij-tile` has been fixed: `host.rs`
    /// can go back to `register_plugin!` and this file can go.
    #[test]
    fn the_library_still_truncates_the_same_key() {
        let decoded = Event::try_from(key_event(char_key('б', None))).unwrap();
        assert_eq!(
            decoded,
            Event::Key(KeyWithModifier::new(BareKey::Char('1'))),
            "zellij-tile no longer truncates the character — keys.rs and host.rs are obsolete"
        );
    }
}
