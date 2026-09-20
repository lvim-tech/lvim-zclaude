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

use crate::model::{Kind, Order, Question, Row};
use crate::scan::HookState;
use std::collections::BTreeMap;
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
/// `2` blue, `3` green, `5` violet, `6` teal, `8` yellow, `9` red.
///
/// **The neutral grey is the one exception**: `text_unselected.base`, the theme's own foreground and
/// the grey `lvim-zbar` writes its bar in. `player_10` was tried first; on EverforestDark it is
/// `#717e6a`, an olive that reads as brown next to the theme's grey-green text.
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
    /// The column headings — the theme's own table heading, not the grey of the data under them.
    pub heading: PaletteColor,
    /// The keys in the footer and the arrow on the selected row: the one accent that says
    /// "this is yours to press".
    pub key: PaletteColor,
    /// The middle step of the age column: older than a few minutes, younger than an hour.
    pub aging: PaletteColor,
    /// The project column: the name of the thing each row is about.
    pub project: PaletteColor,
}

impl Palette {
    /// The board's colours: each one a **named slot of the active theme**, never a colour of the
    /// board's own. The defaults below are the slots; `zpm.kdl` can point any of them somewhere
    /// else with `colour_<part> "<slot>"` — see [`slot`] for the names.
    pub fn from_style(style: &Styling, configuration: &BTreeMap<String, String>) -> Palette {
        let players = style.multiplayer_user_colors;
        let pick = |part: &str, fallback: PaletteColor| {
            configuration
                .get(&format!("colour_{part}"))
                .or_else(|| configuration.get(&format!("color_{part}")))
                .and_then(|name| slot(style, name.trim()))
                .unwrap_or(fallback)
        };
        Palette {
            busy: pick("busy", players.player_3),
            idle: pick("idle", style.text_unselected.emphasis_2),
            muted: pick("muted", style.text_unselected.base),
            branch: pick("branch", players.player_5),
            name: pick("name", players.player_6),
            // The project names down the left — the board's headline column, in the theme's
            // yellow. `emphasis_2` was tried first; on EverforestDark it is `#94992b`, which reads
            // as green. `player_8` (`#af9e6b`) is the yellow of that palette.
            project: pick("project", players.player_8),
            here: pick("here", players.player_3),
            there: pick("there", players.player_2),
            title: pick("title", players.player_2),
            error: pick("error", players.player_9),
            selected: pick("selected", style.text_selected.background),
            heading: pick("heading", players.player_1),
            key: pick("key", players.player_4),
            aging: pick("aging", players.player_7),
        }
    }

    /// The colour of a time: how long ago something last moved.
    ///
    /// Three steps rather than a gradient, because a column four characters wide is read at a
    /// glance and a glance sees "now", "a while", "long ago" — nothing finer.
    pub fn age_colour(&self, seconds: Option<u64>) -> PaletteColor {
        match seconds {
            Some(0..=299) => self.busy,
            Some(300..=3599) => self.aging,
            _ => self.muted,
        }
    }
}

/// A theme slot by name: what `colour_<part>` in the configuration may point at.
///
/// The ten `player_N` are the raw palette (the theme generator keeps the unfloored colours only
/// there); the rest are the slots zellij itself draws with. An unknown name is no colour at all,
/// and the default stands — a typo in a config file must not repaint the board at random.
pub fn slot(style: &Styling, name: &str) -> Option<PaletteColor> {
    let players = style.multiplayer_user_colors;
    Some(match name {
        "player_1" => players.player_1,
        "player_2" => players.player_2,
        "player_3" => players.player_3,
        "player_4" => players.player_4,
        "player_5" => players.player_5,
        "player_6" => players.player_6,
        "player_7" => players.player_7,
        "player_8" => players.player_8,
        "player_9" => players.player_9,
        "player_10" => players.player_10,
        "fg" | "text" => style.text_unselected.base,
        "bg" => style.text_unselected.background,
        "emphasis_0" => style.text_unselected.emphasis_0,
        "emphasis_1" => style.text_unselected.emphasis_1,
        "emphasis_2" => style.text_unselected.emphasis_2,
        "emphasis_3" => style.text_unselected.emphasis_3,
        "selected_fg" => style.text_selected.base,
        "selected_bg" => style.text_selected.background,
        "table_title" => style.table_title.base,
        "frame_selected" => style.frame_selected.base,
        "frame_highlight" => style.frame_highlight.base,
        "success" => style.exit_code_success.base,
        "failure" => style.exit_code_error.base,
        _ => return None,
    })
}

