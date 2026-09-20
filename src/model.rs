// What the board shows, built from a scan.
//
// **By zellij session and pane**, since 2026-09-20: the question the board answers first is
// "where are my agents", so the first level is the zellij session (this one on top), and under
// it one row per agent — which is one row per pane. An agent's pane is what the row carries: its
// tab, its title (what the agent is doing), its project and branch. The agents that run outside
// zellij get a group of their own.
//
// The *stopped* sessions — transcripts nobody runs — come last, by project: one row per project
// with its newest transcript, the rest counted (`×N`) and shown under it when the row is expanded.
// The project is the cwd the transcript was started in, not the directory name under
// `~/.claude/projects/` — that name is the path with `/` turned into `-`, and half the projects
// here have a `-` in their own name, so turning it back is guesswork.

use crate::scan::{HookState, Running, SessionFile};
use std::collections::BTreeMap;

/// How the agents are arranged. `s` cycles through them; `order` in the configuration picks the
/// first. The stopped block comes last whatever the order.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Order {
    /// By zellij session, this one first; under it by urgency, then tab — the default.
    #[default]
    Session,
    /// By project, the way the board was before 2026-09-20; under it by session and tab.
    Project,
    /// One flat list, the most urgent first: waiting, done, busy, idle; then the newest.
    Urgency,
}

impl Order {
    pub fn next(self) -> Order {
        match self {
            Order::Session => Order::Project,
            Order::Project => Order::Urgency,
            Order::Urgency => Order::Session,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Order::Session => "by session",
            Order::Project => "by project",
            Order::Urgency => "by urgency",
        }
    }

    /// `session`, `project` or `urgency`; anything else is the default.
    pub fn parse(name: &str) -> Order {
        match name.trim() {
            "project" => Order::Project,
            "urgency" => Order::Urgency,
            _ => Order::Session,
        }
    }
}

/// What a heading is the heading of.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Group {
    /// A zellij session — a place: `N` starts an agent in it, `Enter` switches to it.
    Session,
    /// A project (the order by project): its directory is known, its session is not.
    Project,
    /// The agents that run outside any zellij session.
    Outside,
    /// The transcripts nobody runs.
    Stopped,
}

/// Where a running session was found in *this* zellij session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Located {
    pub tab_position: usize,
    pub tab_name: String,
    pub pane_id: u32,
}

/// What a row is: a group heading (a zellij session, the agents outside zellij, or the stopped
/// block), an agent, or a stopped transcript. The cursor stops on every row: a heading folds and
/// unfolds (`Tab`), and the heading of a zellij session is also a place — `N` starts an agent
/// there, `Enter` goes there.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Kind {
    /// `agents` under it, `busy` of them. For the stopped block, `agents` is the project count.
    /// `folded`: the rows under it are not shown.
    Header { agents: usize, busy: usize, folded: bool, group: Group },
    Agent,
    Stopped,
}

/// The heading of the block of transcripts nobody runs.
pub const STOPPED: &str = "stopped";
/// The heading for agents that run outside any zellij session.
pub const OUTSIDE: &str = "outside zellij";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    pub kind: Kind,
    /// The project label — or, on a header, the heading itself.
    pub project: String,
    pub cwd: String,
    pub id: String,
    pub branch: String,
    /// Last activity, in seconds since the epoch.
    pub last: u64,
    pub running: Option<Running>,
    pub located: Option<Located>,
    /// How many session files this project has.
    pub count: usize,
    /// A child row is one of the older sessions of the project above it.
    pub child: bool,
}

impl Row {
    pub fn is_header(&self) -> bool {
        matches!(self.kind, Kind::Header { .. })
    }

    pub fn group(&self) -> Option<Group> {
        match self.kind {
            Kind::Header { group, .. } => Some(group),
            _ => None,
        }
    }

    /// The heading of a zellij session — a place: agents can be started in it and the client
    /// can be switched to it. The `stopped` and `outside zellij` headings are not.
    pub fn is_session(&self) -> bool {
        self.group() == Some(Group::Session)
    }

    pub fn is_folded(&self) -> bool {
        matches!(self.kind, Kind::Header { folded: true, .. })
    }

    /// What the hooks said, if anything: waiting for a person, or done.
    pub fn hook_state(&self) -> Option<HookState> {
        self.running.as_ref()?.hook.as_ref().map(|hook| hook.state)
    }

    /// `◆` waiting for a person, `✓` done and unanswered, `●` busy, `◐` running but idle,
    /// `○` not running.
    pub fn glyph(&self) -> &'static str {
        match (self.hook_state(), &self.running) {
            (Some(HookState::Waiting), _) => "◆",
            (Some(HookState::Done), _) => "✓",
            (None, Some(running)) if running.busy => "●",
            (None, Some(_)) => "◐",
            (None, None) => "○",
        }
    }

    /// How much this row wants a look: 0 waiting, 1 done, 2 busy, 3 idle, 4 stopped.
    pub fn urgency(&self) -> u8 {
        match (self.hook_state(), &self.running) {
            (Some(HookState::Waiting), _) => 0,
            (Some(HookState::Done), _) => 1,
            (None, Some(running)) if running.busy => 2,
            (None, Some(_)) => 3,
            (None, None) => 4,
        }
    }

    /// The zellij session the agent runs in: this one's name, another's, or none.
    pub fn session(&self, session_name: Option<&str>) -> Option<String> {
        if self.located.is_some() {
            return Some(session_name.unwrap_or("here").to_string());
        }
        self.running.as_ref()?.zellij_session.clone()
    }

    /// What the `TAB` column says: the tab — with the session in front of it when the row does
    /// not sit under its session's heading (`with_session`). A stopped transcript has nowhere.
    pub fn where_text(&self, session_name: Option<&str>, with_session: bool) -> String {
        let (session, tab) = if let Some(located) = &self.located {
            (session_name.unwrap_or("here").to_string(), located.tab_name.clone())
        } else {
            let Some(running) = self.running.as_ref() else { return "—".to_string() };
            match (running.zellij_session.as_deref(), running.pane.as_ref()) {
                (Some(name), Some(pane)) => (name.to_string(), pane.tab_name.clone()),
                (Some(name), None) => (name.to_string(), "?".to_string()),
                (None, _) => return "—".to_string(),
            }
        };
        if with_session {
            format!("{session} · {tab}")
        } else if tab == "?" {
            format!("{session} · ?")
        } else {
            tab
        }
    }
}

