#!/usr/bin/env bash
# Phase 16.6 — generate the four validator seed files + genesis_validators.toml.
#
# Produces exactly the files docker-compose.yml mounts:
#   ops/docker/validators/val1.pem .. val4.pem      (raw 32-byte hex seeds, gitignored)
#   ops/docker/validators/genesis_validators.toml   (gitignored; regenerate, never commit)
#
# A node's address is its raw Ed25519 public key, so its address hex equals its
# public-key hex; `kvnc address --key-file` derives both without ever echoing the
# seed. Existing valN.pem files are reused, so re-running only rebuilds genesis.
#
# These are DEVNET keys. Never reuse them on public infrastructure.
set -euo pipefail

BIN="${KVNC_BIN:-./target/release/kvnc}"
OUTDIR="${OUTDIR:-ops/docker/validators}"
# MIN_VALIDATOR_STAKE = 50_000 KVNC = 50_000_000_000_000 atoms.
STAKE="${STAKE:-50000000000000}"

if [ ! -x "$BIN" ]; then
  echo "error: kvnc binary not found at '$BIN'." >&2
  echo "       build it first: cargo build --release -p kvnc-cli" >&2
  exit 1
fi

mkdir -p "$OUTDIR"
GENESIS="$OUTDIR/genesis_validators.toml"
TMP="$(mktemp)"
trap 'rm -f "$TMP"' EXIT

{
  echo "# Phase 16.6 — 4-node validator set for genesis (generated $(date -u +%Y-%m-%dT%H:%M:%SZ))."
  echo "# DEVNET KEYS ONLY. Produced by ops/docker/validators/generate.sh; do not commit."
  echo "# Stakes = MIN_VALIDATOR_STAKE = 50_000 KVNC = $STAKE atoms."
} > "$TMP"

for i in 1 2 3 4; do
  KEY="$OUTDIR/val${i}.pem"
  if [ -s "$KEY" ]; then
    echo "-> reusing existing $KEY"
  else
    echo "-> generating validator $i seed -> $KEY"
    ( umask 077; head -c 32 /dev/urandom | od -An -v -tx1 | tr -d ' \n' > "$KEY"; printf '\n' >> "$KEY" )
  fi

  IDENTITY="$("$BIN" address --key-file "$KEY")"
  PK_HEX="$(printf '%s\n' "$IDENTITY" | awk -F': *' '/^Public key:/ {print $2}')"
  if [ -z "$PK_HEX" ]; then
    echo "error: could not derive identity from $KEY" >&2
    exit 1
  fi

  {
    echo "[[validator]]"
    echo "address = \"$PK_HEX\""
    echo "stake = $STAKE"
    echo "public_key = \"$PK_HEX\""
    echo
  } >> "$TMP"
done

mv "$TMP" "$GENESIS"
trap - EXIT

echo
echo "wrote $GENESIS and $OUTDIR/val1..4.pem"
echo "next: docker compose up --build"
