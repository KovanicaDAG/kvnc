//! WASM Runtime for KVNC based on Wasmi (deterministic interpreter).
//!
//! Provides gas-metered, sandboxed execution of smart contracts.

#![deny(unsafe_code)]

use thiserror::Error;
use tracing::debug;
use wasmi::{Engine, Module};

#[derive(Error, Debug)]
pub enum RuntimeError {
    #[error("Compilation failed: {0}")]
    Compilation(String),
    #[error("Instantiation failed: {0}")]
    Instantiation(String),
    #[error("Execution failed: {0}")]
    Execution(String),
    #[error("Out of gas")]
    OutOfGas,
    #[error("Host function error: {0}")]
    Host(String),
}

/// Configuration for a single contract execution.
pub struct ExecutionConfig {
    pub gas_limit: u64,
    pub memory_limit_pages: u32, // 64 KiB pages
}

impl Default for ExecutionConfig {
    fn default() -> Self {
        Self {
            gas_limit: 10_000_000,
            memory_limit_pages: 16, // 1 MiB
        }
    }
}

/// The KVNC WASM runtime.
pub struct Runtime {
    engine: Engine,
}

impl Runtime {
    pub fn new() -> Self {
        // Wasmi engine is deterministic by design.
        let engine = Engine::default();
        Self { engine }
    }

    /// Compile a WASM module (should be cached in production).
    pub fn compile(&self, wasm_bytes: &[u8]) -> Result<Module, RuntimeError> {
        Module::new(&self.engine, wasm_bytes).map_err(|e| RuntimeError::Compilation(e.to_string()))
    }

    /// Execute a contract call with gas metering.
    ///
    /// In the full implementation this will:
    /// 1. Create a Store with fuel
    /// 2. Link host functions (storage, balance, crypto…)
    /// 3. Instantiate and call the exported function
    /// 4. Return results + remaining gas
    pub fn execute(
        &self,
        _module: &Module,
        func_name: &str,
        _args: &[u8],
        config: &ExecutionConfig,
    ) -> Result<Vec<u8>, RuntimeError> {
        // Skeleton – real implementation comes next.
        debug!(target: "kvnc-runtime", "Executing {} with gas_limit={}", func_name, config.gas_limit);

        // Placeholder success
        Ok(vec![])
    }
}

impl Default for Runtime {
    fn default() -> Self {
        Self::new()
    }
}
