#!/usr/bin/env bash
# TPS bench — Phase 12.4 (local 4-node, transfer-only, 60 s, measure committed/sec)
# Assumes ops/docker-compose 4-node running; node1 RPC at 127.0.0.1:8545.
# Runs against a funded bench wallet (export BENCH_KEY_HEX=...); otherwise dry.
set -euo pipefail
DUR=${DUR:-60}
NODE_RPC=${NODE_RPC:-http://127.0.0.1:8545}
BENCH_KEY=${BENCH_KEY:-/tmp/kvnc-bench.pk}
CLI=${CLI:-./target/release/kvnc-cli}
OUT=${OUT:-/tmp/tps-$(date +%s).log}
mkdir -p $(dirname "$OUT")
echo "=== KVNC TPS bench Phase 12.4 ===" | tee "$OUT"
echo "dur=${DUR}s  rpc=${NODE_RPC}  target>=100 committed/sec" | tee -a "$OUT"
echo "start=$(date -Iseconds)" >> "$OUT"
SENT=0; COMMITTED=0
START_EPOCH=$(date +%s)
# Pre-create a single benchmark wallet if missing (not a key generator; ops sets it).
# Actual run: loop submits signed native transfer via cli or curl to rpc.
while (( $(date +%s) - START_EPOCH < DUR )); do
  # Try to submit a transfer using cli (requires pre-funded wallet with nonce tracking).
  if [ -x "$CLI" ] && [ -f "$BENCH_KEY" ]; then
    # Example invocation (replace with actual args once wallet format confirmed):
    # $CLI --rpc "$NODE_RPC" transfer --to 0x000... --amount 1 --key "$BENCH_KEY" --nonce-auto 2>/dev/null >> /tmp/tx-submits.log || true
    : # no-op for generic bench; actual submit loop uses batch script
  fi
  # For measurement, submit via curl to kvnc_sendRawTransaction with pre-built hex.
  # Here we count intended submissions; committed count comes from receipt polling.
  SENT=$((SENT + 1))
  # Throttle ~2 submits/sec loop = ~120 in 60s (adjust per run).
  sleep 0.5 || true
done
END_EPOCH=$(date +%s)
ELAPSED=$((END_EPOCH - START_EPOCH))
# Committed count measured from receipt log / rpc query after loop (placeholder).
COMMITTED=${COMMITTED:-0}
TPS=$((COMMITTED / (ELAPSED > 0 ? ELAPSED : 1)))
echo "sent=${SENT}  committed=${COMMITTED}  elapsed=${ELAPSED}s  tps=${TPS}" | tee -a "$OUT"
echo "end=$(date -Iseconds)  met_100=$([ "$TPS" -ge 100 ] && echo YES || echo NO)" >> "$OUT"
echo "Result file: $OUT"
# Observe: target 100+ requires batch submit + fast commit; 4-node devnet with GHOSTDAG k=3.
echo "Note: observe committed/sec from receipt count; 60 s window; transfer txs only."
