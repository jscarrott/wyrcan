#!/usr/bin/env bash
# Smoke test for claude-board.sh against a throwaway Todoist project; deletes it afterwards.
set -euo pipefail
H=$(dirname "$0")/../hooks/claude-board.sh
W=$(mktemp -d)
export CLAUDE_BOARD_PROJECT="claude-board-test-$$" CLAUDE_BOARD_STATE=$W/state
T=$(tr -d '[:space:]' < ~/.config/todoist/token); API=https://api.todoist.com/api/v1
# Delete the throwaway project even when a check fails part-way (set -e exits through here).
cleanup() {
  local p; p=$(jq -r .p "$W"/state/board-*.json 2>/dev/null) || p=
  [[ -n $p ]] && curl -sf -m 10 -X DELETE -H "Authorization: Bearer $T" "$API/projects/$p"
  rm -rf "$W"
}
trap cleanup EXIT
api() { curl -sf -m 10 -H "Authorization: Bearer $T" "$API$1"; }
for r in repoA repoB; do git init -q $W/$r; done
printf '%s\n' '{"type":"user","message":{"role":"user","content":"Add retry logic to uploader"}}' \
  '{"type":"assistant","message":{"content":[{"type":"text","text":"Done: added retries. Left: tests."}]}}' > $W/t.jsonl
ev() { jq -nc --arg f "$1" --arg t $W/t.jsonl --arg c $W/repoA '{session_id:"sess-1",cwd:$c,transcript_path:$t,tool_input:{file_path:$f}}'; }
ev $W/repoA/x.py | $H edit
ev $W/repoA/y.py | $H edit
ev $W/repoB/z.py | $H edit
ev "" | $H stop
st=$W/state/sess-1.json; task=$(jq -r .task $st)
P=$(jq -r .p $W/state/board-*.json); NR=$(jq -r .nr $W/state/board-*.json); IP=$(jq -r .ip $W/state/board-*.json)
j=$(api /tasks/$task)
echo "$j" | jq '{content,section_id,labels,description}'
[[ $(jq -r .content <<<"$j") == "Add retry logic to uploader" ]]
[[ $(jq -r .section_id <<<"$j") == "$NR" ]]
[[ $(jq -c '.labels|sort' <<<"$j") == '["repoA","repoB"]' ]]
[[ $(api "/tasks?project_id=$P" | jq '.results|length') == 1 ]]
[[ $(api "/comments?task_id=$task" | jq -r '.results[0].content') == "Done: added retries. Left: tests." ]]
ev $W/repoA/x.py | $H edit
[[ $(api /tasks/$task | jq -r .section_id) == "$IP" ]]
# --- second cut: Up next + claim ---
UN=$(jq -r .un $W/state/board-*.json)
if $H next >/dev/null 2>&1; then echo "FAIL: next on empty"; exit 1; fi
mk() { curl -sf -m 10 -X POST -H "Authorization: Bearer $T" -H 'Content-Type: application/json' --data "$1" $API/tasks | jq -r .id; }
a=$(mk "$(jq -nc --arg p $P --arg s $UN '{content:"First job",description:"my brief",project_id:$p,section_id:$s,labels:["mine"]}')")
b=$(mk "$(jq -nc --arg p $P --arg s $UN '{content:"Second job",project_id:$p,section_id:$s}')")
[[ $($H next | jq -r .id) == "$a" ]]
[[ $($H claim sess-2 "$a" $W/repoB | jq -r .description) == "my brief" ]]
[[ $(api /tasks/$a | jq -r .section_id) == "$IP" ]]
[[ $($H next | jq -r .id) == "$b" ]]
c=$($H add "Pen job" "desc" mine)
j=$(api /tasks/$c); [[ $(jq -r .section_id <<<"$j") == "$UN" && $(jq -r .content <<<"$j") == "Pen job" && $(jq -c .labels <<<"$j") == '["mine"]' ]]
[[ $(api "/tasks?project_id=$P&section_id=$UN" | jq -r '[.results[]|select(.parent_id==null)]|sort_by(.child_order)|last.id') == "$c" ]]
jq -nc --arg f $W/repoB/q.py --arg c $W/repoB '{session_id:"sess-2",cwd:$c,tool_input:{file_path:$f}}' | $H edit
j=$(api /tasks/$a); echo "$j" | jq '{description,labels}'
[[ $(jq -r .description <<<"$j") == "my brief"* && $(jq -r .description <<<"$j") == *repoB* ]]
[[ $(jq -c '.labels|sort' <<<"$j") == '["mine","repoB"]' ]]
[[ $(api "/tasks?project_id=$P" | jq '.results|length') == 4 ]]   # no duplicate task for sess-2
echo PASS
