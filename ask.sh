#!/usr/bin/env bash
# Ask the OpenHands agent on the Razer (local Qwen, 35B by default, + tools) and print its final answer.
#   bash ask.sh -q="what is my current IP?"
#   curl -s https://static.wliafdew.dev/tools/openhands/ask.sh | bash -s -- -q="what is my current IP?"
# Options: -q=TEXT | -q TEXT   question (required)
#          -u=URL              OpenHands URL (default http://10.10.0.162:8000, or $OH_URL)
#          -s=N                max agent steps (default 15)
#          -t=SECONDS          give up after this long (default 600)
#          -k=KEY              X-Session-API-Key (or $OH_KEY), only needed if auth is turned on
#          -m=MODEL            big (default, Qwen3.6-35B, smarter) or small (Qwen3.5-4B, ~3-5x faster).
#                              Only one model fits in VRAM; switching costs ~5-12s on the first request.
#          -p=PRESET           ready-made task; -q (optional) adds extra notes. Presets:
#                              movies         global (Box Office Mojo/IMDb data + vuniper) and Vietnam (Galaxy Cinema)
#                              movies-global  global only        movies-vn  Vietnam only
#                              news           top 10 headlines from VnExpress + Thanh Niên (Vietnamese + English)
#                              ip             current public IP
set -euo pipefail
Q="" P="" URL=${OH_URL:-http://10.10.0.162:8000} STEPS=15 TIMEOUT=600 KEY=${OH_KEY:-}
M=big
while [ $# -gt 0 ]; do
  case "$1" in
    -q=*|--q=*) Q=${1#*=} ;;  -q|--q) Q=${2:?}; shift ;;
    -u=*) URL=${1#*=} ;;      -u) URL=${2:?}; shift ;;
    -s=*) STEPS=${1#*=} ;;    -t=*) TIMEOUT=${1#*=} ;;
    -k=*) KEY=${1#*=} ;;
    -m=*) M=${1#*=} ;;        -m) M=${2:?}; shift ;;
    -p=*) P=${1#*=} ;;        -p) P=${2:?}; shift ;;
    -h|--help) sed -n '2,17p' "$0" 2>/dev/null || true; exit 0 ;;
    *) Q="${Q:+$Q }$1" ;;
  esac; shift
done
case "$M" in   # OpenHands agent profiles; same rules and tools, different model
  small|4b) PROFILE=fce49fb9-7db0-4b5b-82df-588f8346a251 ;;   # "api-tools-4b"  -> qwen3.5-4b
  big|35b)  PROFILE=90eebe4e-b9b9-4edb-b668-120e77a026d0 ;;   # "api-tools"     -> qwen3.6-35b
  *) echo "unknown model: $M (small, big)" >&2; exit 2 ;;
esac
PROFILE=${OH_PROFILE:-$PROFILE}
WK=$(date -d 'last sunday' +%GW%V 2>/dev/null || true)   # last finished weekend, e.g. 2026W41 (GNU date)
BOM="https://www.boxofficemojo.com/weekend/${WK:+$WK/}"
G="Global: (1) browser_navigate $BOM then browser_get_content: the weekend chart, top 5 rows, title + weekend gross (Box Office Mojo is IMDb's box office data; imdb.com itself blocks bots, do not try it). (2) browser_navigate https://vuniper.com/movies then browser_get_content: use ONLY the section headed 'In Theaters', first 5 titles + score."
V='Vietnam: browser_navigate https://www.galaxycine.vn/ then browser_get_content: top 5 now-showing movies, title (Vietnamese + English if shown) + rating.'
E='Read each page once, no clicking. If a source fails, say so and continue with the others. Answer as short lists with a heading per source.'
case "$P" in
  "") ;;
  movies) PQ="$G $V $E" ;;
  movies-global) PQ="$G $E" ;;
  movies-vn) PQ="$V $E" ;;
  news) PQ='Get the top 10 news headlines from two Vietnamese news sites. (1) browser_navigate https://vnexpress.net then browser_get_content: the first 10 main news headlines on the homepage, top story first. (2) browser_navigate https://thanhnien.vn then browser_get_content: the first 10 main news headlines on the homepage. Read each page once, no clicking. Keep each title in Vietnamese exactly as shown, then a short, literal English translation (translate plant, job and place names literally; do not guess). Answer as two numbered lists, one heading per source. If a site fails, say so and continue.' ;;
  ip) PQ='Get my current public IP with: curl -s https://api.ipify.org . Reply with only the IP.' ;;
  *) echo "unknown preset: $P (movies, movies-global, movies-vn, news, ip)" >&2; exit 2 ;;
esac
[ -n "$P" ] && Q="$PQ${Q:+ Extra notes: $Q}"
[ -n "$Q" ] || { echo 'usage: ask.sh -q="your question"  or  ask.sh -p=movies' >&2; exit 2; }
command -v python3 >/dev/null || { echo "needs python3" >&2; exit 2; }
H=(-H "content-type: application/json"); [ -n "$KEY" ] && H+=(-H "X-Session-API-Key: $KEY")
j() { python3 -c "import json,sys; print(json.load(sys.stdin)$1)"; }

BODY=$(python3 -c 'import json,sys; print(json.dumps({
  "workspace": {"kind": "LocalWorkspace", "working_dir": "/projects"},
  "agent_profile_id": sys.argv[2], "max_iterations": int(sys.argv[3]), "autotitle": False,
  "initial_message": {"role": "user", "run": True, "content": [{"type": "text", "text": sys.argv[1]}]}}))' "$Q" "$PROFILE" "$STEPS")
ID=$(curl -sf "${H[@]}" -X POST "$URL/api/conversations" -d "$BODY" | j '["id"]') \
  || { echo "cannot reach OpenHands at $URL" >&2; exit 1; }
echo "conversation $ID" >&2

start=$(date +%s)
while :; do
  ST=$(curl -sf "${H[@]}" "$URL/api/conversations/$ID" | j '["execution_status"]')
  case "$ST" in finished|error|stuck|paused) break ;; esac
  [ $(( $(date +%s) - start )) -ge "$TIMEOUT" ] && { echo "timeout after ${TIMEOUT}s (status=$ST)" >&2; exit 1; }
  printf '.' >&2; sleep 3
done
echo " $ST in $(( $(date +%s) - start ))s" >&2
ANS=$(curl -sf "${H[@]}" "$URL/api/conversations/$ID/agent_final_response" | j '["response"]' || true)
if [ -z "$ANS" ]; then
  # Hit the step limit or errored before calling finish: ask the agent to report what happened.
  echo "(no final answer; asking the agent for a summary)" >&2
  ASK=$(python3 -c 'import json; print(json.dumps({"question": "You were stopped before finishing. In a few short lines: did you complete the task? What did you try, why did it fail, and what should the user do next?"}))')
  ANS=$(curl -sf -m 300 "${H[@]}" -X POST "$URL/api/conversations/$ID/ask_agent" -d "$ASK" \
        | python3 -c 'import json,sys; d=json.load(sys.stdin); print(d.get("response") or d.get("answer") or json.dumps(d))' || echo "(summary failed)")
fi
echo "$ANS"
