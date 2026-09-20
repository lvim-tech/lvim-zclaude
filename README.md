# lvim-zclaude

A board of your Claude Code agents, inside zellij. Where each one runs — **by zellij session and
pane**, this session first — whether it is busy, what it is doing, how long ago it moved; the
transcripts nobody runs, by project, last. `Enter` jumps to the pane; the other keys send an agent
a line, look at its screen, stop it, start one next to it, resume a stopped one, or close its pane,
without leaving the board.

```
 lvim-zclaude · Claude Code sessions                          3 running · 24 files
   ST  PROJECT              BRANCH   LAST  NAME                 TAB            N
 ▾ RUST 2 agents · 1 busy
 ▸1 ●   bru                  main     14s   bru-1c               Tab #1         ×1
  2 ◐   lvim-zclaude         main     3m    lvim-zclaude-8a      Tab #2         ×3
 ▾ EXAMS 1 agent · 0 busy
  3 ◆   school-exams         HEAD     1m    school-exams-ec      Tab #1         ×2
 ▾ stopped 2 projects
    ○   mailforge            main     21m   —                    —              ×1
    ○   motion-ui            main     5d    —                    —              ×2
   /mnt/development/biserstoilov/rust/bru · 23aa89d8-216f-428f-ad71-242ed9185b15 · ✳ bru {tests}
   ▏ ● Building now.
   ▏ Running 1 shell command…
   ▏ ⎿  $ cargo build --release
 [↑↓] move [Enter] jump [v] terminal [a] answer [Tab] expand [i] send [f] later [p] paste [S] stop [N] new [X] close [s] order [r] refresh [Esc] hide
```

`●` busy, `◐` running and idle, `○` not running. A heading is a zellij session (`outside zellij`
for agents not under one); under it, one row per agent — one per pane, by tab. Every heading folds
(`Tab`); a session's heading is also a place: `Enter` switches there, `N` starts an agent there.

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

By hand: build (below), copy `lvim-zclaude.wasm` to `~/.config/zellij/plugins/`, and add a keybind
that **sends the board a message** — not one that launches it:

```kdl
bind "a" { MessagePlugin "file:~/.config/zellij/plugins/lvim-zclaude.wasm" { name "open"; floating true; }; SwitchToMode "normal"; }
```

`Esc` hides the board rather than closing it, and `open` brings the hidden pane back at its own
size in one step. A pane that is *launched* — by `LaunchOrFocusPlugin`, or by this message when
no board is running yet — always appears at zellij's default size first and is resized by the
plugin a moment later; no launcher in zellij can hand a plugin its size (see Limits). To make
the **first** opening of a session silent too, preload the board hidden, the way keybind plugins
are preloaded for lvim-zpm:

```kdl
load_plugins {
    "file:~/.config/zellij/plugins/lvim-zclaude.wasm"
}
```

The first time it opens, zellij asks for permissions in the pane — answer `y`. They are remembered
in `~/.cache/zellij/permissions.kdl` (lvim-zpm writes them there at session start, from `zpm.kdl`).

## Keys