/// The label for a project: as much of the tail of its path as it takes to be its own.
///
/// One segment is usually enough. When it is not, another is added, and another — `go/keyforge`
/// under `/mnt/development` and under `RECOVERED/projects` share *two* segments, and a board that
/// printed them the same would be a board with two identical rows and no way to tell which is
/// which. Measured 2026-09-19 on the real data: `go/keyforge` twice, `php/motion-php` twice.
pub fn project_label(cwd: &str, all: &[String]) -> String {
    let tail = |path: &str, segments: usize| {
        let parts: Vec<&str> = path.trim_end_matches('/').rsplit('/').filter(|p| !p.is_empty()).collect();
        let take = segments.min(parts.len());
        parts[..take].iter().rev().copied().collect::<Vec<_>>().join("/")
    };
    let depth = cwd.split('/').filter(|p| !p.is_empty()).count().max(1);
    for segments in 1..=depth {
        let mine = tail(cwd, segments);
        let twins = all.iter().filter(|other| tail(other, segments) == mine).count();
        if twins <= 1 {
            return mine;
        }
    }
    cwd.to_string()
}

/// How long ago, in one column: `12s`, `4m`, `3h`, `6d`.
pub fn age(now: u64, then: u64) -> String {
    if then == 0 || now < then {
        return "—".to_string();
    }
    let seconds = now - then;
    match seconds {
        0..=59 => format!("{seconds}s"),
        60..=3599 => format!("{}m", seconds / 60),
        3600..=86_399 => format!("{}h", seconds / 3600),
        _ => format!("{}d", seconds / 86_400),
    }
}

/// Everything the board needs about one project's sessions, before it becomes rows.
struct Project {
    cwd: String,
    sessions: Vec<(SessionFile, u64, String)>, // file, last, branch
}

fn header(title: &str, group: Group, cwd: &str, agents: usize, busy: usize, folded: bool) -> Row {
    Row {
        kind: Kind::Header { agents, busy, folded, group },
        project: title.to_string(),
        cwd: cwd.to_string(),
        id: String::new(),
        branch: String::new(),
        last: 0,
        running: None,
        located: None,
        count: agents,
        child: false,
    }
}

