// The plugin's keys under another keyboard layout: `Ctrl д` is `Ctrl o` with the Bulgarian (BDS)
// layout on, `э` is `v`. zellij hands a plugin the character, never the key, so the layout is
// undone by table. The tables are the `lvim-zlayout` crate (one file per layout); which ones apply
// is the plugin's `layouts` setting (default `bg-bds`).
//
// Kept identical in lvim-zclaude and lvim-zsession.

pub use lvim_zlayout::Layouts;
use zellij_tile::prelude::*;

/// `Ctrl`/`Alt` + a letter of another layout → the same modifiers + the Latin key. With a modifier
/// the key is never text, so this is always safe; the terminal sends the lowercase letter.
pub fn modified_to_latin(layouts: &Layouts, mut key: KeyWithModifier) -> KeyWithModifier {
    let modified = key.key_modifiers.contains(&KeyModifier::Ctrl)
        || key.key_modifiers.contains(&KeyModifier::Alt);
    if let (true, BareKey::Char(c)) = (modified, key.bare_key) {
        if !c.is_ascii() {
            let lower = c.to_lowercase().next().unwrap_or(c);
            if let Some(latin) = layouts.latin_of(lower) {
                key.bare_key = BareKey::Char(latin);
            }
        }
    }
    key
}

/// A bare letter of another layout → the Latin key, for where a letter is a command rather than
/// text (`v` on the board's list, `y` in a confirmation). Shift is part of the letter (`Х` is `N`).
pub fn plain_to_latin(layouts: &Layouts, key: &KeyWithModifier) -> KeyWithModifier {
    let mut key = key.clone();
    if let BareKey::Char(c) = key.bare_key {
        if let Some(latin) = layouts.latin_of(c) {
            key.bare_key = BareKey::Char(latin);
        }
    }
    key
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bds_keys_are_read_as_the_latin_ones() {
        let bds = Layouts::from_config(Some("bg-bds"));
        let ctrl = |c: char| KeyWithModifier::new(BareKey::Char(c)).with_ctrl_modifier();
        assert_eq!(modified_to_latin(&bds, ctrl('д')), ctrl('o'));
        assert_eq!(modified_to_latin(&bds, ctrl('o')), ctrl('o'));
        let bare = KeyWithModifier::new(BareKey::Char('щ'));
        assert_eq!(plain_to_latin(&bds, &bare).bare_key, BareKey::Char('y'));
        let off = Layouts::from_config(Some("none"));
        assert_eq!(modified_to_latin(&off, ctrl('д')), ctrl('д'));
    }
}
