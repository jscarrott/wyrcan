#!/usr/bin/env bash
# Claude Board: mirror Claude sessions that edit code onto a Todoist project.
#   claude-board.sh edit  (PostToolUse Edit|Write|MultiEdit|NotebookEdit) -> task in "In progress"
#   claude-board.sh stop  (Stop)                                         -> task to "Needs review" + summary comment
#   claude-board.sh next                     -> prints top "Up next" task as JSON (exit 1 if none)   [used by /next]
#   claude-board.sh claim SESSION TASK CWD   -> moves TASK to "In progress", links it to SESSION, prints it as JSON  [used by /next]
#   claude-board.sh state working|waiting|blocked|ended  (hook JSON on stdin; offline, only touches an existing state file)
#   claude-board.sh register-pen SESSION CWD -> state record for a pen (scratch) session
#   claude-board.sh add TITLE DESC [LABEL...] -> new task at the bottom of Up next; prints its id
#   claude-board.sh close SESSION            -> marks the session closed (the TUI calls this on complete)
# Hook modes never fail the hook: every error path exits 0.
set -uo pipefail

action="${1:-}"
TOKEN_FILE="${CLAUDE_BOARD_TOKEN_FILE:-$HOME/.config/todoist/token}"
PROJECT="${CLAUDE_BOARD_PROJECT:-Claude Board}"
STATE="${CLAUDE_BOARD_STATE:-$HOME/.local/state/claude-board}"
API=${CLAUDE_BOARD_API:-https://api.todoist.com/api/v1}
IN_PROGRESS="In progress"
NEEDS_REVIEW="Needs review"
UP_NEXT="Up next"

if [[ ! -r $TOKEN_FILE ]]; then
  echo "claude-board: no Todoist token at $TOKEN_FILE" >&2
  [[ $action =~ ^(next|claim|add)$ ]] && exit 1
  exit 0
fi
TOKEN=$(tr -d '[:space:]' <"$TOKEN_FILE")
mkdir -p "$STATE" || exit 0

case "$action" in
  claim) input=$(jq -nc --arg s "${2:-}" '{session_id:$s}'); claim_task="${3:-}"; claim_cwd="${4:-$PWD}" ;;
  register-pen|close) input=$(jq -nc --arg s "${2:-}" '{session_id:$s}') ;;
  add) input='{"session_id":"add"}' ;;  # no session; "add" is just a lock name
  next) input='{"session_id":"next"}' ;;  # no session; "next" is just a lock name
  *) input=$(cat) ;;
esac

session=$(jq -r '.session_id // empty' <<<"$input")
[[ $session =~ ^[A-Za-z0-9_-]+$ ]] || exit 0
sf="$STATE/$session.json"

# fd 8 guards every state-file write (short, never held across the API). fd 9 serialises whole
# API hooks per session (parallel async edits must not double-create a task); the sync
# state/pen/close modes skip fd 9 so they never queue behind a slow curl.
exec 8>"$sf.flock"
case "$action" in
  state|register-pen|close) ;;
  *) exec 9>"$sf.lock"; flock -w 20 9 || exit 0 ;;
esac

api() { # api METHOD PATH [JSON]
  curl -sf -m 10 -X "$1" -H "Authorization: Bearer $TOKEN" \
    -H 'Content-Type: application/json' ${3:+--data "$3"} "$API$2"
}

zj() { # zellij fields, only those that are set
  jq -nc --arg s "${ZELLIJ_SESSION_NAME:-}" --arg p "${ZELLIJ_PANE_ID:+terminal_$ZELLIJ_PANE_ID}" \
    '{zellij_session:$s, zellij_pane:$p} | with_entries(select(.value != ""))'
}

put() { # put: atomically replace the state file with stdin, under the lock
  flock -w 1 8 || return 1
  cat >"$sf.tmp" && mv "$sf.tmp" "$sf"; local r=$?
  flock -u 8; return $r
}
drop() { flock -w 1 8; rm -f "$sf"; flock -u 8; }

upd() { # upd JQ_ARGS... FILTER: locked read-modify-write of the state file
  flock -w 1 8 || return 1
  jq "$@" "$sf" >"$sf.tmp" && mv "$sf.tmp" "$sf"; local r=$?
  flock -u 8; return $r
}

