//! Sustained-liveness regression test: 4 real validator nodes must keep
//! committing long past the pruning boundary.
//!
//! # Why this test exists
//!
//! The 2-node [`live_vote_integration`] test (and the in-process
//! `multi_node_integration` / `single_node_integration` tests) only assert that
//! the *first* commit happens. That is enough to pass while a latent bug stalls
//! the DAG shortly afterwards:
//!
//! * blocks are produced only on leader rounds, so most rounds have no block;
//! * at commit #4 the just-committed leader is pruned, after which every
//!   `mergeset()` lookup returns `NotFound` and commits stop forever.
//!
//! The observable symptom is a hard stall at committed height 4. This test
//! drives the **real binary** (the actual round/commit loop over real TCP, not
//! `process_block` directly) with four validators and asserts the commit height
//! climbs well past that boundary and keeps climbing.
//!
//! Expected result:
//! * **TODAY (bug present): FAIL** — all four nodes stall at height 4 and the
//!   "reach target" phase times out (~120 s).
//! * **AFTER THE FIX: PASS** — height reaches >= 20, all nodes agree, and it
//!   keeps growing.
//!
//! # Ports
//!
//! The live Docker quorum currently holds RPC `127.0.0.1:8545-8548` and P2P
//! `9000`-ish, so this test uses a **disjoint** range: RPC `8630-8633`, P2P
//! `9230-9233` (see the `*_BASE_PORT` constants). It never touches Docker or the
//! running quorum.
//!
//! # Running
//!
//! The test spawns `target/release/kvnc-node`, so build it first:
//!
//! ```text
//! cargo build --release -p kvnc-node
//! cargo test  -p kvnc-node --test sustained_liveness_integration -- --nocapture
//! ```
//!
//! Runtime is roughly 30-60 s when healthy and up to ~3.5 min in the worst
//! (failing) case; the bounded windows are defined by the `*_TIMEOUT_SECS`
//! constants below. All child processes and temp data dirs are removed on both
//! success and failure (see [`QuorumNode`]'s `Drop`). This mirrors the existing
//! `live_vote_integration` convention of a plain `#[tokio::test]` (no
//! `#[ignore]`).

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context, Result};
use ed25519_dalek::SigningKey;
use kvnc_crypto::generate_keypair;
use kvnc_staking::MIN_VALIDATOR_STAKE;
use kvnc_types::{Address, PublicKey};
use reqwest::Client;
use serde_json::{json, Value};
use tempfile::TempDir;
use tokio::time::sleep;
use tracing::info;

// ---------------------------------------------------------------------------
// Test parameters (all tunable knobs live here)
// ---------------------------------------------------------------------------

/// Number of validators in the quorum (4 -> quorum threshold floor(2T/3)+1 = 3).
const NODES: usize = 4;

/// Consensus round duration, matching the Docker quorum.
const ROUND_DURATION_MS: u64 = 1000;

/// Sustained-liveness target: must commit at least this many leaders.
///
/// This is deliberately `> 4 * (k + 1)` with `k = 3` (i.e. `> 16`): it forces
/// the chain past the point where the buggy prune deletes the just-committed
/// leader (observed stall at height 4).
const TARGET_HEIGHT: u64 = 20;

/// RPC base port. Chosen to avoid the live Docker quorum (8545-8548).
const RPC_BASE_PORT: u16 = 8630;

/// P2P base port. Chosen to avoid the live Docker quorum (~9000).
const P2P_BASE_PORT: u16 = 9230;

/// How long to wait for a freshly started node's JSON-RPC to answer.
const RPC_READY_TIMEOUT_SECS: u64 = 30;

/// How long to wait for each node to see at least one peer.
const PEER_TIMEOUT_SECS: u64 = 30;

/// Bounded window for reaching [`TARGET_HEIGHT`].
const REACH_TARGET_TIMEOUT_SECS: u64 = 120;

/// Bounded window for all four nodes to agree on a height.
const SETTLE_TIMEOUT_SECS: u64 = 30;

/// Bounded window for observing commits *after* the target (proves the commit
/// loop no longer stalls on a dangling `last_committed`).
const GROWTH_TIMEOUT_SECS: u64 = 30;

/// Poll cadence for heights / peers.
const POLL_INTERVAL_MS: u64 = 500;

// ---------------------------------------------------------------------------
// Child process harness
// ---------------------------------------------------------------------------

