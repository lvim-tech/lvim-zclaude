#!/bin/sh
# lvim-zclaude-hook.sh — a Claude Code hook that tells the board when a session waits for you,
# is done, or is working again. Claude Code hands the hook's JSON on stdin; this script turns it
# into one `zellij pipe` per live zellij session, named `lvim-zclaude-event`, with the payload
#
#     session_id <TAB> state <TAB> kind <TAB> cwd <TAB> epoch <TAB> notify <TAB> message
#
# state: waiting (a permission prompt, a question), done (the turn is over, nobody has replied),
# working (a new prompt, a tool that ran — whatever was waiting or done is over). `notify` is 1
# on exactly one pipe — the session the agent runs in, or the first live one when it runs outside
# zellij — so one event is one desktop notification, whatever the number of boards. `message` is
# the notification's own text ("Claude needs your permission to use Bash"), on one line; the
# board shows it under the agent's row as the last thing it said. Empty for Stop.
#
# Register it in ~/.claude/settings.json (see README, "Hooks"). Nothing is written anywhere: no
# status file, no daemon; a board that is not loaded when the event happens does not learn it.
#
# Inside zellij it also tells zellij itself (OSC 7501 program status, written to the agent's
# terminal) and sets the pane's resurrect command to `claude … --resume <session_id>`, so a
# restarted or resurrected session brings the conversation back. SessionStart / SessionEnd only
# do those two.
#
# `sessions/<pid>.json` says only busy or idle, and a session that waits for a permission is
# "busy" there for as long as it waits — this script is the only source of "waiting".

json=$(cat)
value() { printf '%s' "$json" | grep -o "\"$1\":\"[^\"]*\"" | head -1 | cut -d'"' -f4; }
sid=$(value session_id)
event=$(value hook_event_name)
kind=$(value notification_type)
cwd=$(value cwd)
# The text, with its JSON escapes for quotes and line breaks turned into what they mean, on one
# line — a tab or a newline in it would break the payload's fields.
message=$(value message | sed 's/\\n/ /g; s/\\t/ /g; s/\\"/"/g' | tr -s '\t\n' '  ')
[ -n "$sid" ] || exit 0

# The agent's own terminal: the first ancestor with one (claude itself, a hook has none), and
# claude's pid — for zellij's program status and the pane's resurrect command below.
agent_tty=; agent_pid=
p=$PPID
while [ -n "$p" ] && [ "$p" -gt 1 ]; do
  [ -z "$agent_pid" ] && [ "$(ps -o comm= -p "$p" 2>/dev/null)" = claude ] && agent_pid=$p
  t=$(ps -o tty= -p "$p" 2>/dev/null | tr -d ' ')
  case "$t" in pts/*|tty*) agent_tty=/dev/$t; break ;; esac
  p=$(ps -o ppid= -p "$p" 2>/dev/null | tr -d ' ')
done

# zellij's program status (OSC 7501): written to the agent's terminal as if claude had said it,
# so zellij itself knows the pane works, waits for you or is done — its tab markers, its inbox,
# lvim-zsession's ATTENTION column, in every session. Only inside zellij.
status() {
  [ -n "$ZELLIJ" ] && [ -n "$agent_tty" ] && [ -w "$agent_tty" ] || return 0
  printf '\033]7501;%s\033\\' "$1" > "$agent_tty" 2>/dev/null
}
b64() { printf '%s' "$1" | cut -c1-1500 | base64 -w0 2>/dev/null; }

# The pane comes back as this conversation: `claude --resume <id>` with claude's own other
# arguments, in its folder. Set again on every prompt (cheap, and a session started before the
# hook knew it gets it too); cleared when claude ends, so the shell left behind comes back a shell.
resurrect() {
  [ -n "$ZELLIJ_PANE_ID" ] && [ -n "$ZELLIJ_SESSION_NAME" ] || return 0
  if [ "$1" = clear ]; then
    zellij --session "$ZELLIJ_SESSION_NAME" action set-resurrect-command --pane-id "$ZELLIJ_PANE_ID" --clear </dev/null >/dev/null 2>&1
    return 0
  fi
  [ -n "$agent_pid" ] && [ -r "/proc/$agent_pid/cmdline" ] || return 0
  # claude's arguments without the ones that choose a conversation; then this one.
  set --
  skip=0
  for a in $(tr '\0' '\n' < "/proc/$agent_pid/cmdline" | tail -n +2 | tr '\n' ' '); do
    if [ "$skip" = 1 ]; then skip=0; continue; fi
    case "$a" in
      --resume|-r|--session-id) skip=1 ;;
      --resume=*|--session-id=*|--continue|-c|--fork-session) ;;
      *) set -- "$@" "$a" ;;
    esac
  done
  zellij --session "$ZELLIJ_SESSION_NAME" action set-resurrect-command --pane-id "$ZELLIJ_PANE_ID" \
    ${cwd:+--cwd "$cwd"} -- claude "$@" --resume "$sid" </dev/null >/dev/null 2>&1
}

case "$event" in
  SessionStart) status "state=idle:app=claude"; resurrect set; exit 0 ;;
  SessionEnd) status "state=clear"; resurrect clear; exit 0 ;;
  Notification)
    case "$kind" in
      permission_prompt) state=waiting; status "state=blocked:kind=permission:app=claude:msg=$(b64 "$message")" ;;
      elicitation_dialog|elicitation_url_dialog|agent_needs_input)
        state=waiting; status "state=blocked:kind=question:app=claude:msg=$(b64 "$message")" ;;
      idle_prompt) state=done; status "state=done:app=claude" ;;
      *) exit 0 ;;
    esac ;;
  Stop) state=done; kind=Stop; status "state=done:app=claude" ;;
  UserPromptSubmit) state=working; kind=$event; status "state=working:app=claude"; resurrect set ;;
  PostToolUse) state=working; kind=$event; status "state=working:app=claude" ;;
  *) exit 0 ;;
esac

now=$(date +%s)
sessions=$(zellij list-sessions --no-formatting 2>/dev/null | grep -v EXITED | cut -d' ' -f1)
[ -n "$sessions" ] || exit 0
# The one board that tells the desktop: the agent's own session, else the first live one.
own=$ZELLIJ_SESSION_NAME
case " $(printf '%s' "$sessions" | tr '\n' ' ') " in
  *" $own "*) ;;
  *) own=$(printf '%s\n' "$sessions" | head -1) ;;
esac
for s in $sessions; do
  notify=0; [ "$s" = "$own" ] && notify=1
  payload=$(printf '%s\t%s\t%s\t%s\t%s\t%s\t%s' "$sid" "$state" "$kind" "$cwd" "$now" "$notify" "$message")
  timeout 2 zellij --session "$s" pipe --name lvim-zclaude-event -- "$payload" </dev/null >/dev/null 2>&1
done
exit 0
