#!/usr/bin/env bash
# Measure agentstory on this machine's real agent history.
# Usage: scripts/bench.sh [path-to-binary]   (default: target/release/agentstory)
# Uses a throwaway index, so your real one is untouched. Prints a Markdown table.
set -euo pipefail
BIN=${1:-target/release/agentstory}
[ -x "$BIN" ] || { echo "build first: cargo build --release" >&2; exit 1; }
TMP=$(mktemp -d); trap 'rm -rf "$TMP"' EXIT
export AGENTSTORY_DB="$TMP/bench.db"
QUERIES=("memory" "handoff" "error" "deploy" "测试" "按钮" "rate limit" "git push")

# peak RSS in MB and wall seconds of one command
measure() {
  if [[ "$(uname)" == Darwin ]]; then
    /usr/bin/time -l "$@" >/dev/null 2>"$TMP/t" || true
    rss=$(awk '/maximum resident set size/ {print $1}' "$TMP/t"); rss_mb=$(echo "scale=1; $rss/1048576" | bc)
    wall=$(awk '/ real / {print $1}' "$TMP/t")
  else
    /usr/bin/time -v "$@" >/dev/null 2>"$TMP/t" || true
    rss_mb=$(echo "scale=1; $(awk -F: '/Maximum resident/ {print $2}' "$TMP/t")/1024" | bc)
    wall=$(awk -F': ' '/Elapsed \(wall/ {print $2}' "$TMP/t")
  fi
  echo "$rss_mb $wall"
}

ms() { python3 -c 'import sys,time,subprocess
cmd=sys.argv[1:]; n=10; best=1e9; tot=0
for _ in range(n):
    t=time.perf_counter(); subprocess.run(cmd,stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL); d=(time.perf_counter()-t)*1000
    best=min(best,d); tot+=d
print(f"{tot/n:.0f} {best:.0f}")' "$@"; }

size_mb() { du -sk "$1" 2>/dev/null | awk '{printf "%.1f", $1/1024}'; }

read -r cold_rss cold_wall < <(measure "$BIN" index --full)
read -r warm_rss warm_wall < <(measure "$BIN" index)
stats=$("$BIN" --no-update --json stats)
sessions=$(echo "$stats" | python3 -c 'import sys,json; print(sum(r["sessions"] for r in json.load(sys.stdin)))')
turns=$(echo "$stats" | python3 -c 'import sys,json; print(sum(r["turns"] or 0 for r in json.load(sys.stdin)))')
src=0
for d in ~/.claude/projects ~/.cursor/projects ~/.codex/sessions ~/.codex/archived_sessions ~/.hermes/state.db ~/.gemini/tmp ~/.qwen/projects ~/.local/share/opencode; do
  [ -e "$d" ] && src=$(echo "$src + $(du -sk "$d" | awk '{print $1}')" | bc)
done
src_mb=$(echo "scale=0; $src/1024" | bc)

search_at() {  # level → "avg (min, max)" and peak RSS
  local rows="" rss=0
  for q in "${QUERIES[@]}"; do
    read -r avg _ < <(ms "$BIN" --no-update search --level "$1" $q)
    rows+="$avg "
    read -r r _ < <(measure "$BIN" --no-update search --level "$1" $q)
    rss=$(echo "if ($r > $rss) $r else $rss" | bc)
  done
  echo "$(echo "$rows" | python3 -c 'import sys; v=[float(x) for x in sys.stdin.read().split()]; print(f"{sum(v)/len(v):.0f} ms (min {min(v):.0f}, max {max(v):.0f})")'), peak memory $rss MB"
}
chat_search=$(search_at chat)
full_search=$(search_at full)
read -r refresh_avg _ < <(ms "$BIN" sessions -n 5)
id=$("$BIN" --no-update --json sessions -n 1 | python3 -c 'import sys,json; print(json.load(sys.stdin)[0]["id"])')
read -r handoff_avg _ < <(ms "$BIN" --no-update handoff "$id")
read -r handoff_rss _ < <(measure "$BIN" --no-update handoff "$id")
# MCP server idle memory
"$BIN" mcp < <(sleep 3) >/dev/null 2>&1 & pid=$!; sleep 1.5
mcp_rss=$(ps -o rss= -p $pid | awk '{printf "%.1f", $1/1024}'); wait $pid 2>/dev/null || true

bin_size=$(ls -l "$BIN" | awk '{printf "%.1f", $5/1048576}')
echo "| Metric | Value |"
echo "|---|---|"
echo "| Machine | $(sysctl -n machdep.cpu.brand_string 2>/dev/null || uname -m), $(uname -s) |"
echo "| Data | $sessions sessions, $turns turns, ${src_mb} MB of agent history on disk |"
echo "| Binary size | ${bin_size} MB (single static binary, SQLite bundled) |"
echo "| Full index build | ${cold_wall} s, peak memory ${cold_rss} MB |"
echo "| Index size | $(size_mb "$AGENTSTORY_DB") MB |"
echo "| Incremental refresh (nothing changed) | ${warm_wall} s, peak memory ${warm_rss} MB |"
echo "| Search \`--level chat\` (default; 8 queries incl. CJK) | ${chat_search} |"
echo "| Search \`--level full\` (incl. tool output) | ${full_search} |"
echo "| Query incl. auto-refresh (\`sessions\`) | ${refresh_avg} ms |"
echo "| Handoff card | ${handoff_avg} ms, peak memory ${handoff_rss} MB |"
echo "| MCP server idle | ${mcp_rss} MB |"