/// A running `kvnc-node` validator plus everything needed to tear it down.
///
/// `std::process::Child` (not the tokio one) is used so cleanup can happen
/// synchronously from [`Drop`], which runs on both the success path and on
/// panic unwind.
struct QuorumNode {
    index: usize,
    child: Child,
    /// Kept alive purely for RAII: dropping it removes the node's data dir.
    #[allow(dead_code)]
    data_dir: TempDir,
    /// Where the node writes stdout+stderr while it runs (inside `data_dir`).
    temp_log_path: PathBuf,
    /// Stable, temp-dir-independent archive of the log. `Drop` copies the temp
    /// log here *before* `data_dir` (and its `TempDir`) is removed, so the log
    /// survives both success and panic teardown.
    log_path: PathBuf,
    rpc_port: u16,
    #[allow(dead_code)]
    p2p_port: u16,
    #[allow(dead_code)]
    address: Address,
    #[allow(dead_code)]
    public_key: PublicKey,
}

impl QuorumNode {
    /// Provision a fresh data dir (genesis + key) and spawn the node.
    #[allow(clippy::too_many_arguments)]
    fn start(
        index: usize,
        rpc_port: u16,
        p2p_port: u16,
        bootnodes: Vec<String>,
        signing_key: SigningKey,
        public_key: PublicKey,
        address: Address,
        binary_path: &Path,
        genesis_toml: &str,
        log_dir: &Path,
    ) -> Result<Self> {
        let data_dir = TempDir::new().with_context(|| format!("node {index}: create data dir"))?;

        // Every node gets the *same* genesis validator set (all four members) so
        // that `build_committee` derives an identical committee everywhere.
        std::fs::write(data_dir.path().join("genesis_validators.toml"), genesis_toml)
            .with_context(|| format!("node {index}: write genesis_validators.toml"))?;

        // 32-byte hex seed; the node derives its Ed25519 key from this.
        let key_path = data_dir.path().join("validator.key");
        std::fs::write(&key_path, hex::encode(signing_key.to_bytes()))
            .with_context(|| format!("node {index}: write validator.key"))?;

        // Capture node stdout/stderr. The live log lives inside the data dir
        // (removed on teardown); `Drop` copies it into the stable `log_dir`
        // before that happens, so post-mortem survives either outcome.
        let temp_log_path = data_dir.path().join(format!("node{index}.log"));
        let log_path = log_dir.join(format!("node{index}.log"));
        let log = std::fs::File::create(&temp_log_path)
            .with_context(|| format!("node {index}: create {}", temp_log_path.display()))?;
        let log_err = log.try_clone()?;

        let child = Command::new(binary_path)
            .current_dir(data_dir.path())
            .env("KVNC_DATA_DIR", data_dir.path())
            .env("KVNC_RPC_ADDR", "127.0.0.1")
            .env("KVNC_RPC_PORT", rpc_port.to_string())
            .env("KVNC_LISTEN_ADDR", format!("127.0.0.1:{p2p_port}"))
            .env("KVNC_MAX_PEERS", "8")
            .env("KVNC_ROUND_DURATION_MS", ROUND_DURATION_MS.to_string())
            .env("KVNC_RUN_VALIDATOR", "true")
            .env("KVNC_MYSTICGHOST", "true")
            // Disable the RPC rate limiter: the poll-driven `kvnc_blockNumber`
            // calls would otherwise hit HTTP 429 under the default 60 req/min
            // (this is what the previous failure actually was, not a stall).
            .env("KVNC_RPC_RATE_LIMIT_PER_MIN", "0")
            .env("KVNC_VALIDATOR_KEY", key_path.as_os_str())
            .env("KVNC_BOOTNODES", bootnodes.join(","))
            .env("RUST_LOG", "info")
            .stdout(Stdio::from(log))
            .stderr(Stdio::from(log_err))
            .spawn()
            .with_context(|| format!("node {index}: spawn {}", binary_path.display()))?;

        info!(
            node = index,
            rpc = rpc_port,
            p2p = p2p_port,
            bootnodes = ?bootnodes,
            log = %log_path.display(),
            "spawned validator node"
        );

        Ok(Self {
            index,
            child,
            data_dir,
            temp_log_path,
            log_path,
            rpc_port,
            p2p_port,
            address,
            public_key,
        })
    }

