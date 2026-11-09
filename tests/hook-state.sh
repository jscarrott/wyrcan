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

# Offline API: board cache seeded, API refuses connections instantly, so edit/stop run their jq paths.
export CLAUDE_BOARD_API=http://127.0.0.1:9
echo '{"p":"1","un":"2","ip":"3","nr":"4"}' >"$CLAUDE_BOARD_STATE/board-$(printf %s 'Claude Board' | md5sum | cut -c1-8).json"
git init -q "$W/repo"
edit() { jq -nc --arg f "$W/repo/x.py" --arg c "$W/repo" '{session_id:"s",cwd:$c,tool_input:{file_path:$f}}' | $H edit; }
seed() { echo "$1" >"$S"; }

# api_hooks_preserve_agent_state
seed '{"session":"s","task":"t1","kind":"task","agent":"waiting","state_at":123,"repos":[],"dirty":false,"section":"nr"}'
edit || fail "edit exit"
if [[ $(jq -r .dirty "$S") != true ]]; then fail "edit did not reach its logic"; fi
if [[ $(jq -r .agent "$S") != waiting || $(jq -r .state_at "$S") != 123 ]]; then fail "edit clobbered state"; fi
ev | $H stop || fail "stop exit"
if [[ $(jq -r .agent "$S") != waiting || $(jq -r .state_at "$S") != 123 ]]; then fail "stop clobbered state"; fi

# closed: edit deletes (new-task path; API down so no record is left), stop/state leave it alone
C='{"session":"s","task":"t1","kind":"task","agent":"closed","state_at":5,"repos":[],"dirty":true,"section":"ip"}'
seed "$C"; ev | $H stop; ev | $H state working
if [[ $(jq -c . "$S") != $(jq -c . <<<"$C") ]]; then fail "stop/state touched closed file"; fi
seed "$C"; edit
if [[ -e $S ]]; then fail "edit kept closed file"; fi
# pen: edit/stop leave it alone
PEN='{"session":"s","cwd":"/x","kind":"pen","agent":"working","state_at":5}'
seed "$PEN"; edit; ev | $H stop
if [[ $(jq -c . "$S") != $(jq -c . <<<"$PEN") ]]; then fail "edit/stop touched pen"; fi

# state_not_blocked_by_api_lock
seed '{"session":"s","task":"t1","kind":"task","agent":"waiting"}'
flock "$S.lock" sleep 4 & sleep 0.3
t0=$SECONDS; ev | $H state working; t1=$SECONDS
wait
if (( t1 - t0 > 1 )); then fail "state waited on the API lock"; fi
if [[ $(jq -r .agent "$S") != working ]]; then fail "state did not write while API lock held"; fi

# close_marks_closed
$H close s
if [[ $(jq -r .agent "$S") != closed ]]; then fail "close"; fi
echo PASS
