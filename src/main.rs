// lvim-zclaude — the Claude Code sessions board, inside zellij.
//
// Several sessions run at once, in different projects, in panes spread over tabs and sometimes
// over other zellij sessions. Claude Code keeps no list of them: `~/.claude/projects/` is a
// directory per project (its absolute path with `/` turned into `-`) holding one
// `<sessionId>.jsonl` transcript per session, and a *running* session also writes
// `~/.claude/sessions/<pid>.json` — sessionId, cwd, name, status (busy/idle) and the process's
// start time. This plugin reads those metadata, groups them by project, and jumps to the pane the
// process runs in.
//
// **It never shows what was said.** The transcripts are a private conversation; the board is built
// from file names, `stat` times and that small session file. The two things read out of a
// transcript are the project directory and the git branch — the directory under `projects/` cannot
// be turned back into a path, because half of these projects have a `-` in their own name.
//
// A plugin is WASI in a sandbox and cannot open `~/.claude` at all, so every fact arrives through
// one `sh -c` per tick (`scan.rs`) as an `Event::RunCommandResult`.
//
// Measured on this machine before any of it was written (`.claude/action/2026-09-19/mvp.md`):
// `get_pane_pid` answers the pane's process, `claude` is a child of the pane's shell — hence the
// ancestor chain; `Event::Key` reaches a floating plugin and `Esc` → `hide_self()` works; a denied
// permission is logged, but a *returning* call made without one panics the shim and takes the
// plugin down until the session restarts, so nothing is called before `Granted`.
//
// **The board is opened by a message, not by a launch** (`zpm.kdl`: `MessagePlugin … name "open"`),
// and `Esc` hides it instead of closing it. `show()` says why: it is the one way to put the pane on
// the screen at its own size in a single frame. Every step of the pane's life is written to the
// zellij log (`lvim-zclaude: …`), because a plugin that shows nothing has no other way to say
// where it stopped — 2026-09-20 one sat on zellij's "Loading" text for an hour, waiting for a
// permission prompt nobody could see (`.claude/action/2026-09-20/open-without-flash.md`).

// The exports are the wasm's face and nothing else's: in the host test binary a `#[no_mangle]`
// export is a root the linker keeps, and everything behind it wants zellij's host functions,
// which the host does not have (`undefined symbol: host_run_plugin_command`). Without the
// exports nothing in the test binary calls `State`, hence the allowance.
#![cfg_attr(test, allow(dead_code))]
#[cfg(not(test))]
mod host;
mod keys;
mod model;
mod scan;
mod ui;

use model::{Group, Kind, Located, Order, Question, Row};
use scan::{Hook, HookState, Running, SessionFile};
use std::collections::{BTreeMap, BTreeSet};
use zellij_tile::prelude::*;

/// How many lines the block under the list may take at most: three of the agent's last blocks,
/// cut from the top when they are longer than this, so the list above it stays a list.
const PEEK_MAX_LINES: usize = 10;

/// How often the board rescans while it is visible, in seconds (`tick` in the configuration).
///
/// **Two, not ten.** A board on the screen is being watched: an agent that starts, finishes or
/// asks something has to appear there within a glance, and at ten seconds it read as broken
/// (2026-09-20 — "the refresh is very late, or does not happen at all"). One beat is one `sh -c`
/// (a `stat` per transcript, a walk of `/proc`, one `list-panes` per session that has an agent)
/// plus one `dump-screen` for the row under the cursor — tens of milliseconds, and only while
/// the board is open. Hidden, it does not scan at all unless it feeds a bar (`background_tick`).
const DEFAULT_TICK: f64 = 2.0;
/// How often the agent's screen is re-read while the terminal view (`v`) is open or a question
/// is being answered (`a`), in seconds — `preview_tick` in the configuration. One `dump-screen`
/// per beat, ~10 ms; fast enough for a prompt to react as it would under the person's hands.
const DEFAULT_PREVIEW_TICK: f64 = 0.3;

/// The board's size and place on the screen, as shares of it. Any of the four can be given as the
/// plugin's configuration (`x`, `y`, `width`, `height` — in `load_plugins`, or in the braces of
/// a `LaunchOrFocusPlugin`, where everything that is not `floating`/`move_to_focused_tab`/… lands
/// in the configuration) or in the `payload` of the `open` message (`"width=90%,height=80%"`),
/// because a screen this was not measured on may want something else.
///
/// **No launcher can hand a plugin its size.** Checked in zellij 0.44/0.45: `LaunchOrFocusPlugin`,
/// `LaunchPlugin` and `MessagePlugin` parse only booleans (`zellij-utils/src/kdl/mod.rs` 2035,
/// 2091, 2152), the pipe launch has `floating_pane_coordinates = None // TODO`
/// (`zellij-server/src/plugins/mod.rs` 1000), and a new floating pane is placed by
/// `find_room_for_new_pane` (`tab/mod.rs` 7228) and shown before `load()` runs. A plugin can size
/// itself only after `PermissionRequestResult` — a call in `load()` is refused (measured
/// 2026-09-20: `permission 'ChangeApplicationState' denied`). So a pane that is *launched* is seen
/// at zellij's size first, whatever the plugin does; only a pane that already exists, hidden, can
/// appear at its own size — see `show()`.
const SIZE: [(&str, &str); 4] = [("x", "10%"), ("y", "15%"), ("width", "80%"), ("height", "70%")];

#[derive(Clone, Debug, PartialEq, Eq)]
struct Geometry {
    x: String,
    y: String,
    width: String,
    height: String,
}

impl Default for Geometry {
    fn default() -> Geometry {
        Geometry::from_config(&BTreeMap::new())
    }
}

impl Geometry {
    fn from_config(configuration: &BTreeMap<String, String>) -> Geometry {
        let get = |key: &str, fallback: &str| {
            configuration
                .get(key)
                .filter(|value| !value.trim().is_empty())
                .cloned()
                .unwrap_or_else(|| fallback.to_string())
        };
        Geometry {
            x: get(SIZE[0].0, SIZE[0].1),
            y: get(SIZE[1].0, SIZE[1].1),
            width: get(SIZE[2].0, SIZE[2].1),
            height: get(SIZE[3].0, SIZE[3].1),
        }
    }

    /// The `payload` of an `open` message: `key=value` pairs split on `,`, `;` or whitespace.
    /// Unknown keys and empty values are left alone, so a payload of `""` changes nothing.
    fn apply(&mut self, payload: &str) {
        for pair in payload.split([',', ';', ' ', '\t', '\n']) {
            let Some((key, value)) = pair.split_once('=') else { continue };
            let value = value.trim();
            if value.is_empty() {
                continue;
            }
            match key.trim() {
                "x" => self.x = value.to_string(),
                "y" => self.y = value.to_string(),
                "width" => self.width = value.to_string(),
                "height" => self.height = value.to_string(),
                _ => {}
            }
        }
    }
}

/// Hiding the board, not closing it — and why.
///
/// A closed board is launched afresh next time, and a launched pane is always seen at zellij's
/// default size before the plugin can resize it (see `SIZE`): a flash on every opening, proven
/// unavoidable. A hidden board keeps its pane, and `show()` puts that pane back on the screen at
/// its own size in one screen instruction — no default size is ever drawn. The costs: one wasm
/// instance (~1 MB) lives on while hidden, doing nothing (the timer stops); the pane is a hidden
/// member of the tab it was hidden in, so `list-panes` shows it as suppressed, and closing that tab
/// closes it. `close_self` is not used anywhere: a board that is gone comes back with the flash.
fn hide_board() {
    hide_self();
}

/// The pane at the board's size and place. On a **hidden** pane this is also what shows it: zellij
/// takes a suppressed pane out, floats it and gives it these coordinates inside one instruction
/// (`Tab::change_floating_pane_coordinates`, `tab/mod.rs` 7682-7695 in 0.45), and draws to the
/// client only afterwards. The default geometry a re-floated pane gets is set and overwritten
/// before anyone sees it.
fn size_self(geometry: &Geometry) {
    change_floating_panes_coordinates(vec![(
        PaneId::Plugin(get_plugin_ids().plugin_id),
        FloatingPaneCoordinates::new(
            Some(geometry.x.clone()),
            Some(geometry.y.clone()),
            Some(geometry.width.clone()),
            Some(geometry.height.clone()),
            None,
            None,
        )
        .unwrap_or_default(),
    )]);
}

#[derive(Default)]
struct State {
    // --- configuration ---
    claude_dir: String,
    tick: f64,
    geometry: Geometry,
    configuration: BTreeMap<String, String>,
    /// The hook states that go to the desktop (`notify`: `waiting`, `done`; empty — none).
    notify: Vec<String>,
    /// Where the one-line summary for a status bar is written (`summary_file`), if anywhere.
    summary_file: Option<String>,
    /// Whether the summary is also broadcast to the plugins of this session as the
    /// `lvim-zclaude-summary` pipe (`summary_pipe "on"`).
    summary_pipe: bool,
    /// Seconds between scans while hidden and feeding a bar (`background_tick`).
    background_tick: f64,
    /// Seconds between screen reads while the terminal view is open or a question is answered.
    preview_tick: f64,

    // --- zellij ---
    permitted: bool,
    /// The pane is on the screen. Set by `show()`, cleared by `hide_board()`, and corrected from
    /// the pane manifest (`is_suppressed` of the plugin's own pane) — zellij's `Event::Visible`
    /// is only sent to *tiled* panes on a tab switch (`Tab::visible`, `tab/mod.rs` 6591), so it
    /// never reaches a floating or hidden board.
    visible: bool,
    /// Timers set and not yet fired. A `Timer` event does not say which timer it is, so the beat
    /// is one chain: `arm_timer` adds a timer only when none is pending, and `poke` adds one
    /// extra fast one after a key — two chains at most, converging to one.
    timers: u32,
    /// Seconds since the last scan, summed from the timer beats — while the terminal view is
    /// open the beat is fast and the scan still happens every `tick`.
    since_scan: f64,
    own_id: u32,
    /// The tab the plugin's pane is in (its position), from the manifest.
    own_tab: Option<usize>,
    /// The tab the client is looking at, from `TabUpdate`.
    active_tab: Option<usize>,
    /// An `open` message arrived before the permissions did; act on it at `Granted`.
    pending_open: bool,
    /// `hide_self` has been asked for and the manifest has not yet confirmed it. Until it does, a
    /// manifest that still says "shown" is one that was on its way before the hide — believing it
    /// would set `visible`, and the next paint's `size_self` would put the pane straight back on
    /// the screen (seen 2026-09-20 in ROOT: hide, a manifest 5 ms later, the board reappearing).
    hiding: bool,
    /// Written to the log once, at the first paint — the sign that the pane is alive.
    painted_once: bool,
    session_name: Option<String>,
    tabs: Vec<TabInfo>,
    /// Pane id → the pid of the process in it, asked once per pane. `None` when zellij would not
    /// say (a resurrected pane that has not been started yet).
    pane_pids: BTreeMap<u32, Option<i32>>,
    /// Pane id → the tab it is in, from the manifest.
    pane_tabs: BTreeMap<u32, usize>,
    /// The panes the manifest says are focused (one per tab): "the person is looking at it".
    focused: BTreeSet<u32>,

    // --- what the scanner found ---
    now: u64,
    files: Vec<SessionFile>,
    running: Vec<Running>,
    /// Session id → the project's cwd. Read once per session file and kept: a transcript does not
    /// change the directory it was started in.
    cwds: BTreeMap<String, String>,
    /// Session id → git branch. Re-read when the file has changed.
    branches: BTreeMap<String, String>,
    asked_cwd: BTreeSet<String>,
    branch_at: BTreeMap<String, u64>,
    scanning: bool,
    /// zellij session name → its `list-panes -a -j`, from the last scan (the `P` lines). Kept
    /// for the tab ids: the manifest names this session's tabs by position, `new-pane --tab-id`
    /// wants the id, and this answer has both.
    panes: Vec<(String, String)>,

