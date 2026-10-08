//! JSON-RPC server for KVNC nodes.

#![deny(unsafe_code)]

use std::collections::HashMap;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

use axum::{
    extract::{
        ws::WebSocketUpgrade,
        Extension, Json,
    },
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
    Router,
};
use kvnc_consensus::CommitteeInfo;
use kvnc_mempool::Mempool;
use kvnc_staking::StakingState;
use kvnc_storage::Storage;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;
use tokio::net::TcpListener;
use tokio::signal;
use tokio::sync::RwLock;
use tracing::{info, warn};

mod chain_methods;
mod rpc_methods;
mod subscriptions;
pub use chain_methods::*;
pub use rpc_methods::*;
pub use subscriptions::{EventBus, SubscriptionKind};

#[derive(Error, Debug)]
pub enum RpcError {
    #[error("Parse error: {0}")]
    ParseError(String),
    #[error("Invalid request: {0}")]
    InvalidRequest(String),
    #[error("Method not found: {0}")]
    MethodNotFound(String),
    #[error("Invalid params: {0}")]
    InvalidParams(String),
    #[error("Internal error: {0}")]
    InternalError(String),
    #[error("Execution error: {0}")]
    ExecutionError(String),
}

impl RpcError {
    fn code(&self) -> i32 {
        match self {
            RpcError::ParseError(_) => -32700,
            RpcError::InvalidRequest(_) => -32600,
            RpcError::MethodNotFound(_) => -32601,
            RpcError::InvalidParams(_) => -32602,
            RpcError::InternalError(_) => -32603,
            RpcError::ExecutionError(_) => -32000,
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct JsonRpcRequest {
    pub jsonrpc: String,
    pub method: String,
    pub params: Option<Value>,
    pub id: Option<Value>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct JsonRpcResponse {
    pub jsonrpc: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcErrorObject>,
    pub id: Option<Value>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct JsonRpcErrorObject {
    pub code: i32,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

type RpcHandler = Arc<
    dyn Fn(Value) -> Pin<Box<dyn std::future::Future<Output = Result<Value, RpcError>> + Send>>
        + Send
        + Sync,
>;

/// Shared state for RPC handlers providing access to node internals.
#[derive(Clone)]
pub struct RpcState {
    pub storage: Arc<Storage>,
    pub mempool: Arc<Mempool>,
    pub staking: Arc<RwLock<StakingState>>,
    pub committee: CommitteeInfo,
    /// Shared distinct connected-peer count maintained by the network service.
    pub peer_count: Arc<AtomicUsize>,
    /// Fan-out bus for WebSocket subscription events.
    pub events: EventBus,
}

#[derive(Clone)]
pub struct RpcServer {
    methods: Arc<RwLock<HashMap<String, RpcHandler>>>,
    addr: SocketAddr,
    state: RpcState,
}

impl RpcServer {
    pub async fn new(addr: SocketAddr, state: RpcState) -> Self {
        let mut server = Self {
            methods: Arc::new(RwLock::new(HashMap::new())),
            addr,
            state,
        };
        server.register_default_methods().await;
        server
    }

    async fn register_default_methods(&mut self) {
        // Built-in contract methods (separate namespace).
        self.register_stateful("htlc_create", handle_htlc_create)
            .await;
        self.register_stateful("htlc_claim", handle_htlc_claim)
            .await;
        self.register_stateful("htlc_refund", handle_htlc_refund)
            .await;

        self.register_stateful("vault_create", handle_vault_create)
            .await;
        self.register_stateful("vault_claim", handle_vault_claim)
            .await;
        self.register_stateful("vault_cancel", handle_vault_cancel)
            .await;

        self.register_stateful("multisig_create", handle_multisig_create)
            .await;
        self.register_stateful("multisig_propose", handle_multisig_propose)
            .await;
        self.register_stateful("multisig_confirm", handle_multisig_confirm)
            .await;
        self.register_stateful("multisig_execute", handle_multisig_execute)
            .await;

        self.register_stateful("token_create", handle_token_create)
            .await;
        self.register_stateful("token_transfer", handle_token_transfer)
            .await;
        self.register_stateful("token_mint", handle_token_mint)
            .await;
        self.register_stateful("token_burn", handle_token_burn)
            .await;
        self.register_stateful("token_balance", handle_token_balance)
            .await;

        // Chain methods.
        self.register_stateful("kvnc_blockNumber", handle_block_number)
            .await;
        self.register_stateful("kvnc_getBlockByHash", handle_get_block_by_hash)
            .await;
        self.register_stateful("kvnc_getBlockByNumber", handle_get_block_by_number)
            .await;

        // Transaction methods.
        self.register_stateful("kvnc_sendRawTransaction", handle_send_raw_transaction)
            .await;
        self.register_stateful("kvnc_getTransactionReceipt", handle_get_transaction_receipt)
            .await;
        self.register_stateful("kvnc_getTransactionByHash", handle_get_transaction_by_hash)
            .await;

        // Account methods.
        self.register_stateful("kvnc_getBalance", handle_get_balance)
            .await;
        self.register_stateful("kvnc_getNonce", handle_get_nonce)
            .await;
        self.register_stateful("kvnc_getCode", handle_get_code)
            .await;
        self.register_stateful("kvnc_getStorageAt", handle_get_storage_at)
            .await;

        // Staking methods.
        self.register_stateful("kvnc_getValidators", handle_get_validators)
            .await;
        self.register_stateful("kvnc_getStake", handle_get_stake)
            .await;
        self.register_stateful("kvnc_getRewards", handle_get_rewards)
            .await;

        // Mempool methods.
        self.register_stateful(
            "kvnc_getPendingTransactions",
            handle_get_pending_transactions,
        )
        .await;
        self.register_stateful("kvnc_estimateFee", handle_estimate_fee)
            .await;

        // Consensus methods.
        self.register_stateful("kvnc_getLeaderSchedule", handle_get_leader_schedule)
            .await;
        self.register_stateful("kvnc_getCommittee", handle_get_committee)
            .await;
    }

    /// Register a handler that needs access to the shared [`RpcState`].
    async fn register_stateful<F, Fut>(&mut self, name: &str, handler: F)
    where
        F: Fn(Value, RpcState) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = Result<Value, RpcError>> + Send + 'static,
    {
        let state = self.state.clone();
        self.register_method(name, move |params| {
            let state = state.clone();
            handler(params, state)
        })
        .await;
    }

    pub async fn register_method<F, Fut>(&mut self, name: &str, handler: F)
    where
        F: Fn(Value) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = Result<Value, RpcError>> + Send + 'static,
    {
        self.methods.write().await.insert(
            name.to_string(),
            Arc::new(move |params: Value| Box::pin(handler(params))),
        );
    }

    pub async fn start(self) -> Result<tokio::task::JoinHandle<()>, std::io::Error> {
        let app = Router::new()
            .route("/rpc", post(rpc_handler))
            .route("/ws", get(ws_handler))
            .route("/health", get(health_check))
            .layer(Extension(self.methods))
            .layer(Extension(self.state.peer_count.clone()))
            .layer(Extension(self.state.clone()));

        let listener = TcpListener::bind(self.addr).await?;
        let actual_addr = listener.local_addr()?;
        info!("JSON-RPC server listening on http://{}", actual_addr);

        let handle = tokio::spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(shutdown_signal())
                .await
                .unwrap();
        });

        Ok(handle)
    }

    pub fn addr(&self) -> SocketAddr {
        self.addr
    }
}

/// Upgrades an HTTP request to a WebSocket and drives the subscription loop.
async fn ws_handler(
    ws: WebSocketUpgrade,
    Extension(state): Extension<RpcState>,
) -> Response {
    ws.on_upgrade(move |socket| subscriptions::serve_connection(socket, state))
}

async fn rpc_handler(
    Extension(methods): Extension<Arc<RwLock<HashMap<String, RpcHandler>>>>,
    Json(request): Json<JsonRpcRequest>,
) -> Response {
    let id = request.id.clone();

    // Validate JSON-RPC version
    if request.jsonrpc != "2.0" {
        return build_error_response(
            id,
            RpcError::InvalidRequest("Invalid JSON-RPC version".into()),
        );
    }

    // Get the method handler
    let handler = {
        let methods = methods.read().await;
        methods.get(&request.method).cloned()
    };

    let handler = match handler {
        Some(h) => h,
        None => {
            return build_error_response(id, RpcError::MethodNotFound(request.method));
        }
    };

    // Execute the handler
    let params = request.params.unwrap_or(Value::Null);
    match handler(params).await {
        Ok(result) => build_success_response(id, result),
        Err(e) => build_error_response(id, e),
    }
}

#[derive(Debug, Serialize)]
struct HealthResponse {
    status: &'static str,
    peer_count: usize,
}

async fn health_check(Extension(peer_count): Extension<Arc<AtomicUsize>>) -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "ok",
        peer_count: peer_count.load(Ordering::Relaxed),
    })
}

fn build_success_response(id: Option<Value>, result: Value) -> Response {
    let response = JsonRpcResponse {
        jsonrpc: "2.0".to_string(),
        result: Some(result),
        error: None,
        id,
    };
    (StatusCode::OK, Json(response)).into_response()
}

fn build_error_response(id: Option<Value>, error: RpcError) -> Response {
    let response = JsonRpcResponse {
        jsonrpc: "2.0".to_string(),
        result: None,
        error: Some(JsonRpcErrorObject {
            code: error.code(),
            message: error.to_string(),
            data: None,
        }),
        id,
    };
    (StatusCode::OK, Json(response)).into_response()
}

async fn shutdown_signal() {
    let ctrl_c = async {
        signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        signal::unix::signal(signal::unix::SignalKind::terminate())
            .expect("failed to install signal handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
    info!("Shutdown signal received, stopping server...");
}

// Placeholder for contract execution — in production this would connect to a real node
// TODO: Wire up real ContractHost backed by node's Storage when integrated with kvnc-node
async fn execute_contract_call(
    entry: &str,
    contract: [u8; 32],
    caller: [u8; 32],
    height: u64,
    timestamp: u64,
    args: &[u8],
) -> Result<Vec<u8>, RpcError> {
    // TODO: This is a STUB. In production:
    // 1. Connect to kvnc-node's storage (via internal gRPC or shared library)
    // 2. Create a ContractHost backed by that Storage
    // 3. Call kvnc_execution::execute_contract_call(entry, contract, caller, height, timestamp, args, storage)
    // 4. Return the bincode-encoded result

    warn!(
        "STUB execute_contract_call: entry={}, contract={:?}, caller={:?}, height={}, timestamp={}, args_len={}",
        entry, contract, caller, height, timestamp, args.len()
    );

    // Return a placeholder success for now — real implementation needs node integration
    bincode::serialize(&()).map_err(|e| RpcError::InternalError(format!("encode failed: {}", e)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn health_is_ok_with_zero_peers() {
        let peer_count = Arc::new(AtomicUsize::new(0));
        let Json(response) = health_check(Extension(peer_count)).await;
        let response = serde_json::to_value(response).expect("health response serializes");

        assert_eq!(response["status"], "ok");
        assert_eq!(response["peer_count"], 0);
    }

    #[tokio::test]
    async fn health_reports_nonzero_peer_count() {
        let peer_count = Arc::new(AtomicUsize::new(3));
        let Json(response) = health_check(Extension(peer_count)).await;
        let response = serde_json::to_value(response).expect("health response serializes");

        assert_eq!(response["status"], "ok");
        assert_eq!(response["peer_count"], 3);
    }
}
