// What the board shows, built from a scan.
//
// One row per *project* — the cwd the sessions were started in, not the directory name under
// `~/.claude/projects/`. The two are not the same thing: that directory is the path with `/`
// turned into `-`, and half the projects here have a `-` in their own name (`lvim-zbar`,
// `motion-ui`, `school-exams`), so turning it back is guesswork. The cwd is read from the
// transcript instead, once per session file, and kept.
//
// A project's row carries its newest session; the rest are counted (`×N`) and shown under it when
// the row is expanded. A project with two *running* sessions gets a row each, because both are
// places you might want to jump to.

use crate::scan::{Running, SessionFile};
use std::collections::BTreeMap;

/// Where a running session was found in *this* zellij session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Located {
    pub tab_position: usize,
    pub tab_name: String,
    pub pane_id: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
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
    /// `●` busy, `◐` running but idle, `○` not running.
    pub fn glyph(&self) -> &'static str {
        match &self.running {
            Some(running) if running.busy => "●",
            Some(_) => "◐",
            None => "○",
        }
    }

    /// What the `WHERE` column says: a pane in this session, another zellij session, or nothing.
    pub fn where_text(&self, session_name: Option<&str>) -> String {
        if let Some(located) = &self.located {
            let here = session_name.unwrap_or("here");
            return format!("{here} · {}", located.tab_name);
        }
        match self.running.as_ref().and_then(|r| r.zellij_session.as_deref()) {
            Some(name) => name.to_string(),
            None => "—".to_string(),
        }
    }
}