/// Seconds between the two times, or `None` when the time makes no sense (no time at all, or one
/// from the future — a machine whose clock moved).
pub fn since(now: u64, then: u64) -> Option<u64> {
    if then == 0 || now < then {
        None
    } else {
        Some(now - then)
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
    /// What the selected agent's pane calls itself — Claude keeps its own title there. Metadata,
    /// never a word of the conversation.
    pub activity: Option<&'a str>,
    /// A question waiting for `y`: stopping an agent or closing its pane.
    pub prompt: Option<&'a str>,
    /// The input line: its label and what is typed — a line for an agent (`send: `), or the
    /// answer to a question (`resume bru · 23aa89d8 — in session (ROOT, EXAMS): `).
    pub input: Option<(&'a str, &'a str)>,
    /// A line under the list in place of the row's details, when the row has none to show (a
    /// session heading): what the keys do there.
    pub hint: Option<String>,
    /// After the selected row's details: what its hooks say, what is queued for it.
    pub note: Option<String>,
    /// How the agents are arranged — in the title, and the `TAB` column names the session when
    /// the rows do not sit under their session's heading.
    pub order: Order,
    /// `None` until the first `ModeUpdate` — the board then draws without colour rather than guess.
    pub palette: Option<Palette>,
    /// An agent's screen over the list (`v`).
    pub preview: Option<PreviewView<'a>>,
    /// Under the list, for the agent under the cursor: the last thing on its screen, or the
    /// question it is asking with the buttons to answer it.
    pub peek: Option<PeekView<'a>>,
}

/// The agent's screen as the preview shows it: **the pane's own colours** (`dump-screen --ansi`),
/// every key going to the pane. `scroll` is lines up from the bottom.
pub struct PreviewView<'a> {
    pub title: &'a str,
    pub lines: &'a [String],
    pub scroll: usize,
}

/// What sits under the list for the agent under the cursor.
pub struct PeekView<'a> {
    /// The last non-blank lines of its screen (or its last notifications, when the screen cannot
    /// be read), oldest first. Shown when there is no question.
    pub tail: &'a [String],
    /// The question on its screen, when the screen ends with numbered options.
    pub question: Option<&'a Question>,
    /// `a` has been pressed: the digits now answer, not jump.
    pub answering: bool,
}

/// How many lines the block under the list takes: the question and its buttons (and a hint
/// while answering), or the tail — none when there is nothing to show.
pub fn peek_height(peek: Option<&PeekView>) -> usize {
    match peek {
        None => 0,
        Some(peek) => match peek.question {
            Some(_) if peek.answering => 3,
            Some(_) => 2,
            None => peek.tail.len().min(crate::PEEK_MAX_LINES),
        },
    }
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

/// One piece of a styled line: an escape sequence (kept whole, counts for nothing on the screen)
/// or a character that takes a column.
enum Piece {
    Escape(String),
    Char(char),
}

/// A line with ANSI escapes split into its pieces. CSI (`\e[ … final`) and OSC (`\e] … BEL` or
/// `\e\`) are recognised; any other `\e` + one character is kept as a pair. What `dump-screen
/// --ansi` writes is CSI only (`grid.rs` in zellij 0.45), the rest is for whatever else lands here.
fn pieces(text: &str) -> Vec<Piece> {
    let mut out = Vec::new();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\u{1b}' {
            out.push(Piece::Char(c));
            continue;
        }
        let mut escape = String::from(c);
        match chars.next() {
            Some('[') => {
                escape.push('[');
                for d in chars.by_ref() {
                    escape.push(d);
                    if ('\u{40}'..='\u{7e}').contains(&d) {
                        break;
                    }
                }
            }
            Some(']') => {
                escape.push(']');
                while let Some(d) = chars.next() {
                    escape.push(d);
                    if d == '\u{7}' {
                        break;
                    }
                    if d == '\u{1b}' && chars.peek() == Some(&'\\') {
                        escape.push(chars.next().unwrap());
                        break;
                    }
                }
            }
            Some(d) => escape.push(d),
            None => {}
        }
        out.push(Piece::Escape(escape));
    }
    out
}

