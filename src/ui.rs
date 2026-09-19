// Drawing the board.
//
// Plain `println!` with ANSI, not zellij's `Table`: the columns have to be cut to the pane's width
// (a wrapped row makes the cursor look as if it skipped a line), and cutting is this file's job
// either way. `render` is handed the content size, so everything below is written to fit `cols`
// and `rows` exactly.
//
// **Colour is put on a cell only after it has been padded**, and the closing code is a foreground
// reset (`39`) rather than a full reset (`0`). Both rules have the same reason: a full reset would
// also drop the background of a selected row, and colouring before padding would count escape
// bytes as characters and tear every column out of line.

use crate::model::Row;
use zellij_tile::prelude::{PaletteColor, Styling};

const BOLD: &str = "\u{1b}[1m";
const UNBOLD: &str = "\u{1b}[22m";
const RESET_FG: &str = "\u{1b}[39m";
const RESET_BG: &str = "\u{1b}[49m";

/// **Every colour on the board comes from the active zellij theme**, which on this machine is
/// generated from lvim-colorscheme — the board changes with the theme, the way `lvim-zbar` does, and
/// owns no colour of its own.
///
/// The accents are read from `multiplayer_user_colors`, not from `text_unselected` and the rest,
/// for `lvim-zbar`'s reason: the generator writes contrast-floored versions of the palette into the
/// text slots and keeps the *raw* palette only there. On the `lvim` theme the players are
/// `2` blue, `3` green, `5` violet, `6` teal, `8` yellow, `9` red, `10` a muted grey-green.
#[derive(Clone, Copy)]
pub struct Palette {
    pub busy: PaletteColor,
    pub idle: PaletteColor,
    pub muted: PaletteColor,
    pub branch: PaletteColor,
    pub name: PaletteColor,
    pub here: PaletteColor,
    pub there: PaletteColor,
    pub title: PaletteColor,
    pub error: PaletteColor,
    pub selected: PaletteColor,
}

impl Palette {
    pub fn from_style(style: &Styling) -> Palette {
        let players = style.multiplayer_user_colors;
        Palette {
            busy: players.player_3,
            idle: players.player_8,
            muted: players.player_10,
            branch: players.player_5,
            name: players.player_6,
            here: players.player_3,
            there: players.player_2,
            title: players.player_2,
            error: players.player_9,
            selected: style.text_selected.background,
        }
    }
}

/// A colour as an escape for layer `38` (foreground) or `48` (background) — `lvim-zbar`'s `ansi()`.
fn ansi(color: PaletteColor, layer: u8) -> String {
    match color {
        PaletteColor::Rgb((r, g, b)) => format!("\u{1b}[{layer};2;{r};{g};{b}m"),
        PaletteColor::EightBit(n) => format!("\u{1b}[{layer};5;{n}m"),
    }
}

/// Text in a theme colour — or plain, before zellij has said what the theme is.
fn fg(color: Option<PaletteColor>, text: &str) -> String {
    match color {
        Some(color) => format!("{}{text}{RESET_FG}", ansi(color, 38)),
        None => text.to_string(),
    }
}

pub struct View<'a> {
    pub rows: &'a [Row],
    pub cursor: usize,
    pub filter: &'a str,
    pub session_name: Option<&'a str>,
    pub now: u64,
    pub running: usize,
    pub files: usize,
    pub last_error: Option<&'a str>,
    pub permitted: bool,
    /// `None` until the first `ModeUpdate` — the board then draws without colour rather than guess.
    pub palette: Option<Palette>,
}

/// Cut to `width` **by characters**, with an ellipsis when something was cut.
///
/// By characters and not bytes: a project path can hold anything, and slicing a `String` on a byte
/// that is inside a character panics — in a plugin that is the whole board gone.
pub fn truncate(text: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    if text.chars().count() <= width {
        return text.to_string();
    }
    let mut out: String = text.chars().take(width.saturating_sub(1)).collect();
    out.push('…');
    out
}

fn pad(text: &str, width: usize) -> String {
    let text = truncate(text, width);
    let count = text.chars().count();
    format!("{text}{}", " ".repeat(width.saturating_sub(count)))
}

