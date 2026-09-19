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

mod model;
mod scan;
mod ui;

use model::{Located, Row};
use scan::{Running, SessionFile};
use std::collections::{BTreeMap, BTreeSet};
use zellij_tile::prelude::*;

/// How often the board rescans while it is visible, in seconds.
const DEFAULT_TICK: f64 = 10.0;

#[derive(Default)]
struct State {
    // --- configuration ---
    claude_dir: String,
    tick: f64,

    // --- zellij ---
    permitted: bool,
    visible: bool,
    timer_armed: bool,
    own_id: u32,
    session_name: Option<String>,
    tabs: Vec<TabInfo>,
    /// Pane id → the pid of the process in it, asked once per pane. `None` when zellij would not
    /// say (a resurrected pane that has not been started yet).
    pane_pids: BTreeMap<u32, Option<i32>>,
    /// Pane id → the tab it is in, from the manifest.
    pane_tabs: BTreeMap<u32, usize>,

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

    // --- the board ---
    rows: Vec<Row>,
    shown: Vec<Row>,
    cursor: usize,
    filter: String,
    expanded: Vec<String>,
    last_error: Option<String>,
    /// The active zellij theme's colours, from `ModeUpdate`. See `ui::Palette`.
    palette: Option<ui::Palette>,
}

register_plugin!(State);

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

        // These two, and nothing else, before the permissions are in.
        request_permission(&[
            PermissionType::ReadApplicationState,
            PermissionType::ChangeApplicationState,
            PermissionType::RunCommands,
        ]);
        subscribe(&[
            EventType::PermissionRequestResult,
            EventType::ModeUpdate,
            EventType::TabUpdate,
            EventType::PaneUpdate,
            EventType::RunCommandResult,
            EventType::Timer,
            EventType::Key,
            EventType::Visible,
        ]);
    }

    fn update(&mut self, event: Event) -> bool {
        match event {
            Event::PermissionRequestResult(PermissionStatus::Granted) => {
                self.permitted = true;
                self.own_id = get_plugin_ids().plugin_id;
                rename_plugin_pane(self.own_id, "claude");
                change_floating_panes_coordinates(vec![(
                    PaneId::Plugin(self.own_id),
                    FloatingPaneCoordinates::new(
                        Some("10%".to_string()),
                        Some("15%".to_string()),
                        Some("80%".to_string()),
                        Some("70%".to_string()),
                        None,
                        None,
                    )
                    .unwrap_or_default(),
                )]);
                self.visible = true;
                self.scan();
                self.arm_timer();
                true
            }
            Event::PermissionRequestResult(PermissionStatus::Denied) => {
                self.permitted = false;
                self.last_error = Some("permissions denied — the board cannot read anything".into());
                true
            }
            Event::ModeUpdate(mode) => {
                // The theme arrives here, and again on every theme switch — so the board repaints
                // in the new colours without being reopened, as `lvim-zbar` does.
                self.palette = Some(ui::Palette::from_style(&mode.style.colors));
                self.session_name = mode.session_name;
                true
            }
            Event::TabUpdate(tabs) => {
                let changed = self.tabs != tabs;
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
                for (tab, panes) in &manifest.panes {
                    for pane in panes {
                        if pane.is_plugin {
                            continue;
                        }
                        live.insert(pane.id);
                        self.pane_tabs.insert(pane.id, *tab);
                        if !self.pane_pids.contains_key(&pane.id) {
                            let pid = get_pane_pid(PaneId::Terminal(pane.id)).ok();
                            self.pane_pids.insert(pane.id, pid);
                            asked = true;
                        }
                    }
                }
                self.pane_pids.retain(|id, _| live.contains(id));
                self.pane_tabs.retain(|id, _| live.contains(id));
                if asked {
                    self.locate();
                }
                asked
            }
            Event::RunCommandResult(code, out, err, context) => {
                if context.get("step").map(String::as_str) == Some("panes") {
                    self.jump_there(&String::from_utf8_lossy(&out), &context);
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
            Event::Timer(_) => {
                self.timer_armed = false;
                if self.visible && self.permitted {
                    self.scan();
                    self.arm_timer();
                }
                false
            }
            Event::Visible(visible) => {
                self.visible = visible;
                if visible && self.permitted {
                    self.scan();
                    self.arm_timer();
                }
                false
            }
            Event::Key(key) => self.key(key),
            _ => false,
        }
    }

    fn render(&mut self, rows: usize, cols: usize) {
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
        };
        ui::draw(&view, rows, cols);
    }
}

impl State {
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

    fn arm_timer(&mut self) {
        if !self.timer_armed {
            self.timer_armed = true;
            set_timeout(self.tick);
        }
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
        if first_sight {
            self.scan();
        }
    }

    fn rebuild(&mut self) {
        self.rows = model::build_rows(
            &self.files,
            &self.cwds,
            &self.branches,
            &self.running,
            &self.expanded,
        );
        self.locate();
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
    }

    fn key(&mut self, key: KeyWithModifier) -> bool {
        let typing = !self.filter.is_empty();
        let ctrl = key.key_modifiers.contains(&KeyModifier::Ctrl);
        match key.bare_key {
            BareKey::Esc => {
                if typing {
                    self.filter.clear();
                    self.reshow();
                } else {
                    hide_self();
                }
                true
            }
            BareKey::Char('c') if ctrl => {
                hide_self();
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
                true
            }
            BareKey::Char('G') if !typing => {
                self.cursor = self.shown.len().saturating_sub(1);
                true
            }
            BareKey::Char('r') if !typing => {
                self.scan();
                true
            }
            BareKey::Tab => {
                if let Some(row) = self.shown.get(self.cursor) {
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
                self.reshow();
                true
            }
            BareKey::Char(c) if !ctrl => {
                self.filter.push(c);
                self.reshow();
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
            Some((pane, tab)) => {
                eprintln!("lvim-zclaude: switch to {name}, tab {tab}, pane {pane}");
                self.last_error = None;
                hide_self();
                switch_session_with_focus(name, Some(tab), Some((pane, false)));
            }
            None => {
                eprintln!("lvim-zclaude: switch to {name}, no claude pane matched {cwd:?}");
                self.last_error = None;
                hide_self();
                switch_session_with_focus(name, None, None);
            }
        }
    }

    fn move_cursor(&mut self, by: i64) -> bool {
        if self.shown.is_empty() {
            return false;
        }
        let last = (self.shown.len() - 1) as i64;
        self.cursor = (self.cursor as i64 + by).clamp(0, last) as usize;
        true
    }

    /// Enter: go to where the session runs — a pane here, or a pane in another zellij session.
    fn jump(&mut self) {
        let Some(row) = self.shown.get(self.cursor) else {
            eprintln!("lvim-zclaude: jump with no row under the cursor");
            return;
        };
        eprintln!(
            "lvim-zclaude: jump row={} located={:?} running={}",
            row.project,
            row.located,
            row.running.is_some()
        );
        if let Some(located) = row.located.clone() {
            // Focus first, hide second: hiding returns focus to whatever was underneath, and doing
            // that after the focus would undo it.
            focus_pane_with_id(PaneId::Terminal(located.pane_id), true, false);
            hide_self();
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
