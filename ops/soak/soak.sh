#!/usr/bin/env bash
# 6-hour soak — Phase 12.4. MemoryMax=3G (via systemd slice or cgroup v2 memory.max).
# Logs RSS (KB) every 30 s; stops at 6 h (21600 s).
set -euo pipefail
DURATION=${DURATION:-21600}  # 6h
INTERVAL=${INTERVAL:-30}
NODE_PID=${NODE_PID:-}         # set to target node process pid
LOG=${LOG:-/tmp/soak-$(date +%s).log}
mkdir -p $(dirname "$LOG")
# Memory cap setup (best-effort; requires root or pre-set cgroup).
# Example cgroup v2: echo 3G > /sys/fs/cgroup/kvnc-node/memory.max
MEM_LIMIT=${MEM_LIMIT:-3G}
echo "=== KVNC 6h soak Phase 12.4 ===" | tee "$LOG"
echo "mem_max=${MEM_LIMIT}  interval=${INTERVAL}s  duration=${DURATION}s" | tee -a "$LOG"
echo "start=$(date -Iseconds)" >> "$LOG"
# Try to enforce memory limit via cgroup (optional / best-effort).
if [ -d "/sys/fs/cgroup/kvnc-soak" ]; then
  echo "${MEM_LIMIT}" > /sys/fs/cgroup/kvnc-soak/memory.max 2>/dev/null || true
fi
# If NODE_PID not set, try to find kvnc-node binary.
PID=${NODE_PID:-$(pgrep -f 'kvnc-node' | head -n1 || true)}
echo "node_pid=${PID}" | tee -a "$LOG"
START_EPOCH=$(date +%s)
while (( $(date +%s) - START_EPOCH < DURATION )); do
  TS=$(date -Iseconds)
  # Measure RSS: prefer /proc/$PID/status VmRSS, fall back to ps.
  RSS_KB=0
  if [ -n "$PID" ] && [ -r "/proc/$PID/status" ]; then
    RSS_KB=$(awk '/VmRSS:/{print $2}' /proc/$PID/status 2>/dev/null || echo 0)
  elif [ -n "$PID" ]; then
    RSS_KB=$(ps -o rss= -p "$PID" 2>/dev/null | tr -d ' ' || echo 0)
  fi
  # Log: timestamp, pid, rss_kb, elapsed_s
  ELAPSED=$(( $(date +%s) - START_EPOCH ))
  echo "${TS} pid=${PID:-none} rss_kb=${RSS_KB} elapsed_s=${ELAPSED}" | tee -a "$LOG"
  # If rss exceeds 3G (3,145,728 KB) flag it for review.
  if [ -n "$RSS_KB" ] && [ "$RSS_KB" -gt 3145728 ]; then
    echo "WARN ${TS} rss=${RSS_KB} KB exceeds ${MEM_LIMIT} (3G)" >> "$LOG"
  fi
  sleep "$INTERVAL"
done
echo "end=$(date -Iseconds)" >> "$LOG"
echo "=== Soak complete. Log: $LOG ==="
# Final observed RSS snapshot.
echo "final_pid=${PID:-none}" >> "$LOG"
if [ -n "$PID" ] && [ -r "/proc/$PID/status" ]; then
  echo "final_rss_kb=$(awk '/VmRSS:/{print $2}' /proc/$PID/status 2>/dev/null || echo N/A)" >> "$LOG"
fi
# Document observed max/min (post-run summary written to same log by user review).
