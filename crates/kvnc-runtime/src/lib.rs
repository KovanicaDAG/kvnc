//! WASM Runtime for KVNC based on Wasmi (deterministic interpreter).
//!
//! Provides gas-metered, sandboxed execution of smart contracts.
//!
//! ## Execution model (Lane 3b)
//!
//! [`Runtime::execute`] instantiates a compiled [`Module`] against a [`Linker`]
//! that defines every `env`-module import of the FIXED kvnc contract ABI
//! (see `kvnc-common/src/wasm.rs` and `docs/CONTRACTS.md`), places the bincode
//! argument buffer into guest memory through the module's `kvnc_alloc` export,
//! calls the requested entry point and decodes its packed `i64` return value:
//!
//! - error iff `-12 <= ret <= -1` → [`RuntimeError::Contract`] with
//!   `code = -(1 + ret)` (`ContractError::code()`, 0..=11);
//! - otherwise success: `out_ptr = (ret >> 32) as u32`,
//!   `out_len = (ret as u32)`; `out_len == 0` means a null/empty result.
//!
//! The host reads the result buffer out of guest memory and releases both the
//! result buffer and its own argument buffer through the guest's
//! `kvnc_dealloc` export. All `env` imports operate on the caller-supplied
//! [`kvnc_common::Host`] trait object — the runtime itself holds no ledger
//! state and can therefore be reused by any host implementation.

#![deny(unsafe_code)]

use kvnc_common::{Address, Host};
use thiserror::Error;
use tracing::debug;
use wasmi::{
    core::TrapCode,
    errors::{ErrorKind, FuelError},
    Caller, Engine, Extern, Instance, Linker, Memory, Store, StoreLimits, StoreLimitsBuilder,
};

/// Re-export of the wasmi module type so downstream crates (e.g.
/// `kvnc-execution`'s module cache) do not need a direct `wasmi` dependency.
pub use wasmi::Module;

/// WebAssembly page size in bytes (fixed by the spec).
const WASM_PAGE_BYTES: usize = 64 * 1024;

/// Upper bound for argument/result buffers we copy through guest memory.
/// Keeps a corrupt packed return value from triggering a huge host-side
/// allocation (guest memory itself is capped by `memory_limit_pages`).
const MAX_BUFFER_BYTES: usize = 64 * 1024 * 1024;

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
    /// The contract rejected the call. `code` is the packed ABI code decoded
    /// from the entry point's negative return value: `ContractError::code()`
    /// (0..=11, see `kvnc-common`).
    #[error("Contract rejected the call (code {0})")]
    Contract(i32),
    /// A required export (`memory`, `kvnc_alloc`, `kvnc_dealloc` or the
    /// requested entry point) is missing from the module.
    #[error("Missing module export: {0}")]
    MissingExport(String),
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

/// Per-execution store state: the caller's [`Host`] plus the store-level
/// resource limiter (which enforces `ExecutionConfig::memory_limit_pages`).
struct ExecState<'a> {
    host: &'a mut dyn Host,
    limits: StoreLimits,
}

/// The KVNC WASM runtime.
pub struct Runtime {
    engine: Engine,
}

impl Runtime {
    pub fn new() -> Self {
        // Wasmi engine is deterministic by design. Fuel metering must be
        // enabled at engine level or `Store::set_fuel` refuses to run.
        let mut config = wasmi::Config::default();
        config.consume_fuel(true);
        let engine = Engine::new(&config);
        Self { engine }
    }

    /// Compile a WASM module (should be cached in production).
    pub fn compile(&self, wasm_bytes: &[u8]) -> Result<Module, RuntimeError> {
        Module::new(&self.engine, wasm_bytes).map_err(|e| RuntimeError::Compilation(e.to_string()))
    }

