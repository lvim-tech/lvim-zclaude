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

case "$event" in
  Notification)
    case "$kind" in
      permission_prompt|elicitation_dialog|elicitation_url_dialog|agent_needs_input) state=waiting ;;
      idle_prompt) state=done ;;
      *) exit 0 ;;
    esac ;;
  Stop) state=done; kind=Stop ;;
  UserPromptSubmit|PostToolUse) state=working; kind=$event ;;
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