pub fn draw(view: &View, rows_available: usize, cols: usize) {
    let pal = view.palette;
    if !view.permitted {
        println!(
            "{BOLD}{}{UNBOLD}  {}",
            fg(pal.map(|p| p.title), "lvim-zclaude"),
            fg(pal.map(|p| p.muted), "waiting for permissions — answer the prompt with `y`")
        );
        return;
    }

    // Header: what this is, what is typed, and the two counts.
    let counts = format!("{} running · {} files", view.running, view.files);
    let filter = if view.filter.is_empty() {
        String::new()
    } else {
        format!("filter: {}", view.filter)
    };
    let left = truncate("lvim-zclaude · Claude Code sessions", cols);
    let gap = cols
        .saturating_sub(left.chars().count() + filter.chars().count() + counts.chars().count() + 2);
    println!(
        "{BOLD}{}{UNBOLD} {}{}{}",
        fg(pal.map(|p| p.title), &left),
        fg(pal.map(|p| p.name), &filter),
        " ".repeat(gap + 1),
        fg(pal.map(|p| p.muted), &counts)
    );

    if view.rows.is_empty() {
        println!();
        let message = if view.files == 0 {
            "no sessions in ~/.claude/projects"
        } else {
            "nothing matches the filter"
        };
        println!("  {}", fg(pal.map(|p| p.muted), message));
        footer(view, cols);
        return;
    }

    // Columns. Everything but PROJECT and NAME is fixed; those two share what is left.
    let narrow = cols < 60;
    let (w_st, w_branch, w_last, w_where, w_n) = (3, 10, 5, 16, 4);
    let fixed = w_st + w_branch + w_last + w_n + if narrow { 0 } else { w_where } + 6;
    let flexible = cols.saturating_sub(fixed);
    let w_project = if narrow { flexible } else { (flexible * 3) / 5 };
    let w_name = flexible.saturating_sub(w_project);

    let mut header = format!(
        "  {}{}{}{}",
        pad("ST", w_st),
        pad("PROJECT", w_project),
        pad("BRANCH", w_branch),
        pad("LAST", w_last)
    );
    if !narrow {
        header.push_str(&pad("NAME", w_name));
        header.push_str(&pad("WHERE", w_where));
    }
    header.push_str(&pad("N", w_n));
    println!("{}", fg(pal.map(|p| p.muted), &truncate(&header, cols)));

    // The visible window follows the cursor.
    let body = rows_available.saturating_sub(4).max(1);
    let first = view.cursor.saturating_sub(body.saturating_sub(1)).min(view.rows.len());
    for (index, row) in view.rows.iter().enumerate().skip(first).take(body) {
        let selected = index == view.cursor;
        let mark = if selected { "▸" } else { " " };
        let state = pal.map(|p| match &row.running {
            Some(running) if running.busy => p.busy,
            Some(_) => p.idle,
            None => p.muted,
        });
        let project = if row.child {
            format!("  {}", crate::model::age(view.now, row.last))
        } else {
            row.project.clone()
        };
        let name = match &row.running {
            Some(running) => running.name.clone(),
            None if row.child => row.id.chars().take(8).collect(),
            None => "—".to_string(),
        };
        let count = if row.child { String::new() } else { format!("×{}", row.count) };
        let where_text = row.where_text(view.session_name);
        let where_colour = pal.map(|p| {
            if row.located.is_some() {
                p.here
            } else if row.running.is_some() {
                p.there
            } else {
                p.muted
            }
        });
        // A child is one of the older sessions; it is there to be read past, not at.
        let project_colour = pal.map(|p| p.muted);

        let mut line = format!(
            "{} {}{}{}{}",
            fg(state, mark),
            fg(state, &pad(row.glyph(), w_st)),
            if row.child {
                fg(project_colour, &pad(&project, w_project))
            } else {
                format!("{BOLD}{}{UNBOLD}", pad(&project, w_project))
            },
            fg(pal.map(|p| p.branch), &pad(&row.branch, w_branch)),
            fg(pal.map(|p| p.muted), &pad(&crate::model::age(view.now, row.last), w_last))
        );
        if !narrow {
            line.push_str(&fg(pal.map(|p| p.name), &pad(&name, w_name)));
            line.push_str(&fg(where_colour, &pad(&where_text, w_where)));
        }
        line.push_str(&fg(pal.map(|p| p.muted), &pad(&count, w_n)));
        match (selected, pal) {
            (true, Some(p)) => println!("{}{line}{RESET_BG}", ansi(p.selected, 48)),
            // No theme yet: the one row that must stand out still does.
            (true, None) => println!("\u{1b}[7m{line}\u{1b}[27m"),
            _ => println!("{line}"),
        }
    }

    // The selected row's whole truth, which does not fit in the columns.
    if let Some(row) = view.rows.get(view.cursor) {
        let detail = truncate(&format!("  {} · {}", row.cwd, row.id), cols);
        println!("{}", fg(pal.map(|p| p.muted), &detail));
    }
    footer(view, cols);
}

fn footer(view: &View, cols: usize) {
    let pal = view.palette;
    if let Some(error) = view.last_error {
        println!("{}", fg(pal.map(|p| p.error), &truncate(&format!("  {error}"), cols)));
        return;
    }
    let keys = "↑↓ move · Enter jump · Tab expand · type to filter · r refresh · Esc close";
    println!("{}", fg(pal.map(|p| p.muted), &truncate(keys, cols)));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Cutting on a byte inside a character panics, and a panic in a plugin is the board gone.
    #[test]
    fn truncation_counts_characters_not_bytes() {
        assert_eq!(truncate("bru", 10), "bru");
        assert_eq!(truncate("abcdef", 4), "abc…");
        assert_eq!(truncate("", 4), "");
        assert_eq!(truncate("abc", 0), "");
        // Cyrillic is two bytes a character; a byte slice at 4 would be inside one.
        assert_eq!(truncate("Ефективност", 4), "Ефе…");
        assert_eq!(truncate("●", 1), "●");
    }

    #[test]
    fn padding_fills_to_the_column_and_never_past_it() {
        assert_eq!(pad("a", 4), "a   ");
        assert_eq!(pad("abcdef", 4), "abc…");
        assert_eq!(pad("Ефе", 4).chars().count(), 4);
    }

    /// **Colour goes on after padding, and closes the foreground only.** A cell coloured before
    /// padding would be padded to its escape bytes and pull every column out of line; a full reset
    /// would take the selected row's background down with it.
    #[test]
    fn colour_wraps_a_padded_cell_without_changing_its_width() {
        let cell = fg(Some(PaletteColor::Rgb((53, 123, 109))), &pad("bru", 6));
        assert!(cell.starts_with("\u{1b}[38;2;53;123;109m"), "the theme's own RGB, as given");
        assert!(cell.ends_with(RESET_FG), "a foreground reset, not \\x1b[0m");
        assert!(!cell.contains("\u{1b}[0m"));
        let visible: String = {
            let mut out = String::new();
            let mut chars = cell.chars();
            while let Some(c) = chars.next() {
                if c == '\u{1b}' {
                    for c in chars.by_ref() {
                        if c.is_ascii_alphabetic() {
                            break;
                        }
                    }
                } else {
                    out.push(c);
                }
            }
            out
        };
        assert_eq!(visible, "bru   ", "six columns of text, whatever the escapes cost");
    }
}