    // --- the board ---
    rows: Vec<Row>,
    shown: Vec<Row>,
    cursor: usize,
    filter: String,
    expanded: Vec<String>,
    /// The headings whose rows are hidden (`Tab` on a heading).
    folded: Vec<String>,
    /// How the agents are arranged (`s` cycles it).
    order: Order,
    /// Session id → what its Claude Code hooks said last (`lvim-zclaude-event` pipes). Kept
    /// here, not in the scan: the scan reads files, and this comes over a pipe.
    hooks: BTreeMap<String, Hook>,
    /// Session id → its last three `waiting`/`done` hooks, newest last: "the last things it
    /// said", for an agent whose screen cannot be read.
    hook_log: BTreeMap<String, Vec<Hook>>,
    /// The last summary written to `summary_file`, so the file is touched only on a change.
    summary_last: String,
    /// Lines waiting for their agent to finish its turn (`f`).
    queued: Vec<Queued>,
    /// The line being composed is a follow-up (`f`), not an immediate send (`i`).
    composing_queued: bool,
    last_error: Option<String>,
    /// The active zellij theme's colours, from `ModeUpdate`. See `ui::Palette`.
    palette: Option<ui::Palette>,

    // --- acting on an agent ---
    /// Pane id → the title the pane carries. Claude writes its own state there, so this is what
    /// the board shows as "doing" — the pane's own name, never a line of the transcript.
    pane_titles: BTreeMap<u32, String>,
    /// The line being composed for an agent, and the pane it goes to.
    composing: Option<(Target, String)>,
    /// Pid → (zellij session, pane id) last written to the log, so a remote pane is reported once
    /// when it is found and once when it moves, not on every scan. By pid, not session id: two
    /// `claude --continue` on one transcript are two agents in two panes (seen 2026-09-20, ROOT
    /// and RUST), and a map by session id flipped between them on every scan.
    remote_logged: BTreeMap<i32, (String, u32)>,
    /// A pending confirmation: what it will do, and the question under the list.
    pending: Option<(Act, String)>,
    /// A question on the input line — the one piece an `N` still needs — and the launch it
    /// completes.
    asking: Option<Ask>,
    /// The agent's screen, when `v` has opened it over the list.
    showing: Option<Preview>,
    /// What is under the list for the agent under the cursor: the last lines of its screen, or
    /// the question on it.
    peek: Option<Peek>,
    /// A `dump-screen` for the peek is on its way (the session id it is for).
    peek_inflight: Option<String>,
    /// `a`: the digits answer the question under the list instead of jumping.
    answering: bool,
    /// A digit was sent to a prompt and the screen has not been read since. If the question is
    /// still there when it is, the prompt wanted the digit CONFIRMED and one Enter follows —
    /// once, and only while the same question stands.
    sent_answer: Option<(String, String)>,
    /// The board has been shown and has not been given its size since.
    unsized_yet: bool,
}

/// What the board shows under the list for the agent under the cursor — the last thing on its
/// screen, and the question on it when the screen ends with numbered options. Read with
/// `dump-screen` (no colour) whenever the cursor lands on an agent, on every scan, and when its
/// hooks speak. Like the terminal view, this shows the screen and nothing kept: `id` says whose.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Peek {
    pub id: String,
    pub tail: Vec<String>,
    pub question: Option<Question>,
}

/// A pasted text folded onto the one line being composed: line breaks become single spaces, the
/// ends are trimmed. A newline inside `write_chars_to_pane_id` would send half a message.
fn one_line(text: &str) -> String {
    text.split(['\r', '\n'])
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Where an agent's pane is: here, in this zellij session — or in another one, reached through
/// that session's own `zellij --session NAME action …`. The second is what makes `i`, `p` and `X`
/// work from any session: zellij's CLI writes into and closes a pane by id in a session that has
/// no client attached at all (measured 2026-09-20), so the board never has to go there first.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    Local(u32),
    Remote { session: String, pane: u32 },
}

impl std::fmt::Display for Target {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Target::Local(pane) => write!(f, "pane {pane}"),
            Target::Remote { session, pane } => write!(f, "pane {pane} in {session}"),
        }
    }
}

/// The pane of the agent under the cursor, or why there is none.
fn target_of(row: &Row) -> Result<Target, &'static str> {
    if row.is_header() {
        return Err("a session heading — pick an agent under it (N starts one there)");
    }
    if let Some(located) = row.located.as_ref() {
        return Ok(Target::Local(located.pane_id));
    }
    let Some(running) = row.running.as_ref() else { return Err("that session is not running") };
    match (running.zellij_session.as_ref(), running.pane.as_ref()) {
        (Some(session), Some(pane)) => Ok(Target::Remote { session: session.clone(), pane: pane.id }),
        (Some(_), None) => Err("that agent's pane could not be told apart — Enter goes there"),
        (None, _) => Err("that agent does not run under zellij — no pane to reach"),
    }
}

/// An agent's screen, shown over the list (`v`) — **as its terminal**: the pane's own colours,
/// re-read every `preview_tick`, and every key written into the pane as the bytes a terminal
/// would send. Not the pane itself: see `preview` for why that cannot be.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Preview {
    pub target: Target,
    /// `project · SESSION · tab · title`, for the heading.
    pub title: String,
    /// The pane's viewport, top to bottom, with its ANSI styling.
    pub lines: Vec<String>,
}

/// The two acts that do not undo: killing the process, closing the pane it lives in. Both wait for
/// a `y` — one keystroke away from ending someone's work is one keystroke too few.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Act {
    Stop { pid: i32 },
    ClosePane { target: Target },
}

/// A new agent to start — everything `zellij action new-pane` needs. `N` (and `Enter` on a
/// stopped transcript) builds it from the row under the cursor; a piece the row does not settle
/// is `None` and is asked for (see `Missing`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Launch {
    session: Option<String>,
    cwd: Option<String>,
    /// The tab to open in, by its id in that session; `None` — the session's focused tab.
    tab_id: Option<u32>,
    /// After `claude`: nothing for a fresh agent, `--resume <id>` for a transcript.
    args: Vec<String>,
    /// What the footer says about it: `new claude in bru`, `resume bru · 23aa89d8`.
    what: String,
}

impl Launch {
    /// The command line: one path for every session, this one included. Not `open_command_pane`:
    /// from a floating board that opens a *floating* pane (`should_float = None` follows the
    /// visible layer), and it cannot pick a tab. `--tab-id` is what puts the new pane next to
    /// the agent under the cursor rather than wherever that session's focus happens to be.
    fn command(&self) -> Result<Vec<String>, &'static str> {
        let session = self.session.as_deref().map(str::trim).unwrap_or("");
        if session.is_empty() {
            return Err("no session to start it in");
        }
        let cwd = self.cwd.as_deref().map(str::trim).unwrap_or("");
        if !cwd.starts_with('/') {
            return Err("the directory must be an absolute path");
        }
        let mut command: Vec<String> = ["zellij", "--session", session, "action", "new-pane"]
            .into_iter()
            .map(str::to_string)
            .collect();
        if let Some(tab) = self.tab_id {
            command.push("--tab-id".to_string());
            command.push(tab.to_string());
        }
        command.extend(["--cwd".to_string(), cwd.to_string(), "--".to_string(), "claude".to_string()]);
        command.extend(self.args.iter().cloned());
        Ok(command)
    }
}

/// A follow-up: a line for an agent that is busy now, to be sent when it is not — when its hooks
/// say `done`, or the scan finds it idle and not waiting for a person.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Queued {
    session_id: String,
    target: Target,
    line: String,
}

/// The piece of a `Launch` the row under the cursor did not settle.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Missing {
    Session,
    Directory,
}

/// A question on the input line: the label before the cursor, the answer being typed (prefilled
/// with the likeliest one, so `Enter` alone is a "yes"), and the launch it completes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ask {
    label: String,
    line: String,
    missing: Missing,
    launch: Launch,
}

/// **Where `N` opens, and when it asks.** The command needs a session, a directory and (for a
/// transcript) an id. It is *clear* when the row under the cursor settles all of them by itself,
/// and *asked* otherwise — never guessed, because a guess opens a pane in someone's session:
///
/// - an **agent** in a zellij session: its session, its directory, its tab. "One more like this
///   one, next to it" is what a cursor on an agent means. Clear.
/// - a **session heading**: the session is the row. The directory is clear when every agent
///   under it is in the same one (a session that works on one project), and asked when they are
///   not — prefilled with the topmost agent's.
/// - a **stopped transcript**: the directory and the id are the row's; the session is asked — a
///   transcript belongs to no zellij session. Prefilled with this one, and the question doubles
///   as "resume it?": `Enter` is yes, `Esc` is no.
/// - an agent **outside zellij**: the directory is the row's, the session is asked.
///
/// `under` are the distinct directories of the agents in the heading's session, topmost first;
/// `here` is this zellij session's name. Returns the launch and, when something is missing, what
/// and the prefilled answer.
fn plan_start(
    row: &Row,
    under: &[String],
    here: Option<&str>,
    tab_id: Option<u32>,
) -> Result<(Launch, Option<(Missing, String)>), &'static str> {
    let here_name = here.unwrap_or_default().to_string();
    match row.kind {
        Kind::Header { .. } => {
            // A project heading (the order by project): the directory is the row, the session is
            // asked — a project runs anywhere.
            if row.group() == Some(Group::Project) {
                if !row.cwd.starts_with('/') {
                    return Err("this project's directory is not known yet");
                }
                let launch = Launch {
                    session: None,
                    cwd: Some(row.cwd.clone()),
                    tab_id: None,
                    args: vec![],
                    what: format!("new claude in {}", row.project),
                };
                return Ok((launch, Some((Missing::Session, here_name))));
            }
            if !row.is_session() {
                return Err("that heading is not a session");
            }
            let cwds: Vec<String> = under.iter().filter(|cwd| cwd.starts_with('/')).cloned().collect();
            let launch = Launch {
                session: Some(row.project.clone()),
                cwd: None,
                tab_id: None,
                args: vec![],
                what: format!("new claude in {}", row.project),
            };
            match cwds.as_slice() {
                [one] => Ok((Launch { cwd: Some(one.clone()), ..launch }, None)),
                _ => {
                    let prefill = cwds.first().cloned().unwrap_or_default();
                    Ok((launch, Some((Missing::Directory, prefill))))
                }
            }
        }
        Kind::Agent => {
            if !row.cwd.starts_with('/') {
                return Err("this project's directory is not known yet");
            }
            let session = match (row.located.is_some(), row.running.as_ref()) {
                (true, _) => here.map(str::to_string),
                (false, Some(running)) => running.zellij_session.clone(),
                (false, None) => None,
            };
            let launch = Launch {
                session: session.clone(),
                cwd: Some(row.cwd.clone()),
                tab_id,
                args: vec![],
                what: format!("new claude in {}", row.project),
            };
            match session {
                Some(_) => Ok((launch, None)),
                None => Ok((launch, Some((Missing::Session, here_name)))),
            }
        }
        Kind::Stopped => {
            if !row.cwd.starts_with('/') {
                return Err("this project's directory is not known yet");
            }
            let short: String = row.id.chars().take(8).collect();
            let launch = Launch {
                session: None,
                cwd: Some(row.cwd.clone()),
                tab_id: None,
                args: vec!["--resume".to_string(), row.id.clone()],
                what: format!("resume {} · {short}", row.project),
            };
            Ok((launch, Some((Missing::Session, here_name))))
        }
    }
}

// Not `register_plugin!(State)`: the exports are in `host.rs`, because the macro's `update` cuts a
// keystroke to one byte and `б` arrives as `1`. `keys.rs` says why and `host.rs` is the macro with
// that one line changed. What `register_plugin!` leaves in `main` — the panic hook — is here.
fn main() {
    std::panic::set_hook(Box::new(|info| {
        report_panic(info);
    }));
}