    /// Block until this node's JSON-RPC answers `kvnc_blockNumber`.
    async fn wait_ready(&self, client: &Client, timeout_secs: u64) -> Result<()> {
        let deadline = Instant::now() + Duration::from_secs(timeout_secs);
        loop {
            if get_height(client, self.rpc_port).await.is_ok() {
                return Ok(());
            }
            if Instant::now() >= deadline {
                bail!(
                    "node {} RPC on 127.0.0.1:{} not ready after {}s",
                    self.index,
                    self.rpc_port,
                    timeout_secs
                );
            }
            sleep(Duration::from_millis(POLL_INTERVAL_MS)).await;
        }
    }

    /// One-line liveness snapshot for the end-of-test report.
    ///
    /// Uses `try_wait`, which reaps the child if it already exited (and caches
    /// the status), or reports `running` if it is still alive.
    fn status_line(&mut self) -> String {
        match self.child.try_wait() {
            Ok(Some(status)) => format!("exited={status}"),
            Ok(None) => "running".to_string(),
            Err(e) => format!("try_wait-error={e}"),
        }
    }

    /// Print `node{i} pid=... exited=<status|running> log=<stable path>`.
    fn report_status(&mut self) {
        let status = self.status_line();
        eprintln!(
            "[sustained_liveness] node {} pid={} rpc=127.0.0.1:{} {} log={}",
            self.index,
            self.child.id(),
            self.rpc_port,
            status,
            self.log_path.display()
        );
    }

    /// Stop the node and wait for it to exit. Idempotent with the `Drop` guard.
    fn stop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for QuorumNode {
    fn drop(&mut self) {
        // Best-effort SIGKILL so no node process ever outlives the test, even on
        // panic unwind.
        let _ = self.child.kill();
        let _ = self.child.wait();

        // `Drop::drop` runs *before* the fields are dropped, so `data_dir`
        // (and its `TempDir`) still exists here: copy the live log into the
        // stable archive before it is removed.
        match std::fs::copy(&self.temp_log_path, &self.log_path) {
            Ok(bytes) => eprintln!(
                "[sustained_liveness] preserved node {} log ({} bytes) -> {}",
                self.index,
                bytes,
                self.log_path.display()
            ),
            Err(e) => eprintln!(
                "[sustained_liveness] failed to preserve node {} log {} -> {}: {e}",
                self.index,
                self.temp_log_path.display(),
                self.log_path.display()
            ),
        }
    }
}

// ---------------------------------------------------------------------------
// JSON-RPC helpers
// ---------------------------------------------------------------------------

/// One JSON-RPC round trip.
async fn rpc_call(client: &Client, port: u16, method: &str, params: Value) -> Result<Value> {
    let resp = client
        .post(format!("http://127.0.0.1:{port}/rpc"))
        .json(&json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
            "id": 1,
        }))
        .send()
        .await
        .with_context(|| format!("RPC POST {method} on port {port} (connect/send)"))?;

    // Read the raw body first so we can distinguish connection refused (handled
    // by the `?` above) from an HTTP non-200 from a JSON-RPC `error` object from
    // a plain malformed body.
    let status = resp.status();
    let body = resp
        .text()
        .await
        .with_context(|| format!("RPC read body {method} on port {port}"))?;

    if !status.is_success() {
        bail!("RPC {method} on port {port}: HTTP {status} (non-200 body: {body})");
    }

    let value: Value = serde_json::from_str(&body)
        .with_context(|| format!("RPC decode {method} on port {port}: invalid JSON `{body}`"))?;

    if let Some(err) = value.get("error") {
        bail!("RPC {method} on port {port}: JSON-RPC error object: {err}");
    }

    Ok(value)
}

/// `kvnc_blockNumber` -> latest committed leader height.
async fn get_height(client: &Client, port: u16) -> Result<u64> {
    let resp = rpc_call(client, port, "kvnc_blockNumber", json!([])).await?;
    let raw = resp
        .get("result")
        .and_then(Value::as_str)
        .with_context(|| format!("kvnc_blockNumber on port {port}: missing string result"))?;
    let raw = raw.strip_prefix("0x").unwrap_or(raw);
    u64::from_str_radix(raw, 16)
        .with_context(|| format!("kvnc_blockNumber on port {port}: invalid hex `{raw}`"))
}

/// `/health` -> connected peer count.
async fn peer_count(client: &Client, port: u16) -> Result<u64> {
    let resp = client
        .get(format!("http://127.0.0.1:{port}/health"))
        .send()
        .await
        .with_context(|| format!("health GET on port {port}"))?
        .json::<Value>()
        .await
        .with_context(|| format!("health decode on port {port}"))?;
    resp.get("peer_count")
        .and_then(Value::as_u64)
        .with_context(|| format!("health on port {port}: missing peer_count"))
}

