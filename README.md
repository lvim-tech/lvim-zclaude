# lvim-zclaude

A board of your Claude Code sessions, inside zellij. Which sessions exist, grouped by project;
which one is running right now and whether it is busy; how long ago it was touched; and `Enter` to
jump to the pane it runs in.

```
 lvim-zclaude · Claude Code sessions                          8 running · 24 files
   ST  PROJECT              BRANCH   LAST  NAME                 WHERE          N
 ▸ ●   bru                  main     14s   bru-1c               RUST · Tab 1   ×1
   ◐   school-exams         HEAD     1m    school-exams-ec      EXAMS          ×2
   ◐   mailforge            main     21m   mailforge-28         GO             ×1
   ○   motion-ui            main     5d    —                    —              ×2
   /mnt/development/biserstoilov/rust/bru · 23aa89d8-216f-428f-ad71-242ed9185b15
 ↑↓ move · Enter jump · Tab expand · type to filter · r refresh · Esc close
```

`●` busy, `◐` running and idle, `○` not running.

## What it reads, and what it does not

Claude Code keeps `~/.claude/projects/<project path with / turned into ->/<sessionId>.jsonl` — one
transcript per session — and, while a session is running, `~/.claude/sessions/<pid>.json` with its
id, directory, name, status and process start time.

The board is built from **metadata**: file names, `stat` times, and that small session file. It
never shows what was said. The only things read out of a transcript are the project directory (the
first `"cwd"` in the first 16 KB) and the git branch (the last `"gitBranch"` in the last 64 KB) —
the directory name under `projects/` cannot be turned back into a path, because plenty of projects
have a `-` in their own name. A transcript is never read whole; they reach tens of megabytes.

Nothing is ever written to `~/.claude`.

## Install

With [lvim-zpm](https://github.com/lvim-tech/lvim-zpm), add to `~/.config/zellij/zpm.kdl`:

```kdl
plugins {
    "lvim-tech/lvim-zclaude"
}
```

By hand: build (below), copy `lvim-zclaude.wasm` to `~/.config/zellij/plugins/`, and add a keybind:

```kdl
bind "a" { LaunchOrFocusPlugin "file:~/.config/zellij/plugins/lvim-zclaude.wasm" { floating true; move_to_focused_tab true; }; SwitchToMode "normal"; }
```

The first time it opens, zellij asks for permissions in the pane — answer `y`. They are remembered
in `~/.cache/zellij/permissions.kdl`.

## Keys

| key | does |
|---|---|
| `↑` `↓`, `j` `k` | move (`j`/`k` type into the filter once it is not empty) |
| `Enter` | jump to the pane the session runs in |
| `Tab` | expand a project: its older sessions, newest first |
| any letter | filter over project, branch, name and where |
| `Backspace` | undo one letter |
| `r` | rescan now |
| `g` `G`, `PageUp` `PageDown` | first, last, page |
| `Esc` | clear the filter, or close the board |
| `Ctrl c` | close the board |

## Permissions

`ReadApplicationState` (the panes and tabs, and `get_pane_pid` — how a `claude` process is matched
to a pane), `ChangeApplicationState` (focusing that pane; naming and sizing the board's own pane),
`RunCommands` (one `sh -c` per scan — a plugin is WASI in a sandbox and cannot read `~/.claude`
itself).

## Configuration

| key | default | meaning |
|---|---|---|
| `claude_dir` | `$HOME/.claude` | where Claude Code keeps its data |
| `tick` | `10` | seconds between scans, **while the board is visible** |

```kdl
bind "a" { LaunchOrFocusPlugin "file:~/.config/zellij/plugins/lvim-zclaude.wasm" { floating true; claude_dir "/some/copy"; tick "5"; }; }
```

## Building

```sh
cargo build --release --target wasm32-wasip1
cp target/wasm32-wasip1/release/lvim-zclaude.wasm ~/.config/zellij/plugins/
```

`cargo test` runs the unit tests for the pure parts (the scanner's parser, the rows, the column
cutting) on the host.

## Limits

- `Enter` on a session in **another zellij session** switches the client there and focuses its
  `claude` pane, found through `zellij --session NAME action list-panes` by command and directory.
  If the pane cannot be told apart, the client is switched to the session without a focus.
- Every colour comes from the active zellij theme (on the lvim themes, lvim-colorscheme's palette);
  the board owns none of its own.
- Stopped sessions are listed, not resumed. The board never starts, resumes or kills anything.
- A session whose pane has not been started yet (a resurrected, suspended pane) has no process, so
  it cannot be located.

## Licence

BSD-3-Clause.