/// Build the rows: the agents in the `order` asked for (by zellij session — `here` first, then by
/// name, the ones outside zellij last — by project, or flat by urgency); then the stopped
/// transcripts by project, newest first, children under an expanded project.
///
/// `cwds` maps a session id to the project it belongs to, `branches` to its branch — both learned
/// by the scanner and kept across scans. A file whose cwd is not known yet is grouped under its
/// directory name, so it shows up rather than disappearing until the next scan. `folded` names
/// the headings whose rows are left out (the heading itself stays, with its counts).
pub fn build_rows(
    files: &[SessionFile],
    cwds: &BTreeMap<String, String>,
    branches: &BTreeMap<String, String>,
    running: &[Running],
    expanded: &[String],
    here: Option<&str>,
    folded: &[String],
    order: Order,
) -> Vec<Row> {
    let is_folded = |title: &str| folded.iter().any(|name| name == title);
    let mut projects: BTreeMap<String, Project> = BTreeMap::new();
    for file in files {
        let cwd = cwds.get(&file.id).cloned().unwrap_or_else(|| file.dir.clone());
        let branch = branches.get(&file.id).cloned().unwrap_or_default();
        let last = running
            .iter()
            .find(|r| r.session_id == file.id)
            .map(|r| r.updated_at.max(file.mtime))
            .unwrap_or(file.mtime);
        projects
            .entry(cwd.clone())
            .or_insert_with(|| Project { cwd, sessions: Vec::new() })
            .sessions
            .push((file.clone(), last, branch));
    }
    // **A running session with no transcript yet.** A bare `claude` gets a new session id and
    // writes `sessions/<pid>.json` at once, but its `.jsonl` appears only with the first message —
    // so for the minutes before that its project may not exist on disk at all (seen 2026-09-20:
    // `tally` started, the board saying "not running"). Its cwd is still a project.
    for live in running {
        if live.cwd.is_empty() {
            continue;
        }
        projects
            .entry(live.cwd.clone())
            .or_insert_with(|| Project { cwd: live.cwd.clone(), sessions: Vec::new() });
    }
    for project in projects.values_mut() {
        project.sessions.sort_by(|a, b| b.1.cmp(&a.1));
    }
    let all_cwds: Vec<String> = projects.keys().cloned().collect();
    let label_of = |cwd: &str| project_label(cwd, &all_cwds);
    let count_of = |cwd: &str| projects.get(cwd).map(|p| p.sessions.len()).unwrap_or(0);

    // --- the agents ---
    let agent_row = |live: &Running| {
        let file = projects
            .get(&live.cwd)
            .and_then(|p| p.sessions.iter().find(|(file, _, _)| file.id == live.session_id));
        let (last, branch) = match file {
            Some((_, last, branch)) => (*last, branch.clone()),
            None => (live.updated_at, String::new()),
        };
        Row {
            kind: Kind::Agent,
            project: label_of(&live.cwd),
            cwd: live.cwd.clone(),
            id: live.session_id.clone(),
            branch,
            last,
            running: Some(live.clone()),
            located: None,
            count: count_of(&live.cwd),
            child: false,
        }
    };
    // An agent that needs a person comes first in its group; then by tab and pane, an agent
    // whose pane is not known after the ones whose is.
    let by_urgency_then_pane = |row: &Row| {
        let live = row.running.as_ref();
        let pane = live.and_then(|l| l.pane.as_ref());
        (row.urgency(), match pane {
            Some(pane) => (0, pane.tab, pane.id),
            None => (1, 0, live.map(|l| l.pid as u32).unwrap_or(0)),
        })
    };
    let session_rank = |name: &str| {
        if Some(name) == here {
            0
        } else if name == OUTSIDE {
            2
        } else {
            1
        }
    };

    // Group name → (group, cwd, its agents). The urgency order has one nameless group.
    let mut groups: Vec<(String, Group, String, Vec<Row>)> = Vec::new();
    for live in running {
        let (name, group, cwd) = match order {
            Order::Session => match live.zellij_session.clone() {
                Some(name) => (name, Group::Session, String::new()),
                None => (OUTSIDE.to_string(), Group::Outside, String::new()),
            },
            Order::Project => (label_of(&live.cwd), Group::Project, live.cwd.clone()),
            Order::Urgency => (String::new(), Group::Outside, String::new()),
        };
        match groups.iter_mut().find(|(n, _, _, _)| *n == name) {
            Some((_, _, _, agents)) => agents.push(agent_row(live)),
            None => groups.push((name, group, cwd, vec![agent_row(live)])),
        }
    }
    match order {
        Order::Session => groups.sort_by(|(a, ..), (b, ..)| session_rank(a).cmp(&session_rank(b)).then(a.cmp(b))),
        Order::Project => groups.sort_by(|(a, ..), (b, ..)| a.cmp(b)),
        Order::Urgency => {}
    }

    let mut rows: Vec<Row> = Vec::new();
    for (name, group, cwd, agents) in groups.iter_mut() {
        match order {
            Order::Urgency => agents.sort_by(|a, b| a.urgency().cmp(&b.urgency()).then(b.last.cmp(&a.last))),
            Order::Session => agents.sort_by_key(by_urgency_then_pane),
            // Under a project: by session (this one first), then as in a session.
            Order::Project => agents.sort_by_key(|row| {
                let session = row.session(here).unwrap_or_else(|| OUTSIDE.to_string());
                (session_rank(&session), session, by_urgency_then_pane(row))
            }),
        }
        let busy = agents.iter().filter(|row| row.running.as_ref().is_some_and(|l| l.busy)).count();
        if order != Order::Urgency {
            rows.push(header(name, *group, cwd, agents.len(), busy, is_folded(name)));
            if is_folded(name) {
                continue;
            }
        }
        rows.append(agents);
    }

    // --- the stopped transcripts, by project ---
    let held = |id: &str| running.iter().any(|live| live.session_id == id);
    let mut stopped: Vec<Vec<Row>> = Vec::new();
    for project in projects.values() {
        let mut free = project.sessions.iter().filter(|(file, _, _)| !held(&file.id));
        let Some((file, last, branch)) = free.next() else { continue };
        let label = label_of(&project.cwd);
        let count = project.sessions.len();
        let mut group = vec![Row {
            kind: Kind::Stopped,
            project: label.clone(),
            cwd: project.cwd.clone(),
            id: file.id.clone(),
            branch: branch.clone(),
            last: *last,
            running: None,
            located: None,
            count,
            child: false,
        }];
        if expanded.iter().any(|cwd| *cwd == project.cwd) {
            for (file, last, branch) in free {
                group.push(Row {
                    kind: Kind::Stopped,
                    project: label.clone(),
                    cwd: project.cwd.clone(),
                    id: file.id.clone(),
                    branch: branch.clone(),
                    last: *last,
                    running: None,
                    located: None,
                    count,
                    child: true,
                });
            }
        }
        stopped.push(group);
    }
    stopped.sort_by(|a, b| b[0].last.cmp(&a[0].last).then(a[0].project.cmp(&b[0].project)));
    if !stopped.is_empty() {
        rows.push(header(STOPPED, Group::Stopped, "", stopped.len(), 0, is_folded(STOPPED)));
        if !is_folded(STOPPED) {
            rows.extend(stopped.into_iter().flatten());
        }
    }
    rows
}

/// The one-line summary for a status bar, twice: as prose (`3 agents · 1 busy · 1 waiting`) and
/// as `key=value` pairs (`agents=3 busy=1 waiting=1 done=0 idle=1`) — the board's API for bars,
/// written to `summary_file` and broadcast as the `lvim-zclaude-summary` pipe.
pub fn summary(running: &[Running]) -> (String, String) {
    let waiting = running
        .iter()
        .filter(|live| live.hook.as_ref().map(|h| h.state) == Some(HookState::Waiting))
        .count();
    let done = running
        .iter()
        .filter(|live| live.hook.as_ref().map(|h| h.state) == Some(HookState::Done))
        .count();
    let busy = running.iter().filter(|live| live.busy && live.hook.is_none()).count();
    let idle = running.len() - waiting - done - busy;
    let mut prose = format!("{} agent{}", running.len(), if running.len() == 1 { "" } else { "s" });
    for (count, word) in [(busy, "busy"), (waiting, "waiting"), (done, "done")] {
        if count > 0 {
            prose.push_str(&format!(" · {count} {word}"));
        }
    }
    let pairs = format!("agents={} busy={busy} waiting={waiting} done={done} idle={idle}", running.len());
    (prose, pairs)
}

