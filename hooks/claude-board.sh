#!/usr/bin/env bash
# Claude Board: mirror Claude sessions that edit code onto a Todoist project.
#   claude-board.sh edit  (PostToolUse Edit|Write|MultiEdit|NotebookEdit) -> task in "In progress"
#   claude-board.sh stop  (Stop)                                         -> task to "Needs review" + summary comment
#   claude-board.sh next                     -> prints top "Up next" task as JSON (exit 1 if none)   [used by /next]
#   claude-board.sh claim SESSION TASK CWD   -> moves TASK to "In progress" and links it to SESSION  [used by /next]
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
API=https://api.todoist.com/api/v1
IN_PROGRESS="In progress"
NEEDS_REVIEW="Needs review"
UP_NEXT="Up next"

[[ -r $TOKEN_FILE ]] || { echo "claude-board: no Todoist token at $TOKEN_FILE" >&2; exit 0; }
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

# Serialise per session: async hooks for parallel edits can overlap.
exec 9>"$sf.lock"
flock -w 20 9 || exit 0

api() { # api METHOD PATH [JSON]
  curl -sf -m 10 -X "$1" -H "Authorization: Bearer $TOKEN" \
    -H 'Content-Type: application/json' ${3:+--data "$3"} "$API$2"
}

zj() { # zellij fields, only those that are set
  jq -nc --arg s "${ZELLIJ_SESSION_NAME:-}" --arg p "${ZELLIJ_PANE_ID:+terminal_$ZELLIJ_PANE_ID}" \
    '{zellij_session:$s, zellij_pane:$p} | with_entries(select(.value != ""))'
}

upd() { jq "$@" "$sf" >"$sf.tmp" && mv "$sf.tmp" "$sf"; } # upd JQ_ARGS... FILTER

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
  [[ $(jq -r .agent "$sf") == closed ]] && exit 0
  upd --arg a "$2" --argjson t "$(date +%s)" --argjson z "$(zj)" '.agent=$a | .state_at=$t | . + $z'
  exit 0 ;;
register-pen)
  [[ -n ${3:-} ]] || exit 0
  jq -n --arg s "$session" --arg cwd "$3" --argjson t "$(date +%s)" --argjson z "$(zj)" \
    '{session:$s, cwd:$cwd, kind:"pen", agent:"working", state_at:$t, zellij_session:null, zellij_pane:null} + $z' >"$sf"
  exit 0 ;;
close)
  [[ -s $sf ]] && upd --argjson t "$(date +%s)" '.agent="closed" | .state_at=$t'
  exit 0 ;;
edit|stop)
  if [[ -s $sf ]]; then
    [[ $(jq -r '.kind // "task"' "$sf") == pen ]] && exit 0
    if [[ $(jq -r .agent "$sf") == closed ]]; then
      [[ $action == stop ]] && exit 0
      rm -f "$sf" # a closed session's next edit starts a new task
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
      '{session:$s, cwd:$cwd, repos:[{root:$r,ref:$ref}], dirty:true, section:"ip", kind:"task", agent:"working", state_at:$t, zellij_session:null, zellij_pane:null} + $z' >"$sf"
    task=$(api POST /tasks "$(jq -nc --arg c "$title" --arg d "$(describe)" --arg p "$P" --arg sec "$IP" \
      --arg l "$(basename "$root")" '{content:$c, description:$d, project_id:$p, section_id:$sec, labels:[$l]}')" \
      | jq -r '.id // empty')
    [[ -n $task ]] || { rm -f "$sf"; exit 0; }
    jq --arg t "$task" '.task=$t' "$sf" >"$sf.tmp" && mv "$sf.tmp" "$sf"
    exit 0
  fi

  task=$(jq -r .task "$sf")
  jq '.dirty=true' "$sf" >"$sf.tmp" && mv "$sf.tmp" "$sf"
  if ! jq -e --arg r "$root" 'any(.repos[]; .root==$r)' "$sf" >/dev/null; then
    jq --arg r "$root" --arg ref "$ref" '.repos += [{root:$r,ref:$ref}]' "$sf" >"$sf.tmp" && mv "$sf.tmp" "$sf"
    api POST "/tasks/$task" "$(jq -nc --arg d "$(describe)" --argjson l "$(jq -c '(.labels0 // []) + [.repos[].root | split("/") | last] | unique' "$sf")" \
      '{description:$d, labels:$l}')" >/dev/null
  fi
  if [[ $(jq -r .section "$sf") != ip ]]; then
    # shortcut: if you already completed the task in Todoist this moves it but leaves it completed; reopen by hand
    api POST "/tasks/$task/move" "$(jq -nc --arg s "$IP" '{section_id:$s}')" >/dev/null \
      && jq '.section="ip"' "$sf" >"$sf.tmp" && mv "$sf.tmp" "$sf"
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
    jq '.section="nr" | .dirty=false' "$sf" >"$sf.tmp" && mv "$sf.tmp" "$sf"
  fi
  ;;
add)
  [[ -n ${2:-} ]] || { echo "usage: claude-board.sh add TITLE DESCRIPTION [LABEL...]" >&2; exit 1; }
  api POST /tasks "$(jq -nc --arg c "$2" --arg d "${3:-}" --arg p "$P" --arg s "$UN" --args '$ARGS.positional[3:] as $l
    | {content:$c, description:$d, project_id:$p, section_id:$s, labels:$l}' "$@")" | jq -er '.id' \
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
      kind:"task", agent:"working", state_at:$now, zellij_session:null, zellij_pane:null} + $z' >"$sf"
  echo "claimed $claim_task"
  ;;
esac
exit 0
