// The scanner: one `sh -c` per tick, one `RunCommandResult` back.
//
// A zellij plugin is WASI in a sandbox — it cannot open `~/.claude` at all, and it has no clock.
// So everything the board knows arrives as the output of one shell script: the time, the session
// files, the cwd and branch of the ones we do not know yet, the running processes, and the names
// of the zellij servers they live under.
//
// **No transcript is ever read whole.** They reach tens of megabytes; the script uses `stat` for
// times, `head -c 16384` for the cwd (the first records are `{"type":"mode"…}` with no cwd, so one
// line is not enough) and `tail -c 65536` for the branch, which changes as the session goes on.
//
// The output is one label per line, tab separated, so the parser is a `split('\t')` and never a
// JSON reader. The one piece of real JSON — `~/.claude/sessions/<pid>.json`, about 650 bytes — is
// read with [`field`], the same "find the key, take the value" trick `lvim-zpm` uses.

/// `$NEED_HEAD` / `$NEED_TAIL` are replaced with the paths the plugin still wants, and `CLAUDE_DIR`
/// is written in front of the script — `run_command` takes no environment, and
/// `run_command_with_env_variables_and_cwd` would be a second call shape for one variable.
const SCAN: &str = r#"
D="${CLAUDE_DIR:-$HOME/.claude}"
printf 'T\t%s\n' "$(date +%s)"
for f in "$D"/projects/*/*.jsonl; do
  [ -f "$f" ] || continue
  d=${f%/*}; d=${d##*/}; b=${f##*/}; b=${b%.jsonl}
  printf 'F\t%s\t%s\t%s\n' "$(stat -c '%Y	%s' "$f")" "$d" "$b"
done
for f in __NEED_HEAD__; do
  [ -f "$f" ] || continue
  b=${f##*/}; b=${b%.jsonl}
  printf 'H\t%s\t%s\n' "$b" "$(head -c 16384 "$f" | grep -o '"cwd":"[^"]*"' | head -1 | cut -d'"' -f4)"
done
for f in __NEED_TAIL__; do
  [ -f "$f" ] || continue
  b=${f##*/}; b=${b%.jsonl}
  printf 'B\t%s\t%s\n' "$b" "$(tail -c 65536 "$f" | grep -o '"gitBranch":"[^"]*"' | tail -1 | cut -d'"' -f4)"
done
zpids=""
for j in "$D"/sessions/*.json; do
  [ -f "$j" ] || continue
  p=${j##*/}; p=${p%.json}
  case "$p" in *[!0-9]*) continue;; esac
  [ -r "/proc/$p/stat" ] || continue
  rest=$(cat "/proc/$p/stat"); rest=${rest#*') '}
  set -- $rest
  ppid=$2; start=${20}
  argv0=$(tr '\0' '\n' < "/proc/$p/cmdline" 2>/dev/null | head -1)
  chain=""; q=$ppid
  while [ -n "$q" ] && [ "$q" -gt 1 ] 2>/dev/null; do
    chain="$chain,$q"; zpids="$zpids $q"
    r2=$(cat "/proc/$q/stat" 2>/dev/null) || break
    r2=${r2#*') '}; set -- $r2; q=$2
  done
  printf 'R\t%s\t%s\t%s\t%s\t' "$p" "$start" "${chain#,}" "$argv0"
  tr -d '\n' < "$j"; echo
done
for q in $(printf '%s\n' $zpids | sort -u); do
  c=$(tr '\0' '\n' < "/proc/$q/cmdline" 2>/dev/null)
  a0=$(printf '%s\n' "$c" | sed -n 1p)
  a1=$(printf '%s\n' "$c" | sed -n 2p)
  a2=$(printf '%s\n' "$c" | sed -n 3p)
  case "$a0" in
    *zellij) [ "$a1" = "--server" ] && printf 'Z\t%s\t%s\n' "$q" "${a2##*/}" ;;
  esac
done
for q in $(printf '%s\n' $zpids | sort -u); do
  c=$(tr '\0' '\n' < "/proc/$q/cmdline" 2>/dev/null)
  a0=$(printf '%s\n' "$c" | sed -n 1p); a1=$(printf '%s\n' "$c" | sed -n 2p); a2=$(printf '%s\n' "$c" | sed -n 3p)
  case "$a0" in
    *zellij) [ "$a1" = "--server" ] && printf 'P\t%s\t%s\n' "${a2##*/}" "$(zellij --session "${a2##*/}" action list-panes -a -j 2>/dev/null </dev/null | tr -d '\n')" ;;
  esac
done
"#;

/// The script with the two lists put in.
///
/// An empty list would leave the glob-less `for f in ;` as a syntax error, so it becomes a name
/// that cannot exist and the loop's `[ -f ]` skips it. The paths are slugs and uuids — no spaces,
/// no quoting problem.
pub fn script(claude_dir: &str, need_head: &[String], need_tail: &[String]) -> String {
    let fill = |list: &[String]| {
        if list.is_empty() {
            "/nonexistent-lvim-zclaude".to_string()
        } else {
            list.join(" ")
        }
    };
    // Double quotes, so the default `$HOME/.claude` still expands and a directory with a space
    // in it does not become two words.
    let header = format!("CLAUDE_DIR=\"{}\"\n", claude_dir.replace('"', ""));
    header
        + &SCAN
            .replace("__NEED_HEAD__", &fill(need_head))
            .replace("__NEED_TAIL__", &fill(need_tail))
}

/// One `.jsonl` in `~/.claude/projects/<dir>/`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionFile {
    pub id: String,
    pub dir: String,
    pub mtime: u64,
    pub size: u64,
}

/// A session that is running now, from `~/.claude/sessions/<pid>.json` plus `/proc`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Running {
    pub pid: i32,
    pub session_id: String,
    pub cwd: String,
    pub name: String,
    pub busy: bool,
    pub updated_at: u64,
    /// Parent pids, nearest first, up to init. The pane a session runs in is found by looking for
    /// one of these among the panes' pids — `claude` is a child of the pane's shell.
    pub ancestors: Vec<i32>,
    /// The zellij server in the chain, if the session runs under one.
    pub zellij_session: Option<String>,
    /// The pane it runs in, in *that* zellij session — from that session's `list-panes` (the `P`
    /// lines), matched by `claude_pane`. Filled by the board after the parse, not here, because
    /// the match needs the cwd. `None` when the session is not under zellij, or the pane could not
    /// be told apart.
    pub pane: Option<RemotePane>,
    /// What the session's Claude Code hooks said last — waiting for a person, or done with its
    /// turn — when `hooks/lvim-zclaude-hook.sh` is installed. `None`: working, or no hooks.
    pub hook: Option<Hook>,
}

/// A word from a Claude Code hook about one session. `sessions/<pid>.json` knows only busy and
/// idle; "waiting for you" — a permission prompt, a question — exists nowhere else.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hook {
    pub state: HookState,
    /// The `notification_type` behind it (`permission_prompt`, `elicitation_dialog`, …), or the
    /// hook's name for `done` (`Stop`, `idle_prompt`).
    pub kind: String,
    /// When, in seconds since the epoch.
    pub at: u64,
    /// What the notification said (`message` of a `Notification` hook: "Claude needs your
    /// permission to use Bash"), on one line. Empty for `Stop`. **This is the agent's own word to
    /// the person, not a line of the conversation** — the same text the desktop notification
    /// carries.
    pub message: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HookState {
    /// Needs a person: a permission, an answer.
    Waiting,
    /// Finished its turn and nobody has replied yet.
    Done,
}

/// One event from the hook script, as its pipe payload says it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HookEvent {
    pub session_id: String,
    /// `None`: the session is working again — whatever was waiting or done is over.
    pub hook: Option<Hook>,
    pub cwd: String,
    /// This board is the one that should tell the desktop (the hook picks exactly one).
    pub notify: bool,
}

/// The payload of a `lvim-zclaude-event` pipe:
/// `session_id \t state \t kind \t cwd \t epoch \t notify [\t message]` — the shape
/// `hooks/lvim-zclaude-hook.sh` writes. `state` is `waiting`, `done` or `working`. Anything
/// short or unknown is `None`: a stray pipe must not invent an agent. The seventh field is the
/// notification's own text, and a payload without it (an older hook script) still works.
pub fn hook_event(payload: &str) -> Option<HookEvent> {
    let parts: Vec<&str> = payload.trim_end_matches('\n').split('\t').collect();
    if parts.len() < 5 || parts[0].is_empty() {
        return None;
    }
    let at = parts[4].trim().parse().unwrap_or(0);
    let kind = parts[2].trim().to_string();
    let message = parts.get(6).map(|m| m.split_whitespace().collect::<Vec<_>>().join(" ")).unwrap_or_default();
    let hook = match parts[1].trim() {
        "waiting" => Some(Hook { state: HookState::Waiting, kind, at, message }),
        "done" => Some(Hook { state: HookState::Done, kind, at, message }),
        "working" => None,
        _ => return None,
    };
    Some(HookEvent {
        session_id: parts[0].trim().to_string(),
        hook,
        cwd: parts[3].trim().to_string(),
        notify: parts.get(5).map(|n| n.trim() == "1").unwrap_or(false),
    })
}

/// A `claude` pane in another zellij session, as `list-panes -a -j` describes it: enough to write
/// into it, close it, jump to it, and say what it is doing (its title, which Claude keeps current).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemotePane {
    pub id: u32,
    /// The tab's position — what `switch_session_with_focus` and the `TAB` column use.
    pub tab: usize,
    /// The tab's id — what `new-pane --tab-id` takes. Positions shift when tabs are moved or
    /// closed; ids do not. Falls back to the position when the answer has no `tab_id`.
    pub tab_id: u32,
    pub tab_name: String,
    pub title: String,
}