/// Wait (best-effort, bounded) for every node to have at least one peer, then
/// assert connectivity. Without peers no votes can reach quorum.
async fn wait_for_peers(client: &Client, nodes: &[QuorumNode], timeout_secs: u64) {
    let deadline = Instant::now() + Duration::from_secs(timeout_secs);
    let mut counts = vec![0u64; nodes.len()];
    loop {
        for (i, node) in nodes.iter().enumerate() {
            match peer_count(client, node.rpc_port).await {
                Ok(c) => counts[i] = c,
                Err(e) => eprintln!(
                    "[sustained_liveness] node {i} (rpc 127.0.0.1:{}) /health peer_count probe failed: {e:#}",
                    node.rpc_port
                ),
            }
        }
        if counts.iter().all(|c| *c >= 1) || Instant::now() >= deadline {
            break;
        }
        sleep(Duration::from_millis(POLL_INTERVAL_MS)).await;
    }
    info!(?counts, "observed peer counts");
    for (i, c) in counts.iter().enumerate() {
        assert!(
            *c >= 1,
            "node {i} has no peers after {timeout_secs}s; the quorum cannot form"
        );
    }
}

// ---------------------------------------------------------------------------
// Sustained-liveness polling
// ---------------------------------------------------------------------------

/// Dump per-node liveness + RPC diagnostics. Called right before a convergence
/// failure so the report contains the *real* RPC error string and whether the
/// node process is still alive (rather than a bare "RPC unavailable").
async fn dump_diagnostics(client: &Client, nodes: &mut [QuorumNode]) {
    eprintln!("[sustained_liveness] ===== convergence diagnostics =====");
    for node in nodes.iter_mut() {
        node.report_status();
        match get_height(client, node.rpc_port).await {
            Ok(h) => eprintln!(
                "[sustained_liveness]   node {} kvnc_blockNumber ok height={h}",
                node.index
            ),
            Err(e) => eprintln!(
                "[sustained_liveness]   node {} kvnc_blockNumber error: {e:#}",
                node.index
            ),
        }
        match peer_count(client, node.rpc_port).await {
            Ok(c) => eprintln!(
                "[sustained_liveness]   node {} /health peer_count={c}",
                node.index
            ),
            Err(e) => eprintln!(
                "[sustained_liveness]   node {} /health error: {e:#}",
                node.index
            ),
        }
    }
    eprintln!("[sustained_liveness] ===== end diagnostics =====");
}

/// Poll all nodes until `done` is satisfied or `timeout` elapses.
///
/// While polling, each node's committed height must be **non-decreasing** once
/// it has been observed (`last_committed` must never dangle or roll back). The
/// returned vector contains one resolved height per node from the satisfying
/// sample.
///
/// Takes `&mut [QuorumNode]` so that a convergence failure can inspect each
/// child's exit status via [`QuorumNode::report_status`].
async fn wait_until(
    client: &Client,
    nodes: &mut [QuorumNode],
    timeout: Duration,
    monotonic: &mut [Option<u64>],
    mut done: impl FnMut(&[Option<u64>]) -> bool,
) -> Result<Vec<u64>> {
    let deadline = Instant::now() + timeout;
    let mut last: Vec<Option<u64>> = vec![None; nodes.len()];
    loop {
        for (i, node) in nodes.iter_mut().enumerate() {
            match get_height(client, node.rpc_port).await {
                Ok(h) => {
                    if let Some(prev) = monotonic[i] {
                        assert!(
                            h >= prev,
                            "node {i} committed height regressed from {prev} to {h}: \
                             a committed decision was rolled back (dangling last_committed)"
                        );
                    }
                    monotonic[i] = Some(h);
                    last[i] = Some(h);
                }
                Err(err) => {
                    // Surface the concrete transport/RPC error instead of
                    // collapsing it to `None`: connection refused vs timeout vs
                    // JSON-RPC error vs non-200.
                    eprintln!(
                        "[sustained_liveness] node {i} (rpc 127.0.0.1:{}) \
                         kvnc_blockNumber failed: {err:#}",
                        node.rpc_port
                    );
                    last[i] = None;
                }
            }
        }

        if done(&last) {
            if last.iter().any(Option::is_none) {
                dump_diagnostics(client, nodes).await;
            }
            return last
                .into_iter()
                .enumerate()
                .map(|(i, h)| h.with_context(|| format!("node {i} RPC unavailable at convergence")))
                .collect();
        }

        if Instant::now() >= deadline {
            dump_diagnostics(client, nodes).await;
            bail!(
                "timed out after {:?} waiting for sustained liveness; \
                 last committed heights per node: {:?}",
                timeout,
                last
            );
        }
        sleep(Duration::from_millis(POLL_INTERVAL_MS)).await;
    }
}

