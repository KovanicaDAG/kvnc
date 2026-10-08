//! Prometheus metrics for MysticGhost consensus.

use once_cell::sync::Lazy;
use prometheus_client::encoding::EncodeLabelSet;
use prometheus_client::metrics::counter::Counter;
use prometheus_client::metrics::family::Family;
use prometheus_client::metrics::gauge::Gauge;
use prometheus_client::metrics::histogram::Histogram;
use prometheus_client::encoding::text::{encode, encode_eof};
use prometheus_client::registry::{Registry, Unit};
use std::sync::Mutex;

/// Global metrics registry.
static REGISTRY: Lazy<Mutex<Registry>> = Lazy::new(|| Mutex::new(Registry::default()));

/// Get the global metrics registry.
pub fn registry() -> &'static Mutex<Registry> {
    &REGISTRY
}

/// Labels for MysticGhost metrics.
#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
pub struct MysticGhostLabels {
    pub result: &'static str, // "success", "fallback", "error"
}

/// MysticGhost mergeset size (number of blocks in mergeset before colouring).
static MYSTICGHOST_MERGESET_SIZE: Lazy<Gauge> = Lazy::new(|| {
    let gauge = Gauge::default();
    let mut registry = REGISTRY.lock().unwrap();
    registry.register(
        "mysticghost_mergeset_size",
        "Number of blocks in the mergeset before GHOSTDAG colouring",
        gauge.clone(),
    );
    gauge
});

/// MysticGhost colouring duration in milliseconds.
static MYSTICGHOST_COLOURING_DURATION_MS: Lazy<Family<MysticGhostLabels, Histogram>> =
    Lazy::new(|| {
        let family = Family::<MysticGhostLabels, Histogram>::new_with_constructor(|| {
            let buckets: Vec<f64> = prometheus_client::metrics::histogram::exponential_buckets(1.0, 2.0, 20).collect();
            Histogram::new(buckets)
        });
        let mut registry = REGISTRY.lock().unwrap();
        registry.register(
            "mysticghost_colouring_duration_ms",
            "Duration of GHOSTDAG colouring in milliseconds",
            family.clone(),
        );
        family
    });

/// Number of DAG blocks currently in memory/storage.
static DAG_BLOCKS_IN_MEMORY: Lazy<Gauge> = Lazy::new(|| {
    let gauge = Gauge::default();
    let mut registry = REGISTRY.lock().unwrap();
    registry.register(
        "dag_blocks_in_memory",
        "Current number of DAG blocks stored",
        gauge.clone(),
    );
    gauge
});

/// Total number of non-blue blocks pruned.
static MYSTICGHOST_PRUNED_BLOCKS_TOTAL: Lazy<Counter> = Lazy::new(|| {
    let counter = Counter::default();
    let mut registry = REGISTRY.lock().unwrap();
    registry.register(
        "mysticghost_pruned_blocks_total",
        "Total number of non-blue blocks pruned after commit",
        counter.clone(),
    );
    counter
});

/// Total number of waves pruned.
static MYSTICGHOST_PRUNED_WAVES_TOTAL: Lazy<Counter> = Lazy::new(|| {
    let counter = Counter::default();
    let mut registry = REGISTRY.lock().unwrap();
    registry.register(
        "mysticghost_pruned_waves_total",
        "Total number of waves pruned (all blocks in old waves)",
        counter.clone(),
    );
    counter
});

/// Record mergeset size.
pub fn record_mergeset_size(size: usize) {
    MYSTICGHOST_MERGESET_SIZE.set(size as i64);
}

/// Record colouring duration.
pub fn record_colouring_duration_ms(duration_ms: f64, result: &'static str) {
    let labels = MysticGhostLabels { result };
    MYSTICGHOST_COLOURING_DURATION_MS
        .get_or_create(&labels)
        .observe(duration_ms);
}

/// Record pruned blocks count.
pub fn record_pruned_blocks(count: u64) {
    MYSTICGHOST_PRUNED_BLOCKS_TOTAL.inc_by(count);
}

