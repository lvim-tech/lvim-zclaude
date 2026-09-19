// Drawing the board.
//
// Plain `println!` with ANSI, not zellij's `Table`: the columns have to be cut to the pane's width
// (a wrapped row makes the cursor look as if it skipped a line), and cutting is this file's job
// either way. `render` is handed the content size, so everything below is written to fit `cols`
// and `rows` exactly.

use crate::model::Row;

const DIM: &str = "\u{1b}[2m";
const BOLD: &str = "\u{1b}[1m";
const RED: &str = "\u{1b}[31m";
const SELECTED: &str = "\u{1b}[7m";
const RESET: &str = "\u{1b}[0m";

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
        println!("{BOLD}lvim-zclaude{RESET}  waiting for permissions — answer the prompt with `y`");
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
    println!("{BOLD}{left}{RESET} {filter}{}{DIM}{counts}{RESET}", " ".repeat(gap + 1));

    if view.rows.is_empty() {
        println!();
        let message = if view.files == 0 {
            "no sessions in ~/.claude/projects"
        } else {
            "nothing matches the filter"
        };
        println!("  {DIM}{message}{RESET}");
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
    println!("{DIM}{}{RESET}", truncate(&header, cols));

    // The visible window follows the cursor.
    let body = rows_available.saturating_sub(4).max(1);
    let first = view.cursor.saturating_sub(body.saturating_sub(1)).min(view.rows.len());
    for (index, row) in view.rows.iter().enumerate().skip(first).take(body) {
        let selected = index == view.cursor;
        let mark = if selected { "▸" } else { " " };
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

        let mut line = format!(
            "{mark} {}{}{}{}",
            pad(row.glyph(), w_st),
            pad(&project, w_project),
            pad(&row.branch, w_branch),
            pad(&crate::model::age(view.now, row.last), w_last)
        );
        if !narrow {
            line.push_str(&pad(&name, w_name));
            line.push_str(&pad(&row.where_text(view.session_name), w_where));
        }
        line.push_str(&pad(&count, w_n));
        let line = truncate(&line, cols);
        if selected {
            println!("{SELECTED}{line}{RESET}");
        } else {
            println!("{line}");
        }
    }

    // The selected row's whole truth, which does not fit in the columns.
    if let Some(row) = view.rows.get(view.cursor) {
        println!("{DIM}{}{RESET}", truncate(&format!("  {} · {}", row.cwd, row.id), cols));
    }
    footer(view, cols);
}

fn footer(view: &View, cols: usize) {
    if let Some(error) = view.last_error {
        println!("{RED}{}{RESET}", truncate(&format!("  {error}"), cols));
        return;
    }
    let keys = "↑↓ move · Enter jump · Tab expand · type to filter · r refresh · Esc close";
    println!("{DIM}{}{RESET}", truncate(keys, cols));
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
}
