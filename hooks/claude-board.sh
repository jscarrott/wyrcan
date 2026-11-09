#!/usr/bin/env bash
# Claude Board: mirror Claude sessions that edit code onto a Todoist project.
#   claude-board.sh edit  (PostToolUse Edit|Write|MultiEdit|NotebookEdit) -> task in "In progress"
#   claude-board.sh stop  (Stop)                                         -> task to "Needs review" + summary comment
#   claude-board.sh next                     -> prints top "Up next" task as JSON (exit 1 if none)   [used by /next]
#   claude-board.sh claim SESSION TASK CWD   -> moves TASK to "In progress" and links it to SESSION  [used by /next]
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

read -r P UN IP NR <<<"$(board_ids)" || exit 0
if [[ -z ${NR:-} ]]; then
  [[ $action == next || $action == claim ]] && { echo "claude-board: can't reach Todoist" >&2; exit 1; }
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
    jq -nc --arg s "$session" --arg cwd "$cwd" --arg r "$root" --arg ref "$ref" \
      '{session:$s, cwd:$cwd, repos:[{root:$r,ref:$ref}], dirty:true, section:"ip"}' >"$sf"
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
  jq -n --arg s "$session" --arg cwd "$claim_cwd" --arg task "$claim_task" --argjson t "$t" \
    '{session:$s, cwd:$cwd, task:$task, repos:[], dirty:false, section:"ip", note:($t.description // ""), labels0:($t.labels // [])}' >"$sf"
  echo "claimed $claim_task"
  ;;
esac
exit 0