#[derive(Clone, Debug, Default)]
pub struct Scan {
    pub now: u64,
    pub files: Vec<SessionFile>,
    /// `sessionId` → cwd, from `head`.
    pub heads: Vec<(String, String)>,
    /// `sessionId` → branch, from `tail`.
    pub branches: Vec<(String, String)>,
    pub running: Vec<Running>,
    /// zellij session name → its `list-panes -a -j`, on one line, for every zellij session that
    /// hosts a running claude (the `P` lines). ~10 KB and ~10 ms a session, measured 2026-09-20.
    pub panes: Vec<(String, String)>,
}

/// Read the scanner's output.
///
/// Unknown labels and short lines are skipped rather than failing the whole scan: a machine that
/// grows a new kind of file in `~/.claude` must not take the board down.
pub fn parse(out: &str) -> Scan {
    let mut scan = Scan::default();
    // pid of a zellij server → session name, collected from the `Z` lines.
    let mut servers: Vec<(i32, String)> = Vec::new();
    let mut raw_running: Vec<(Running, Vec<i32>)> = Vec::new();

    for line in out.lines() {
        let mut parts = line.split('\t');
        match parts.next() {
            Some("T") => {
                if let Some(now) = parts.next().and_then(|t| t.trim().parse().ok()) {
                    scan.now = now;
                }
            }
            Some("F") => {
                let (mtime, size, dir, id) = (parts.next(), parts.next(), parts.next(), parts.next());
                if let (Some(mtime), Some(size), Some(dir), Some(id)) = (mtime, size, dir, id) {
                    scan.files.push(SessionFile {
                        id: id.to_string(),
                        dir: dir.to_string(),
                        mtime: mtime.trim().parse().unwrap_or(0),
                        size: size.trim().parse().unwrap_or(0),
                    });
                }
            }
            Some("H") => {
                if let (Some(id), Some(cwd)) = (parts.next(), parts.next()) {
                    if !cwd.is_empty() {
                        scan.heads.push((id.to_string(), cwd.to_string()));
                    }
                }
            }
            Some("B") => {
                if let (Some(id), Some(branch)) = (parts.next(), parts.next()) {
                    if !branch.is_empty() {
                        scan.branches.push((id.to_string(), branch.to_string()));
                    }
                }
            }
            Some("R") => {
                let pid = parts.next().and_then(|p| p.parse::<i32>().ok());
                let start = parts.next().unwrap_or("").trim().to_string();
                let chain: Vec<i32> = parts
                    .next()
                    .unwrap_or("")
                    .split(',')
                    .filter_map(|p| p.trim().parse().ok())
                    .collect();
                let argv0 = parts.next().unwrap_or("");
                let json = parts.next().unwrap_or("");
                let Some(pid) = pid else { continue };
                // Two guards, both from the plan: a pid can be reused between the session file
                // being written and this scan, and something that is not claude can hold the pid.
                let claimed = field(json, "procStart").unwrap_or_default();
                if !claimed.is_empty() && claimed != start {
                    continue;
                }
                if !argv0.rsplit('/').next().unwrap_or("").starts_with("claude") {
                    continue;
                }
                let running = Running {
                    pid,
                    session_id: field(json, "sessionId").unwrap_or_default(),
                    cwd: field(json, "cwd").unwrap_or_default(),
                    name: field(json, "name").unwrap_or_default(),
                    busy: field(json, "status").as_deref() == Some("busy"),
                    updated_at: field(json, "updatedAt")
                        .and_then(|u| u.parse::<u64>().ok())
                        .map(|ms| if ms > 1_000_000_000_000 { ms / 1000 } else { ms })
                        .unwrap_or(0),
                    ancestors: chain.clone(),
                    zellij_session: None,
                    pane: None,
                    hook: None,
                };
                raw_running.push((running, chain));
            }
            Some("Z") => {
                if let (Some(pid), Some(name)) = (parts.next(), parts.next()) {
                    if let Ok(pid) = pid.parse() {
                        servers.push((pid, name.to_string()));
                    }
                }
            }
            Some("P") => {
                if let (Some(name), Some(json)) = (parts.next(), parts.next()) {
                    if !name.is_empty() && json.trim_start().starts_with('[') {
                        scan.panes.push((name.to_string(), json.to_string()));
                    }
                }
            }
            _ => {}
        }
    }

    for (mut running, chain) in raw_running {
        running.zellij_session = chain
            .iter()
            .find_map(|pid| servers.iter().find(|(p, _)| p == pid).map(|(_, n)| n.clone()));
        scan.running.push(running);
    }
    scan
}

