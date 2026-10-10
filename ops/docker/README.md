# KVNC Docker

## Build the node image

```bash
docker build -t kvnc-node:local .
```

The image contains only the `kvnc-node` binary and a minimal `debian:bookworm-slim`
runtime. It runs as the unprivileged `kvnc` user (uid/gid 10001), stores chain data
under `/var/lib/kvnc` (declared `VOLUME`), and exposes P2P `8000` / JSON-RPC `8545`.
A plain `docker run kvnc-node:local` prints `--help`; it never boots a node by
default.

## Run the local 4-node devnet

```bash
docker compose up --build        # start (builds the image once)
docker compose ps                # health of all four nodes
docker compose logs -f node1
docker compose down              # stop, keep volumes
docker compose down -v           # stop and wipe all chain data
```

Topology (`docker-compose.yml`):

| Node  | RPC (internal) | Host RPC        | Data volume  | Bootnodes                    |
|-------|----------------|-----------------|--------------|------------------------------|
| node1 | 8545           | `127.0.0.1:8545`| node1-data   | node2, node3, node4          |
| node2 | 8546           | —               | node2-data   | node1, node3, node4          |
| node3 | 8547           | —               | node3-data   | node1, node2, node4          |
| node4 | 8548           | —               | node4-data   | node1, node2, node3          |

Only node1 publishes its RPC port, bound to loopback. To reach another node's RPC,
query it from inside the network, e.g.:

```bash
docker compose exec node2 curl -s http://127.0.0.1:8546/health
docker compose exec node2 curl -s -X POST http://127.0.0.1:8546/rpc \
  -H 'content-type: application/json' \
  -d '{"jsonrpc":"2.0","id":1,"method":"kvnc_blockNumber","params":[]}'
```

or add a `ports:` mapping in `docker-compose.yml` and re-run `docker compose up -d`.

### Genesis and validator seeds

- **No genesis file is required.** On a fresh data directory the node writes an
  initial genesis state itself (`crates/kvnc-node/src/main.rs::init_genesis`).
  `docker compose down -v` gives every node a fresh genesis; plain `down` keeps it.
- **Validator seeds are never committed.** The image entrypoint
  (`ops/docker/entrypoint.sh`) generates a random 32-byte hex seed at
  `/var/lib/kvnc/validator.key` on first boot and passes it to the node via
  `KVNC_VALIDATOR_KEY`. The seed lives only in the node's data volume and survives
  restarts. Point `KVNC_VALIDATOR_KEY` at your own file to override.
- This repo's node has **no faucet and no `ALLOW_RESET`/mining toggles** to disable;
  it always runs the consensus loop as configured. Keep the host RPC binding on
  loopback (the compose file already does) and never expose this devnet publicly.

### Current devnet limitation

The node bootstrap builds a **per-node, single-authority committee** from its local
identity (`build_committee`). A genesis/committee format that carries several
validators does not exist yet, so the four nodes cannot form a real M-of-N quorum.
The devnet exercises P2P connectivity, gossip, the mempool and the JSON-RPC surface.
True multi-validator finality is a follow-up (RFC-006-era consensus work).

## Adding or removing a node

1. Add a `nodeN` service to `docker-compose.yml` copied from `node2`, with a new
   `KVNC_RPC_PORT`, its own `<nodeN>-data` volume, a distinct healthcheck URL, and
   `KVNC_BOOTNODES` listing the existing nodes.
2. Add the new node's `host:8000` to the other nodes' `KVNC_BOOTNODES` and `<nodeN>-data`
   to the `volumes:` section.
3. `docker compose up -d`; scale down with `docker compose rm -sf nodeN` followed by
   removing its service and volume.

## Healthcheck

`GET /health` returns `{"status":"ok","peer_count":N}` (HTTP 200). The image and
compose healthchecks treat a successful request as healthy; `peer_count` is
informational and `0` is healthy.