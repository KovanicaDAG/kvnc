#!/bin/bash
# Phase 16.6 — Generate 4 validator keystores (offline, never commit to repo).
# Run once on an air-gapped / build host. Keys stay in /tmp/kvnc-validators
# (or mount into container at /var/lib/kvnc/validator.keystore).
# Already gitignored via keystore/ and *.pem rules.
set -euo pipefail

BIN="${KVNC_BIN:-./target/release/kvnc}"
OUTDIR="${OUTDIR:-/tmp/kvnc-validators}"
PASS="${PASSPHRASE:-devnet-phase16.6}"
mkdir -p "$OUTDIR"

for i in 1 2 3 4; do
  echo "-> generating validator $i -> $OUTDIR/validator${i}.keystore"
  "$BIN" keygen --passphrase "$PASS" --output "$OUTDIR/validator${i}.keystore"
done

# Export hex seeds for node consumption (node reads 32-byte hex seed, not encrypted keystore)
for i in 1 2 3 4; do
  echo "-> exporting seed $i -> $OUTDIR/validator${i}.pem"
  # Note: 'export' prints private seed to stdout; redirect to file.
  # In production, do this on an air-gapped host only.
  "$BIN" export "$OUTDIR/validator${i}.keystore" --passphrase "$PASS" > "$OUTDIR/validator${i}.pem" 2>/dev/null || echo "export blocked by guard; manually extract seed"
done

echo "4 keystores written to $OUTDIR"
echo "Import each address with: $BIN import $OUTDIR/validator{i}.keystore"
