#!/usr/bin/env bash
# Offline tests for the agent-state modes of claude-board.sh.
set -uo pipefail
H=$(cd "$(dirname "$0")/.." && pwd)/hooks/claude-board.sh
W=$(mktemp -d); trap 'rm -rf "$W"' EXIT
export CLAUDE_BOARD_STATE=$W/state CLAUDE_BOARD_TOKEN_FILE=$W/token
echo dummy >"$W/token"; mkdir -p "$CLAUDE_BOARD_STATE"
S=$CLAUDE_BOARD_STATE/s.json
fail() { echo "FAIL: $*"; exit 1; }
ev() { jq -nc --arg s "${1:-s}" '{session_id:$s}'; }

# state_ignores_unknown_session
ev nope | $H state working
if [[ -e $CLAUDE_BOARD_STATE/nope.json ]]; then fail "state created a file"; fi

# state_transitions
echo '{"session":"s","task":"t1","kind":"task"}' >"$S"
for a in working blocked waiting ended; do
  ev | ZELLIJ_PANE_ID=7 ZELLIJ_SESSION_NAME=main $H state $a
  if [[ $(jq -r .agent "$S") != "$a" ]]; then fail "agent != $a"; fi
  if [[ $(jq -r '.state_at|type' "$S") != number ]]; then fail "state_at not a number after $a"; fi
  if [[ $(jq -r .zellij_pane "$S") != terminal_7 ]]; then fail "pane after $a"; fi
  if [[ $(jq -r .zellij_session "$S") != main ]]; then fail "zellij session after $a"; fi
done

# register_pen_creates_pen_record
$H register-pen pen1 "$W"
P=$CLAUDE_BOARD_STATE/pen1.json
if [[ $(jq -r '.kind' "$P") != pen ]]; then fail "pen kind"; fi
if [[ $(jq -r '.agent' "$P") != working ]]; then fail "pen agent"; fi
if [[ $(jq 'has("task")' "$P") != false ]]; then fail "pen has task"; fi

# api_hooks_preserve_agent_state
git init -q "$W/repo"
echo '{"session":"s","task":"t1","kind":"task","agent":"waiting","state_at":123,"repos":[],"dirty":false,"section":"nr"}' >"$S"
jq -nc --arg f "$W/repo/x.py" --arg c "$W/repo" '{session_id:"s",cwd:$c,tool_input:{file_path:$f}}' | $H edit \
  || fail "edit exit"
if [[ $(jq -r .agent "$S") != waiting || $(jq -r .state_at "$S") != 123 ]]; then fail "edit clobbered state"; fi
ev | $H stop || fail "stop exit"
if [[ $(jq -r .agent "$S") != waiting || $(jq -r .state_at "$S") != 123 ]]; then fail "stop clobbered state"; fi

# close_marks_closed
$H close s
if [[ $(jq -r .agent "$S") != closed ]]; then fail "close"; fi
echo PASS
