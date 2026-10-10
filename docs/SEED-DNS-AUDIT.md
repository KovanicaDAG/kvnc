# Seed DNS — kvnc (Kovanica) status

Status: **OPEN — kvnc-native seed DNS not yet registered; no seed host is referenced in code.**

## Rule (HARD RULE 1)
kvnc is an independent chain. It must **never** hard-code, dial or document another project's
infrastructure. Any `*.kovanica.online` seed reference that previously appeared in this repo was
a cross-project leak and has been **removed from code, config defaults and docs**.

## Current code state (verified)
- `kvnc-network::NetworkConfig::default()` → `bootstrap_nodes: Vec::new()` (no hard-coded seeds).
- `kvnc_node::NodeConfig::default()` → `bootnodes: Vec::new()` (no hard-coded seeds).
- Seeds are supplied at deploy time via `KVNC_BOOTNODES` (comma-separated `host:port`) or the
  `[node] bootnodes` TOML key. Unit tests assert defaults stay empty.
- P2P listen port: **8000** (`0.0.0.0:8000`) on all nodes; libp2p multiaddr form is
  `/dns4/<seed-host>/tcp/8000/p2p/<peer-id>`.
- Placeholders in docs/tests use the reserved `.invalid` TLD (`seed1.kvnc.invalid:8000`) so they
  can never resolve accidentally.

## TODO before public testnet (must be done by the operator)
1. Register **3+ kvnc-native DNS names** (e.g. `seed1/2/3.<kvnc-domain>`) — new names, not reused
   from any other project.
2. Each name must resolve **A/AAAA directly to the seed origin IP** (grey-cloud / no CDN proxy);
   libp2p uses TCP+Noise, so a TLS-terminating proxy is not usable.
3. Publish each seed's real libp2p **PeerId** (derived from its persistent `node_key` file) — the
   `/p2p/` component must match or the dial fails.
4. Set `KVNC_BOOTNODES=seed1.<domain>:8000,seed2.<domain>:8000,seed3.<domain>:8000` on participants.
5. Verify from a clean host: `nc -vz seed1.<domain> 8000` resolves to the origin IP, not a proxy.

## Operator env safety (unchanged)
`KVNC_ALLOW_RESET=0`, `KVNC_FAUCET=0`, `KVNC_OPERATOR=0`, `KVNC_MINE=0` on public-facing nodes, and
`KVNC_DATA` must be preserved after the first genesis write.

## Note on the previous revision
An earlier revision of this document reconciled a "port 8000 vs 9000" gap by querying
`explorer.kovanica.online/api/bootstrap`, i.e. a different project's endpoint, and treated its
P2P peers as kvnc's own. That was incorrect and is superseded by this revision. kvnc's canonical
P2P port is a local decision: **8000**.
