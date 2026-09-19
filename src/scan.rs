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
             Z\t779771\tRUST\n"
        );
        let scan = parse(&out);
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
}