/// A question an agent has put on its screen with numbered options — Claude Code's permission
/// prompt (`Do you want to proceed?` / `1. Yes` / `2. Yes, and don't ask again…` / `3. No…`),
/// its plan approval, its `AskUserQuestion`. The board turns it into buttons.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Question {
    pub text: String,
    /// The number the option is chosen with, and its text.
    pub options: Vec<(u8, String)>,
}

/// A screen line without Claude Code's box: the `│` at either end, the `❯` marker, the spaces.
fn unframed(line: &str) -> String {
    let trimmed = line.trim();
    let trimmed = trimmed.strip_prefix('│').unwrap_or(trimmed);
    let trimmed = trimmed.strip_suffix('│').unwrap_or(trimmed);
    trimmed.trim().to_string()
}

/// `N. text` → `(N, text)`, with or without the `❯` in front.
fn numbered(line: &str) -> Option<(u8, String)> {
    let line = unframed(line);
    let line = line.strip_prefix('❯').map(str::trim_start).unwrap_or(&line);
    let (number, rest) = line.split_once(". ")?;
    let number: u8 = number.trim().parse().ok()?;
    let rest = rest.trim();
    if rest.is_empty() {
        return None;
    }
    Some((number, rest.to_string()))
}

/// **The question on a screen, if the last thing on it is one.** The rule: the bottom of the
/// screen (blank lines, the `╰` of the box, the input line `❯` and the status line under it are
/// looked past) ends with options numbered 1, 2, 3… in order — a line under an option that is
/// not numbered and not blank is that option's wrap — and the question is the nearest non-blank
/// line above the first option. A numbered list further up, with the agent still talking under
/// it, is not a question: an answer is not a prompt.
pub fn question(lines: &[String]) -> Option<Question> {
    // Walk up from the bottom past what is under a prompt.
    // **Case does not matter here.** Claude Code writes `Esc to cancel · Tab to amend` under a
    // permission prompt with a capital E; a lowercase-only test walked into it, the walk up
    // stopped on a line that is not an option, and a real prompt was read as no prompt at all —
    // the board then showed three raw lines with the question itself scrolled off (2026-09-20).
    let is_chrome = |line: &str| {
        let text = unframed(line);
        let lower = text.to_lowercase();
        text.is_empty()
            || text.starts_with('╰')
            || text.starts_with('─')
            || text == "❯"
            || text.starts_with("⏸")
            || lower.starts_with("esc to")
            || lower.starts_with("tab to")
            || lower.contains("auto-compact")
            || lower.contains("tip:")
            || text.starts_with("⎿")
    };
    let mut end = lines.len();
    while end > 0 && is_chrome(&lines[end - 1]) {
        end -= 1;
    }
    if end == 0 {
        return None;
    }
    // The options, bottom up. A wrapped option continues *under* its number, so walking up an
    // unnumbered line is held until the numbered line above claims it; what is still held when
    // the numbers stop is the question itself (it sits right above the first option).
    let mut options: Vec<(u8, String)> = Vec::new();
    let mut held: Vec<String> = Vec::new();
    let mut at = end;
    while at > 0 {
        let line = &lines[at - 1];
        if let Some((number, text)) = numbered(line) {
            let mut text = text;
            for wrap in held.drain(..).rev() {
                text.push(' ');
                text.push_str(&wrap);
            }
            options.push((number, text));
            at -= 1;
            continue;
        }
        let text = unframed(line);
        if options.is_empty() || text.is_empty() || text.starts_with('╭') || held.len() >= 2 {
            break;
        }
        held.push(text);
        at -= 1;
    }
    options.reverse();
    if options.is_empty() || options[0].0 != 1 {
        return None;
    }
    if options.iter().enumerate().any(|(i, (n, _))| *n as usize != i + 1) {
        return None;
    }
    // The question: what was held above the first option, else the nearest non-blank line.
    let text = if held.is_empty() {
        let mut up = at;
        let mut found = String::new();
        while up > 0 {
            let candidate = unframed(&lines[up - 1]);
            if !candidate.is_empty() {
                found = candidate;
                break;
            }
            up -= 1;
        }
        found
    } else {
        held.reverse();
        held.join(" ")
    };
    Some(Question { text, options })
}

/// Only the walk up from the bottom of a screen uses this: once a real line is found, everything
/// above it is the conversation and is kept as it is — a `❯` there is an option of a prompt, not
/// furniture.
fn prompt_furniture(line: &str) -> bool {
    line.is_empty()
        || line.starts_with('╰')
        || line.starts_with('╭')
        || line.starts_with('─')
        || line.starts_with('›')
        || line == "❯"
        || line == "|"
        || line.starts_with("⏸")
        || line.starts_with("esc to")
        || line.contains("Tip:")
        || line.contains("auto-compact")
        || line.contains("shift+tab")
        || line.contains("for agents")
        || line.contains("to interrupt")
}

/// The screen without its box and without the prompt at the foot: what the agent said.
fn said(lines: &[String]) -> Vec<String> {
    let cleaned: Vec<String> = lines.iter().map(|line| unframed(line)).collect();
    let mut end = cleaned.len();
    while end > 0 && prompt_furniture(&cleaned[end - 1]) {
        end -= 1;
    }
    cleaned[..end].to_vec()
}