    /// Execute a contract call with gas metering.
    ///
    /// Steps:
    /// 1. Create a [`Store`] with `config.gas_limit` fuel and a resource
    ///    limiter enforcing `config.memory_limit_pages`.
    /// 2. Link the `env` host functions (ABI-fixed, see module docs) over
    ///    `host`.
    /// 3. Instantiate the module and place `args` into guest memory via the
    ///    guest's `kvnc_alloc` export.
    /// 4. Call `func_name(args_ptr, args_len)`, decode the packed `i64`
    ///    result, free both guest buffers and return the raw result bytes.
    ///
    /// Contract-level failures surface as [`RuntimeError::Contract`]; out of
    /// fuel as [`RuntimeError::OutOfGas`]; missing exports as
    /// [`RuntimeError::MissingExport`].
    pub fn execute(
        &self,
        module: &Module,
        func_name: &str,
        args: &[u8],
        config: &ExecutionConfig,
        host: &mut dyn Host,
    ) -> Result<Vec<u8>, RuntimeError> {
        debug!(
            target: "kvnc-runtime",
            "Executing {} ({} arg bytes) with gas_limit={}",
            func_name,
            args.len(),
            config.gas_limit
        );
        if args.len() > MAX_BUFFER_BYTES {
            return Err(RuntimeError::Execution(
                "argument buffer exceeds the maximum copy size".to_string(),
            ));
        }

        let limits = StoreLimitsBuilder::new()
            .memory_size(config.memory_limit_pages as usize * WASM_PAGE_BYTES)
            .trap_on_grow_failure(true)
            .build();
        let mut store = Store::new(&self.engine, ExecState { host, limits });
        // Enforces `memory_limit_pages` on memory creation and `memory.grow`.
        store.limiter(|state| &mut state.limits);
        store
            .set_fuel(config.gas_limit)
            .map_err(|e| RuntimeError::Execution(format!("fuel metering unavailable: {e}")))?;

        let linker = build_linker(&self.engine)?;
        let instance = linker
            .instantiate(&mut store, module)
            .and_then(|pre| pre.start(&mut store))
            .map_err(|e| {
                if is_out_of_fuel(&e) {
                    RuntimeError::OutOfGas
                } else {
                    RuntimeError::Instantiation(e.to_string())
                }
            })?;

        let memory = instance
            .get_export(&store, "memory")
            .and_then(Extern::into_memory)
            .ok_or_else(|| RuntimeError::MissingExport("memory".to_string()))?;

        // Copy the bincode argument buffer into guest memory.
        let (args_ptr, args_len) = place_args(&mut store, &instance, &memory, args)?;

        let entry = instance
            .get_typed_func::<(i32, i32), i64>(&store, func_name)
            .map_err(|_| RuntimeError::MissingExport(func_name.to_string()))?;

        let ret = match entry.call(&mut store, (args_ptr, args_len)) {
            Ok(ret) => ret,
            Err(e) => {
                // Best-effort cleanup; the Store (and with it every guest
                // allocation) is discarded right after this call anyway.
                dealloc(&mut store, &instance, args_ptr, args_len);
                return Err(map_call_error(e));
            }
        };

        // Packed return decode — see the module docs for the FIXED rule.
        if (-12..=-1).contains(&ret) {
            dealloc(&mut store, &instance, args_ptr, args_len);
            return Err(RuntimeError::Contract((-1 - ret) as i32));
        }

        let out_ptr = ((ret >> 32) as u32) as usize;
        let out_len = (ret as u32) as usize;
        if out_len == 0 {
            // Null/empty result (e.g. `()` results) — nothing to read/free.
            dealloc(&mut store, &instance, args_ptr, args_len);
            return Ok(Vec::new());
        }
        if out_len > MAX_BUFFER_BYTES || out_ptr.saturating_add(out_len) > memory.data_size(&store)
        {
            dealloc(&mut store, &instance, args_ptr, args_len);
            return Err(RuntimeError::Execution(format!(
                "packed result out of bounds (ptr={out_ptr}, len={out_len})"
            )));
        }
        let mut out = vec![0u8; out_len];
        memory
            .read(&store, out_ptr, &mut out)
            .map_err(|e| RuntimeError::Execution(format!("result read out of bounds: {e}")))?;
        dealloc(&mut store, &instance, out_ptr as i32, out_len as i32);
        dealloc(&mut store, &instance, args_ptr, args_len);

        if let Ok(remaining) = store.get_fuel() {
            debug!(
                target: "kvnc-runtime",
                "contract call finished, {} fuel consumed",
                config.gas_limit.saturating_sub(remaining)
            );
        }
        Ok(out)
    }
}

