# 6h MemoryMax=3G Soak — Phase 12.4

Run: `./ops/soak/soak.sh` (sets NODE_PID if needed; uses 30 s interval).
Log output: `/tmp/soak-<epoch>.log`

## Documented observed RSS (fill post-run)
- Run date: 2026-10-08 (scheduled; result pending full 6h execution)
- Memory cap: 3G (3,145,728 KB)
- Sampling: /proc/<pid>/status VmRSS every 30 s
- Expected range (4-node devnet, transfer-only load): 400–1200 MB (approximate from prior runs; update after soak)
- Alert threshold: >3G triggers WARNING in log.
- Crate change: none (node/ops version only).

## Usage
```bash
NODE_PID=$(pgrep -f kvnc-node | head -1)
./ops/soak/soak.sh
```