/// The last `count` **blocks** of what the agent said — a block being what a blank line separates,
/// which is one thing it did: a message, a tool call with its result, a thought.
///
/// Three lines were the first cut of this and they cut through the middle of a sentence as often
/// as not; a block is the unit a person reads. `max_lines` keeps a long block from eating the
/// list: the oldest block goes first, and a single block longer than the cap is shown by its
/// last lines.
pub fn tail_blocks(lines: &[String], count: usize, max_lines: usize) -> Vec<String> {
    let said = said(lines);
    let mut blocks: Vec<Vec<String>> = Vec::new();
    for line in said {
        if line.is_empty() {
            if blocks.last().is_some_and(|block| !block.is_empty()) {
                blocks.push(Vec::new());
            }
        } else {
            if blocks.last().is_none() {
                blocks.push(Vec::new());
            }
            blocks.last_mut().expect("a block was just pushed").push(line);
        }
    }
    blocks.retain(|block| !block.is_empty());
    let mut out: Vec<String> = Vec::new();
    for block in blocks.iter().rev().take(count) {
        let mut block = block.clone();
        block.extend(out);
        out = block;
    }
    while out.len() > max_lines {
        out.remove(0);
    }
    out
}

/// Keep the rows a filter names. Case-insensitive, substring, over the columns a person can see.
/// A heading stays when anything under it stays.
pub fn filter(rows: &[Row], needle: &str, session_name: Option<&str>) -> Vec<Row> {
    if needle.is_empty() {
        return rows.to_vec();
    }
    let needle = needle.to_lowercase();
    let mut out: Vec<Row> = Vec::new();
    let mut heading: Option<Row> = None;
    for row in rows {
        if row.is_header() {
            heading = Some(row.clone());
            continue;
        }
        let name = row.running.as_ref().map(|r| r.name.as_str()).unwrap_or("");
        let haystack = format!(
            "{} {} {} {}",
            row.project,
            row.branch,
            name,
            row.where_text(session_name, true)
        );
        if haystack.to_lowercase().contains(&needle) {
            if let Some(heading) = heading.take() {
                out.push(heading);
            }
            out.push(row.clone());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scan::RemotePane;

    fn file(id: &str, dir: &str, mtime: u64) -> SessionFile {
        SessionFile { id: id.to_string(), dir: dir.to_string(), mtime, size: 10 }
    }

    fn running(id: &str, cwd: &str, busy: bool, updated: u64) -> Running {
        Running {
            pid: 1,
            session_id: id.to_string(),
            cwd: cwd.to_string(),
            name: format!("{id}-name"),
            busy,
            updated_at: updated,
            ancestors: vec![],
            zellij_session: Some("RUST".to_string()),
            pane: None,
            hook: None,
        }
    }

    fn in_session(mut live: Running, session: Option<&str>, pane: Option<(u32, usize, &str)>) -> Running {
        live.zellij_session = session.map(str::to_string);
        live.pane = pane.map(|(id, tab, title)| RemotePane {
            id,
            tab,
            tab_id: tab as u32 + 10,
            tab_name: format!("Tab #{}", tab + 1),
            title: title.to_string(),
        });
        live
    }

    /// **The ambiguity the cwd exists to solve**, and the deeper one the real data had: two
    /// projects that share their last *two* segments.
    #[test]
    fn a_project_is_named_by_as_much_of_its_path_as_it_takes() {
        let all = vec![
            "/mnt/dev/rust/bru".to_string(),
            "/home/x/RECOVERED/rust/bru".to_string(),
            "/mnt/dev/lua".to_string(),
            "/mnt/development/go/keyforge".to_string(),
            "/home/x/RECOVERED/projects/go/keyforge".to_string(),
        ];
        assert_eq!(project_label("/mnt/dev/lua", &all), "lua", "unique on its own");
        assert_eq!(project_label("/mnt/dev/rust/bru", &all), "dev/rust/bru");
        assert_eq!(project_label("/home/x/RECOVERED/rust/bru", &all), "RECOVERED/rust/bru");
        // Two segments are the same for both, so a third is what tells them apart.
        assert_eq!(project_label("/mnt/development/go/keyforge", &all), "development/go/keyforge");
        assert_eq!(
            project_label("/home/x/RECOVERED/projects/go/keyforge", &all),
            "projects/go/keyforge"
        );
    }

    #[test]
    fn age_is_one_column_wide() {
        assert_eq!(age(100, 100), "0s");
        assert_eq!(age(100, 60), "40s");
        assert_eq!(age(10_000, 6_400), "1h", "3600 seconds is the hour boundary");
        assert_eq!(age(10_000, 9_400), "10m");
        assert_eq!(age(100_000, 10_000), "1d", "over a day is days, not 25h");
        assert_eq!(age(100_000, 90_000), "2h");
        assert_eq!(age(1_000_000, 100_000), "10d");
        assert_eq!(age(100, 0), "—", "a session file with no time is not 56 years old");
        assert_eq!(age(100, 200), "—", "and neither is one from the future");
    }

    /// **The first level is the zellij session, this one on top**; under it, one row per pane,
    /// by tab. The stopped transcripts come after all of them, by project.
    #[test]
    fn agents_are_grouped_by_zellij_session_with_this_one_first_and_the_stopped_last() {
        let files = vec![
            file("a1", "-mnt-dev-bru", 500),
            file("a2", "-mnt-dev-bru", 400),
            file("b1", "-mnt-dev-lua", 900),
            file("c1", "-mnt-dev-go", 300),
        ];
        let cwds = BTreeMap::from([
            ("a1".to_string(), "/mnt/dev/bru".to_string()),
            ("a2".to_string(), "/mnt/dev/bru".to_string()),
            ("b1".to_string(), "/mnt/dev/lua".to_string()),
            ("c1".to_string(), "/mnt/dev/go".to_string()),
        ]);
        let branches = BTreeMap::from([("a1".to_string(), "main".to_string())]);
        let live = vec![
            in_session(running("b1", "/mnt/dev/lua", false, 1000), Some("EXAMS"), Some((0, 0, "✳ lua"))),
            in_session(running("a1", "/mnt/dev/bru", true, 1000), Some("RUST"), Some((2, 1, "✳ bru"))),
            in_session(running("x9", "/tmp/x", false, 1000), None, None),
        ];

        let rows = build_rows(&files, &cwds, &branches, &live, &[], Some("RUST"), &[], Order::Session);
        let headings: Vec<&str> = rows.iter().filter(|r| r.is_header()).map(|r| r.project.as_str()).collect();
        assert_eq!(headings, ["RUST", "EXAMS", OUTSIDE, STOPPED]);
        assert_eq!(rows[0].kind, Kind::Header { agents: 1, busy: 1, folded: false, group: Group::Session });
        assert_eq!(rows[1].kind, Kind::Agent);
        assert_eq!(rows[1].project, "bru");
        assert_eq!(rows[1].branch, "main", "the agent's transcript lends its branch");
        assert_eq!(rows[1].count, 2, "and its project's file count");
        assert_eq!(rows[1].where_text(Some("RUST"), false), "Tab #2");
        let pane = rows[1].running.as_ref().unwrap().pane.as_ref().unwrap();
        assert_eq!(pane.title, "✳ bru", "the pane's title is what the agent is doing");
        assert_eq!(rows[1].session(Some("RUST")).as_deref(), Some("RUST"));
        let outside = rows.iter().position(|r| r.project == OUTSIDE).unwrap();
        assert_eq!(rows[outside + 1].id, "x9");
        assert_eq!(rows[outside + 1].where_text(None, false), "—");

        let stopped = rows.iter().position(|r| r.project == STOPPED).unwrap();
        assert_eq!(rows[stopped].kind, Kind::Header { agents: 2, busy: 0, folded: false, group: Group::Stopped }, "bru and go; lua is held");
        assert_eq!(rows[stopped + 1].project, "bru", "the newest free transcript first");
        assert_eq!(rows[stopped + 1].id, "a2", "a1 is running, so a2 is the one that is stopped");
        assert_eq!(rows[stopped + 2].project, "go");
        assert_eq!(rows.len(), 9, "four headings, three agents, two stopped");
        // Only a zellij session's heading is a place; the other two are not.
        assert!(rows[0].is_session(), "RUST");
        assert!(!rows[outside].is_session() && !rows[stopped].is_session());

        // Expanding a stopped project adds its older free transcripts under it, and only there.
        let rows = build_rows(&files, &cwds, &branches, &live, &["/mnt/dev/bru".to_string()], Some("RUST"), &[], Order::Session);
        let stopped = rows.iter().position(|r| r.project == STOPPED).unwrap();
        assert_eq!(rows[stopped + 1].id, "a2");
        assert!(!rows[stopped + 1].child);
        assert_eq!(rows[stopped + 2].project, "go", "a1 is not a child: it runs");

        // **Folding** a heading keeps the heading, with its counts, and drops what is under it —
        // any heading, the stopped block included. The rest is untouched.
        let folded = ["RUST".to_string(), STOPPED.to_string()];
        let rows = build_rows(&files, &cwds, &branches, &live, &[], Some("RUST"), &folded, Order::Session);
        assert_eq!(rows[0].kind, Kind::Header { agents: 1, busy: 1, folded: true, group: Group::Session });
        assert!(rows[0].is_folded());
        assert_eq!(rows[1].project, "EXAMS", "RUST's agent is not shown");
        assert_eq!(rows[2].id, "b1", "EXAMS is open");
        let stopped = rows.iter().position(|r| r.project == STOPPED).unwrap();
        assert_eq!(stopped, rows.len() - 1, "nothing under the folded stopped block");
        assert_eq!(rows[stopped].kind, Kind::Header { agents: 2, busy: 0, folded: true, group: Group::Stopped });
    }

    /// Two agents in one session and one project are two panes, so two rows, by tab.
    #[test]
    fn two_agents_in_one_session_are_two_rows_by_tab() {
        let files = vec![file("a1", "-d", 500), file("a2", "-d", 400)];
        let cwds = BTreeMap::from([
            ("a1".to_string(), "/d".to_string()),
            ("a2".to_string(), "/d".to_string()),
        ]);
        let live = vec![
            in_session(running("a1", "/d", false, 600), Some("RUST"), Some((5, 2, ""))),
            in_session(running("a2", "/d", true, 700), Some("RUST"), Some((3, 0, ""))),
        ];
        let rows = build_rows(&files, &cwds, &BTreeMap::new(), &live, &[], None, &[], Order::Session);
        assert_eq!(rows.len(), 3, "one heading, two agents, nothing stopped");
        assert_eq!(rows[1].id, "a2", "tab 1 before tab 3");
        assert_eq!(rows[2].id, "a1");
    }

    /// A bare `claude` has a session file from its first second and a transcript only from its
    /// first message. In between it is running, and the board must say so.
    #[test]
    fn a_running_session_without_a_transcript_yet_is_still_a_row() {
        let files = vec![file("old", "-mnt-dev-tally", 500)];
        let cwds = BTreeMap::from([("old".to_string(), "/mnt/dev/tally".to_string())]);
        let live = vec![running("fresh", "/mnt/dev/tally", false, 900)];
        let rows = build_rows(&files, &cwds, &BTreeMap::new(), &live, &[], None, &[], Order::Session);
        assert_eq!(rows[1].kind, Kind::Agent);
        assert_eq!(rows[1].id, "fresh");
        assert_eq!(rows[1].project, "tally");
        assert_eq!(rows[1].last, 900);
        assert_eq!(rows[1].count, 1, "the old transcript is counted with it");
        assert!(rows[1].branch.is_empty(), "no transcript, no branch");
        // The old transcript is stopped, under the same project.
        let stopped = rows.iter().position(|r| r.project == STOPPED).unwrap();
        assert_eq!(rows[stopped + 1].id, "old");
    }

    fn with_hook(mut live: Running, state: HookState) -> Running {
        live.hook = Some(crate::scan::Hook { state, kind: "permission_prompt".into(), at: 5, message: String::new() });
        live
    }

    /// **The prompt as Claude Code draws it** (2.x): a box, the question, `❯` on the chosen
    /// option, the options numbered from 1, one of them wrapped onto a second line. The
    /// buttons under the list are read out of exactly this.
    const PERMISSION_PROMPT: &str = "\
● I'll build it now.

╭──────────────────────────────────────────────────────────────╮
│ Bash command                                                 │
│                                                              │
│   cargo build --release                                      │
│   Build the project                                          │
│                                                              │
│ Do you want to proceed?                                      │
│ ❯ 1. Yes                                                     │
│   2. Yes, and don't ask again for cargo build commands in    │
│   /mnt/dev/bru                                               │
│   3. No, and tell Claude what to do differently (esc)        │
│                                                              │
╰──────────────────────────────────────────────────────────────╯
";

    #[test]
    fn a_numbered_prompt_at_the_bottom_of_the_screen_is_a_question() {
        let lines: Vec<String> = PERMISSION_PROMPT.lines().map(str::to_string).collect();
        let q = question(&lines).expect("the permission prompt");
        assert_eq!(q.text, "Do you want to proceed?");
        assert_eq!(q.options.len(), 3);
        assert_eq!(q.options[0], (1, "Yes".to_string()), "the ❯ is not part of the option");
        assert_eq!(
            q.options[1],
            (2, "Yes, and don't ask again for cargo build commands in /mnt/dev/bru".to_string()),
            "a wrapped option is joined"
        );
        assert_eq!(q.options[2].1, "No, and tell Claude what to do differently (esc)");

        // Four options, no box, a question that is not the line right above: still found. The
        // blank lines and the input line under the prompt do not hide it.
        let plan = "\
 Would you like to proceed?

 ❯ 1. Yes, and auto-accept edits
   2. Yes, and manually approve edits
   3. No, keep planning
   4. Type something else

──────────────────────────
❯
";
        let lines: Vec<String> = plan.lines().map(str::to_string).collect();
        let q = question(&lines).unwrap();
        assert_eq!(q.text, "Would you like to proceed?");
        assert_eq!(q.options.iter().map(|(n, _)| *n).collect::<Vec<_>>(), [1, 2, 3, 4]);
        assert_eq!(q.options[3].1, "Type something else");

        // A numbered list in an answer, with the agent working on under it, is not a question:
        // the list is not the last thing on the screen.
        let answer = "\
● Three steps:
  1. build
  2. test
  3. ship

  Running 1 shell command…
  ⎿  $ cargo build

✻ Effecting… (6m 25s)
";
        let lines: Vec<String> = answer.lines().map(str::to_string).collect();
        assert_eq!(question(&lines), None);
        // A list that starts at 2, or skips a number, is not a prompt either.
        let odd: Vec<String> = ["Pick:", "  2. a", "  3. b"].iter().map(|s| s.to_string()).collect();
        assert_eq!(question(&odd), None);
        assert_eq!(question(&[]), None);
    }

    #[test]
    fn the_tail_of_a_screen_is_its_last_blocks_without_the_frame() {
        let lines: Vec<String> = PERMISSION_PROMPT.lines().map(str::to_string).collect();
        let last = tail_blocks(&lines, 3, 10);
        assert!(last.iter().any(|line| line.contains("Do you want to proceed?")), "{last:?}");
        assert!(last.last().is_some_and(|line| line.contains("3. No")), "{last:?}");
        assert_eq!(tail_blocks(&[], 3, 10), Vec::<String>::new());
    }

    /// **The line under a prompt is not an option.** `Esc to cancel · Tab to amend` is written
    /// with capitals; when the walk up from the bottom missed it, a real permission prompt read
    /// as no prompt and the board showed three stray lines instead of the question and buttons.
    #[test]
    fn a_prompt_is_found_under_its_esc_and_tab_line() {
        let screen = "\
● Diagnosing message sending

  Do you want to proceed?
  ❯ 1. Yes
    2. Yes, and switch to auto mode · auto mode handles these prompts for you
    3. No

  Esc to cancel · Tab to amend
";
        let lines: Vec<String> = screen.lines().map(str::to_string).collect();
        let found = question(&lines).expect("the prompt is there, under its own footer");
        assert_eq!(found.text, "Do you want to proceed?");
        assert_eq!(found.options.len(), 3);
        assert_eq!(found.options[0], (1, "Yes".to_string()));
        assert_eq!(found.options[2], (3, "No".to_string()));
    }

    /// **What the agent did, not where you type.** An idle screen ends with the input box, the
    /// hints under it and whatever is half-typed there; none of that is the agent's last word.
    #[test]
    fn the_blocks_walk_past_the_input_box() {
        let screen = "\
● Reloaded in ROOT.

  Ran 1 shell command
  ⎿  $ zellij action start-or-reload-plugin

✻ Brewed for 58s · done 5:51 PM

╭──────────────────────────────────────────╮
│ › do the commits                         │
╰──────────────────────────────────────────╯
  ▸▸ accept edits on (shift+tab to cycle) · ← for agents
";
        let lines: Vec<String> = screen.lines().map(str::to_string).collect();
        assert_eq!(
            tail_blocks(&lines, 3, 10),
            [
                "● Reloaded in ROOT.",
                "Ran 1 shell command",
                "⎿  $ zellij action start-or-reload-plugin",
                "✻ Brewed for 58s · done 5:51 PM"
            ],
            "three blocks, however many lines they are — and not the typed line or the hints"
        );

        // An agent waiting with nothing typed shows a bare `❯` and its own hint line; neither is
        // a thing it said, and the board showed exactly those two before this.
        let idle = "\
● Reloaded in ROOT.

╭──────────────────────────────────────────╮
│ ❯                                        │
╰──────────────────────────────────────────╯
  ‖ manual mode on · ? for shortcuts · ← for agents
";
        let lines: Vec<String> = idle.lines().map(str::to_string).collect();
        assert_eq!(tail_blocks(&lines, 3, 10), ["● Reloaded in ROOT."]);
    }

    /// **The other two orders.** By project: a heading per project (its directory on it), the
    /// agents under it by session, this one first. By urgency: flat, the one that waits first,
    /// then done, busy, idle — and the stopped block last in every order.
    #[test]
    fn the_order_by_project_and_by_urgency() {
        let files = vec![file("a1", "-d", 500), file("z9", "-e", 100)];
        let cwds = BTreeMap::from([
            ("a1".to_string(), "/mnt/dev/bru".to_string()),
            ("z9".to_string(), "/mnt/dev/old".to_string()),
        ]);
        let live = vec![
            in_session(running("a1", "/mnt/dev/bru", true, 900), Some("EXAMS"), Some((1, 0, ""))),
            in_session(running("b2", "/mnt/dev/bru", false, 800), Some("RUST"), Some((2, 0, ""))),
            in_session(with_hook(running("c3", "/mnt/dev/lua", false, 700), HookState::Waiting), Some("EXAMS"), Some((3, 1, ""))),
            in_session(with_hook(running("d4", "/mnt/dev/lua", false, 600), HookState::Done), None, None),
        ];
        let rows = build_rows(&files, &cwds, &BTreeMap::new(), &live, &[], Some("RUST"), &[], Order::Project);
        let headings: Vec<(&str, Option<Group>)> =
            rows.iter().filter(|r| r.is_header()).map(|r| (r.project.as_str(), r.group())).collect();
        assert_eq!(headings, [("bru", Some(Group::Project)), ("lua", Some(Group::Project)), (STOPPED, Some(Group::Stopped))]);
        assert_eq!(rows[0].cwd, "/mnt/dev/bru", "a project heading carries its directory");
        assert_eq!(rows[1].id, "b2", "under bru: this session (RUST) first");
        assert_eq!(rows[2].id, "a1");
        assert_eq!(rows[1].where_text(Some("RUST"), true), "RUST · Tab #1", "the session is in the column here");
        assert_eq!(rows[4].id, "c3", "under lua: the one that waits before the one that is done");
        assert_eq!(rows[4].glyph(), "◆");
        assert_eq!(rows[5].glyph(), "✓");

        let rows = build_rows(&files, &cwds, &BTreeMap::new(), &live, &[], Some("RUST"), &[], Order::Urgency);
        assert!(!rows[0].is_header(), "flat: no heading over the agents");
        let ids: Vec<&str> = rows.iter().filter(|r| r.kind == Kind::Agent).map(|r| r.id.as_str()).collect();
        assert_eq!(ids, ["c3", "d4", "a1", "b2"], "waiting, done, busy, idle");
        assert_eq!(rows.iter().map(Row::urgency).collect::<Vec<_>>()[..4], [0, 1, 2, 3]);
        assert_eq!(rows[4].project, STOPPED);
        assert_eq!(rows[5].id, "z9");

        // In the session order the one that waits is first in its session, whatever its tab.
        let rows = build_rows(&files, &cwds, &BTreeMap::new(), &live, &[], Some("RUST"), &[], Order::Session);
        let exams = rows.iter().position(|r| r.project == "EXAMS").unwrap();
        assert_eq!(rows[exams + 1].id, "c3");
        assert_eq!(rows[exams + 2].id, "a1");

        let (prose, pairs) = summary(&live);
        assert_eq!(prose, "4 agents · 1 busy · 1 waiting · 1 done");
        assert_eq!(pairs, "agents=4 busy=1 waiting=1 done=1 idle=1");
        assert_eq!(summary(&[]).0, "0 agents");
        assert_eq!(Order::Session.next().next().next(), Order::Session);
        assert_eq!(Order::parse("urgency"), Order::Urgency);
        assert_eq!(Order::parse("banana"), Order::Session);
    }

    #[test]
    fn the_filter_reads_the_columns_a_person_can_see_and_keeps_the_heading() {
        let files = vec![file("a1", "-d", 500), file("b1", "-e", 400)];
        let cwds = BTreeMap::from([
            ("a1".to_string(), "/mnt/dev/bru".to_string()),
            ("b1".to_string(), "/mnt/dev/lua".to_string()),
        ]);
        let live = vec![in_session(running("a1", "/mnt/dev/bru", false, 600), Some("EXAMS"), None)];
        let rows = build_rows(&files, &cwds, &BTreeMap::new(), &live, &[], None, &[], Order::Session);
        assert_eq!(filter(&rows, "", None).len(), rows.len());
        let bru = filter(&rows, "BRU", None);
        assert_eq!(bru.len(), 2, "the EXAMS heading and the bru agent; case-insensitive");
        assert!(bru[0].is_header());
        let exams = filter(&rows, "exams", None);
        assert_eq!(exams.len(), 2, "a session's name finds its agents");
        let lua = filter(&rows, "lua", None);
        assert_eq!(lua.len(), 2, "the stopped heading and lua");
        assert_eq!(lua[0].project, STOPPED);
        assert_eq!(filter(&rows, "nothing", None).len(), 0);
    }
}