impl ZellijPlugin for State {
    fn load(&mut self, configuration: BTreeMap<String, String>) {
        self.claude_dir = configuration
            .get("claude_dir")
            .cloned()
            .unwrap_or_else(|| "$HOME/.claude".to_string());
        self.tick = configuration
            .get("tick")
            .and_then(|tick| tick.parse::<f64>().ok())
            .filter(|tick| *tick > 0.0)
            .unwrap_or(DEFAULT_TICK);

        self.geometry = Geometry::from_config(&configuration);
        self.order = Order::parse(configuration.get("order").map(String::as_str).unwrap_or(""));
        self.notify = configuration
            .get("notify")
            .map(String::as_str)
            .unwrap_or("waiting,done")
            .split([',', ' '])
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect();
        self.summary_file = configuration
            .get("summary_file")
            .map(|path| path.trim().to_string())
            .filter(|path| !path.is_empty());
        self.summary_pipe = matches!(
            configuration.get("summary_pipe").map(|v| v.trim()),
            Some("on" | "true" | "1" | "yes")
        );
        self.background_tick = configuration
            .get("background_tick")
            .and_then(|tick| tick.parse::<f64>().ok())
            .filter(|tick| *tick > 0.0)
            .unwrap_or(30.0);
        self.preview_tick = configuration
            .get("preview_tick")
            .and_then(|tick| tick.parse::<f64>().ok())
            .filter(|tick| *tick > 0.0)
            .unwrap_or(DEFAULT_PREVIEW_TICK);
        // Kept whole: the colours are resolved against the theme, which only arrives later.
        self.configuration = configuration.clone();
        eprintln!(
            "lvim-zclaude: load, configuration keys [{}], geometry {}/{} {}x{}",
            configuration.keys().cloned().collect::<Vec<_>>().join(" "),
            self.geometry.x,
            self.geometry.y,
            self.geometry.width,
            self.geometry.height
        );

        // These two, and nothing else, before the permissions are in.
        request_permission(&[
            PermissionType::ReadApplicationState,
            PermissionType::ChangeApplicationState,
            PermissionType::RunCommands,
            PermissionType::WriteToStdin, // `i`: a line typed here lands in the agent's pane
        ]);
        subscribe(&[
            EventType::PermissionRequestResult,
            EventType::ModeUpdate,
            EventType::TabUpdate,
            EventType::PaneUpdate,
            EventType::RunCommandResult,
            EventType::Timer,
            EventType::Key,
            EventType::PastedText, // a paste while composing lands in the line, whole
            EventType::Visible,
        ]);
    }

    fn update(&mut self, event: Event) -> bool {
        match event {
            Event::PermissionRequestResult(PermissionStatus::Granted) => {
                self.permitted = true;
                self.own_id = get_plugin_ids().plugin_id;
                eprintln!("lvim-zclaude: permissions granted, plugin id {}", self.own_id);
                rename_plugin_pane(self.own_id, "claude");
                // Whether the pane is on the screen is not known here: a preloaded board
                // (`load_plugins`) starts hidden, a launched one starts shown. The manifest that
                // follows the rename says which (`PaneUpdate` below), and an `open` that came
                // first is honoured now.
                self.unsized_yet = true;
                if self.pending_open {
                    self.pending_open = false;
                    self.show();
                } else if self.feeds_bars() {
                    // A preloaded, hidden board with a bar to feed: the first scan now.
                    self.scan();
                    self.arm_timer();
                }
                true
            }
            Event::PermissionRequestResult(PermissionStatus::Denied) => {
                self.permitted = false;
                eprintln!("lvim-zclaude: permissions denied");
                self.last_error = Some("permissions denied — the board cannot read anything".into());
                true
            }
            Event::ModeUpdate(mode) => {
                // The theme arrives here, and again on every theme switch — so the board repaints
                // in the new colours without being reopened, as `lvim-zbar` does.
                self.palette = Some(ui::Palette::from_style(&mode.style.colors, &self.configuration));
                self.session_name = mode.session_name;
                true
            }
            Event::TabUpdate(tabs) => {
                let changed = self.tabs != tabs;
                self.active_tab = tabs.iter().find(|tab| tab.active).map(|tab| tab.position);
                self.tabs = tabs;
                if changed {
                    self.locate();
                }
                changed
            }
            Event::PaneUpdate(manifest) => {
                if !self.permitted {
                    return false;
                }
                let mut live: BTreeSet<u32> = BTreeSet::new();
                let mut asked = false;
                let mut titles = false;
                let mut shown = false;
                for (tab, panes) in &manifest.panes {
                    for pane in panes {
                        if pane.is_plugin {
                            if pane.id == self.own_id {
                                self.own_tab = Some(*tab);
                                shown = self.seen_own_pane(pane.is_suppressed) || shown;
                            }
                            continue;
                        }
                        live.insert(pane.id);
                        self.pane_tabs.insert(pane.id, *tab);
                        if pane.is_focused {
                            self.focused.insert(pane.id);
                        } else {
                            self.focused.remove(&pane.id);
                        }
                        // The pane names itself, and Claude keeps that name current — so the board
                        // can say what an agent is doing without reading a word it said.
                        if self.pane_titles.get(&pane.id) != Some(&pane.title) {
                            self.pane_titles.insert(pane.id, pane.title.clone());
                            titles = true;
                        }
                        if !self.pane_pids.contains_key(&pane.id) {
                            let pid = get_pane_pid(PaneId::Terminal(pane.id)).ok();
                            self.pane_pids.insert(pane.id, pid);
                            asked = true;
                        }
                    }
                }
                self.pane_pids.retain(|id, _| live.contains(id));
                self.pane_tabs.retain(|id, _| live.contains(id));
                self.pane_titles.retain(|id, _| live.contains(id));
                if asked {
                    self.locate();
                }
                asked || titles || shown
            }
            Event::RunCommandResult(code, out, err, context) => {
                if context.get("step").map(String::as_str) == Some("panes") {
                    self.jump_there(&String::from_utf8_lossy(&out), &context);
                    return true;
                }
                if context.get("step").map(String::as_str) == Some("paste") {
                    let text = String::from_utf8_lossy(&out).to_string();
                    if code == Some(0) {
                        self.paste_arrived(&text, &context);
                    } else {
                        self.last_error = Some("could not read the clipboard".into());
                    }
                    return true;
                }
                if context.get("step").map(String::as_str) == Some("preview") {
                    self.preview_arrived(code, &out, &err);
                    return true;
                }
                if context.get("step").map(String::as_str) == Some("peek") {
                    self.peek_arrived(code, &out, &context);
                    return true;
                }
                if context.get("step").map(String::as_str) == Some("keys") {
                    // A key written to a pane in another session: silence on success, a word
                    // on failure — the footer must not say something on every keystroke.
                    if code != Some(0) {
                        let reason = String::from_utf8_lossy(&err);
                        let reason = reason.lines().last().unwrap_or("zellij said no").trim();
                        eprintln!("lvim-zclaude: keys failed: {reason}");
                        self.last_error = Some(format!("the key did not reach the pane: {reason}"));
                        return true;
                    }
                    return false;
                }
                if let Some(step @ ("remote-send" | "remote-close" | "launch")) =
                    context.get("step").map(String::as_str)
                {
                    let what = context.get("what").cloned().unwrap_or_default();
                    self.last_error = match code {
                        Some(0) => Some(what),
                        _ => {
                            let reason = String::from_utf8_lossy(&err);
                            let reason = reason.lines().last().unwrap_or("zellij said no").trim();
                            eprintln!("lvim-zclaude: {step} failed: {reason}");
                            Some(format!("{what} — failed: {reason}"))
                        }
                    };
                    // A closed pane or a started agent changes what runs: rescan. (A launch
                    // writes its `sessions/<pid>.json` within the first second; if this scan
                    // is too early, the timer's next one catches it.)
                    if step != "remote-send" {
                        self.scan();
                    }
                    return true;
                }
                if context.get("step").map(String::as_str) == Some("stop") {
                    self.last_error = match code {
                        Some(0) => None,
                        _ => Some(
                            String::from_utf8_lossy(&err)
                                .lines()
                                .next()
                                .unwrap_or("could not stop it")
                                .to_string(),
                        ),
                    };
                    // The agent is on its way out; the board should stop claiming it runs.
                    self.scan();
                    return true;
                }
                if context.get("step").map(String::as_str) != Some("scan") {
                    return false;
                }
                self.scanning = false;
                let text = String::from_utf8_lossy(&out).to_string();
                if code != Some(0) {
                    let message = String::from_utf8_lossy(&err);
                    let message = message.lines().next().unwrap_or("the scan failed");
                    self.last_error = Some(message.to_string());
                } else {
                    self.last_error = None;
                }
                self.take(scan::parse(&text));
                true
            }
            Event::Timer(elapsed) => {
                self.timers = self.timers.saturating_sub(1);
                if !self.permitted {
                    return false;
                }
                if self.visible {
                    // **An open board is live.** Every beat re-reads the screen it is showing —
                    // the terminal view, or the lines under the cursor — so what the agent says
                    // appears as it is said and a prompt turns into buttons while you look at it.
                    // The scan behind it (the list itself: who is running, where, since when)
                    // keeps its own slower pace, because that is a `sh -c` and this is not.
                    self.since_scan += elapsed;
                    if self.showing.is_some() {
                        self.refresh_preview();
                    } else {
                        self.peek_now(true);
                    }
                    if self.since_scan >= self.tick {
                        self.since_scan = 0.0;
                        self.scan();
                    }
                    self.arm_timer();
                } else if self.feeds_bars() {
                    // Hidden, but a bar reads the summary: keep it current, slowly.
                    self.scan();
                    self.arm_timer();
                }
                false
            }
            Event::Visible(visible) => {
                // Only a tiled pane gets this, on a tab switch; a floating board never does. Kept
                // for the day the board is embedded: hidden means no scanning.
                eprintln!("lvim-zclaude: visible {visible}");
                self.visible = visible;
                if visible && self.permitted {
                    self.scan();
                    self.arm_timer();
                }
                false
            }
            Event::Key(key) => self.key(key),
            Event::PastedText(text) => self.pasted(&text),
            _ => false,
        }
    }

    /// A message from a keybind (`MessagePlugin`) or the CLI (`zellij pipe`): the way the board is
    /// opened. `open` shows it (the payload may carry `x`/`y`/`width`/`height`), `hide` hides it,
    /// `toggle` does whichever applies. Anything else is ignored without a word — every plugin
    /// receives every broadcast pipe (`hints_set` from the bar, on each attach), and those are not
    /// worth a log line.
    fn pipe(&mut self, message: PipeMessage) -> bool {
        if matches!(message.name.as_str(), "open" | "hide" | "toggle") {
            eprintln!(
                "lvim-zclaude: pipe {:?} name={} payload={:?}",
                message.source, message.name, message.payload
            );
        }
        match message.name.as_str() {
            "open" => {
                if let Some(payload) = message.payload.as_deref() {
                    self.geometry.apply(payload);
                }
                // The board's own key while the terminal view is open: the list comes back.
                // `Ctrl ]` does the same from inside the view, but this one is a zellij keybind
                // and reaches the board whatever the terminal makes of a control character.
                if self.visible && self.showing.is_some() {
                    eprintln!("lvim-zclaude: open while the terminal view is up — back to the list");
                    self.showing = None;
                    return true;
                }
                self.open();
                true
            }
            "hide" => {
                self.hide();
                true
            }
            "toggle" => {
                if self.visible {
                    self.hide();
                } else {
                    self.open();
                }
                true
            }
            // From `hooks/lvim-zclaude-hook.sh`: a session is waiting, done, or working again.
            "lvim-zclaude-event" => self.hook_event(message.payload.as_deref().unwrap_or("")),
            _ => false,
        }
    }

    fn render(&mut self, rows: usize, cols: usize) {
        if !self.painted_once {
            self.painted_once = true;
            eprintln!("lvim-zclaude: first paint {rows}x{cols}, permitted {}", self.permitted);
        }
        // A pane that was *launched* on the screen (a `LaunchOrFocusPlugin`, a pipe to a board
        // that was not running) sits at zellij's default size until here: the pane exists, is
        // floating and is about to be painted — the first moment a size of ours is not overwritten
        // by the default that came with floating it. A hidden pane is never sized from here:
        // sizing it would show it.
        if self.unsized_yet && self.permitted && self.visible {
            self.unsized_yet = false;
            size_self(&self.geometry);
        }
        let view = ui::View {
            rows: &self.shown,
            cursor: self.cursor,
            filter: &self.filter,
            session_name: self.session_name.as_deref(),
            now: self.now,
            running: self.running.len(),
            files: self.files.len(),
            last_error: self.last_error.as_deref(),
            permitted: self.permitted,
            palette: self.palette,
            activity: self.activity(),
            prompt: self.pending.as_ref().map(|(_, question)| question.as_str()),
            input: match (self.composing.as_ref(), self.asking.as_ref()) {
                (Some((_, line)), _) => Some(("send: ", line.as_str())),
                (None, Some(ask)) => Some((ask.label.as_str(), ask.line.as_str())),
                (None, None) => None,
            },
            hint: self.hint(),
            note: self.note(),
            order: self.order,
            preview: self.showing.as_ref().map(|p| ui::PreviewView {
                title: &p.title,
                lines: &p.lines,
                scroll: 0,
            }),
            peek: self
                .peek
                .as_ref()
                .filter(|peek| self.current().is_some_and(|row| row.kind == Kind::Agent && row.id == peek.id))
                .map(|peek| ui::PeekView {
                    tail: &peek.tail,
                    question: peek.question.as_ref(),
                    answering: self.answering,
                }),
        };
        ui::draw(&view, rows, cols);
    }
}