board_ids() { # prints "project_id up_next_id in_progress_id needs_review_id", cached
  local cache="$STATE/board-$(printf %s "$PROJECT" | md5sum | cut -c1-8).json"
  if [[ -s $cache ]] && jq -e .un "$cache" >/dev/null; then jq -r '"\(.p) \(.un) \(.ip) \(.nr)"' "$cache"; return; fi
  local p sections name id ids=()
  # shortcut: reads only the first page of projects (default 50), fine for a personal account
  p=$(api GET /projects | jq -r --arg n "$PROJECT" '.results[] | select(.name==$n) | .id' | head -1)
  [[ -n $p ]] || p=$(api POST /projects "$(jq -nc --arg n "$PROJECT" '{name:$n}')" | jq -r .id)
  [[ -n $p && $p != null ]] || return 1
  sections=$(api GET "/sections?project_id=$p") || return 1
  for name in "$UP_NEXT" "$IN_PROGRESS" "$NEEDS_REVIEW"; do
    id=$(jq -r --arg n "$name" '.results[] | select(.name==$n) | .id' <<<"$sections" | head -1)
    [[ -n $id ]] || id=$(api POST /sections "$(jq -nc --arg n "$name" --arg p "$p" '{name:$n,project_id:$p}')" | jq -r .id)
    [[ -n $id && $id != null ]] || return 1
    ids+=("$id")
  done
  jq -nc --arg p "$p" --arg un "${ids[0]}" --arg ip "${ids[1]}" --arg nr "${ids[2]}" '{p:$p,un:$un,ip:$ip,nr:$nr}' >"$cache"
  echo "$p ${ids[*]}"
}

repo_info() { # repo_info DIR -> "root<TAB>ref" for the jj/git repo containing DIR
  local d=$1 root ref
  while [[ ! -d $d ]]; do d=$(dirname "$d"); done
  if root=$(cd "$d" && jj root --ignore-working-copy 2>/dev/null); then
    ref="jj $(cd "$root" && jj log --ignore-working-copy --no-graph -r @ -T 'change_id.short()' 2>/dev/null)"
  elif root=$(git -C "$d" rev-parse --show-toplevel 2>/dev/null); then
    ref="git $(git -C "$root" branch --show-current 2>/dev/null)"
  else
    return 1
  fi
  printf '%s\t%s\n' "$root" "$ref"
}

describe() { # task description from state
  jq -r '(if (.note // "") != "" then .note + "\n\n---\n" else "" end)
         + "Repos:\n" + ([.repos[] | "- \(.root) (\(.ref))"] | join("\n"))
         + "\n\nResume: `cd \(.cwd) && claude --resume \(.session)`"' "$sf"
}

first_prompt() {
  local t; t=$(jq -r '.transcript_path // empty' <<<"$input")
  [[ -r $t ]] || return
  jq -rn 'first(inputs | select(.type=="user" and (.message.content|type)=="string")
                 | .message.content | select(startswith("<")|not))' "$t" 2>/dev/null \
    | tr '\n' ' ' | cut -c1-100
}

# Modes that never touch the API: handled before the board lookup so they work offline.
case "$action" in
state)
  [[ -s $sf && ${2:-} =~ ^(working|waiting|blocked|ended)$ ]] || exit 0
  # closed guard lives inside the locked filter so a concurrent `close` can't be overwritten
  upd --arg a "$2" --argjson t "$(date +%s)" --argjson z "$(zj)" 'if .agent=="closed" then . elif .agent==$a then . + $z else .agent=$a | .state_at=$t | . + $z end'
  exit 0 ;;
register-pen)
  [[ -n ${3:-} ]] || exit 0
  jq -n --arg s "$session" --arg cwd "$3" --argjson t "$(date +%s)" --argjson z "$(zj)" \
    '{session:$s, cwd:$cwd, kind:"pen", agent:"working", state_at:$t, zellij_session:null, zellij_pane:null} + $z' | put
  exit 0 ;;
close)
  [[ -s $sf ]] && upd --argjson t "$(date +%s)" '.agent="closed" | .state_at=$t'
  exit 0 ;;
edit|stop)
  if [[ -s $sf ]]; then
    [[ $(jq -r '.kind // "task"' "$sf") == pen ]] && exit 0
    if [[ -z $(jq -r '.task // empty' "$sf") ]]; then
      drop # half-created record (edit killed between POST and upd): start over
    elif [[ $(jq -r .agent "$sf") == closed ]]; then
      [[ $action == stop ]] && exit 0
      drop # a closed session's next edit starts a new task
    fi
  fi ;;
esac

read -r P UN IP NR <<<"$(board_ids)" || exit 0
if [[ -z ${NR:-} ]]; then
  [[ $action == next || $action == claim || $action == add ]] && { echo "claude-board: can't reach Todoist" >&2; exit 1; }
  exit 0
fi