| key | does |
|---|---|
| `↑` `↓`, `j` `k` | move (`j`/`k` type into the filter once it is not empty) |
| `Enter` | on an agent: jump to its pane; on a session heading: switch to that session; on a stopped transcript: offer to resume it (see `N`) |
| `v` | the agent's screen **as its terminal**: its pane's own colours (`dump-screen --ansi`), re-read every `preview_tick` (0.3 s), and every key typed into that pane — `Enter`, `Esc`, arrows, digits, `Ctrl c`, `Shift Tab`, letters in any alphabet. Answer a prompt there as you would in the pane. `Esc` brings the list back (so do `Ctrl ]` and the board's own key, `Ctrl s, a`). `Esc` is the one key the pane never sees: reaching for it to close a view is reflex, and in `claude` it cancels the turn. To send an escape to the agent, press `Ctrl [` — that IS the escape byte, and it goes through |
| `a` | answer the question under the list (see below): the digits `1`–`9` go to the agent's pane, one keystroke each; `Enter` confirms when the prompt wants it; `Esc` stops answering. Only while answering — otherwise a digit jumps |
| `Tab` | on a heading: fold or unfold it (`←` folds, `→` unfolds; a filter looks through folded headings); on a stopped project: its older transcripts, newest first |
| `i` | type a line — in any alphabet — and send it to the agent's pane, as if typed there; a paste while typing joins the line |
| `f` | the same line, **for later**: sent when the agent's turn is over — its hooks say `done`, or the scan finds it idle and not waiting for a person (a line typed into a permission prompt would answer it) |
| `s` | the order: by session (default) → by project → by urgency (waiting, done, busy, idle — flat) → … |
| `1`–`9` | the Nth agent or transcript on the board (the digit stands beside it), and straight to it |
| `p` | send the clipboard to the agent's pane as it is — several lines at once |
| `S` | stop the agent (`kill` its process) — asks first |
| `N` | a new agent **where the row says**: on an agent, a fresh `claude` in its directory, its zellij session, its tab; on a session heading, in that session; on a stopped transcript, `claude --resume <id>` — that one, not the newest (`--continue`). What the row does not settle is asked on the input line, prefilled: the session (a transcript belongs to none; an agent outside zellij), or the directory (a session whose agents work in several). `Enter` accepts, `Esc` cancels, `Ctrl u` clears |
| `X` | close the pane the agent lives in — asks first |
| any letter | filter over project, branch, name and where |
| `Backspace` | undo one letter |
| `r` | rescan now |
| `g` `G`, `PageUp` `PageDown` | first, last, page |
| `Esc` | clear the filter, or hide the board |
| `Ctrl c` | hide the board |

## Under the list: the last thing the agent said, and the buttons

With the cursor on an agent, the board reads its screen (`dump-screen` of its pane, on every
scan, on every hook event, and whenever the cursor lands on it) and shows the last three
non-blank lines under the list. When the screen **ends with numbered options** — Claude Code's
permission prompt, its plan approval, an `AskUserQuestion` —

```
  ◆ Do you want to proceed?
  [1] Yes  [2] Yes, and don't ask again for cargo build commands in /mnt/dev/bru  [3] No, and tell Claude what to do differently (esc)
```

the question is shown with a button per option. `a` arms them: the digit you press is written
into the agent's pane exactly as if pressed there, and the screen is read again at once. The board
never presses a button by itself. An agent whose pane cannot be read (outside zellij) shows its
last three notifications from the hooks instead.

## Hooks: waiting, done, and the desktop

`~/.claude/sessions/<pid>.json` says only *busy* or *idle* — and a session that waits at a
permission prompt is "busy" there for as long as it waits. "Waiting for you" exists only in
Claude Code's hooks. `hooks/lvim-zclaude-hook.sh` turns them into `zellij pipe` messages
(`lvim-zclaude-event`) to every live zellij session; the board marks the agent `◆` (waiting: a
permission, a question), `✓` (done: the turn is over and nobody has replied), puts it first in its
group, tells the desktop through `notify-send` (unless you are looking at that pane), and sends a
queued follow-up (`f`) when the turn is over. Register the script in `~/.claude/settings.json`
(the plugin never writes there):

```json
{
  "hooks": {
    "Notification":     [{ "matcher": "permission_prompt|idle_prompt|elicitation_dialog|elicitation_url_dialog|agent_needs_input",
                           "hooks": [{ "type": "command", "command": "/path/to/lvim-zclaude/hooks/lvim-zclaude-hook.sh", "timeout": 5 }] }],
    "Stop":             [{ "hooks": [{ "type": "command", "command": "/path/to/lvim-zclaude/hooks/lvim-zclaude-hook.sh", "timeout": 5 }] }],
    "UserPromptSubmit": [{ "hooks": [{ "type": "command", "command": "/path/to/lvim-zclaude/hooks/lvim-zclaude-hook.sh", "timeout": 5 }] }],
    "PostToolUse":      [{ "hooks": [{ "type": "command", "command": "/path/to/lvim-zclaude/hooks/lvim-zclaude-hook.sh", "timeout": 5 }] }]
  }
}
```

The pipe's payload is `session_id\tstate\tkind\tcwd\tepoch\tnotify\tmessage`; the last field is
the notification's own text ("Claude needs your permission to use Bash"), which the board shows
after the row's details and under the list when the screen cannot be read.
`PostToolUse` is what clears `◆` the moment a permission is answered; it costs one `zellij pipe`
per live session on every tool call (~10 ms each). Leave it out and `◆` stays until the turn ends
(`Stop`). No file is written and nothing runs in the background: a board loaded *after* an event
does not know it — preload the board (`load_plugins`) and it always is. The hook picks one
session for the desktop notification (the agent's own, else the first live one), so one event is
one notification however many boards are open.

## For bars: the summary API

Any status bar can show the fleet. When `summary_file` is set, or `summary_pipe "on"`, the board
keeps a two-line summary current — scanning every `background_tick` seconds even while hidden —
and publishes it **only when it changes**:

```
3 agents · 1 busy · 1 waiting
agents=3 busy=1 waiting=1 done=0 idle=1
```

- **file** — `summary_file "/run/user/1000/lvim-zclaude"`: `head -1` is the prose, line 2 the
  pairs. A zjstatus `command` widget, an lvim-zbar script, anything that reads a file.
- **pipe** — `summary_pipe "on"`: the same text is broadcast to every plugin of the session as
  `zellij pipe --name lvim-zclaude-summary`; a bar that listens for that name is told rather than
  polling. The raw hook events (`lvim-zclaude-event`, payload
  `session_id\tstate\tkind\tcwd\tepoch\tnotify`) reach every plugin too, straight from the hook.

Every session's board publishes the same numbers (the fleet is machine-wide); with a file, the
last writer wins and they agree.

## Permissions

`ReadApplicationState` (the panes and tabs, and `get_pane_pid` — how a `claude` process is matched
to a pane), `ChangeApplicationState` (focusing that pane, closing it, naming and sizing the board's
own pane), `RunCommands` (one `sh -c` per scan, the `kill` behind `S`, the pane `N` opens — a plugin
is WASI in a sandbox and cannot read `~/.claude` itself), `WriteToStdin` (`i` — the line goes into
the agent's pane).

**A pane that stays on zellij's "Loading" text, or shows a permission prompt after an upgrade,
is this:** zellij's cache holds one list of permissions per plugin *path*, and every grant
**rewrites** that list with what the granting instance asked for. A zellij session keeps the wasm
it compiled first, so a session that still runs an older build of this plugin (asking for three
permissions) puts its three back into the cache each time the board is opened there — and the
newer build (asking for four) is prompted again in every other session. Reload the board in
**every** running session after a build (`zellij --session NAME action start-or-reload-plugin
file:…/lvim-zclaude.wasm` where it is open), or start the sessions afresh; a prompt that is not
answered leaves that instance "loading" until it is closed
(`zellij --session NAME action close-pane --pane-id plugin_N`).

## Configuration

| key | default | meaning |
|---|---|---|
| `claude_dir` | `$HOME/.claude` | where Claude Code keeps its data |
| `tick` | `10` | seconds between scans, **while the board is visible** |
| `x` `y` | `10%` `15%` | where the board's floating pane sits |
| `width` `height` | `80%` `70%` | how big it is |
| `order` | `session` | the first order: `session`, `project` or `urgency` (`s` cycles) |
| `notify` | `waiting,done` | which hook states go to the desktop (`notify-send`); empty for none |
| `summary_file` | — | a file the summary is written to, for a bar (see "For bars") |
| `summary_pipe` | `off` | `on`: the summary is also broadcast as the `lvim-zclaude-summary` pipe |
| `background_tick` | `30` | seconds between scans while hidden — only when a bar is fed |
| `preview_tick` | `0.3` | seconds between screen reads while the terminal view (`v`) is open or a question is being answered (`a`) |

The configuration goes where the plugin is loaded — in `load_plugins`, or in the braces of a
`LaunchOrFocusPlugin` (everything there that is not one of zellij's own `floating`,
`move_to_focused_tab`, `in_place`, `close_replaced_pane`, `skip_plugin_cache` reaches the plugin):

```kdl
load_plugins {
    "file:~/.config/zellij/plugins/lvim-zclaude.wasm" { claude_dir "/some/copy"; tick "5"; width "90%"; }
}
```

The four geometry keys can also ride on the `open` message, as its `payload`, and win over the
configuration:

```kdl
bind "a" { MessagePlugin "file:~/.config/zellij/plugins/lvim-zclaude.wasm" { name "open"; floating true; payload "width=90%,height=80%"; }; }
```

## Building

```sh
cargo build --release --target wasm32-wasip1
cp target/wasm32-wasip1/release/lvim-zclaude.wasm ~/.config/zellij/plugins/
```

`cargo test` runs the unit tests for the pure parts (the scanner's parser, the rows, the column
cutting, the key decoding) on the host. One of them, `the_library_still_truncates_the_same_key`,
is a canary: when it fails, `zellij-tile` has been fixed and `host.rs`/`keys.rs` can go.

## Limits

- `Enter` on a session in **another zellij session** switches the client there and focuses its
  `claude` pane, found through `zellij --session NAME action list-panes` by command and directory.
  If the pane cannot be told apart, the client is switched to the session without a focus.
- Every colour comes from the active zellij theme (on the lvim themes, lvim-colorscheme's palette);
  the board owns none of its own. The neutral grey is the theme's own foreground, the accents are
  its `multiplayer_user_colors`, and the age column fades through three of them.
- "What it is doing" is the agent pane's **title**, which Claude keeps current — never a word out
  of the transcript.
- **Every key works on an agent in any zellij session.** For an agent in another session the
  board reads that session's `zellij --session NAME action list-panes -a -j` on each scan (the
  pane, its tab and its title — "what it is doing"), writes with `… action write-chars --pane-id`
  and closes with `… action close-pane --pane-id`; none of it needs a client attached there. The
  pane is matched by command and directory; when a session runs two agents in the same directory
  the first one answers, and when none can be told apart the footer says so and `Enter` still
  switches to the session. `S` sends the signal to the pid, wherever it is.
- Stopping sends `TERM`, not `KILL`: the agent writes its session file on the way out.
- **`N` never guesses a session.** It opens through `zellij --session NAME action new-pane
  --tab-id ID --cwd DIR -- claude …` for every session, this one included — not through the
  plugin API, which from a floating board opens a floating pane and cannot pick a tab. The tab id
  comes from that session's `list-panes -a -j` (the manifest names tabs by position only). On a
  session heading there is no tab: the pane goes to the session's focused tab. A session with no
  agent is not on the board, but its name can be typed at the session question. The new agent
  appears on the board at the next scan, once it has written `~/.claude/sessions/<pid>.json`.
- **The terminal view is the pane's screen and keyboard, not the pane.** A plugin's pane shows
  what the plugin prints; a terminal pane's grid lives in the server that owns it, and no plugin
  call hands a plugin another pane's output or draws one pane inside another
  (`replace_pane_with_existing_pane` swaps places, `break_panes_to_tab_with_index` moves within
  one session). Another zellij session is another server process: its pane can only be read
  with `dump-screen` and typed into with `write`, both through the CLI. So `v` reads the pane
  every `preview_tick` with its colours and writes every key into it as the bytes a terminal
  sends; the cursor and mouse are not carried, and a key zellij binds for itself never reaches
  the board. For the real pane under your hands there is `Enter`.
- `v` and the block under the list are the two places the board shows the conversation: the
  pane's screen, on the screen only, nothing kept.
- **Nothing is answered for you.** zj-agent-mob (whose `waiting`, follow-ups, digits, orders
  and summary this borrows) can approve permission prompts from rules; this board sends only
  what you pressed — `i`, `f`, the keys in `v`, the digit after `a` — and never a `y` of its own.
- **The plugin decodes its own keystrokes.** `zellij-tile`'s `register_plugin!` cuts a key's
  character to one byte (`char_index as u8 as char` in the `zellij-utils` it links in, still so in
  0.45.1), so `б` would arrive as `1`. The decoding runs inside the plugin, not in zellij, so the
  plugin does it itself: `src/host.rs` is the macro written out, `src/keys.rs` reads the character
  whole. No patched zellij is needed. The line `i` composes is one line: a pasted text with line
  breaks is folded onto it.
- **A launched pane flashes; a hidden one does not.** No keybind action can hand a plugin its
  floating size (`LaunchOrFocusPlugin`, `LaunchPlugin` and `MessagePlugin` parse only booleans;
  the pipe launch carries `floating_pane_coordinates = None // TODO` in zellij-server), a new
  floating pane is placed at zellij's default geometry and shown before the plugin's `load()`
  runs, and a plugin may resize itself only after its permissions are handled. So the first
  launch of a session is seen at the default size for the tens of milliseconds it takes to load
  and resize — unless the board is preloaded hidden with `load_plugins`. Every opening after that
  is silent: `Esc` hides the pane (its geometry is kept), and `open` shows it through
  `change_floating_panes_coordinates`, which un-hides, floats and sizes a hidden pane inside one
  screen instruction; opened from another tab, the pane is moved there with its exact geometry
  before it is focused. `show_self` and `LaunchOrFocusPlugin` would both re-float it at the
  default geometry first, which is why neither is used.
- A session whose pane has not been started yet (a resurrected, suspended pane) has no process, so
  it cannot be located.

## Licence

BSD-3-Clause.