/// The label for a project: its directory's name, and the parent too when the name alone would
/// name two different projects (`rust/bru` beside `RECOVERED/…/bru`).
pub fn project_label(cwd: &str, all: &[String]) -> String {
    let base = |path: &str| path.rsplit('/').next().unwrap_or(path).to_string();
    let mine = base(cwd);
    let twins = all.iter().filter(|other| base(other) == mine).count();
    if twins <= 1 {
        return mine;
    }
    let parent = cwd.trim_end_matches('/');
    let parent = &parent[..parent.len().saturating_sub(mine.len() + 1)];
    match parent.rsplit('/').next() {
        Some(up) if !up.is_empty() => format!("{up}/{mine}"),
        _ => mine,
    }
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

/// Build the rows: newest first, running before stopped, children under an expanded project.
///
/// `cwds` maps a session id to the project it belongs to, `branches` to its branch — both learned
/// by the scanner and kept across scans. A file whose cwd is not known yet is grouped under its
/// directory name, so it shows up rather than disappearing until the next scan.
pub fn build_rows(
    files: &[SessionFile],
    cwds: &BTreeMap<String, String>,
    branches: &BTreeMap<String, String>,
    running: &[Running],
    expanded: &[String],
) -> Vec<Row> {
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

    let all_cwds: Vec<String> = projects.keys().cloned().collect();
    let mut rows: Vec<Row> = Vec::new();

    for project in projects.values_mut() {
        project.sessions.sort_by(|a, b| b.1.cmp(&a.1));
        let count = project.sessions.len();
        let label = project_label(&project.cwd, &all_cwds);

        // A running session is a row of its own; the rest hang off the newest one.
        let mut shown: Vec<usize> = Vec::new();
        for (index, (file, _, _)) in project.sessions.iter().enumerate() {
            if running.iter().any(|r| r.session_id == file.id) {
                shown.push(index);
            }
        }
        if shown.is_empty() {
            shown.push(0);
        }

        for index in &shown {
            let (file, last, branch) = &project.sessions[*index];
            rows.push(Row {
                project: label.clone(),
                cwd: project.cwd.clone(),
                id: file.id.clone(),
                branch: branch.clone(),
                last: *last,
                running: running.iter().find(|r| r.session_id == file.id).cloned(),
                located: None,
                count,
                child: false,
            });
        }

        if expanded.iter().any(|cwd| *cwd == project.cwd) {
            for (index, (file, last, branch)) in project.sessions.iter().enumerate() {
                if shown.contains(&index) {
                    continue;
                }
                rows.push(Row {
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
    }

    // Busy first, then running, then by last activity. A child stays with its parent, so the sort
    // is on the parent's key and children keep the order they were pushed in.
    let key = |row: &Row| match &row.running {
        Some(r) if r.busy => 0,
        Some(_) => 1,
        None => 2,
    };
    let mut groups: Vec<Vec<Row>> = Vec::new();
    for row in rows {
        if row.child {
            if let Some(last) = groups.last_mut() {
                last.push(row);
                continue;
            }
        }
        groups.push(vec![row]);
    }
    groups.sort_by(|a, b| {
        key(&a[0])
            .cmp(&key(&b[0]))
            .then(b[0].last.cmp(&a[0].last))
            .then(a[0].project.cmp(&b[0].project))
    });
    groups.into_iter().flatten().collect()
}

/// Keep the rows a filter names. Case-insensitive, substring, over the columns a person can see.
pub fn filter(rows: &[Row], needle: &str, session_name: Option<&str>) -> Vec<Row> {
    if needle.is_empty() {
        return rows.to_vec();
    }
    let needle = needle.to_lowercase();
    rows.iter()
        .filter(|row| {
            let name = row.running.as_ref().map(|r| r.name.as_str()).unwrap_or("");
            let haystack = format!(
                "{} {} {} {}",
                row.project,
                row.branch,
                name,
                row.where_text(session_name)
            );
            haystack.to_lowercase().contains(&needle)
        })
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

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
        }
    }

    /// **The ambiguity the cwd exists to solve.** Two projects can end in the same name, and the
    /// directory under `~/.claude/projects` cannot be turned back into a path.
    #[test]
    fn a_project_shows_its_parent_only_when_its_name_is_not_its_own() {
        let all = vec![
            "/mnt/dev/rust/bru".to_string(),
            "/home/x/RECOVERED/rust/bru".to_string(),
            "/mnt/dev/lua".to_string(),
        ];
        assert_eq!(project_label("/mnt/dev/rust/bru", &all), "rust/bru");
        assert_eq!(project_label("/home/x/RECOVERED/rust/bru", &all), "rust/bru");
        assert_eq!(project_label("/mnt/dev/lua", &all), "lua", "unique, so no parent");
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

    #[test]
    fn rows_are_grouped_by_project_with_the_running_ones_first() {
        let files = vec![
            file("a1", "-mnt-dev-bru", 500),
            file("a2", "-mnt-dev-bru", 400),
            file("b1", "-mnt-dev-lua", 900),
        ];
        let cwds = BTreeMap::from([
            ("a1".to_string(), "/mnt/dev/bru".to_string()),
            ("a2".to_string(), "/mnt/dev/bru".to_string()),
            ("b1".to_string(), "/mnt/dev/lua".to_string()),
        ]);
        let branches = BTreeMap::from([("a1".to_string(), "main".to_string())]);
        let live = vec![running("a1", "/mnt/dev/bru", true, 1000)];

        let rows = build_rows(&files, &cwds, &branches, &live, &[]);
        assert_eq!(rows.len(), 2, "one row per project while nothing is expanded");
        assert_eq!(rows[0].project, "bru", "the busy one first, not the newest file");
        assert_eq!(rows[0].count, 2, "and it knows the project has two sessions");
        assert_eq!(rows[0].glyph(), "●");
        assert_eq!(rows[0].last, 1000, "updatedAt beats the file's mtime");
        assert_eq!(rows[1].project, "lua");
        assert_eq!(rows[1].glyph(), "○");

        // Expanding the project adds its older sessions under it, and only there.
        let rows = build_rows(&files, &cwds, &branches, &live, &["/mnt/dev/bru".to_string()]);
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[1].id, "a2");
        assert!(rows[1].child);
        assert_eq!(rows[2].project, "lua", "the child stayed with its parent");
    }

    /// Two sessions running in one project are two places to jump to, so each gets a row.
    #[test]
    fn two_running_sessions_in_one_project_are_two_rows() {
        let files = vec![file("a1", "-d", 500), file("a2", "-d", 400)];
        let cwds = BTreeMap::from([
            ("a1".to_string(), "/d".to_string()),
            ("a2".to_string(), "/d".to_string()),
        ]);
        let live = vec![
            running("a1", "/d", false, 600),
            running("a2", "/d", true, 700),
        ];
        let rows = build_rows(&files, &cwds, &BTreeMap::new(), &live, &[]);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].id, "a2", "busy first");
        assert_eq!(rows[1].id, "a1");
    }

    #[test]
    fn the_filter_reads_the_columns_a_person_can_see() {
        let files = vec![file("a1", "-d", 500), file("b1", "-e", 400)];
        let cwds = BTreeMap::from([
            ("a1".to_string(), "/mnt/dev/bru".to_string()),
            ("b1".to_string(), "/mnt/dev/lua".to_string()),
        ]);
        let rows = build_rows(&files, &cwds, &BTreeMap::new(), &[], &[]);
        assert_eq!(filter(&rows, "", None).len(), 2);
        assert_eq!(filter(&rows, "BRU", None).len(), 1, "case-insensitive");
        assert_eq!(filter(&rows, "nothing", None).len(), 0);
    }

    #[test]
    fn where_names_the_pane_here_or_the_other_session() {
        let mut row = Row {
            project: "bru".into(),
            cwd: "/d".into(),
            id: "a1".into(),
            branch: String::new(),
            last: 0,
            running: Some(running("a1", "/d", true, 0)),
            located: None,
            count: 1,
            child: false,
        };
        assert_eq!(row.where_text(Some("ZTEST")), "RUST", "not here, but named");
        row.located = Some(Located { tab_position: 0, tab_name: "Tab 1".into(), pane_id: 3 });
        assert_eq!(row.where_text(Some("ZTEST")), "ZTEST · Tab 1");
        row.located = None;
        row.running = None;
        assert_eq!(row.where_text(Some("ZTEST")), "—");
    }
}