/// The value of `"key"` in a flat JSON object, as text.
///
/// Enough for `sessions/<pid>.json`, which is flat and small, and one function instead of a JSON
/// crate in a plugin that must stay under a megabyte. Quoted values are returned unquoted; numbers
/// and booleans come back as they are written. Escapes are not unescaped — no field bru reads has
/// one, and a wrong cwd is better caught than silently half-decoded.
pub fn field(json: &str, key: &str) -> Option<String> {
    let needle = format!("\"{key}\":");
    let at = json.find(&needle)? + needle.len();
    let rest = json[at..].trim_start();
    if let Some(text) = rest.strip_prefix('"') {
        let end = text.find('"')?;
        Some(text[..end].to_string())
    } else {
        let end = rest.find([',', '}']).unwrap_or(rest.len());
        let value = rest[..end].trim();
        if value.is_empty() {
            None
        } else {
            Some(value.to_string())
        }
    }
}

/// The top-level objects of a JSON array, as slices — what `zellij action list-panes -j` answers.
///
/// A brace counter that respects strings, rather than a JSON crate: the objects are flat enough for
/// [`field`] once they are apart, and a plugin that has to stay under a megabyte does not carry a
/// parser for one call.
pub fn objects(json: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let (mut depth, mut start, mut in_string, mut escaped) = (0usize, 0usize, false, false);
    for (at, c) in json.char_indices() {
        if in_string {
            match c {
                _ if escaped => escaped = false,
                '\\' => escaped = true,
                '"' => in_string = false,
                _ => {}
            }
            continue;
        }
        match c {
            '"' => in_string = true,
            '{' => {
                if depth == 0 {
                    start = at;
                }
                depth += 1;
            }
            '}' if depth > 0 => {
                depth -= 1;
                if depth == 0 {
                    out.push(&json[start..=at]);
                }
            }
            _ => {}
        }
    }
    out
}