/// True when every node reports the same height and that height is `>= min`.
fn all_equal_and_at_least(sample: &[Option<u64>], min: u64) -> bool {
    let heights: Option<Vec<u64>> = sample.iter().copied().collect();
    match heights {
        Some(v) if !v.is_empty() => v.iter().all(|h| *h == v[0]) && v[0] >= min,
        _ => false,
    }
}

/// Max height observed in a sample (0 if none).
fn max_height(sample: &[Option<u64>]) -> u64 {
    sample.iter().flatten().copied().max().unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

/// Resolve `<workspace>/target/release/kvnc-node` from the crate manifest dir.
fn node_binary_path() -> PathBuf {
    std::env::var("CARGO_MANIFEST_DIR")
        .map(PathBuf::from)
        .map(|p| p.join("../.."))
        .unwrap_or_else(|_| PathBuf::from("."))
        .join("target/release/kvnc-node")
}

/// Resolve the stable, temp-dir-independent directory that per-node logs are
/// archived into.
///
/// Overridable with `KVNC_TEST_LOG_DIR`; otherwise defaults to
/// `<workspace>/target/quorum-logs/<unix_ts>` so successive runs never clobber
/// each other.
fn stable_log_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("KVNC_TEST_LOG_DIR") {
        return PathBuf::from(dir);
    }
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    std::env::var("CARGO_MANIFEST_DIR")
        .map(PathBuf::from)
        .map(|p| p.join("../.."))
        .unwrap_or_else(|_| PathBuf::from("."))
        .join("target/quorum-logs")
        .join(ts.to_string())
}

/// Build the shared `genesis_validators.toml` (all four members, equal stake).
fn build_genesis_toml(validators: &[(SigningKey, PublicKey, Address)]) -> String {
    let mut out = String::from("# generated by sustained_liveness_integration\n");
    for (_, pk, addr) in validators {
        out.push_str("[[validator]]\n");
        out.push_str(&format!("address = \"{}\"\n", hex::encode(addr.0)));
        out.push_str(&format!("stake = {MIN_VALIDATOR_STAKE}\n"));
        out.push_str(&format!("public_key = \"{}\"\n\n", hex::encode(pk.0)));
    }
    out
}

// ---------------------------------------------------------------------------
// The test
// ---------------------------------------------------------------------------