/// Record pruned waves count.
pub fn record_pruned_waves(count: u64) {
    MYSTICGHOST_PRUNED_WAVES_TOTAL.inc_by(count);
}

/// Current block height.
static BLOCK_HEIGHT: Lazy<Gauge> = Lazy::new(|| {
    let gauge = Gauge::default();
    let mut registry = REGISTRY.lock().unwrap();
    registry.register(
        "block_height",
        "Current chain block height",
        gauge.clone(),
    );
    gauge
});

/// Number of connected peers.
static PEER_COUNT: Lazy<Gauge> = Lazy::new(|| {
    let gauge = Gauge::default();
    let mut registry = REGISTRY.lock().unwrap();
    registry.register(
        "peer_count",
        "Number of connected P2P peers",
        gauge.clone(),
    );
    gauge
});

/// Size of the mempool (pending transactions).
static MEMPOOL_SIZE: Lazy<Gauge> = Lazy::new(|| {
    let gauge = Gauge::default();
    let mut registry = REGISTRY.lock().unwrap();
    registry.register(
        "mempool_size",
        "Number of pending transactions in mempool",
        gauge.clone(),
    );
    gauge
});

/// Commit latency in milliseconds.
static COMMIT_LATENCY: Lazy<Gauge> = Lazy::new(|| {
    let gauge = Gauge::default();
    let mut registry = REGISTRY.lock().unwrap();
    registry.register(
        "commit_latency",
        "Commit latency in milliseconds (proxy)",
        gauge.clone(),
    );
    gauge
});

/// Mergeset size.
static MERGESET_SIZE: Lazy<Gauge> = Lazy::new(|| {
    let gauge = Gauge::default();
    let mut registry = REGISTRY.lock().unwrap();
    registry.register(
        "mergeset_size",
        "Number of blocks in the mergeset (proxy)",
        gauge.clone(),
    );
    gauge
});

/// RSS memory proxy (MB).
static RSS_PROXY: Lazy<Gauge> = Lazy::new(|| {
    let gauge = Gauge::default();
    let mut registry = REGISTRY.lock().unwrap();
    registry.register(
        "rss_proxy",
        "Approximate RSS memory footprint in MB (proxy)",
        gauge.clone(),
    );
    gauge
});

/// Record block height.
pub fn record_block_height(height: i64) {
    BLOCK_HEIGHT.set(height);
}

/// Record peer count.
pub fn record_peer_count(count: i64) {
    PEER_COUNT.set(count);
}

/// Record mempool size.
pub fn record_mempool_size(size: i64) {
    MEMPOOL_SIZE.set(size);
}

/// Record commit latency (ms).
pub fn record_commit_latency(ms: f64) {
    COMMIT_LATENCY.set(ms as i64);
}

/// Record mergeset size.
pub fn record_mergeset_size_metric(size: usize) {
    MERGESET_SIZE.set(size as i64);
}

/// Record RSS proxy (MB).
pub fn record_rss_proxy(mb: i64) {
    RSS_PROXY.set(mb);
}

/// Encode the metrics registry to the Prometheus text format.
pub fn metrics_text() -> String {
    let mut buffer = String::new();
    let reg = REGISTRY.lock().unwrap();
    let _ = encode(&mut buffer, &*reg);
    buffer
}

/// Encode registry content (without EOF) for streaming responses.
pub fn metrics_text_stream(buffer: &mut String) {
    let reg = REGISTRY.lock().unwrap();
    let _ = encode(buffer, &*reg);
}

/// Update DAG blocks in memory gauge.
pub fn update_dag_blocks_in_memory(count: i64) {
    DAG_BLOCKS_IN_MEMORY.set(count);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_metrics_registration() {
        // Just verify metrics can be registered and used
        record_mergeset_size(100);
        record_colouring_duration_ms(10.5, "success");
        record_colouring_duration_ms(5.2, "fallback");
        record_pruned_blocks(50);
        record_pruned_waves(2);
        update_dag_blocks_in_memory(1000);
    }
}