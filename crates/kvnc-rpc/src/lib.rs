//! JSON-RPC server for KVNC nodes.

#![deny(unsafe_code)]

use std::collections::HashMap;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;

use axum::{
    extract::{Extension, Json},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
    Router,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;
use tokio::net::TcpListener;
use tokio::signal;
use tokio::sync::RwLock;
use tracing::{info, warn};

mod rpc_methods;
pub use rpc_methods::*;

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

#[derive(Clone)]
pub struct RpcServer {
    methods: Arc<RwLock<HashMap<String, RpcHandler>>>,
    addr: SocketAddr,
}

impl RpcServer {
    pub async fn new(addr: SocketAddr) -> Self {
        let mut server = Self {
            methods: Arc::new(RwLock::new(HashMap::new())),
            addr,
        };
        server.register_default_methods().await;
        server
    }

    async fn register_default_methods(&mut self) {
        // HTLC methods
        self.register_method("htlc_create", handle_htlc_create)
            .await;
        self.register_method("htlc_claim", handle_htlc_claim).await;
        self.register_method("htlc_refund", handle_htlc_refund)
            .await;

        // Vault methods
        self.register_method("vault_create", handle_vault_create)
            .await;
        self.register_method("vault_claim", handle_vault_claim)
            .await;
        self.register_method("vault_cancel", handle_vault_cancel)
            .await;

        // Multisig methods
        self.register_method("multisig_create", handle_multisig_create)
            .await;
        self.register_method("multisig_propose", handle_multisig_propose)
            .await;
        self.register_method("multisig_confirm", handle_multisig_confirm)
            .await;
        self.register_method("multisig_execute", handle_multisig_execute)
            .await;

        // Token methods
        self.register_method("token_create", handle_token_create)
            .await;
        self.register_method("token_transfer", handle_token_transfer)
            .await;
        self.register_method("token_mint", handle_token_mint).await;
        self.register_method("token_burn", handle_token_burn).await;
        self.register_method("token_balance", handle_token_balance)
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
            .route("/health", get(health_check))
            .layer(Extension(self.methods));

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

async fn health_check() -> &'static str {
    "OK"
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