/// The pane `claude` runs in, in another zellij session, from that session's `list-panes -a -j`:
/// its pane id, its tab, and its title.
///
/// Matched on the command **and** the directory, because a session can run two `claude`s — in two
/// projects — and the one to jump to is the one whose cwd the board row carries. When no pane has
/// that directory but exactly one runs `claude`, that one is it: a `cd` inside the pane moves its
/// cwd and not the session file's.
pub fn claude_pane(json: &str, cwd: &str) -> Option<RemotePane> {
    let is_claude = |object: &str| {
        field(object, "pane_command")
            .map(|command| {
                let program = command.split_whitespace().next().unwrap_or("");
                program.rsplit('/').next().unwrap_or("") == "claude"
            })
            .unwrap_or(false)
    };
    let panes: Vec<&str> = objects(json)
        .into_iter()
        .filter(|object| field(object, "is_plugin").as_deref() != Some("true"))
        .filter(|object| is_claude(object))
        .collect();
    let pick = |object: &str| -> Option<RemotePane> {
        let id = field(object, "id")?.parse().ok()?;
        let tab = field(object, "tab_position")?.parse().ok()?;
        Some(RemotePane {
            id,
            tab,
            tab_id: field(object, "tab_id").and_then(|t| t.parse().ok()).unwrap_or(tab as u32),
            tab_name: field(object, "tab_name").unwrap_or_else(|| format!("Tab {}", tab + 1)),
            title: field(object, "title").unwrap_or_default(),
        })
    };
    if let Some(object) = panes
        .iter()
        .find(|object| field(object, "pane_cwd").as_deref() == Some(cwd))
    {
        return pick(object);
    }
    if panes.len() == 1 {
        return pick(panes[0]);
    }
    None
}

