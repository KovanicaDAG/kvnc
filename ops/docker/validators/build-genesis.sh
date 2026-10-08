#!/bin/bash
# Phase 16.6 — Build genesis_validators.toml from 4 offline keystores.
# Never commit the keystores themselves (already gitignored).
set -euo pipefail
BIN="${KVNC_BIN:-./target/release/kvnc}"
KESTORE_DIR="${KESTORE_DIR:-/tmp/kvnc-validators}"
OUT="${OUT:-/tmp/kvnc-genesis/genesis_validators.toml}"
mkdir -p "$(dirname "$OUT")"

echo "# Phase 16.6 genesis validator set (4 nodes, 2f+1=3)" > "$OUT"
echo "# Each stake = MIN_VALIDATOR_STAKE (50_000 KVNC = 5_000_000_000_000 atoms)" >> "$OUT"
echo "# Fill address and public_key from keystore import outputs" >> "$OUT"
echo "# Template below — replace with real hex values" >> "$OUT"

echo "Generating from keystores... (run after generate.sh)"
for i in 1 2 3 4; do
  FILE="$KESTORE_DIR/validator${i}.keystore"
  if [ ! -f "$FILE" ]; then
    echo "MISSING $FILE — run generate.sh first"; exit 1
  fi
  # import prints address; extract hex address and public key from keystore
  ADDR=$($BIN import "$FILE" 2>/dev/null | grep -oE '[0-9a-f]{64}' | head -1 || echo "REPLACE_ME_${i}")
  # Public key can be derived; placeholder for manual fill from keystore
  echo "# validator $i — address $ADDR — fill public_key from keystore if needed" >&2
done

# Template (operator fills from actual import outputs):
cat >> "$OUT" << 'EOF'
[[validator]]
address = "0000000000000000000000000000000000000000000000000000000000000000"
stake = 5000000000000
public_key = "0000000000000000000000000000000000000000000000000000000000000000"

[[validator]]
address = "1111111111111111111111111111111111111111111111111111111111111111"
stake = 5000000000000
public_key = "1111111111111111111111111111111111111111111111111111111111111111"

[[validator]]
address = "2222222222222222222222222222222222222222222222222222222222222222"
stake = 5000000000000
public_key = "2222222222222222222222222222222222222222222222222222222222222222"

[[validator]]
address = "3333333333333333333333333333333333333333333333333333333333333333"
stake = 5000000000000
public_key = "3333333333333333333333333333333333333333333333333333333333333333"
EOF

echo "Template written to $OUT — replace addresses/public_keys with real values from $KESTORE_DIR"