/// Cut a **styled** line to `width` visible columns: the escapes are copied and not counted, the
/// characters are counted, an ellipsis marks a cut — and a line that carried any escape ends
/// with a full reset, because `dump-screen --ansi` leaves the last colour open at the end of a
/// line (it closes only the whole dump) and the next line the board prints would inherit it.
pub fn cut_ansi(text: &str, width: usize) -> String {
    let pieces = pieces(text);
    let visible = pieces.iter().filter(|p| matches!(p, Piece::Char(_))).count();
    let styled = pieces.iter().any(|p| matches!(p, Piece::Escape(_)));
    let keep = if visible <= width { visible } else { width.saturating_sub(1) };
    let mut out = String::new();
    let mut seen = 0;
    for piece in &pieces {
        match piece {
            Piece::Escape(escape) => out.push_str(escape),
            Piece::Char(c) => {
                if seen < keep {
                    out.push(*c);
                    seen += 1;
                }
            }
        }
    }
    if visible > width && width > 0 {
        out.push('…');
    }
    if styled {
        out.push_str("\u{1b}[0m");
    }
    if width == 0 {
        return String::new();
    }
    out
}

/// The visible characters of a styled line — what `cut_ansi` counts.
pub fn strip_ansi(text: &str) -> String {
    pieces(text)
        .into_iter()
        .filter_map(|p| match p {
            Piece::Char(c) => Some(c),
            Piece::Escape(_) => None,
        })
        .collect()
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

    if let Some(preview) = view.preview.as_ref() {
        draw_preview(view, preview, rows_available, cols);
        return;
    }

    // Header: what this is, what is typed, and the two counts.
    let counts = format!("{} running · {} files", view.running, view.files);
    let filter = if view.filter.is_empty() {
        String::new()
    } else {
        format!("filter: {}", view.filter)
    };
    let left = truncate(&format!("lvim-zclaude · agents {}", view.order.label()), cols);
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
    // Two columns before ST: the cursor mark and the digit — the digit with a space after it,
    // so `1 ◐ rust` and not `1◐ rust`; a row without a digit has the same two spaces.
    let (w_st, w_branch, w_last, w_where, w_n) = (3, 10, 5, 16, 4);
    let fixed = w_st + w_branch + w_last + w_n + if narrow { 0 } else { w_where } + 7;
    let flexible = cols.saturating_sub(fixed);
    let w_project = if narrow { flexible } else { (flexible * 3) / 5 };
    let w_name = flexible.saturating_sub(w_project);

    let mut header = format!(
        "   {}{}{}{}",
        pad("ST", w_st),
        pad("PROJECT", w_project),
        pad("BRANCH", w_branch),
        pad("LAST", w_last)
    );
    if !narrow {
        header.push_str(&pad("NAME", w_name));
        header.push_str(&pad("TAB", w_where));
    }
    header.push_str(&pad("N", w_n));
    println!("{}", fg(pal.map(|p| p.heading), &truncate(&header, cols)));

    // The visible window follows the cursor. Title, column headings, the detail line and the
    // footer are four; the block under the list takes what it takes.
    let body = rows_available
        .saturating_sub(4 + peek_height(view.peek.as_ref()))
        .max(1);
    let first = view.cursor.saturating_sub(body.saturating_sub(1)).min(view.rows.len());
    // The first nine agents and transcripts are numbered: a digit jumps to them.
    let mut numbers: Vec<Option<usize>> = Vec::with_capacity(view.rows.len());
    let mut nth = 0;
    for row in view.rows {
        numbers.push(if row.is_header() {
            None
        } else {
            nth += 1;
            (nth <= 9).then_some(nth)
        });
    }
    for (index, row) in view.rows.iter().enumerate().skip(first).take(body) {
        let selected = index == view.cursor;
        // A heading: the zellij session (or the stopped block) and what is under it — `▾` open,
        // `▸` folded. Selected, it is marked and backed like any other selected row.
        if let Kind::Header { agents, busy, folded, .. } = &row.kind {
            let what = if row.project == crate::model::STOPPED {
                format!("{agents} project{}", if *agents == 1 { "" } else { "s" })
            } else {
                format!(
                    "{agents} agent{} · {busy} busy",
                    if *agents == 1 { "" } else { "s" }
                )
            };
            // Cut the two texts, not the coloured line: an escape cut in half is garbage.
            let room = cols.saturating_sub(4);
            let name = truncate(&row.project, room);
            let what = truncate(&what, room.saturating_sub(name.chars().count() + 1));
            let mark = if selected { "▸" } else { " " };
            let fold = if *folded { "▸" } else { "▾" };
            let line = format!(
                "{}{} {BOLD}{}{UNBOLD} {}",
                fg(pal.map(|p| p.key), mark),
                fg(pal.map(|p| p.title), fold),
                fg(pal.map(|p| p.title), &name),
                fg(pal.map(|p| p.muted), &what)
            );
            let width = 3 + name.chars().count() + 1 + what.chars().count();
            let fill = " ".repeat(cols.saturating_sub(width));
            match (selected, pal) {
                (true, Some(p)) => println!("{}{line}{fill}{RESET_BG}", ansi(p.selected, 48)),
                (true, None) => println!("\u{1b}[7m{line}{fill}\u{1b}[27m"),
                _ => println!("{line}"),
            }
            continue;
        }
        let mark = if selected { "▸" } else { " " };
        let mark_colour = if selected { pal.map(|p| p.key) } else { None };
        // The digit that jumps here, in the column between the mark and the glyph.
        let number = numbers[index].map(|n| format!("{n} ")).unwrap_or_else(|| "  ".to_string());
        let state = pal.map(|p| match (row.hook_state(), &row.running) {
            (Some(HookState::Waiting), _) => p.error,
            (Some(HookState::Done), _) => p.aging,
            (None, Some(running)) if running.busy => p.busy,
            (None, Some(_)) => p.idle,
            (None, None) => p.muted,
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
        let where_text = row.where_text(view.session_name, view.order != Order::Session);
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

        // A busy agent is bold as well as coloured: on a board of a dozen rows the one that is
        // working has to be findable without reading the column.
        // Bold when it needs eyes: working, or waiting for a person.
        let glyph = match (row.hook_state(), &row.running) {
            (Some(HookState::Waiting), _) => format!("{BOLD}{}{UNBOLD}", pad(row.glyph(), w_st)),
            (None, Some(running)) if running.busy => format!("{BOLD}{}{UNBOLD}", pad(row.glyph(), w_st)),
            _ => pad(row.glyph(), w_st),
        };
        let age_colour = pal.map(|p| p.age_colour(since(view.now, row.last)));
        let mut line = format!(
            "{}{}{}{}{}{}",
            fg(mark_colour, mark),
            fg(pal.map(|p| p.key), &number),
            fg(state, &glyph),
            if row.child {
                fg(project_colour, &pad(&project, w_project))
            } else {
                format!("{BOLD}{}{UNBOLD}", fg(pal.map(|p| p.project), &pad(&project, w_project)))
            },
            fg(pal.map(|p| p.branch), &pad(&row.branch, w_branch)),
            fg(age_colour, &pad(&crate::model::age(view.now, row.last), w_last))
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

    // The selected row's whole truth, which does not fit in the columns — and what its agent is
    // doing, which is the pane's own title and nothing out of the transcript.
    if let Some(row) = view.rows.get(view.cursor).filter(|row| !row.is_header()) {
        let mut detail = match view.activity {
            Some(doing) if !doing.is_empty() => {
                format!("  {} · {} · {doing}", row.cwd, row.id)
            }
            _ => format!("  {} · {}", row.cwd, row.id),
        };
        if let Some(note) = view.note.as_deref() {
            detail.push_str(" · ");
            detail.push_str(note);
        }
        println!("{}", fg(pal.map(|p| p.muted), &truncate(&detail, cols)));
    } else if let Some(hint) = view.hint.as_deref() {
        println!("{}", fg(pal.map(|p| p.muted), &truncate(hint, cols)));
    }
    if let Some(peek) = view.peek.as_ref() {
        draw_peek(view, peek, cols);
    }
    footer(view, cols);
}

/// Under the list: the question the agent asks and the buttons that answer it — or, when it asks
/// nothing, the last lines of its screen. **A button is pressed by a person** (`a`, then the
/// digit); the board never presses one.
fn draw_peek(view: &View, peek: &PeekView, cols: usize) {
    let pal = view.palette;
    let Some(question) = peek.question else {
        for line in peek.tail.iter().rev().take(crate::PEEK_MAX_LINES).collect::<Vec<_>>().into_iter().rev() {
            println!(
                "  {} {}",
                fg(pal.map(|p| p.key), "▏"),
                fg(pal.map(|p| p.muted), &truncate(line, cols.saturating_sub(4)))
            );
        }
        return;
    };
    let mark = if peek.answering { "◆ answering: " } else { "◆ " };
    println!(
        "  {}{}",
        fg(pal.map(|p| p.error), &format!("{BOLD}{mark}{UNBOLD}")),
        fg(pal.map(|p| p.error), &truncate(&question.text, cols.saturating_sub(4 + mark.chars().count())))
    );
    // The buttons share the line: each gets its slice of the width, the number always whole.
    let count = question.options.len().max(1);
    let each = cols.saturating_sub(2 + count * 2) / count;
    let mut line = String::from("  ");
    for (number, text) in &question.options {
        let cap = format!("[{number}]");
        let label = truncate(text, each.saturating_sub(cap.chars().count() + 1));
        let button = if peek.answering {
            format!("{BOLD}{}{UNBOLD}", fg(pal.map(|p| p.key), &format!("{cap} {label}")))
        } else {
            format!("{} {}", fg(pal.map(|p| p.key), &cap), fg(pal.map(|p| p.muted), &label))
        };
        line.push_str(&button);
        line.push_str("  ");
    }
    println!("{line}");
    if peek.answering {
        println!(
            "{}",
            fg(
                pal.map(|p| p.muted),
                &truncate("  the digit goes to the agent's pane · Enter confirms if it asks to · Esc stops answering", cols)
            )
        );
    }
}

/// The agent's screen, as its terminal shows it: a heading that says whose, the last lines of its
/// pane **in the pane's own colours** (`dump-screen --ansi`, cut with `cut_ansi` so an escape is
/// never split and no colour leaks past a line), and one key — every other key is the agent's.
fn draw_preview(view: &View, preview: &PreviewView, rows_available: usize, cols: usize) {
    let pal = view.palette;
    println!(
        "{BOLD}{}{UNBOLD} {}",
        fg(pal.map(|p| p.title), "terminal"),
        fg(pal.map(|p| p.muted), &truncate(preview.title, cols.saturating_sub(10)))
    );
    let body = rows_available.saturating_sub(3).max(1);
    let total = preview.lines.len();
    let end = total.saturating_sub(preview.scroll.min(total.saturating_sub(body)));
    let start = end.saturating_sub(body);
    for line in &preview.lines[start..end] {
        println!("{}", cut_ansi(line, cols));
    }
    for _ in (end - start)..body {
        println!();
    }
    if preview.lines.is_empty() {
        println!("{}", fg(pal.map(|p| p.muted), "  reading the pane…"));
    }
    footer(view, cols);
}

/// `[k]` in the accent, the rest in grey — for the footer, where the brackets are the only thing
/// that says which character to press.
fn keycap(pal: Option<Palette>, key: &str, what: &str) -> String {
    format!(
        "{}{}",
        fg(pal.map(|p| p.key), &format!("[{key}]")),
        fg(pal.map(|p| p.muted), what)
    )
}

fn footer(view: &View, cols: usize) {
    let pal = view.palette;
    // A question outranks everything: it is the only line that is waiting for an answer.
    if let Some(prompt) = view.prompt {
        // Cut the question, not the line: the colours are escapes, and cutting a coloured line by
        // characters would cut them in half.
        let room = cols.saturating_sub(32);
        println!(
            "  {} {}",
            fg(pal.map(|p| p.error), &truncate(prompt, room)),
            keycap(pal, "y", " yes · anything else cancels")
        );
        return;
    }
    if let Some((label, input)) = view.input {
        // The label is cut first, to leave the answer at least a third of the line: a long
        // label with no room for what is being typed would be a question with no answer.
        let label = truncate(label, (cols * 2 / 3).saturating_sub(3));
        let room = cols.saturating_sub(label.chars().count() + 4);
        println!(
            "  {}{}{}",
            fg(pal.map(|p| p.key), &label),
            fg(pal.map(|p| p.name), &truncate(input, room)),
            fg(pal.map(|p| p.key), "▏")
        );
        return;
    }
    if let Some(error) = view.last_error {
        println!("{}", fg(pal.map(|p| p.error), &truncate(&format!("  {error}"), cols)));
        return;
    }
    if view.preview.is_some() {
        // Two keys are the board's — the one that leaves and the one that sends an escape in its
        // place; everything else — Enter, arrows, digits, Ctrl c — is typed into the agent's pane.
        println!(
            "  {}{}{}{}",
            fg(pal.map(|p| p.muted), "every key goes to the agent · "),
            keycap(pal, "Esc", " back to the list"),
            keycap(pal, "Ctrl [", " sends Esc to the agent"),
            fg(pal.map(|p| p.muted), " (Ctrl ] also leaves)")
        );
        return;
    }
    let keys: &[(&str, &str)] = if view.peek.as_ref().is_some_and(|peek| peek.answering) {
        &[("1-9", " answer "), ("Enter", " confirm "), ("Esc", " stop answering")]
    } else {
        &[
            ("↑↓", " move "),
            ("Enter", " jump "),
            ("v", " terminal "),
            ("a", " answer "),
            ("Tab", " expand "),
            ("i", " send "),
            ("f", " later "),
            ("p", " paste "),
            ("S", " stop "),
            ("N", " new "),
            ("X", " close "),
            ("s", " order "),
            ("r", " refresh "),
            ("Esc", " hide"),
        ]
    };
    let line: String = keys.iter().map(|(key, what)| keycap(pal, key, what)).collect();
    // Narrow pane: the keys that act, without the words.
    if cols < 70 {
        let short: String = keys
            .iter()
            .map(|(key, _)| fg(pal.map(|p| p.key), &format!("[{key}]")))
            .collect();
        println!("  {short}");
        return;
    }
    println!("  {line}");
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

    /// **An escape is not a character.** The pane's colours are kept whole, the width is counted
    /// on what is seen, and no line leaves a colour open for the board's next line to inherit.
    #[test]
    fn a_styled_line_is_cut_by_what_is_seen_and_closes_its_colour() {
        let line = "\u{1b}[38;2;153;153;153m\u{1b}[49m❯ 1. Yes\u{1b}[22m and more";
        assert_eq!(strip_ansi(line), "❯ 1. Yes and more");
        // Fits: unchanged, plus the reset.
        assert_eq!(cut_ansi(line, 40), format!("{line}\u{1b}[0m"));
        // Cut at 8 visible: seven characters, the ellipsis, the escapes that came before the cut
        // kept whole, and the reset. The escape in the middle sits past the cut and is kept too —
        // it costs nothing on the screen.
        let cut = cut_ansi(line, 8);
        assert_eq!(strip_ansi(&cut), "❯ 1. Ye…");
        assert!(cut.starts_with("\u{1b}[38;2;153;153;153m\u{1b}[49m❯ 1. Ye"));
        assert!(cut.ends_with("\u{1b}[0m"));
        // Escapes are never split: a cut right at an escape's boundary keeps it whole.
        let cut = cut_ansi("ab\u{1b}[1mcd", 3);
        assert_eq!(cut, "ab\u{1b}[1m…\u{1b}[0m");
        // A plain line gets no reset, and width 0 is nothing.
        assert_eq!(cut_ansi("plain text", 5), "plai…");
        assert_eq!(cut_ansi("plain", 5), "plain");
        assert_eq!(cut_ansi("\u{1b}[1mx", 0), "");
        // An OSC (a hyperlink) is one piece, up to its BEL.
        assert_eq!(strip_ansi("\u{1b}]8;;http://x\u{7}link\u{1b}]8;;\u{7}"), "link");
        // The dump's own ending, `\e[m`, is an escape like any other.
        assert_eq!(strip_ansi("done\u{1b}[m"), "done");
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
    /// A slot name points at the theme, never at a colour: change the theme and every part of the
    /// board changes with it, including the parts a configuration file has pointed elsewhere.
    #[test]
    fn a_configured_colour_is_a_theme_slot_not_a_colour() {
        let style = Styling::default();
        assert_eq!(slot(&style, "player_8"), Some(style.multiplayer_user_colors.player_8));
        assert_eq!(slot(&style, "emphasis_2"), Some(style.text_unselected.emphasis_2));
        assert_eq!(slot(&style, "fg"), Some(style.text_unselected.base));
        // A name that is not a slot leaves the default alone rather than inventing a colour.
        assert_eq!(slot(&style, "#ffcc00"), None);
        assert_eq!(slot(&style, "banana"), None);

        let mut configuration = BTreeMap::new();
        configuration.insert("colour_project".to_string(), "player_2".to_string());
        configuration.insert("colour_name".to_string(), "nonsense".to_string());
        let palette = Palette::from_style(&style, &configuration);
        assert_eq!(palette.project, style.multiplayer_user_colors.player_2, "the configured slot");
        assert_eq!(palette.name, style.multiplayer_user_colors.player_6, "the default stands");
    }

    /// Three steps and no fourth: the time column is read at a glance.
    #[test]
    fn the_age_column_fades_in_three_steps() {
        let p = Palette {
            busy: PaletteColor::EightBit(1),
            idle: PaletteColor::EightBit(2),
            muted: PaletteColor::EightBit(3),
            branch: PaletteColor::EightBit(4),
            name: PaletteColor::EightBit(5),
            here: PaletteColor::EightBit(6),
            there: PaletteColor::EightBit(7),
            title: PaletteColor::EightBit(8),
            error: PaletteColor::EightBit(9),
            selected: PaletteColor::EightBit(10),
            heading: PaletteColor::EightBit(11),
            key: PaletteColor::EightBit(12),
            aging: PaletteColor::EightBit(13),
            project: PaletteColor::EightBit(14),
        };
        assert_eq!(p.age_colour(Some(0)), p.busy, "just now");
        assert_eq!(p.age_colour(Some(299)), p.busy, "still within five minutes");
        assert_eq!(p.age_colour(Some(300)), p.aging, "the five minute step");
        assert_eq!(p.age_colour(Some(3599)), p.aging, "still within the hour");
        assert_eq!(p.age_colour(Some(3600)), p.muted, "the hour step");
        // No time, or a time from the future: the same grey as long ago, never a bright colour.
        assert_eq!(p.age_colour(None), p.muted);
    }

    #[test]
    fn a_time_that_makes_no_sense_is_no_time() {
        assert_eq!(since(100, 40), Some(60));
        assert_eq!(since(100, 100), Some(0));
        assert_eq!(since(100, 0), None, "a session file with no time");
        assert_eq!(since(100, 200), None, "a clock that moved");
    }

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