impl Default for Runtime {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Guest-memory helpers (used by the `env` host functions)
// ---------------------------------------------------------------------------

/// The ABI requires every contract module to export its linear memory.
fn guest_memory(caller: &Caller<'_, ExecState<'_>>) -> Result<Memory, wasmi::Error> {
    caller
        .get_export("memory")
        .and_then(Extern::into_memory)
        .ok_or_else(|| wasmi::Error::new("module does not export `memory`"))
}

/// Bounds-checked read of `len` bytes at `ptr` from the caller's memory.
fn read_guest(
    caller: &Caller<'_, ExecState<'_>>,
    ptr: i32,
    len: i32,
) -> Result<Vec<u8>, wasmi::Error> {
    if len < 0 || ptr < 0 {
        return Err(wasmi::Error::new("negative guest memory offset"));
    }
    if len == 0 {
        return Ok(Vec::new());
    }
    let memory = guest_memory(caller)?;
    // Check against total memory size *before* allocating — `ptr`/`len` are
    // guest-controlled.
    if len as usize > memory.data_size(caller) {
        return Err(wasmi::Error::new(
            "guest memory slice exceeds linear memory size",
        ));
    }
    let mut buf = vec![0u8; len as usize];
    memory
        .read(caller, ptr as usize, &mut buf)
        .map_err(|e| wasmi::Error::new(format!("guest memory read out of bounds: {e}")))?;
    Ok(buf)
}

/// Bounds-checked write of `bytes` at `ptr` into the caller's memory.
fn write_guest(
    caller: &mut Caller<'_, ExecState<'_>>,
    ptr: i32,
    bytes: &[u8],
) -> Result<(), wasmi::Error> {
    if ptr < 0 {
        return Err(wasmi::Error::new("negative guest memory offset"));
    }
    let memory = guest_memory(caller)?;
    memory
        .write(caller, ptr as usize, bytes)
        .map_err(|e| wasmi::Error::new(format!("guest memory write out of bounds: {e}")))?;
    Ok(())
}

/// Read a fixed 32-byte address argument (ABI: every address slot is 32B).
fn read_address(
    caller: &Caller<'_, ExecState<'_>>,
    ptr: i32,
    len: i32,
) -> Result<Address, wasmi::Error> {
    if len as usize != core::mem::size_of::<Address>() {
        return Err(wasmi::Error::new("expected a 32-byte address argument"));
    }
    let bytes = read_guest(caller, ptr, len)?;
    <Address>::try_from(bytes.as_slice())
        .map_err(|_| wasmi::Error::new("expected a 32-byte address argument"))
}

/// Pack a [`kvnc_common::ContractError`] into the host-function error slot.
/// The guest maps any non-zero rc to `Custom(rc.unsigned_abs())`; the
/// `-1 - code` shape mirrors the entry-point packing rule.
fn pack_contract_error(e: kvnc_common::ContractError) -> i32 {
    -1 - e.code()
}

// ---------------------------------------------------------------------------
// Buffer placement / release
// ---------------------------------------------------------------------------

/// Place `args` into guest memory via the guest's `kvnc_alloc` export.
fn place_args(
    store: &mut Store<ExecState<'_>>,
    instance: &Instance,
    memory: &Memory,
    args: &[u8],
) -> Result<(i32, i32), RuntimeError> {
    if args.is_empty() {
        // TODO: non-contract path — a module without `kvnc_alloc` can still
        // be invoked with an empty argument buffer (ptr=0, len=0). Modules
        // that need arguments must export the fixed `kvnc_alloc`/`kvnc_dealloc`
        // pair; the ABI defines no other host→guest copy mechanism.
        return Ok((0, 0));
    }
    if args.len() > memory.data_size(&*store) {
        return Err(RuntimeError::Execution(
            "argument buffer larger than guest memory".to_string(),
        ));
    }
    let alloc = instance
        .get_typed_func::<(i32,), i32>(&*store, "kvnc_alloc")
        .map_err(|_| RuntimeError::MissingExport("kvnc_alloc".to_string()))?;
    let ptr = alloc
        .call(&mut *store, (args.len() as i32,))
        .map_err(map_call_error)?;
    if ptr <= 0 {
        return Err(RuntimeError::Execution(
            "kvnc_alloc returned a null pointer".to_string(),
        ));
    }
    memory
        .write(store, ptr as usize, args)
        .map_err(|e| RuntimeError::Execution(format!("argument write out of bounds: {e}")))?;
    Ok((ptr, args.len() as i32))
}

/// Best-effort release of a guest buffer via `kvnc_dealloc`.
///
/// Failures (missing export, exhausted fuel after a trap) are ignored: the
/// whole Store — and with it every guest allocation — is dropped as soon as
/// `execute` returns.
fn dealloc(store: &mut Store<ExecState<'_>>, instance: &Instance, ptr: i32, len: i32) {
    if ptr <= 0 || len <= 0 {
        return;
    }
    if let Ok(func) = instance.get_typed_func::<(i32, i32), ()>(&*store, "kvnc_dealloc") {
        let _ = func.call(store, (ptr, len));
    }
}

// ---------------------------------------------------------------------------
// Error mapping
// ---------------------------------------------------------------------------

fn is_out_of_fuel(e: &wasmi::Error) -> bool {
    if e.as_trap_code() == Some(TrapCode::OutOfFuel) {
        return true;
    }
    matches!(e.kind(), ErrorKind::Fuel(FuelError::OutOfFuel))
}

fn map_call_error(e: wasmi::Error) -> RuntimeError {
    if is_out_of_fuel(&e) {
        RuntimeError::OutOfGas
    } else {
        RuntimeError::Execution(e.to_string())
    }
}

// ---------------------------------------------------------------------------
// The `env` linker (FIXED ABI — see kvnc-common/src/wasm.rs)
// ---------------------------------------------------------------------------

fn build_linker<'a>(engine: &Engine) -> Result<Linker<ExecState<'a>>, RuntimeError> {
    let mut linker: Linker<ExecState<'a>> = Linker::new(engine);

    // All definition names are distinct, so the linker cannot fail; the
    // Result is still surfaced rather than unwrapped.
    macro_rules! define {
        ($name:literal, $func:expr) => {
            linker.func_wrap("env", $name, $func).map_err(|e| {
                RuntimeError::Execution(format!("failed to define env/{}: {}", $name, e))
            })?;
        };
    }

    // kvnc_caller(out_ptr: i32) — writes 32 bytes.
    define!("kvnc_caller", |mut caller: Caller<'_, ExecState<'_>>,
                            out_ptr: i32|
     -> Result<(), wasmi::Error> {
        let addr = caller.data().host.caller();
        write_guest(&mut caller, out_ptr, &addr)
    });

    // kvnc_contract_address(out_ptr: i32) — writes 32 bytes.
    define!("kvnc_contract_address", |mut caller: Caller<
        '_,
        ExecState<'_>,
    >,
                                      out_ptr: i32|
     -> Result<(), wasmi::Error> {
        let addr = caller.data().host.contract_address();
        write_guest(&mut caller, out_ptr, &addr)
    });

    // kvnc_block_height() -> i64
    define!(
        "kvnc_block_height",
        |caller: Caller<'_, ExecState<'_>>| -> i64 {
            // Values that do not fit the i64 host slot report -1; the guest
            // treats negatives as 0 (see kvnc-common/src/wasm.rs notes).
            i64::try_from(caller.data().host.block_height()).unwrap_or(-1)
        }
    );

    // kvnc_timestamp() -> i64
    define!(
        "kvnc_timestamp",
        |caller: Caller<'_, ExecState<'_>>| -> i64 {
            i64::try_from(caller.data().host.timestamp()).unwrap_or(-1)
        }
    );

    // kvnc_balance_of(ptr: i32, len: i32) -> i64 — u64 balance as i64, -1 = error.
    define!("kvnc_balance_of", |caller: Caller<'_, ExecState<'_>>,
                                ptr: i32,
                                len: i32|
     -> Result<i64, wasmi::Error> {
        let addr = read_address(&caller, ptr, len)?;
        let balance = caller.data().host.balance_of(&addr);
        // ABI slot is i64: balances >= 2^63 read as -1 → guest sees 0.
        // (The native path carries the full u128 range.)
        Ok(i64::try_from(balance).unwrap_or(-1))
    });

    // kvnc_transfer(fp, fl, tp, tl: i32, amount: i64) -> i32 — 0 = ok.
    define!("kvnc_transfer", |mut caller: Caller<'_, ExecState<'_>>,
                              fp: i32,
                              fl: i32,
                              tp: i32,
                              tl: i32,
                              amount: i64|
     -> Result<i32, wasmi::Error> {
        if amount < 0 {
            return Ok(pack_contract_error(kvnc_common::ContractError::Overflow));
        }
        let from = read_address(&caller, fp, fl)?;
        let to = read_address(&caller, tp, tl)?;
        match caller.data_mut().host.transfer(&from, &to, amount as u128) {
            Ok(()) => Ok(0),
            Err(e) => Ok(pack_contract_error(e)),
        }
    });

    // kvnc_storage_get(kp, kl, out_ptr, out_cap: i32) -> i32
    // Two-step protocol: out_cap = 0 → needed len (or -1 when absent);
    // a too-small out_cap → -2.
    define!("kvnc_storage_get", |mut caller: Caller<
        '_,
        ExecState<'_>,
    >,
                                 kp: i32,
                                 kl: i32,
                                 out_ptr: i32,
                                 out_cap: i32|
     -> Result<i32, wasmi::Error> {
        let key = read_guest(&caller, kp, kl)?;
        let Some(value) = caller.data().host.storage_get(&key) else {
            return Ok(-1);
        };
        if out_cap == 0 {
            // Note: a stored zero-length value also reports 0 here and is
            // therefore indistinguishable from "absent"; kvnc contracts
            // never persist empty values.
            return Ok(i32::try_from(value.len()).unwrap_or(-1));
        }
        if out_cap < 0 || value.len() > out_cap as usize {
            return Ok(-2);
        }
        write_guest(&mut caller, out_ptr, &value)?;
        Ok(value.len() as i32)
    });

    // kvnc_storage_set(kp, kl, vp, vl: i32) -> i32 — 0 = ok.
    // `Host::storage_set` has no error channel (overlay append is
    // infallible); only guest-memory faults can fail the call.
    define!("kvnc_storage_set", |mut caller: Caller<
        '_,
        ExecState<'_>,
    >,
                                 kp: i32,
                                 kl: i32,
                                 vp: i32,
                                 vl: i32|
     -> Result<i32, wasmi::Error> {
        let key = read_guest(&caller, kp, kl)?;
        let value = read_guest(&caller, vp, vl)?;
        caller.data_mut().host.storage_set(&key, &value);
        Ok(0)
    });

    // kvnc_emit_event(tp, tl, dp, dl: i32)
    define!("kvnc_emit_event", |mut caller: Caller<
        '_,
        ExecState<'_>,
    >,
                                tp: i32,
                                tl: i32,
                                dp: i32,
                                dl: i32|
     -> Result<(), wasmi::Error> {
        let topic = read_guest(&caller, tp, tl)?;
        let data = read_guest(&caller, dp, dl)?;
        caller.data_mut().host.emit_event(&topic, &data);
        Ok(())
    });

    Ok(linker)
}