/// The id of the tab a terminal pane is in, from a session's `list-panes -a -j` — for a pane
/// this plugin knows by id from its own manifest, which names tabs by position only. `None` when
/// the pane is not in the answer or the answer carries no `tab_id` (an older zellij).
pub fn tab_id_of(json: &str, pane_id: u32) -> Option<u32> {
    objects(json)
        .into_iter()
        .filter(|object| field(object, "is_plugin").as_deref() != Some("true"))
        .find(|object| field(object, "id").and_then(|id| id.parse::<u32>().ok()) == Some(pane_id))
        .and_then(|object| field(object, "tab_id"))
        .and_then(|id| id.parse().ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SESSION_JSON: &str = r#"{"sessionId":"23aa89d8-216f","cwd":"/mnt/dev/bru","name":"bru-1c","status":"busy","procStart":"3502756","updatedAt":1789243219192,"pid":295646}"#;

    #[test]
    fn a_flat_json_field_is_read_without_a_json_crate() {
        assert_eq!(field(SESSION_JSON, "sessionId").as_deref(), Some("23aa89d8-216f"));
        assert_eq!(field(SESSION_JSON, "cwd").as_deref(), Some("/mnt/dev/bru"));
        assert_eq!(field(SESSION_JSON, "status").as_deref(), Some("busy"));
        // A number keeps its own spelling, and a key that is not there is not an error.
        assert_eq!(field(SESSION_JSON, "updatedAt").as_deref(), Some("1789243219192"));
        assert_eq!(field(SESSION_JSON, "pid").as_deref(), Some("295646"));
        assert_eq!(field(SESSION_JSON, "nothing"), None);
    }

    #[test]
    fn the_scanner_output_becomes_files_and_running_sessions() {
        let out = format!(
            "T\t1789000000\n\
             F\t1788999000\t27000000\t-mnt-dev-bru\t23aa89d8-216f\n\
             F\t1788000000\t4096\t-mnt-dev-lua\taaaabbbb-cccc\n\
             H\t23aa89d8-216f\t/mnt/dev/bru\n\
             B\t23aa89d8-216f\tmain\n\
             R\t295646\t3502756\t295600,779771,1\t/usr/bin/claude\t{SESSION_JSON}\n\
             Z\t779771\tRUST\n\
             P\tRUST\t[{{\"id\":3,\"is_plugin\":false}}]\n\
             P\tGONE\t\n"
        );
        let scan = parse(&out);
        assert_eq!(scan.panes.len(), 1, "a session that answered nothing has no panes");
        assert_eq!(scan.panes[0].0, "RUST");
        assert_eq!(scan.now, 1789000000);
        assert_eq!(scan.files.len(), 2);
        assert_eq!(scan.files[0].id, "23aa89d8-216f");
        assert_eq!(scan.files[0].mtime, 1788999000);
        assert_eq!(scan.heads, [("23aa89d8-216f".to_string(), "/mnt/dev/bru".to_string())]);
        assert_eq!(scan.branches, [("23aa89d8-216f".to_string(), "main".to_string())]);

        assert_eq!(scan.running.len(), 1);
        let running = &scan.running[0];
        assert_eq!(running.pid, 295646);
        assert_eq!(running.name, "bru-1c");
        assert!(running.busy);
        assert_eq!(running.ancestors, [295600, 779771, 1]);
        // The zellij server in the chain names the session the pane lives in.
        assert_eq!(running.zellij_session.as_deref(), Some("RUST"));
        // Milliseconds become seconds, so `last` compares with a file's mtime.
        assert_eq!(running.updated_at, 1789243219);
    }

    /// **The two guards that keep a stale session file from inventing a session.** A pid is
    /// reused, and a session file outlives the process that wrote it.
    #[test]
    fn a_reused_pid_and_a_process_that_is_not_claude_are_skipped() {
        let stale = format!("R\t295646\t9999999\t1\t/usr/bin/claude\t{SESSION_JSON}\n");
        assert!(parse(&stale).running.is_empty(), "procStart does not match");

        let other = format!("R\t295646\t3502756\t1\t/usr/bin/zsh\t{SESSION_JSON}\n");
        assert!(parse(&other).running.is_empty(), "the process is not claude");

        let good = format!("R\t295646\t3502756\t1\t/usr/bin/claude\t{SESSION_JSON}\n");
        assert_eq!(parse(&good).running.len(), 1);
    }

    /// An empty want-list must not leave `for f in ;` behind — that is a syntax error, and the
    /// whole scan would come back as one stderr line.
    #[test]
    fn the_script_survives_wanting_nothing() {
        let script = script("$HOME/.claude", &[], &[]);
        assert!(script.starts_with("CLAUDE_DIR=\"$HOME/.claude\"\n"));
        assert!(!script.contains("__NEED_HEAD__") && !script.contains("__NEED_TAIL__"));
        assert!(script.contains("/nonexistent-lvim-zclaude"));
        assert!(!script.contains("in ;"), "an empty list would be a syntax error");

        let some = script_with(&["/a/b.jsonl".to_string()]);
        assert!(some.contains("for f in /a/b.jsonl; do"));
    }

    fn script_with(head: &[String]) -> String {
        script("$HOME/.claude", head, &[])
    }

    /// The shape `zellij action list-panes -a -j` answered for the EXAMS session on 2026-09-19,
    /// trimmed to the fields that matter plus one nested value, which is what the brace counter is
    /// there to survive.
    const PANES: &str = r#"[
      {"id":2,"is_plugin":true,"plugin_url":"zellij:status-bar","tab_position":0},
      {"id":0,"is_plugin":false,"tab_id":7,"tab_position":0,"tab_name":"Tab #1","pane_command":"claude --continue","pane_cwd":"/mnt/dev/apps/school-exams","cursor_coordinates_in_pane":[3,4],"title":"✳ School-exams {tasks}"},
      {"id":4,"is_plugin":false,"tab_position":0,"pane_command":"secmon-tui","pane_cwd":"/mnt/dev/apps/school-exams"},
      {"id":7,"is_plugin":false,"tab_position":2,"pane_command":"/usr/bin/claude","pane_cwd":"/mnt/dev/rust/bru"}
    ]"#;

    #[test]
    fn a_json_array_splits_into_its_objects_through_strings_and_nesting() {
        let objects = objects(PANES);
        assert_eq!(objects.len(), 4, "a brace inside a title string is not a brace");
        assert!(objects[1].contains("\"pane_command\":\"claude --continue\""));
        assert_eq!(field(objects[3], "id").as_deref(), Some("7"));
    }

    /// **Command and directory together**, because one zellij session can run two claudes.
    #[test]
    fn the_claude_pane_is_found_by_command_and_directory() {
        let exams = claude_pane(PANES, "/mnt/dev/apps/school-exams").unwrap();
        assert_eq!((exams.id, exams.tab), (0, 0));
        assert_eq!(exams.tab_name, "Tab #1");
        assert_eq!(exams.tab_id, 7, "the tab's id, for `new-pane --tab-id`");
        assert_eq!(exams.title, "✳ School-exams {tasks}", "the title is what the agent is doing");
        let bru = claude_pane(PANES, "/mnt/dev/rust/bru").unwrap();
        assert_eq!((bru.id, bru.tab), (7, 2), "a full path is claude too");
        assert_eq!(bru.tab_name, "Tab 3", "no tab name in the answer: the position, as zellij shows it");
        assert_eq!(bru.tab_id, 2, "no tab id in the answer: the position stands in");
        assert_eq!(bru.title, "");
        // Two claudes and neither in this directory: no guess.
        assert_eq!(claude_pane(PANES, "/somewhere/else"), None);
        // One claude and a directory that moved under it: that one.
        let one = r#"[{"id":5,"is_plugin":false,"tab_position":1,"pane_command":"claude","pane_cwd":"/moved"}]"#;
        assert_eq!(claude_pane(one, "/original").map(|p| (p.id, p.tab)), Some((5, 1)));
        // A secmon pane in the right directory is not a claude pane.
        let none = r#"[{"id":4,"is_plugin":false,"tab_position":0,"pane_command":"secmon-tui","pane_cwd":"/d"}]"#;
        assert_eq!(claude_pane(none, "/d"), None);
    }

    /// The hook script's payload, and the two things that must not become an event: a short
    /// line, and a state nobody defined.
    #[test]
    fn a_hook_payload_becomes_an_event() {
        let waiting = hook_event("23aa-89d8\twaiting\tpermission_prompt\t/mnt/dev/bru\t1789000000\t1\tClaude needs your  permission to use Bash\n").unwrap();
        assert_eq!(waiting.session_id, "23aa-89d8");
        assert_eq!(
            waiting.hook,
            Some(Hook {
                state: HookState::Waiting,
                kind: "permission_prompt".into(),
                at: 1789000000,
                message: "Claude needs your permission to use Bash".into(),
            })
        );
        assert_eq!(waiting.cwd, "/mnt/dev/bru");
        assert!(waiting.notify);
        let done = hook_event("23aa-89d8\tdone\tStop\t/mnt/dev/bru\t1789000001\t0").unwrap();
        assert_eq!(done.hook.as_ref().map(|h| h.state), Some(HookState::Done));
        assert_eq!(done.hook.unwrap().message, "", "an older hook script: six fields, no message");
        assert!(!done.notify, "another board tells the desktop");
        let working = hook_event("23aa-89d8\tworking\tUserPromptSubmit\t/mnt/dev/bru\t1789000002").unwrap();
        assert_eq!(working.hook, None, "working clears whatever was there");
        assert!(!working.notify, "no sixth field: no notification");
        assert_eq!(hook_event("23aa-89d8\tblocked\tx\t/d\t1"), None, "a state nobody defined");
        assert_eq!(hook_event("hints_set"), None, "a stray broadcast");
        assert_eq!(hook_event("\twaiting\tk\t/d\t1\t1"), None, "no session id");
    }

    /// A pane this session knows by id (from the manifest) is given its tab's id by the same
    /// `list-panes` answer — the manifest has positions only, `new-pane --tab-id` wants ids.
    #[test]
    fn a_local_pane_learns_its_tab_id_from_list_panes() {
        assert_eq!(tab_id_of(PANES, 0), Some(7));
        assert_eq!(tab_id_of(PANES, 4), None, "no tab_id in that object");
        assert_eq!(tab_id_of(PANES, 99), None, "no such pane");
        // A plugin pane with that id is not a terminal pane.
        assert_eq!(tab_id_of(PANES, 2), None);
    }
}