/// Boot four real validators and prove the commit loop keeps running well past
/// the pruning boundary. See the module docs for the expected pre/post-fix
/// behaviour.
#[tokio::test]
async fn four_validators_sustain_commits_past_pruning_boundary() -> Result<()> {
    let _ = tracing_subscriber::fmt().with_env_filter("info").try_init();

    let binary_path = node_binary_path();
    if !binary_path.exists() {
        bail!(
            "release binary {} not found; build it first with \
             `cargo build --release -p kvnc-node`",
            binary_path.display()
        );
    }
    info!(binary = %binary_path.display(), "using node binary");

    // Stable log archive: survives the per-node TempDir teardown. Override with
    // KVNC_TEST_LOG_DIR; default <workspace>/target/quorum-logs/<unix_ts>.
    let log_dir = stable_log_dir();
    std::fs::create_dir_all(&log_dir)
        .with_context(|| format!("create stable log dir {}", log_dir.display()))?;
    eprintln!(
        "[sustained_liveness] preserving node logs under: {}",
        log_dir.display()
    );

    // Four distinct validator identities.
    let mut validators: Vec<(SigningKey, PublicKey, Address)> = Vec::with_capacity(NODES);
    for _ in 0..NODES {
        let (sk, pk) = generate_keypair();
        let addr = Address::from_public_key(&pk);
        validators.push((sk, pk, addr));
    }
    let genesis_toml = build_genesis_toml(&validators);

    let client = Client::new();

    // Start nodes sequentially so each new node dials already-running peers;
    // the network layer also retries bootstrap dials, so late nodes converge.
    let mut quorum: Vec<QuorumNode> = Vec::with_capacity(NODES);
    for i in 0..NODES {
        let rpc_port = RPC_BASE_PORT + i as u16;
        let p2p_port = P2P_BASE_PORT + i as u16;
        let bootnodes = (0..NODES)
            .filter(|j| *j != i)
            .map(|j| format!("127.0.0.1:{}", P2P_BASE_PORT + j as u16))
            .collect::<Vec<_>>();
        let (sk, pk, addr) = validators[i].clone();

        let node = QuorumNode::start(
            i,
            rpc_port,
            p2p_port,
            bootnodes,
            sk,
            pk,
            addr,
            &binary_path,
            &genesis_toml,
            &log_dir,
        )
        .with_context(|| format!("starting validator node {i}"))?;
        node.wait_ready(&client, RPC_READY_TIMEOUT_SECS)
            .await
            .with_context(|| format!("validator node {i} RPC never became ready"))?;
        quorum.push(node);
    }

    // Best-effort connectivity check (fails fast with a clear message if the
    // cluster never links up at all).
    wait_for_peers(&client, &quorum, PEER_TIMEOUT_SECS).await;

    let mut monotonic: Vec<Option<u64>> = vec![None; NODES];

    // --- Phase A: reach the sustained-liveness target -----------------------
    // TODAY this times out with all four nodes stuck at height 4.
    let reached = wait_until(
        &client,
        &mut quorum,
        Duration::from_secs(REACH_TARGET_TIMEOUT_SECS),
        &mut monotonic,
        |s| max_height(s) >= TARGET_HEIGHT,
    )
    .await
    .with_context(|| {
        format!(
            "committed height did not reach {TARGET_HEIGHT} within \
             {REACH_TARGET_TIMEOUT_SECS}s (sustained-liveness stall)"
        )
    })?;
    info!(
        heights = ?reached,
        target = TARGET_HEIGHT,
        "target committed height reached"
    );

    // --- Phase B: all four nodes agree -------------------------------------
    let converged = wait_until(
        &client,
        &mut quorum,
        Duration::from_secs(SETTLE_TIMEOUT_SECS),
        &mut monotonic,
        |s| all_equal_and_at_least(s, TARGET_HEIGHT),
    )
    .await
    .context("nodes did not converge on an equal committed height at/after the target")?;

    let base = converged[0];
    assert!(
        converged.iter().all(|h| *h == base),
        "all four nodes must report the same committed height, got {converged:?}"
    );
    assert!(
        base >= TARGET_HEIGHT,
        "converged height {base} is below the target {TARGET_HEIGHT}"
    );
    info!(height = base, "all four nodes converged on an equal height");

    // --- Phase C: keep committing past the target --------------------------
    // If pruning left `last_committed` dangling, the height freezes at `base`
    // and this phase times out.
    let grown = wait_until(
        &client,
        &mut quorum,
        Duration::from_secs(GROWTH_TIMEOUT_SECS),
        &mut monotonic,
        |s| max_height(s) >= base + 1,
    )
    .await
    .context(
        "no further commits after reaching the target: the commit loop stalled \
         (suspected dangling last_committed after pruning)",
    )?;
    info!(heights = ?grown, from = base, "commit loop advanced past the target");

    // --- Final: re-converge and confirm strictly-positive sustained growth --
    let final_heights = wait_until(
        &client,
        &mut quorum,
        Duration::from_secs(SETTLE_TIMEOUT_SECS),
        &mut monotonic,
        |s| all_equal_and_at_least(s, base + 1),
    )
    .await
    .context("nodes failed to re-converge after advancing past the target")?;

    let final_height = final_heights[0];
    assert!(
        final_heights.iter().all(|h| *h == final_height),
        "final committed heights must be equal across all nodes, got {final_heights:?}"
    );
    assert!(
        final_height > base,
        "committed height must keep growing after the target (base={base}, final={final_height})"
    );
    info!(
        final_height,
        base, "sustained liveness confirmed past pruning boundary"
    );

    // End-of-test liveness snapshot for every node (pid + exited/running).
    for node in quorum.iter_mut() {
        node.report_status();
    }

    // Explicit teardown on the success path; `Drop` is the backstop for panics
    // and copies each live log into the stable archive before the temp dir dies.
    for node in quorum.iter_mut() {
        node.stop();
    }
    eprintln!(
        "[sustained_liveness] node logs archived under: {}",
        log_dir.display()
    );
    Ok(())
}