impl State {
    /// The manifest showed the plugin's own pane. Returns true when the board has just come onto
    /// the screen without `show()` — launched visible, or floated by zellij for a permission prompt
    /// — and so needs its size, a scan and the timer.
    fn seen_own_pane(&mut self, hidden: bool) -> bool {
        if hidden {
            self.hiding = false;
        } else if self.hiding {
            // Still "shown": a manifest from before the hide landed. Not a word, not a change.
            return false;
        }
        if hidden == !self.visible {
            return false;
        }
        eprintln!(
            "lvim-zclaude: own pane is {} (manifest), tab {:?}",
            if hidden { "hidden" } else { "shown" },
            self.own_tab
        );
        self.visible = !hidden;
        if hidden {
            return false;
        }
        self.unsized_yet = true;
        self.scan();
        self.arm_timer();
        true
    }

    /// `open`: show the board, or remember to once the permissions are in.
    fn open(&mut self) {
        if !self.permitted {
            self.pending_open = true;
            return;
        }
        self.show();
    }

    /// Put the hidden pane on the screen at its own size, in the tab the client is looking at,
    /// focused — without a frame at any other size.
    ///
    /// 1. `size_self`: on a hidden pane this un-hides, floats and sizes it in one instruction
    ///    (see `size_self`). If the pane is hidden in a tab the client is not looking at, that
    ///    frame is drawn to a tab nobody sees.
    /// 2. If that tab is not the active one, `break_panes_to_tab_with_index` moves the pane —
    ///    a floating pane travels with its exact geometry (`screen.rs` 6590-6605 in 0.45:
    ///    fixed x/y/width/height from the pane itself) and lands unfocused.
    /// 3. `focus_pane_with_id`: the focus, and the floating layer shown if it was hidden.
    ///
    /// Two frames at most (unfocused then focused frame colour), never two sizes. `show_self` and
    /// `LaunchOrFocusPlugin` both re-float a hidden pane at zellij's default geometry
    /// (`tab/mod.rs` 7028 and 7099 in 0.45), which is exactly the flash this avoids.
    fn show(&mut self) {
        let own = PaneId::Plugin(self.own_id);
        size_self(&self.geometry);
        if let (Some(here), Some(active)) = (self.own_tab, self.active_tab) {
            if here != active {
                eprintln!("lvim-zclaude: show — moving the pane from tab {here} to tab {active}");
                break_panes_to_tab_with_index(&[own], active, true);
                self.own_tab = Some(active);
            }
        }
        focus_pane_with_id(own, true, false);
        eprintln!(
            "lvim-zclaude: show at {}/{} {}x{}",
            self.geometry.x, self.geometry.y, self.geometry.width, self.geometry.height
        );
        self.visible = true;
        self.hiding = false;
        self.unsized_yet = false;
        self.scan();
        self.arm_timer();
    }

    /// Take the board off the screen. The timer is left to run out; it does nothing while hidden.
    /// Answering stops with it: no digit may go anywhere from a board nobody sees.
    fn hide(&mut self) {
        eprintln!("lvim-zclaude: hide");
        self.visible = false;
        self.hiding = true;
        self.answering = false;
        hide_board();
    }

    /// Ask the host for everything, once. The two lists are what is still unknown: the cwd of a
    /// session file that has never been read, and the branch of one that has changed since it was.
    fn scan(&mut self) {
        if self.scanning {
            return;
        }
        let path = |file: &SessionFile| {
            format!("{}/projects/{}/{}.jsonl", self.claude_dir, file.dir, file.id)
        };
        let need_cwd: Vec<String> = self
            .files
            .iter()
            .filter(|file| !self.cwds.contains_key(&file.id) && !self.asked_cwd.contains(&file.id))
            .map(path)
            .collect();
        let need_branch: Vec<String> = self
            .files
            .iter()
            .filter(|file| self.branch_at.get(&file.id) != Some(&file.mtime))
            .map(path)
            .collect();
        for file in &self.files {
            self.asked_cwd.insert(file.id.clone());
            self.branch_at.insert(file.id.clone(), file.mtime);
        }
        let script = scan::script(&self.claude_dir, &need_cwd, &need_branch);
        self.scanning = true;
        run_command(
            &["sh", "-c", &script],
            BTreeMap::from([("step".to_string(), "scan".to_string())]),
        );
    }

    /// The next beat: after `preview_tick` while the screen is being watched, after `tick` while
    /// the board is on the screen, after `background_tick` while it is hidden and feeds a bar.
    fn arm_timer(&mut self) {
        if self.timers == 0 {
            self.timers += 1;
            set_timeout(if !self.visible {
                self.background_tick
            } else if self.fast() {
                self.preview_tick
            } else {
                self.tick
            });
        }
    }

    /// Whether the screen is being watched: the terminal view is open, or a question is answered.
    fn fast(&self) -> bool {
        // Anything on the screen is watched, not just the terminal view: the beat is the fast one
        // for as long as the board is open.
        self.visible || self.showing.is_some() || self.answering
    }

    /// One extra fast beat, after a key went to the pane — so what it did is seen at once, not
    /// at the end of a slow beat that may have been set before the view opened.
    fn poke(&mut self) {
        self.timers += 1;
        set_timeout(self.preview_tick);
    }

    /// Whether the board has a bar to feed (`summary_file` or `summary_pipe`) — the one reason
    /// to scan while hidden.
    fn feeds_bars(&self) -> bool {
        self.summary_file.is_some() || self.summary_pipe
    }

    /// Fold a scan into what is known, then rebuild the board.
    fn take(&mut self, scan: scan::Scan) {
        if scan.now > 0 {
            self.now = scan.now;
        }
        // The first scan cannot ask for cwds — it does not know the ids yet. It learns them here,
        // so one more scan follows immediately and the board is complete on the second answer.
        let first_sight = scan
            .files
            .iter()
            .any(|file| !self.cwds.contains_key(&file.id) && !self.asked_cwd.contains(&file.id));
        self.files = scan.files;
        self.running = scan.running;
        self.panes = scan.panes;
        // **The pane of every agent in another zellij session**, from that session's own
        // `list-panes`: what makes `i`, `p`, `X` and "what it is doing" work from here. Matched by
        // cwd, as the jump always was (`scan::claude_pane`).
        for running in &mut self.running {
            let Some(session) = running.zellij_session.as_deref() else { continue };
            if Some(session) == self.session_name.as_deref() {
                continue; // this session: the manifest knows its panes better
            }
            let Some((_, json)) = self.panes.iter().find(|(name, _)| name == session) else { continue };
            running.pane = scan::claude_pane(json, &running.cwd);
            if let Some(pane) = running.pane.as_ref() {
                let now = (session.to_string(), pane.id);
                if self.remote_logged.get(&running.pid) != Some(&now) {
                    eprintln!(
                        "lvim-zclaude: agent {} ({}) is pane {} in {session}, tab {:?}, title {:?}",
                        running.name, running.pid, pane.id, pane.tab_name, pane.title
                    );
                    self.remote_logged.insert(running.pid, now);
                }
            }
        }
        // A running session states its own cwd in `sessions/<pid>.json` — free, and better than
        // the transcript, whose first 16 KB may hold no `"cwd"` at all. Measured 2026-09-19: one
        // project showed up as its directory slug (`-mnt-development-biserstoilov-lua`) because
        // of exactly that.
        for running in &self.running {
            if !running.session_id.is_empty() && !running.cwd.is_empty() {
                self.cwds.insert(running.session_id.clone(), running.cwd.clone());
            }
        }
        for (id, cwd) in scan.heads {
            self.cwds.insert(id, cwd);
        }
        for (id, branch) in scan.branches {
            self.branches.insert(id, branch);
        }
        self.rebuild();
        self.peek_now(true);
        if first_sight {
            self.scan();
        }
    }

    fn rebuild(&mut self) {
        // What the hooks said, onto the agents the scan found.
        for live in &mut self.running {
            live.hook = self.hooks.get(&live.session_id).cloned();
        }
        // A filter looks through folded headings: what matches must be seen, whatever is folded.
        let folded: &[String] = if self.filter.is_empty() { &self.folded } else { &[] };
        self.rows = model::build_rows(
            &self.files,
            &self.cwds,
            &self.branches,
            &self.running,
            &self.expanded,
            self.session_name.as_deref(),
            folded,
            self.order,
        );
        self.locate();
        self.deliver_queued();
        self.write_summary();
    }

    /// A word from a Claude Code hook (`lvim-zclaude-event`): remember it, tell the desktop if
    /// this board is the one to, and redraw. `working` forgets the session's entry.
    fn hook_event(&mut self, payload: &str) -> bool {
        // A CLI pipe ends with an empty payload — the end of the stream, not a message. It says
        // nothing and is not a fault, so it passes without a word in the log.
        if payload.trim().is_empty() {
            return false;
        }
        let Some(event) = scan::hook_event(payload) else {
            eprintln!("lvim-zclaude: hook event not understood: {payload:?}");
            return false;
        };
        eprintln!(
            "lvim-zclaude: hook event {} {:?} cwd={} notify={}",
            event.session_id, event.hook, event.cwd, event.notify
        );
        match &event.hook {
            Some(hook) => {
                self.hooks.insert(event.session_id.clone(), hook.clone());
                let log = self.hook_log.entry(event.session_id.clone()).or_default();
                log.push(hook.clone());
                if log.len() > 3 {
                    log.remove(0);
                }
            }
            None => {
                self.hooks.remove(&event.session_id);
            }
        }
        self.rebuild();
        // The agent under the cursor spoke: what is under the list is read again.
        if self.current().is_some_and(|row| row.id == event.session_id) {
            self.peek_now(true);
        }
        if let (true, Some(hook)) = (event.notify, event.hook.as_ref()) {
            self.tell_desktop(&event.session_id, &event.cwd, hook);
        }
        true
    }

