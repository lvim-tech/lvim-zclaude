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

const RESET_FG: &str = "\u{1b}[39m";
const BOLD: &str = "\u{1b}[1m";
const UNBOLD: &str = "\u{1b}[22m";
/// The selected row: a background, so the colours in it stay readable. Reverse video would flip
/// every foreground with it and turn the whole line into one block.
const SELECT_BG: &str = "\u{1b}[48;5;237m";
const RESET_BG: &str = "\u{1b}[49m";

// The palette, one place. 256-colour indices rather than the sixteen names, because the named ones
// are whatever the terminal theme made of them and this board is read at a glance.
const C_BUSY: u8 = 78; // green — working right now
const C_IDLE: u8 = 179; // amber — running, waiting for you
const C_STOPPED: u8 = 243; // grey — not running
const C_BRANCH: u8 = 140; // violet
const C_LAST: u8 = 245;
const C_NAME: u8 = 80; // cyan
const C_HERE: u8 = 78; // this session's pane — the one Enter can reach
const C_THERE: u8 = 68; // another zellij session
const C_NONE: u8 = 240;
const C_COUNT: u8 = 244;
const C_TITLE: u8 = 111;
const C_DIM: u8 = 244;
const C_ERROR: u8 = 203;

fn fg(color: u8, text: &str) -> String {
    format!("\u{1b}[38;5;{color}m{text}{RESET_FG}")
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
    if !view.permitted {
        println!(
            "{BOLD}{}{UNBOLD}  {}",
            fg(C_TITLE, "lvim-zclaude"),
            fg(C_DIM, "waiting for permissions — answer the prompt with `y`")
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
        fg(C_TITLE, &left),
        fg(C_NAME, &filter),
        " ".repeat(gap + 1),
        fg(C_DIM, &counts)
    );

    if view.rows.is_empty() {
        println!();
        let message = if view.files == 0 {
            "no sessions in ~/.claude/projects"
        } else {
            "nothing matches the filter"
        };
        println!("  {}", fg(C_DIM, message));
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
    println!("{}", fg(C_DIM, &truncate(&header, cols)));

    // The visible window follows the cursor.
    let body = rows_available.saturating_sub(4).max(1);
    let first = view.cursor.saturating_sub(body.saturating_sub(1)).min(view.rows.len());
    for (index, row) in view.rows.iter().enumerate().skip(first).take(body) {
        let selected = index == view.cursor;
        let mark = if selected { "▸" } else { " " };
        let state = match &row.running {
            Some(running) if running.busy => C_BUSY,
            Some(_) => C_IDLE,
            None => C_STOPPED,
        };
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
        let where_colour = if row.located.is_some() {
            C_HERE
        } else if row.running.is_some() {
            C_THERE
        } else {
            C_NONE
        };
        // A child is one of the older sessions; it is there to be read past, not at.
        let project_colour = if row.child { C_DIM } else { state };

        let mut line = format!(
            "{} {}{}{}{}",
            fg(state, mark),
            fg(state, &pad(row.glyph(), w_st)),
            if row.child {
                fg(project_colour, &pad(&project, w_project))
            } else {
                format!("{BOLD}{}{UNBOLD}", pad(&project, w_project))
            },
            fg(C_BRANCH, &pad(&row.branch, w_branch)),
            fg(C_LAST, &pad(&crate::model::age(view.now, row.last), w_last))
        );
        if !narrow {
            line.push_str(&fg(C_NAME, &pad(&name, w_name)));
            line.push_str(&fg(where_colour, &pad(&where_text, w_where)));
        }
        line.push_str(&fg(C_COUNT, &pad(&count, w_n)));
        if selected {
            println!("{SELECT_BG}{line}{RESET_BG}");
        } else {
            println!("{line}");
        }
    }

    // The selected row's whole truth, which does not fit in the columns.
    if let Some(row) = view.rows.get(view.cursor) {
        let detail = truncate(&format!("  {} · {}", row.cwd, row.id), cols);
        println!("{}", fg(C_DIM, &detail));
    }
    footer(view, cols);
}

fn footer(view: &View, cols: usize) {
    if let Some(error) = view.last_error {
        println!("{}", fg(C_ERROR, &truncate(&format!("  {error}"), cols)));
        return;
    }
    let keys = "↑↓ move · Enter jump · Tab expand · type to filter · r refresh · Esc close";
    println!("{}", fg(C_DIM, &truncate(keys, cols)));
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
        let cell = fg(C_NAME, &pad("bru", 6));
        assert!(cell.starts_with("\u{1b}[38;5;80m"));
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