case "$action" in
edit)
  file=$(jq -r '.tool_input.file_path // .tool_input.notebook_path // empty' <<<"$input")
  [[ -n $file ]] || exit 0
  IFS=$'\t' read -r root ref <<<"$(repo_info "$(dirname "$file")")" || exit 0
  [[ -n ${root:-} ]] || exit 0
  cwd=$(jq -r '.cwd // empty' <<<"$input")

  if [[ ! -s $sf ]]; then
    title=$(first_prompt); title=${title:-Claude session in $(basename "$root")}
    jq -nc --arg s "$session" --arg cwd "$cwd" --arg r "$root" --arg ref "$ref" --argjson t "$(date +%s)" --argjson z "$(zj)" \
      '{session:$s, cwd:$cwd, repos:[{root:$r,ref:$ref}], dirty:true, section:"ip", kind:"task", agent:"working", state_at:$t, zellij_session:null, zellij_pane:null} + $z' | put
    task=$(api POST /tasks "$(jq -nc --arg c "$title" --arg d "$(describe)" --arg p "$P" --arg sec "$IP" \
      --arg l "$(basename "$root")" '{content:$c, description:$d, project_id:$p, section_id:$sec, labels:[$l]}')" \
      | jq -r '.id // empty')
    [[ -n $task ]] || { drop; exit 0; }
    upd --arg t "$task" '.task=$t'
    exit 0
  fi

  task=$(jq -r .task "$sf")
  upd '.dirty=true'
  if ! jq -e --arg r "$root" 'any(.repos[]; .root==$r)' "$sf" >/dev/null; then
    upd --arg r "$root" --arg ref "$ref" '.repos += [{root:$r,ref:$ref}]'
    api POST "/tasks/$task" "$(jq -nc --arg d "$(describe)" --argjson l "$(jq -c '(.labels0 // []) + [.repos[].root | split("/") | last] | unique' "$sf")" \
      '{description:$d, labels:$l}')" >/dev/null
  fi
  if [[ $(jq -r .section "$sf") != ip ]]; then
    # shortcut: if you already completed the task in Todoist this moves it but leaves it completed; reopen by hand
    api POST "/tasks/$task/move" "$(jq -nc --arg s "$IP" '{section_id:$s}')" >/dev/null \
      && upd '.section="ip"'
  fi
  ;;
stop)
  [[ -s $sf ]] || exit 0
  task=$(jq -r .task "$sf")
  if [[ $(jq -r .dirty "$sf") == true ]]; then
    msg=$(jq -r '.last_assistant_message // empty' <<<"$input")
    if [[ -z $msg ]]; then
      t=$(jq -r '.transcript_path // empty' <<<"$input")
      [[ -r $t ]] && msg=$(jq -rs '[.[] | select(.type=="assistant") | .message.content[]? | select(.type=="text") | .text] | last // empty' "$t" 2>/dev/null)
    fi
    msg=$(tail -c 1500 <<<"$msg")
    [[ -n $msg ]] && api POST /comments "$(jq -nc --arg t "$task" --arg c "$msg" '{task_id:$t, content:$c}')" >/dev/null
  fi
  if api POST "/tasks/$task/move" "$(jq -nc --arg s "$NR" '{section_id:$s}')" >/dev/null; then
    upd '.section="nr" | .dirty=false'
  fi
  ;;
add)
  [[ -n ${2:-} ]] || { echo "usage: claude-board.sh add TITLE DESCRIPTION [LABEL...]" >&2; exit 1; }
  api POST /tasks "$(jq -nc --arg c "$2" --arg d "${3:-}" --arg p "$P" --arg s "$UN" \
    '{content:$c, description:$d, project_id:$p, section_id:$s, labels:$ARGS.positional}' --args -- "${@:4}")" | jq -er '.id' \
    || { echo "claude-board: add failed" >&2; exit 1; }
  ;;
next)
  api GET "/tasks?project_id=$P&section_id=$UN" \
    | jq -ce '[.results[] | select(.parent_id == null)] | sort_by(.child_order) | first // empty
              | {id, content, description, labels}' || { echo "claude-board: Up next is empty" >&2; exit 1; }
  ;;
claim)
  [[ -n $claim_task ]] || { echo "usage: claude-board.sh claim SESSION TASK CWD" >&2; exit 1; }
  t=$(api GET "/tasks/$claim_task") || { echo "claude-board: task $claim_task not found" >&2; exit 1; }
  api POST "/tasks/$claim_task/move" "$(jq -nc --arg s "$IP" '{section_id:$s}')" >/dev/null || exit 1
  # repos start empty; the edit hook fills them in and appends them under the task's own description
  jq -n --arg s "$session" --arg cwd "$claim_cwd" --arg task "$claim_task" --argjson t "$t" --argjson now "$(date +%s)" --argjson z "$(zj)" \
    '{session:$s, cwd:$cwd, task:$task, repos:[], dirty:false, section:"ip", note:($t.description // ""), labels0:($t.labels // []),
      kind:"task", agent:"working", state_at:$now, zellij_session:null, zellij_pane:null} + $z' | put
  jq -c '{id, content, description, labels}' <<<"$t"
  ;;
esac
exit 0