    /// **What is under the list.** For the agent under the cursor with a pane: its screen, read
    /// with `dump-screen` — the last three lines, and the question if the screen ends in one.
    /// Without a pane (outside zellij, or a pane that could not be told apart): its last three
    /// notifications from the hooks. `force` reads again for the same agent (a scan, a hook, a
    /// digit sent); otherwise only a change of row reads.
    fn peek_now(&mut self, force: bool) {
        if !self.visible || !self.permitted {
            return;
        }
        let Some(row) = self.current().cloned() else {
            self.peek = None;
            return;
        };
        if row.kind != Kind::Agent {
            self.peek = None;
            self.answering = false;
            return;
        }
        if !force && self.peek.as_ref().is_some_and(|peek| peek.id == row.id) {
            return;
        }
        let target = match target_of(&row) {
            Ok(target) => target,
            Err(_) => {
                let tail: Vec<String> = self
                    .hook_log
                    .get(&row.id)
                    .map(|hooks| {
                        hooks
                            .iter()
                            .map(|hook| {
                                let glyph = match hook.state {
                                    HookState::Waiting => "◆",
                                    HookState::Done => "✓",
                                };
                                if hook.message.is_empty() {
                                    format!("{glyph} {}", hook.kind)
                                } else {
                                    format!("{glyph} {} — {}", hook.kind, hook.message)
                                }
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                self.peek = Some(Peek { id: row.id.clone(), tail, question: None });
                self.answering = false;
                return;
            }
        };
        if self.peek_inflight.is_some() {
            return; // one at a time; the next beat or scan reads again
        }
        let Some((session, pane)) = self.place_of(&target) else { return };
        self.peek_inflight = Some(row.id.clone());
        run_command(
            &["zellij", "--session", &session, "action", "dump-screen", "--pane-id", &format!("terminal_{pane}")],
            BTreeMap::from([("step".to_string(), "peek".to_string()), ("id".to_string(), row.id.clone())]),
        );
    }

    /// The screen for the peek came back: the tail and the question out of it. An answered
    /// question (no longer on the screen) ends the answering.
    fn peek_arrived(&mut self, code: Option<i32>, out: &[u8], context: &BTreeMap<String, String>) {
        self.peek_inflight = None;
        let Some(id) = context.get("id").cloned() else { return };
        if code != Some(0) {
            return;
        }
        let text = String::from_utf8_lossy(out);
        let lines: Vec<String> = text.lines().map(|line| line.trim_end().to_string()).collect();
        let question = model::question(&lines);
        let tail = model::tail_blocks(&lines, 3, PEEK_MAX_LINES);
        if self.answering && question.is_none() && self.current().is_some_and(|row| row.id == id) {
            eprintln!("lvim-zclaude: the question is gone from the screen — answering ends");
            self.answering = false;
        }
        // The digit went in and the same question is still on the screen: this prompt confirms.
        if let Some((asked_id, asked)) = self.sent_answer.take() {
            let still_there = question.as_ref().is_some_and(|now| now.text == asked);
            if asked_id == id && still_there {
                if let Some(target) = self.current().and_then(|row| target_of(row).ok()) {
                    eprintln!("lvim-zclaude: the question stands — confirming with Enter");
                    self.write_bytes(&target, vec![b'\r']);
                    self.poke();
                }
            }
        }
        self.peek = Some(Peek { id, tail, question });
    }

    /// The session and pane id a target is reached through — this session's own name for a
    /// local one, which is not known before the first `ModeUpdate`.
    fn place_of(&self, target: &Target) -> Option<(String, u32)> {
        match target {
            Target::Remote { session, pane } => Some((session.clone(), *pane)),
            Target::Local(pane) => Some((self.session_name.clone()?, *pane)),
        }
    }

    /// `a` — answer the question under the list: from here on the digits go to the agent's
    /// pane, one keystroke each, exactly as pressed in the pane itself. Nothing is sent by this
    /// key; it only arms the digits.
    fn answer(&mut self) -> bool {
        let has_question = self
            .peek
            .as_ref()
            .filter(|peek| self.current().is_some_and(|row| row.id == peek.id))
            .is_some_and(|peek| peek.question.is_some());
        if has_question {
            self.answering = true;
            self.last_error = None;
            self.peek_now(true);
            self.arm_timer();
        } else {
            self.last_error = Some("no question on its screen — v opens its terminal".into());
        }
        true
    }

    /// Every key while answering: a digit or Enter goes to the pane, Esc stops, nothing else
    /// does anything — a letter here must not become a filter or a command by accident.
    fn answer_key(&mut self, key: &KeyWithModifier, ctrl: bool) -> bool {
        let target = self
            .current()
            .and_then(|row| target_of(row).ok());
        let Some(target) = target else {
            self.answering = false;
            return true;
        };
        match key.bare_key {
            BareKey::Esc => self.answering = false,
            BareKey::Char('c') if ctrl => self.answering = false,
            BareKey::Char(c @ '1'..='9') if !ctrl => {
                eprintln!("lvim-zclaude: answer {c} to {target}");
                self.write_bytes(&target, vec![c as u8]);
                // Some prompts act on the digit alone; others want it confirmed. Rather than
                // guess — and rather than send an Enter that would post `1` as a message when
                // the digit landed in the input — the screen is read again and the Enter follows
                // only if the same question is still standing.
                let asked = self
                    .peek
                    .as_ref()
                    .and_then(|peek| peek.question.as_ref())
                    .map(|question| question.text.clone());
                if let (Some(id), Some(asked)) = (self.current().map(|row| row.id.clone()), asked) {
                    self.sent_answer = Some((id, asked));
                }
                self.peek_now(true);
                self.poke();
            }
            BareKey::Enter => {
                self.write_bytes(&target, vec![b'\r']);
                self.peek_now(true);
                self.poke();
            }
            _ => {}
        }
        true
    }

    /// `notify-send`, unless the state is not in `notify` or the person is already looking at
    /// the agent (its pane is focused in the tab this session is showing). The hook sends
    /// `notify=1` to exactly one board, so one event is one notification.
    fn tell_desktop(&mut self, session_id: &str, cwd: &str, hook: &Hook) {
        let state = match hook.state {
            HookState::Waiting => "waiting",
            HookState::Done => "done",
        };
        if !self.notify.iter().any(|s| s == state) {
            return;
        }
        let row = self.rows.iter().find(|row| row.id == session_id && row.running.is_some());
        if let Some(located) = row.and_then(|row| row.located.as_ref()) {
            if self.focused.contains(&located.pane_id) && Some(located.tab_position) == self.active_tab {
                eprintln!("lvim-zclaude: {state} in pane {} — on screen, not notified", located.pane_id);
                return;
            }
        }
        let project = row
            .map(|row| row.project.clone())
            .unwrap_or_else(|| cwd.rsplit('/').next().unwrap_or(cwd).to_string());
        let title = match hook.state {
            HookState::Waiting => format!("{project} is waiting for you"),
            HookState::Done => format!("{project} is done"),
        };
        let place = row
            .map(|row| row.where_text(self.session_name.as_deref(), true))
            .unwrap_or_else(|| self.session_name.clone().unwrap_or_default());
        let body = format!("{} · {place}", hook.kind);
        run_command(
            &["notify-send", "-a", "lvim-zclaude", "--", &title, &body],
            BTreeMap::from([("step".to_string(), "notify".to_string())]),
        );
    }

    /// The follow-ups whose agent is free now: sent, with the newline that makes them a message.
    /// Free means idle by the session file and not waiting for a person by the hooks — a line
    /// typed into a permission prompt would answer it.
    fn deliver_queued(&mut self) {
        if self.queued.is_empty() {
            return;
        }
        let mut kept: Vec<Queued> = Vec::new();
        for queued in std::mem::take(&mut self.queued) {
            let live = self.running.iter().find(|live| live.session_id == queued.session_id);
            let free = live.is_some_and(|live| {
                !live.busy && live.hook.as_ref().map(|hook| hook.state) != Some(HookState::Waiting)
            });
            if free {
                eprintln!("lvim-zclaude: follow-up to {} delivered", queued.target);
                let what = format!("follow-up sent to {}", queued.target);
                self.write_to(&queued.target, &queued.line, what, true);
            } else {
                kept.push(queued);
            }
        }
        self.queued = kept;
    }

    /// **The board's API for bars.** The summary — one line of prose, one of `key=value` — goes
    /// out only when it changed: into `summary_file` for anything that can read a file (a
    /// zjstatus `command`, an lvim-zbar script), and as the `lvim-zclaude-summary` pipe to
    /// every plugin of this session for a bar that would rather be told than poll. The hook
    /// events themselves (`lvim-zclaude-event`) already reach every plugin, from the hook.
    fn write_summary(&mut self) {
        if !self.feeds_bars() {
            return;
        }
        // **The agent you are looking at is not news.** A bar that turns red because the pane in
        // front of you asked you something is telling you what the screen already says — and it
        // would be red for most of a working day. The one agent whose pane is focused in this
        // session's active tab is left out of the count; every other board still counts it,
        // because on their screen it IS news.
        let watched: Vec<String> = self
            .rows
            .iter()
            .filter(|row| {
                row.located.as_ref().is_some_and(|located| {
                    self.focused.contains(&located.pane_id)
                        && Some(located.tab_position) == self.active_tab
                })
            })
            .map(|row| row.id.clone())
            .collect();
        let counted: Vec<scan::Running> = self
            .running
            .iter()
            .filter(|live| !watched.contains(&live.session_id))
            .cloned()
            .collect();
        let (prose, pairs) = model::summary(&counted);
        let text = format!("{prose}\n{pairs}\n");
        if text == self.summary_last {
            return;
        }
        self.summary_last = text.clone();
        eprintln!("lvim-zclaude: summary {pairs}");
        if let Some(path) = self.summary_file.as_deref() {
            run_command(
                &["sh", "-c", "printf '%s' \"$1\" > \"$2\"", "lvim-zclaude", &text, path],
                BTreeMap::from([("step".to_string(), "summary".to_string())]),
            );
        }
        if let (true, Some(session)) = (self.summary_pipe, self.session_name.as_deref()) {
            run_command(
                &["zellij", "--session", session, "pipe", "--name", "lvim-zclaude-summary", "--", text.trim_end()],
                BTreeMap::from([("step".to_string(), "summary".to_string())]),
            );
        }
    }

    /// The note after the selected row's details: what the hooks say and what is queued.
    fn note(&self) -> Option<String> {
        let row = self.current()?;
        let mut parts: Vec<String> = Vec::new();
        if let Some(hook) = row.running.as_ref().and_then(|live| live.hook.as_ref()) {
            let since = model::age(self.now, hook.at);
            let mut part = match hook.state {
                HookState::Waiting => format!("waiting for you: {} ({since})", hook.kind),
                HookState::Done => format!("done ({since}, {})", hook.kind),
            };
            if !hook.message.is_empty() {
                part.push_str(" — ");
                part.push_str(&hook.message);
            }
            parts.push(part);
        }
        let queued = self.queued.iter().filter(|q| q.session_id == row.id).count();
        if queued > 0 {
            parts.push(format!("{queued} follow-up{} queued", if queued == 1 { "" } else { "s" }));
        }
        if parts.is_empty() {
            None
        } else {
            Some(parts.join(" · "))
        }
    }

    /// `Tab` on a heading: fold it or unfold it. `←` folds, `→` unfolds.
    fn fold(&mut self, name: &str, fold: Option<bool>) {
        let at = self.folded.iter().position(|folded| folded == name);
        match (at, fold) {
            (Some(at), None | Some(false)) => {
                self.folded.remove(at);
            }
            (None, None | Some(true)) => self.folded.push(name.to_string()),
            _ => return,
        }
        eprintln!("lvim-zclaude: fold {name:?} -> {:?}", self.folded);
        self.rebuild();
    }

    /// Match every running session to a pane of this zellij session: the pane whose process is the
    /// session's own process or one of its ancestors — `claude` runs inside the pane's shell.
    fn locate(&mut self) {
        let name_of = |position: usize, tabs: &[TabInfo]| {
            tabs.iter()
                .find(|tab| tab.position == position)
                .map(|tab| tab.name.clone())
                .unwrap_or_else(|| format!("Tab {}", position + 1))
        };
        for row in &mut self.rows {
            row.located = None;
            let Some(running) = row.running.as_ref() else { continue };
            for (pane, pid) in &self.pane_pids {
                let Some(pid) = pid else { continue };
                if *pid == running.pid || running.ancestors.contains(pid) {
                    let position = self.pane_tabs.get(pane).copied().unwrap_or(0);
                    row.located = Some(Located {
                        tab_position: position,
                        tab_name: name_of(position, &self.tabs),
                        pane_id: *pane,
                    });
                    break;
                }
            }
        }
        self.reshow();
    }

    fn reshow(&mut self) {
        self.shown = model::filter(&self.rows, &self.filter, self.session_name.as_deref());
        if self.cursor >= self.shown.len() {
            self.cursor = self.shown.len().saturating_sub(1);
        }
        self.peek_now(false);
    }

    /// The row under the cursor: an agent, a stopped transcript, or a heading.
    fn current(&self) -> Option<&Row> {
        self.shown.get(self.cursor)
    }

    /// The directories of the agents in a zellij session, in the board's order (by tab, then
    /// pane) — from what the scan found, not from the rows, so a folded heading answers too.
    fn cwds_in(&self, session: &str) -> Vec<String> {
        let mut agents: Vec<&Running> = self
            .running
            .iter()
            .filter(|live| live.zellij_session.as_deref() == Some(session) && live.cwd.starts_with('/'))
            .collect();
        agents.sort_by_key(|live| match live.pane.as_ref() {
            Some(pane) => (0, pane.tab, pane.id),
            None => (1, 0, live.pid as u32),
        });
        let mut cwds: Vec<String> = Vec::new();
        for live in agents {
            if !cwds.contains(&live.cwd) {
                cwds.push(live.cwd.clone());
            }
        }
        cwds
    }

    /// The line under the list when a heading is selected: what the keys do there.
    fn hint(&self) -> Option<String> {
        let row = self.current()?;
        if !row.is_header() {
            return None;
        }
        let fold = if row.is_folded() { "Tab unfolds" } else { "Tab folds" };
        if !row.is_session() {
            return Some(format!("  {fold}"));
        }
        let there = if Some(row.project.as_str()) == self.session_name.as_deref() {
            "this session".to_string()
        } else {
            format!("Enter switches to {}", row.project)
        };
        Some(format!("  {fold} · {there} · N starts a new claude in {}", row.project))
    }

    fn key(&mut self, key: KeyWithModifier) -> bool {
        let typing = !self.filter.is_empty();
        let ctrl = key.key_modifiers.contains(&KeyModifier::Ctrl);

        // A key reaches only a pane that is on the screen and focused. If the board still thinks
        // it is hidden, it is wrong: a `hide` followed at once by a `switch_session` (Enter on a
        // session heading) leaves the session without a client, no manifest ever confirms the
        // hide, and `hiding` would stay set for good (seen 2026-09-20 in ZTEST: shown again by
        // zellij's own `launch-or-focus`, the board neither sized nor scanned). The key settles it.
        if self.hiding || !self.visible {
            eprintln!("lvim-zclaude: a key arrived while the board thought it was hidden — it is shown");
            self.hiding = false;
            self.visible = true;
            self.unsized_yet = true;
            self.scan();
            self.arm_timer();
        }

        // Composing a line for an agent: every key belongs to the line until Enter sends it or Esc
        // drops it. Nothing here filters, moves or acts.
        if self.composing.is_some() {
            return self.compose_key(&key, ctrl);
        }
        // Answering a question on the input line: the same, until Enter accepts or Esc drops it.
        if self.asking.is_some() {
            return self.ask_key(&key, ctrl);
        }
        // A question is on the footer: `y` does it, anything else does not. No second meaning for
        // any other key while something is waiting to be confirmed.
        if let Some((act, _)) = self.pending.take() {
            if key.bare_key == BareKey::Char('y') && !ctrl {
                self.perform(act);
            } else {
                self.last_error = Some("cancelled".into());
            }
            return true;
        }
        // The agent's screen is up as its terminal: every key is the agent's but one.
        if self.showing.is_some() {
            return self.preview_key(&key, ctrl);
        }
        // Answering the question under the list: the digits are the agent's.
        if self.answering {
            return self.answer_key(&key, ctrl);
        }

        match key.bare_key {
            BareKey::Esc => {
                if typing {
                    self.filter.clear();
                    self.rebuild();
                } else {
                    self.hide();
                }
                true
            }
            BareKey::Left | BareKey::Right if !typing => {
                if let Some(row) = self.current().filter(|row| row.is_header()).cloned() {
                    self.fold(&row.project, Some(key.bare_key == BareKey::Left));
                }
                true
            }
            BareKey::Char('c') if ctrl => {
                self.hide();
                true
            }
            BareKey::Down => self.move_cursor(1),
            BareKey::Up => self.move_cursor(-1),
            BareKey::PageDown => self.move_cursor(10),
            BareKey::PageUp => self.move_cursor(-10),
            BareKey::Char('j') if !typing => self.move_cursor(1),
            BareKey::Char('k') if !typing => self.move_cursor(-1),
            BareKey::Char('g') if !typing => {
                self.cursor = 0;
                self.reshow();
                true
            }
            // The one key that leaves the terminal view — pressed on the list it means nothing,
            // but it is logged, so a live session can show whether it arrives at all.
            BareKey::Char(']') if ctrl => {
                eprintln!("lvim-zclaude: Ctrl ] arrived on the list");
                true
            }
            BareKey::Char('G') if !typing => {
                self.cursor = self.shown.len().saturating_sub(1);
                self.reshow();
                true
            }
            BareKey::Char('r') if !typing => {
                self.scan();
                true
            }
            BareKey::Char('i') if !typing => self.compose(),
            BareKey::Char('f') if !typing => {
                let started = self.compose();
                self.composing_queued = self.composing.is_some();
                started
            }
            BareKey::Char('s') if !typing => {
                self.order = self.order.next();
                eprintln!("lvim-zclaude: order {}", self.order.label());
                self.rebuild();
                true
            }
            // A digit: the Nth agent or transcript on the board, and straight to it.
            BareKey::Char(c @ '1'..='9') if !typing => {
                let nth = c as usize - '1' as usize;
                let at = self.shown.iter().enumerate().filter(|(_, row)| !row.is_header()).nth(nth);
                if let Some((at, _)) = at {
                    self.cursor = at;
                    self.jump();
                }
                true
            }
            BareKey::Char('p') if !typing => self.paste_to_agent(),
            BareKey::Char('v') if !typing => self.preview(),
            BareKey::Char('a') if !typing => self.answer(),
            BareKey::Char('S') if !typing => self.ask_stop(),
            BareKey::Char('N') if !typing => self.start_agent(),
            BareKey::Char('X') if !typing => self.ask_close_pane(),
            BareKey::Tab => {
                // A heading folds and unfolds; a stopped project unfolds its older transcripts.
                if let Some(row) = self.current().filter(|row| row.is_header()).cloned() {
                    self.fold(&row.project, None);
                    return true;
                }
                if let Some(row) = self.current().filter(|row| row.kind == Kind::Stopped) {
                    eprintln!(
                        "lvim-zclaude: expand {} cwd={} count={} child={}",
                        row.project, row.cwd, row.count, row.child
                    );
                    let cwd = row.cwd.clone();
                    match self.expanded.iter().position(|open| *open == cwd) {
                        Some(at) => {
                            self.expanded.remove(at);
                        }
                        None => self.expanded.push(cwd),
                    }
                    self.rebuild();
                }
                true
            }
            BareKey::Enter => {
                self.jump();
                true
            }
            BareKey::Backspace => {
                self.filter.pop();
                self.rebuild();
                true
            }
            BareKey::Char(c) if !ctrl => {
                self.filter.push(c);
                self.rebuild();
                true
            }
            _ => false,
        }
    }

    /// The other session answered with its panes: switch the client there, onto claude's pane.
    ///
    /// When the pane cannot be picked out — two claudes and neither in this directory — the switch
    /// still happens, to the session, without a pane: the person asked to go there, and landing in
    /// the right session with the wrong pane focused is closer than staying put.
    fn jump_there(&mut self, json: &str, context: &BTreeMap<String, String>) {
        let Some(name) = context.get("session") else { return };
        let cwd = context.get("cwd").map(String::as_str).unwrap_or("");
        match scan::claude_pane(json, cwd) {
            Some(pane) => {
                eprintln!("lvim-zclaude: switch to {name}, tab {}, pane {}", pane.tab, pane.id);
                self.last_error = None;
                self.hide();
                switch_session_with_focus(name, Some(pane.tab), Some((pane.id, false)));
            }
            None => {
                eprintln!("lvim-zclaude: switch to {name}, no claude pane matched {cwd:?}");
                self.last_error = None;
                self.hide();
                switch_session_with_focus(name, None, None);
            }
        }
    }

    /// What the selected agent's pane calls itself — here from the manifest, in another zellij
    /// session from that session's `list-panes` (the last scan).
    fn activity(&self) -> Option<&str> {
        let row = self.current()?;
        if let Some(located) = row.located.as_ref() {
            return self.pane_titles.get(&located.pane_id).map(String::as_str);
        }
        row.running.as_ref()?.pane.as_ref().map(|pane| pane.title.as_str())
    }

    /// `i` — start a line for the agent under the cursor, wherever its pane is.
    fn compose(&mut self) -> bool {
        let Some(row) = self.current() else { return false };
        match target_of(row) {
            Ok(target) => {
                self.composing = Some((target, String::new()));
                self.last_error = None;
            }
            Err(why) => self.last_error = Some(why.into()),
        }
        true
    }

    /// Text into an agent's pane, as if typed there. Here through the plugin API; in another
    /// zellij session through that session's CLI, which lands in the same server call
    /// (`WriteToPaneId`). The `--` keeps a line that starts with `-` from being read as a flag.
    /// With `submit`, Enter follows the text as a keystroke of its own.
    ///
    /// **A newline written with the text is not Enter.** `write-chars` puts what it is given into
    /// the pane as typed characters, and Claude Code's input reads a line break arriving inside
    /// text the way it reads Shift-Enter: the line grows a second row and sits there, unsent
    /// (2026-09-20 — the board said "sent" and the agent never saw it). The key that sends is a
    /// carriage return on its own. Remotely the two go in one `sh -c`, so the order cannot be
    /// undone by two commands racing.
    fn write_to(&mut self, target: &Target, text: &str, what: String, submit: bool) {
        let text = text.strip_suffix('\n').unwrap_or(text);
        match target {
            Target::Local(pane) => {
                write_chars_to_pane_id(text, PaneId::Terminal(*pane));
                if submit {
                    write_to_pane_id(vec![b'\r'], PaneId::Terminal(*pane));
                }
                self.last_error = Some(what);
            }
            Target::Remote { session, pane } => {
                self.last_error = Some(format!("{what}…"));
                let pane = format!("terminal_{pane}");
                if submit {
                    let script = "zellij --session \"$1\" action write-chars --pane-id \"$2\" -- \"$3\"; \
                                  zellij --session \"$1\" action write --pane-id \"$2\" 13";
                    run_command(
                        &["sh", "-c", script, "lvim-zclaude", session, &pane, text],
                        BTreeMap::from([
                            ("step".to_string(), "remote-send".to_string()),
                            ("what".to_string(), what),
                        ]),
                    );
                } else {
                    run_command(
                        &["zellij", "--session", session, "action", "write-chars", "--pane-id", &pane, "--", text],
                        BTreeMap::from([
                            ("step".to_string(), "remote-send".to_string()),
                            ("what".to_string(), what),
                        ]),
                    );
                }
            }
        }
    }

    /// Text pasted onto the board (bracketed paste, which zellij hands over whole as UTF-8): while a
    /// line is being composed it joins the line. The line is one line, so a paste of several is
    /// folded onto it — a newline in the middle would send half a message.
    fn pasted(&mut self, text: &str) -> bool {
        if let Some(ask) = self.asking.as_mut() {
            ask.line.push_str(&one_line(text));
            return true;
        }
        let Some((_, line)) = self.composing.as_mut() else { return false };
        line.push_str(&one_line(text));
        true
    }

    /// `p` — send the clipboard to the agent, as it is, without composing anything.
    ///
    /// The clipboard is read by the host (`wl-paste`) and written into the pane as bytes; it is
    /// the way to send several lines at once, which the one-line `i` cannot.
    fn paste_to_agent(&mut self) -> bool {
        let Some(row) = self.current() else { return false };
        let target = match target_of(row) {
            Ok(target) => target,
            Err(why) => {
                self.last_error = Some(why.into());
                return true;
            }
        };
        let (session, pane) = match &target {
            Target::Local(pane) => (String::new(), *pane),
            Target::Remote { session, pane } => (session.clone(), *pane),
        };
        self.last_error = Some("reading the clipboard…".into());
        run_command(
            &[
                "sh",
                "-c",
                "wl-paste --no-newline 2>/dev/null || xclip -selection clipboard -o 2>/dev/null",
            ],
            BTreeMap::from([
                ("step".to_string(), "paste".to_string()),
                ("pane".to_string(), pane.to_string()),
                ("session".to_string(), session),
            ]),
        );
        true
    }

    /// The clipboard came back: into the agent's pane it goes, with the newline that makes it a
    /// message rather than a half-typed prompt.
    fn paste_arrived(&mut self, text: &str, context: &BTreeMap<String, String>) {
        let Some(pane) = context.get("pane").and_then(|id| id.parse::<u32>().ok()) else { return };
        let target = match context.get("session").filter(|s| !s.is_empty()) {
            Some(session) => Target::Remote { session: session.clone(), pane },
            None => Target::Local(pane),
        };
        if text.is_empty() {
            self.last_error = Some("the clipboard is empty".into());
            return;
        }
        let what = format!("sent {} characters to {target}", text.chars().count());
        self.write_to(&target, text, what, true);
    }

    /// Every key while a line is being composed. The character comes whole, whatever the alphabet
    /// — `keys.rs` decodes it, not `zellij-tile`.
    fn compose_key(&mut self, key: &KeyWithModifier, ctrl: bool) -> bool {
        let Some((pane, line)) = self.composing.as_mut() else { return false };
        match key.bare_key {
            BareKey::Esc => {
                self.composing = None;
            }
            BareKey::Char('c') if ctrl => {
                self.composing = None;
            }
            BareKey::Enter => {
                let (target, line) = (pane.clone(), line.clone());
                self.composing = None;
                let queued = std::mem::take(&mut self.composing_queued);
                if line.is_empty() {
                    return true;
                }
                if queued {
                    // `f`: for later — when the agent's turn is over (see `deliver_queued`).
                    let Some(id) = self.current().map(|row| row.id.clone()) else { return true };
                    self.queued.push(Queued { session_id: id, target: target.clone(), line });
                    self.last_error = Some(format!("follow-up queued for {target}"));
                    self.deliver_queued(); // free already? then now
                    return true;
                }
                // The newline is what makes it a message and not a half-typed prompt: the same
                // thing that happens when the key is pressed in the pane itself.
                self.write_to(&target, &line, format!("sent to {target}"), true);
            }
            BareKey::Backspace => {
                line.pop();
            }
            BareKey::Char('u') if ctrl => line.clear(),
            BareKey::Char(c) if !ctrl => line.push(c),
            _ => return false,
        }
        true
    }

    /// Every key while a question is on the input line. `Enter` accepts what is there (the
    /// prefilled answer, unless it was edited); `Esc` is no; `Ctrl u` clears the line, which is
    /// how a prefilled answer is replaced without holding Backspace.
    fn ask_key(&mut self, key: &KeyWithModifier, ctrl: bool) -> bool {
        let Some(ask) = self.asking.as_mut() else { return false };
        match key.bare_key {
            BareKey::Esc => {
                self.asking = None;
                self.last_error = Some("cancelled".into());
            }
            BareKey::Char('c') if ctrl => {
                self.asking = None;
                self.last_error = Some("cancelled".into());
            }
            BareKey::Enter => {
                let Some(ask) = self.asking.take() else { return false };
                let answer = ask.line.trim().to_string();
                let launch = match ask.missing {
                    Missing::Session => Launch { session: Some(answer), ..ask.launch },
                    Missing::Directory => Launch { cwd: Some(answer), ..ask.launch },
                };
                self.launch(launch);
            }
            BareKey::Backspace => {
                ask.line.pop();
            }
            BareKey::Char('u') if ctrl => ask.line.clear(),
            BareKey::Char(c) if !ctrl => ask.line.push(c),
            _ => return false,
        }
        true
    }

    /// `S` — ask before stopping the agent's process.
    fn ask_stop(&mut self) -> bool {
        let Some(row) = self.current() else { return false };
        match row.running.as_ref() {
            Some(running) => {
                let question = format!("stop {} (pid {})?", row.project, running.pid);
                self.pending = Some((Act::Stop { pid: running.pid }, question));
            }
            None => self.last_error = Some("nothing is running on this row".into()),
        }
        true
    }

    /// `X` — ask before closing the pane the agent lives in, here or in another zellij session.
    fn ask_close_pane(&mut self) -> bool {
        let Some(row) = self.current() else { return false };
        match target_of(row) {
            Ok(target) => {
                let question = format!("close the pane of {} ({target})?", row.project);
                self.pending = Some((Act::ClosePane { target }, question));
            }
            Err(why) => self.last_error = Some(why.into()),
        }
        true
    }

    /// `N` — a new agent where the row under the cursor says, or a question for the piece the row
    /// does not say. `plan_start` is the rule; this is the wiring.
    ///
    /// On an agent: a fresh `claude` in its project, its session, its tab. Not `--continue`: that
    /// would open the transcript the agent is already holding. On a session heading: in that
    /// session. On a stopped transcript: `claude --resume <id>` (not `--continue`, which takes
    /// the newest transcript of the directory, and the row is a *particular* one) — the same as
    /// `Enter` there.
    fn start_agent(&mut self) -> bool {
        let Some(row) = self.current().cloned() else {
            self.last_error = Some("nothing under the cursor".into());
            return true;
        };
        let under = if row.is_session() { self.cwds_in(&row.project) } else { Vec::new() };
        let tab_id = self.tab_of(&row);
        match plan_start(&row, &under, self.session_name.as_deref(), tab_id) {
            Ok((launch, None)) => self.launch(launch),
            Ok((launch, Some((missing, prefill)))) => self.ask(missing, prefill, launch),
            Err(why) => self.last_error = Some(why.into()),
        }
        true
    }

    /// The id of the tab the row's agent is in — in its own session's numbering, which is what
    /// `new-pane --tab-id` there takes. Here from this session's `list-panes` by pane id (the
    /// manifest has positions only), elsewhere from the pane the scan matched.
    fn tab_of(&self, row: &Row) -> Option<u32> {
        if let Some(located) = row.located.as_ref() {
            let name = self.session_name.as_deref()?;
            let (_, json) = self.panes.iter().find(|(session, _)| session == name)?;
            return scan::tab_id_of(json, located.pane_id);
        }
        row.running.as_ref()?.pane.as_ref().map(|pane| pane.tab_id)
    }

    /// The zellij sessions the board knows of: this one, then the ones with agents. A hint for the
    /// session question — a session without an agent is not listed, but typing its name works.
    fn known_sessions(&self) -> Vec<String> {
        let mut names: Vec<String> = self.session_name.iter().cloned().collect();
        for row in self.rows.iter().filter(|row| row.is_session()) {
            if !names.contains(&row.project) {
                names.push(row.project.clone());
            }
        }
        names
    }

    /// Put the question on the input line.
    fn ask(&mut self, missing: Missing, prefill: String, launch: Launch) {
        let label = match missing {
            Missing::Session => {
                format!("{} — in session ({}): ", launch.what, self.known_sessions().join(", "))
            }
            Missing::Directory => format!("{} — in directory: ", launch.what),
        };
        eprintln!("lvim-zclaude: N asks {missing:?} for {:?}, prefilled {prefill:?}", launch.what);
        self.asking = Some(Ask { label, line: prefill, missing, launch });
        self.last_error = None;
    }

    /// Start it. The answer comes back as a `launch` step; when it is this session, the board
    /// hides itself so the new pane is what the person sees.
    fn launch(&mut self, launch: Launch) {
        let command = match launch.command() {
            Ok(command) => command,
            Err(why) => {
                self.last_error = Some(why.into());
                return;
            }
        };
        let session = launch.session.clone().unwrap_or_default();
        let session = session.trim().to_string();
        eprintln!("lvim-zclaude: launch {:?}", command);
        let argv: Vec<&str> = command.iter().map(String::as_str).collect();
        run_command(
            &argv,
            BTreeMap::from([
                ("step".to_string(), "launch".to_string()),
                ("what".to_string(), format!("{} — started in {session}", launch.what)),
            ]),
        );
        self.last_error = Some(format!("{} in {session}…", launch.what));
        if Some(session.as_str()) == self.session_name.as_deref() {
            self.hide();
        }
    }

    /// `v` — the agent's own screen, **as its terminal**: read with `zellij --session S action
    /// dump-screen --pane-id terminal_N --ansi` (any session, no client needed there) every
    /// `preview_tick`, and every key written into that pane as the bytes a terminal sends —
    /// `Enter`, `Esc`, the arrows, `1`, `Ctrl c`, all of it. It looks like the pane and answers
    /// like the pane; `Ctrl ]` is the one key that stays here and brings the list back.
    ///
    /// **Why not the pane itself.** A plugin's pane shows what `render` prints and nothing else:
    /// a terminal pane's grid lives in the `Tab` of the server that owns it, and no plugin call
    /// (`zellij-tile` 0.44.3, `shim.rs`, every `pub fn` read) hands a plugin another pane's
    /// output or draws a pane inside a plugin. `replace_pane_with_existing_pane` swaps places —
    /// the board would be gone — and `break_panes_to_tab_with_index` moves a pane between tabs
    /// of the **same** session only. Another zellij session is another server process: the CLI's
    /// `dump-screen` is the only way to see its pane, `write` the only way to type into it. To
    /// have the real pane under the hands there is `Enter` (`focus_pane_with_id` here,
    /// `switch_session_with_focus` there); this view is for answering without leaving.
    ///
    /// **This and the block under the list are the two places the board shows the conversation
    /// itself** — rule 7 of `CLAUDE.md` keeps the transcripts private. Only the screen, only
    /// while shown, nothing kept beyond the current frame.
    fn preview(&mut self) -> bool {
        let Some(row) = self.current() else { return false };
        let target = match target_of(row) {
            Ok(target) => target,
            Err(why) => {
                self.last_error = Some(why.into());
                return true;
            }
        };
        // A local target cannot be read before zellij has said what this session is called.
        let _session = match &target {
            Target::Remote { session, .. } => session.clone(),
            Target::Local(_) => match self.session_name.clone() {
                Some(session) => session,
                None => {
                    self.last_error = Some("this session's name is not known yet".into());
                    return true;
                }
            },
        };
        // **The row's own name leads, and the session is named once.** `where_text` already carries
        // the session, so putting it in front of that read as `ROOT · ROOT · Tab #1`. The pane's
        // title is last and only when it says something: a pane running `claude` under a shell is
        // titled `zsh`, which names the shell rather than whose screen this is.
        let mut title = format!(
            "{} · {}",
            row.project,
            row.where_text(self.session_name.as_deref(), true)
        );
        match self.activity() {
            Some(activity) if !activity.is_empty() && activity != row.project => {
                title.push_str(" · ");
                title.push_str(activity);
            }
            _ => {}
        }
        eprintln!("lvim-zclaude: terminal view of {target}");
        self.answering = false;
        self.showing = Some(Preview { target, title, lines: Vec::new() });
        self.refresh_preview();
        self.poke();
        true
    }

    fn refresh_preview(&mut self) {
        let Some(preview) = self.showing.as_ref() else { return };
        let (session, pane) = match &preview.target {
            Target::Remote { session, pane } => (session.clone(), *pane),
            Target::Local(pane) => (self.session_name.clone().unwrap_or_default(), *pane),
        };
        run_command(
            &[
                "zellij",
                "--session",
                &session,
                "action",
                "dump-screen",
                "--pane-id",
                &format!("terminal_{pane}"),
                "--ansi",
            ],
            BTreeMap::from([("step".to_string(), "preview".to_string())]),
        );
    }

    /// The agent's screen came back, styled. The pane's viewport is what `dump-screen` gives
    /// without `--full`; trailing blank lines (blank once the escapes are taken out) are dropped
    /// so the last thing said sits at the bottom.
    fn preview_arrived(&mut self, code: Option<i32>, out: &[u8], err: &[u8]) {
        let Some(preview) = self.showing.as_mut() else { return };
        if code != Some(0) {
            let reason = String::from_utf8_lossy(err);
            preview.lines = vec![format!("could not read the pane: {}", reason.trim())];
            return;
        }
        let text = String::from_utf8_lossy(out);
        let mut lines: Vec<String> = text.lines().map(|line| line.trim_end().to_string()).collect();
        while lines.last().is_some_and(|line| ui::strip_ansi(line).trim().is_empty()) {
            lines.pop();
        }
        preview.lines = lines;
    }

    /// Every key while the terminal view is open: `Ctrl ]` closes it, anything else goes to the
    /// pane as bytes and the screen is read again at once. A key with no bytes (`CapsLock`) is
    /// swallowed. Modified keys are logged — the one way to see on a live session which of them
    /// zellij lets through to a plugin.
    fn preview_key(&mut self, key: &KeyWithModifier, ctrl: bool) -> bool {
        let Some(preview) = self.showing.as_ref() else { return false };
        // **`Esc` leaves the terminal, it does not reach the agent.** Sending it there is what a
        // person does by reflex to close a view, and in `claude` that keystroke cancels whatever
        // it was doing — an expensive way to learn the key. The agent can still be sent an escape:
        // `Ctrl [` IS the escape byte (`[` is 0x5b, and Ctrl clears the top bits: 0x1b), so it
        // falls through to `bytes_of` below and arrives as `Esc` in the pane.
        if key.bare_key == BareKey::Esc && key.key_modifiers.is_empty()
            || ctrl && key.bare_key == BareKey::Char(']')
        {
            eprintln!("lvim-zclaude: terminal view closed");
            self.showing = None;
            return true;
        }
        if !key.key_modifiers.is_empty() {
            eprintln!("lvim-zclaude: terminal key {key}");
        }
        let Some(bytes) = keys::bytes_of(key) else { return true };
        let target = preview.target.clone();
        self.write_bytes(&target, bytes);
        self.refresh_preview();
        self.poke();
        true
    }

    /// Raw bytes into an agent's pane — a keystroke. Here through the plugin API; in another
    /// zellij session through `zellij --session S action write --pane-id terminal_N b1 b2 …`,
    /// which is the same server call (`WriteToPaneId`) made from outside.
    fn write_bytes(&mut self, target: &Target, bytes: Vec<u8>) {
        match target {
            Target::Local(pane) => write_to_pane_id(bytes, PaneId::Terminal(*pane)),
            Target::Remote { session, pane } => {
                let mut argv: Vec<String> = ["zellij", "--session", session, "action", "write", "--pane-id"]
                    .into_iter()
                    .map(str::to_string)
                    .collect();
                argv.push(format!("terminal_{pane}"));
                argv.extend(bytes.iter().map(|b| b.to_string()));
                let argv: Vec<&str> = argv.iter().map(String::as_str).collect();
                run_command(&argv, BTreeMap::from([("step".to_string(), "keys".to_string())]));
            }
        }
    }

    /// Do what was confirmed.
    fn perform(&mut self, act: Act) {
        match act {
            Act::Stop { pid } => {
                // TERM, not KILL: claude writes its session file on the way out, and a killed
                // agent leaves the board lying about what is running until the next scan.
                run_command(
                    &["sh", "-c", &format!("kill {pid}")],
                    BTreeMap::from([("step".to_string(), "stop".to_string())]),
                );
                self.last_error = Some(format!("stopping pid {pid}…"));
            }
            Act::ClosePane { target: Target::Local(pane_id) } => {
                close_pane_with_id(PaneId::Terminal(pane_id));
                self.last_error = Some(format!("closed pane {pane_id}"));
            }
            Act::ClosePane { target: target @ Target::Remote { .. } } => {
                let Target::Remote { session, pane } = &target else { unreachable!() };
                self.last_error = Some(format!("closing {target}…"));
                run_command(
                    &[
                        "zellij",
                        "--session",
                        session,
                        "action",
                        "close-pane",
                        "--pane-id",
                        &format!("terminal_{pane}"),
                    ],
                    BTreeMap::from([
                        ("step".to_string(), "remote-close".to_string()),
                        ("what".to_string(), format!("closed {target}")),
                    ]),
                );
            }
        }
    }

    /// Move by `by` rows; the cursor stops on every row, headings included.
    fn move_cursor(&mut self, by: i64) -> bool {
        if self.shown.is_empty() {
            return false;
        }
        let last = self.shown.len() as i64 - 1;
        self.cursor = (self.cursor as i64 + by).clamp(0, last) as usize;
        self.peek_now(false);
        true
    }

    /// Enter: go to where the agent runs — a pane here, or a pane in another zellij session. On
    /// a session heading, to that session. On a stopped transcript there is nowhere to go, so
    /// the offer is to bring it back (`--resume`), with the question `N` would ask.
    fn jump(&mut self) {
        let Some(row) = self.current().cloned() else {
            eprintln!("lvim-zclaude: jump with no row under the cursor");
            return;
        };
        eprintln!(
            "lvim-zclaude: jump row={} kind={:?} located={:?} running={}",
            row.project,
            row.kind,
            row.located,
            row.running.is_some()
        );
        if row.is_session() {
            if Some(row.project.as_str()) == self.session_name.as_deref() {
                self.last_error = Some("this is the session you are in".into());
            } else {
                self.last_error = None;
                self.hide();
                switch_session(Some(&row.project));
            }
            return;
        }
        if row.kind == Kind::Stopped {
            self.start_agent();
            return;
        }
        if let Some(located) = row.located.clone() {
            // Focus first, hide second: hiding returns focus to whatever was underneath, and doing
            // that after the focus would undo it.
            focus_pane_with_id(PaneId::Terminal(located.pane_id), true, false);
            self.hide();
        } else if let Some((name, pane)) = row
            .running
            .as_ref()
            .and_then(|r| Some((r.zellij_session.clone()?, r.pane.clone()?)))
        {
            // The pane in the other session is already known from the last scan: straight there.
            eprintln!("lvim-zclaude: switch to {name}, tab {}, pane {} (known)", pane.tab, pane.id);
            self.last_error = None;
            self.hide();
            switch_session_with_focus(&name, Some(pane.tab), Some((pane.id, false)));
        } else if let Some(name) = row.running.as_ref().and_then(|r| r.zellij_session.clone()) {
            // **Another zellij session: ask it where its claude is, then switch the client there.**
            // The pane ids of another session are not in this plugin's manifest, so they are asked
            // for with `list-panes`; the answer comes back as a `RunCommandResult` and
            // `jump_there` finishes the job. The cwd goes along to pick the right pane when that
            // session runs more than one claude.
            let cwd = row.running.as_ref().map(|r| r.cwd.clone()).unwrap_or_else(|| row.cwd.clone());
            self.last_error = Some(format!("going to {name}…"));
            run_command(
                &["zellij", "--session", &name, "action", "list-panes", "-a", "-j"],
                BTreeMap::from([
                    ("step".to_string(), "panes".to_string()),
                    ("session".to_string(), name.clone()),
                    ("cwd".to_string(), cwd),
                ]),
            );
        } else {
            self.last_error = Some("that session is not running".to_string());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{one_line, plan_start, Geometry, Launch, Missing};
    use crate::model::{Group, Kind, Located, Row};
    use crate::scan::{RemotePane, Running};
    use std::collections::BTreeMap;

    fn row(kind: Kind, project: &str, cwd: &str, id: &str) -> Row {
        Row {
            kind,
            project: project.to_string(),
            cwd: cwd.to_string(),
            id: id.to_string(),
            branch: String::new(),
            last: 0,
            running: None,
            located: None,
            count: 1,
            child: false,
        }
    }

    fn agent(cwd: &str, session: Option<&str>, tab_id: Option<u32>) -> Row {
        let mut row = row(Kind::Agent, cwd.rsplit('/').next().unwrap(), cwd, "s1");
        row.running = Some(Running {
            pid: 7,
            session_id: "s1".to_string(),
            cwd: cwd.to_string(),
            name: String::new(),
            busy: false,
            updated_at: 0,
            ancestors: vec![],
            zellij_session: session.map(str::to_string),
            pane: tab_id.map(|tab_id| RemotePane {
                id: 3,
                tab: 0,
                tab_id,
                tab_name: "Tab #1".to_string(),
                title: String::new(),
            }),
            hook: None,
        });
        row
    }

    /// **The boundary between "clear" and "ask".** Clear when the row settles session and
    /// directory by itself; asked otherwise, prefilled with the likeliest answer.
    #[test]
    fn n_starts_where_the_row_says_and_asks_for_what_it_does_not() {
        // An agent in another session: its session, its directory, its tab. Clear.
        let there = agent("/mnt/dev/bru", Some("EXAMS"), Some(4));
        let (launch, ask) = plan_start(&there, &[], Some("RUST"), Some(4)).unwrap();
        assert_eq!(ask, None);
        assert_eq!(launch.session.as_deref(), Some("EXAMS"));
        assert_eq!(launch.cwd.as_deref(), Some("/mnt/dev/bru"));
        assert_eq!(launch.tab_id, Some(4));
        assert!(launch.args.is_empty(), "a fresh claude, not --continue");
        assert_eq!(
            launch.command().unwrap(),
            ["zellij", "--session", "EXAMS", "action", "new-pane", "--tab-id", "4", "--cwd", "/mnt/dev/bru", "--", "claude"]
        );

        // An agent here (located): this session. Clear.
        let mut here = agent("/mnt/dev/bru", None, None);
        here.located = Some(Located { tab_position: 1, tab_name: "Tab #2".into(), pane_id: 9 });
        let (launch, ask) = plan_start(&here, &[], Some("RUST"), None).unwrap();
        assert_eq!((launch.session.as_deref(), ask), (Some("RUST"), None));

        // An agent outside zellij: the session is asked, prefilled with this one.
        let outside = agent("/mnt/dev/bru", None, None);
        let (launch, ask) = plan_start(&outside, &[], Some("RUST"), None).unwrap();
        assert_eq!(ask, Some((Missing::Session, "RUST".to_string())));
        assert_eq!(launch.session, None);
        assert_eq!(launch.command(), Err("no session to start it in"), "not without an answer");

        // A session heading whose agents share one directory: clear. Two directories: asked,
        // prefilled with the topmost.
        let heading = row(Kind::Header { agents: 2, busy: 0, folded: false, group: Group::Session }, "EXAMS", "", "");
        let same = ["/a".to_string()];
        let (launch, ask) = plan_start(&heading, &same, Some("RUST"), None).unwrap();
        assert_eq!(ask, None);
        assert_eq!((launch.session.as_deref(), launch.cwd.as_deref()), (Some("EXAMS"), Some("/a")));
        assert_eq!(launch.tab_id, None, "the session's focused tab");
        let mixed = ["/a".to_string(), "/b".to_string()];
        let (_, ask) = plan_start(&heading, &mixed, Some("RUST"), None).unwrap();
        assert_eq!(ask, Some((Missing::Directory, "/a".to_string())));
        let (_, ask) = plan_start(&heading, &[], Some("RUST"), None).unwrap();
        assert_eq!(ask, Some((Missing::Directory, String::new())), "no agents: nothing to prefill");

        // The `stopped` heading is no place to start anything.
        let stopped = row(Kind::Header { agents: 1, busy: 0, folded: false, group: Group::Stopped }, crate::model::STOPPED, "", "");
        assert!(plan_start(&stopped, &[], Some("RUST"), None).is_err());

        // A stopped transcript: `--resume <its id>`, the session asked and prefilled with this
        // one — so `Enter` alone resumes it here.
        let transcript = row(Kind::Stopped, "bru", "/mnt/dev/bru", "23aa89d8-216f-4c00");
        let (launch, ask) = plan_start(&transcript, &[], Some("RUST"), None).unwrap();
        assert_eq!(ask, Some((Missing::Session, "RUST".to_string())));
        assert_eq!(launch.args, ["--resume", "23aa89d8-216f-4c00"]);
        assert_eq!(launch.what, "resume bru · 23aa89d8");
        let answered = Launch { session: Some(" EXAMS ".to_string()), ..launch };
        assert_eq!(
            answered.command().unwrap(),
            ["zellij", "--session", "EXAMS", "action", "new-pane", "--cwd", "/mnt/dev/bru", "--", "claude", "--resume", "23aa89d8-216f-4c00"]
        );

        // A directory that is not known yet (a slug) is refused before anything is asked.
        let unknown = row(Kind::Stopped, "-mnt-dev-x", "-mnt-dev-x", "id");
        assert!(plan_start(&unknown, &[], Some("RUST"), None).is_err());
        let relative = Launch { cwd: Some("bru".to_string()), ..answered };
        assert_eq!(relative.command(), Err("the directory must be an absolute path"));
    }

    #[test]
    fn a_paste_is_folded_onto_one_line() {
        assert_eq!(one_line("виж го\r\n\n  и кажи \n"), "виж го и кажи");
        assert_eq!(one_line("one line"), "one line");
        assert_eq!(one_line("\n\n"), "");
    }

    #[test]
    fn the_geometry_comes_from_the_configuration_then_the_payload() {
        let mut geometry = Geometry::from_config(&BTreeMap::from([
            ("width".to_string(), "90%".to_string()),
            ("x".to_string(), "  ".to_string()), // blank: the default stays
        ]));
        assert_eq!(geometry.width, "90%");
        assert_eq!(geometry.x, "10%");
        geometry.apply("height=50%, y=25%;junk=1 width= ");
        assert_eq!(geometry.height, "50%");
        assert_eq!(geometry.y, "25%");
        assert_eq!(geometry.width, "90%", "an empty value changes nothing");
        geometry.apply("");
        assert_eq!(geometry, Geometry::from_config(&BTreeMap::from([
            ("x".to_string(), "10%".to_string()),
            ("y".to_string(), "25%".to_string()),
            ("width".to_string(), "90%".to_string()),
            ("height".to_string(), "50%".to_string()),
        ])));
    }
}
