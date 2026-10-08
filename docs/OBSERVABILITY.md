# Phase 21 — Observability (kvnc)

## Done (skeleton)
- `kvnc-consensus/src/metrics.rs` — registry, `record_block_height`, `record_mergeset_size`, `record_rss_proxy`.
- `prometheus-client = 0.25` in workspace.

## Minimal / next
- `/metrics` text exposition (HTTP TCP port, same as RPC or separate).
- Grafana JSON dashboard (`ops/grafana/` — not yet created).
- Alert rules YAML (`peer_drop`, `sync_stall`, `commit_lag`, `high_memory`).
- Structured logging (`tracing`) + optional `trace_id` per round.

## Exit
- Scrape `kvnc_node_block_height`, `kvnc_node_peer_count`, `kvnc_node_commit_latency`, `kvnc_node_mergeset_size`; dashboard shows live commit rate.
