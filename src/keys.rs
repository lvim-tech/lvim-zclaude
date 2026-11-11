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

#[cfg(test)]
mod tests {
    use super::*;
    use zellij_tile::shim::plugin_api::key::{ProtobufKeyModifier, ProtobufNamedKey};

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
