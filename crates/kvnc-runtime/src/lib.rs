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

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct TestHost {
        caller: Address,
    }

    impl Host for TestHost {
        fn caller(&self) -> Address {
            self.caller
        }

        fn contract_address(&self) -> Address {
            [0xCD; 32]
        }

        fn block_height(&self) -> u64 {
            42
        }

        fn timestamp(&self) -> u64 {
            1234
        }

        fn balance_of(&self, _addr: &Address) -> u128 {
            0
        }

        fn transfer(
            &mut self,
            _from: &Address,
            _to: &Address,
            _amount: u128,
        ) -> kvnc_common::ContractResult<()> {
            Ok(())
        }

        fn emit_event(&mut self, _topic: &[u8], _data: &[u8]) {}

        fn storage_get(&self, _key: &[u8]) -> Option<Vec<u8>> {
            None
        }

        fn storage_set(&mut self, _key: &[u8], _value: &[u8]) {}
    }

    #[derive(Clone, Copy)]
    struct ModuleOptions {
        memory: bool,
        alloc_export: bool,
        dealloc_export: bool,
        entry_export: bool,
        caller_import: bool,
        initial_pages: u32,
    }

    impl Default for ModuleOptions {
        fn default() -> Self {
            Self {
                memory: true,
                alloc_export: true,
                dealloc_export: true,
                entry_export: true,
                caller_import: false,
                initial_pages: 1,
            }
        }
    }

    /// Build a minimal module using the runtime's `(i32, i32) -> i64` entry
    /// ABI. Defined function indices are alloc, dealloc, and entry, after any
    /// optional imported host function.
    fn test_module(entry_body: &[u8], options: ModuleOptions) -> Vec<u8> {
        let mut wasm = b"\0asm\x01\0\0\0".to_vec();

        // Types: entry, alloc, dealloc, and kvnc_caller.
        let mut types = vec![4];
        types.extend([0x60, 2, 0x7f, 0x7f, 1, 0x7e]);
        types.extend([0x60, 1, 0x7f, 1, 0x7f]);
        types.extend([0x60, 2, 0x7f, 0x7f, 0]);
        types.extend([0x60, 1, 0x7f, 0]);
        append_section(&mut wasm, 1, &types);

        if options.caller_import {
            let mut imports = vec![1];
            append_name(&mut imports, "env");
            append_name(&mut imports, "kvnc_caller");
            imports.extend([0, 3]); // function import, type index 3
            append_section(&mut wasm, 2, &imports);
        }

        append_section(&mut wasm, 3, &[3, 1, 2, 0]);

        if options.memory {
            let mut memory = vec![1, 0]; // one memory, limits with minimum only
            append_u32(&mut memory, options.initial_pages);
            append_section(&mut wasm, 5, &memory);
        }

        let imported_functions = u32::from(options.caller_import);
        let mut exports = Vec::new();
        let mut export_count = 0;
        if options.memory {
            append_name(&mut exports, "memory");
            exports.extend([2, 0]);
            export_count += 1;
        }
        if options.alloc_export {
            append_name(&mut exports, "kvnc_alloc");
            exports.push(0);
            append_u32(&mut exports, imported_functions);
            export_count += 1;
        }
        if options.dealloc_export {
            append_name(&mut exports, "kvnc_dealloc");
            exports.push(0);
            append_u32(&mut exports, imported_functions + 1);
            export_count += 1;
        }
        if options.entry_export {
            append_name(&mut exports, "entry");
            exports.push(0);
            append_u32(&mut exports, imported_functions + 2);
            export_count += 1;
        }
        if export_count > 0 {
            let mut section = Vec::new();
            append_u32(&mut section, export_count);
            section.extend(exports);
            append_section(&mut wasm, 7, &section);
        }

        let mut alloc_body = vec![0, 0x41]; // i32.const 64; end
        append_i32(&mut alloc_body, 64);
        alloc_body.push(0x0b);
        let dealloc_body = [0, 0x0b]; // end
        let mut code = vec![3];
        for body in [&alloc_body[..], &dealloc_body[..], entry_body] {
            append_u32(&mut code, body.len() as u32);
            code.extend(body);
        }
        append_section(&mut wasm, 10, &code);
        wasm
    }

    fn append_section(wasm: &mut Vec<u8>, id: u8, payload: &[u8]) {
        wasm.push(id);
        append_u32(wasm, payload.len() as u32);
        wasm.extend(payload);
    }

    fn append_name(out: &mut Vec<u8>, name: &str) {
        append_u32(out, name.len() as u32);
        out.extend(name.as_bytes());
    }

    fn append_u32(out: &mut Vec<u8>, mut value: u32) {
        loop {
            let mut byte = (value & 0x7f) as u8;
            value >>= 7;
            if value != 0 {
                byte |= 0x80;
            }
            out.push(byte);
            if value == 0 {
                break;
            }
        }
    }

    fn append_i64(out: &mut Vec<u8>, mut value: i64) {
        loop {
            let byte = (value as u8) & 0x7f;
            value >>= 7;
            let done = (value == 0 && byte & 0x40 == 0) || (value == -1 && byte & 0x40 != 0);
            out.push(if done { byte } else { byte | 0x80 });
            if done {
                break;
            }
        }
    }

    fn append_i32(out: &mut Vec<u8>, mut value: i32) {
        loop {
            let byte = (value as u8) & 0x7f;
            value >>= 7;
            let done = (value == 0 && byte & 0x40 == 0) || (value == -1 && byte & 0x40 != 0);
            out.push(if done { byte } else { byte | 0x80 });
            if done {
                break;
            }
        }
    }

    fn execute(
        wasm: &[u8],
        args: &[u8],
        config: &ExecutionConfig,
        host: &mut TestHost,
    ) -> Result<Vec<u8>, RuntimeError> {
        let runtime = Runtime::new();
        let module = runtime.compile(wasm)?;
        runtime.execute(&module, "entry", args, config, host)
    }

    fn generous_config() -> ExecutionConfig {
        ExecutionConfig {
            gas_limit: 100_000,
            memory_limit_pages: 2,
        }
    }

    #[test]
    fn packed_success_returns_guest_bytes_and_receives_argument_bytes() {
        // Return `(args_ptr << 32) | args_len` from the guest.
        let body = [0, 0x20, 0, 0xad, 0x42, 32, 0x86, 0x20, 1, 0xad, 0x84, 0x0b];
        let wasm = test_module(&body, ModuleOptions::default());
        let args = b"argument bytes are copied into guest memory";
        let result = execute(&wasm, args, &generous_config(), &mut TestHost::default())
            .expect("guest should return its argument buffer");
        assert_eq!(result, args);
    }

    #[test]
    fn packed_contract_error_codes_are_decoded() {
        for ret in (-12..=-1).rev() {
            let mut body = vec![0, 0x42]; // no locals; i64.const
            append_i64(&mut body, ret);
            body.push(0x0b);
            let wasm = test_module(&body, ModuleOptions::default());
            let error = execute(&wasm, &[], &generous_config(), &mut TestHost::default())
                .expect_err("negative ABI error return must reject the call");
            assert!(
                matches!(error, RuntimeError::Contract(code) if code == (-1 - ret) as i32),
                "return {ret} decoded incorrectly: {error:?}"
            );
        }
    }

    #[test]
    fn low_gas_rejects_an_infinite_guest_loop() {
        let body = [0, 0x03, 0x40, 0x0c, 0, 0x0b, 0x42, 0, 0x0b];
        let wasm = test_module(&body, ModuleOptions::default());
        let config = ExecutionConfig {
            gas_limit: 100,
            memory_limit_pages: 1,
        };
        let error = execute(&wasm, &[], &config, &mut TestHost::default())
            .expect_err("the loop must exhaust its fuel");
        assert!(matches!(error, RuntimeError::OutOfGas), "{error:?}");
    }

    #[test]
    fn required_exports_are_reported_as_missing() {
        let body = [0, 0x42, 0, 0x0b];
        let options = ModuleOptions {
            entry_export: false,
            ..ModuleOptions::default()
        };
        let wasm = test_module(&body, options);
        let error = execute(&wasm, &[], &generous_config(), &mut TestHost::default())
            .expect_err("missing entry point must be rejected");
        assert!(matches!(error, RuntimeError::MissingExport(name) if name == "entry"));

        let options = ModuleOptions {
            memory: false,
            ..ModuleOptions::default()
        };
        let wasm = test_module(&body, options);
        let error = execute(&wasm, &[], &generous_config(), &mut TestHost::default())
            .expect_err("missing memory must be rejected");
        assert!(matches!(error, RuntimeError::MissingExport(name) if name == "memory"));

        let options = ModuleOptions {
            alloc_export: false,
            ..ModuleOptions::default()
        };
        let wasm = test_module(&body, options);
        let error = execute(
            &wasm,
            b"non-empty",
            &generous_config(),
            &mut TestHost::default(),
        )
        .expect_err("missing allocator must be rejected for non-empty arguments");
        assert!(matches!(error, RuntimeError::MissingExport(name) if name == "kvnc_alloc"));
    }

    #[test]
    fn guest_can_call_host_caller_import() {
        let mut body = vec![0, 0x41];
        append_i32(&mut body, 128);
        body.extend([0x10, 0]); // caller(128)
        body.push(0x42);
        append_i64(&mut body, (128_i64 << 32) | 32);
        body.push(0x0b);
        let options = ModuleOptions {
            caller_import: true,
            ..ModuleOptions::default()
        };
        let wasm = test_module(&body, options);
        let mut host = TestHost { caller: [0xAB; 32] };
        let result = execute(&wasm, &[], &generous_config(), &mut host)
            .expect("kvnc_caller should be linked and callable");
        assert_eq!(result, [0xAB; 32]);
    }

    #[test]
    fn memory_grow_is_limited_by_configured_page_cap() {
        // Request one additional page from a module that starts with one page;
        // the store is configured with a one-page maximum.
        let body = [0, 0x41, 1, 0x40, 0, 0x1a, 0x42, 0, 0x0b];
        let wasm = test_module(&body, ModuleOptions::default());
        let config = ExecutionConfig {
            gas_limit: 10_000,
            memory_limit_pages: 1,
        };
        let error = execute(&wasm, &[], &config, &mut TestHost::default())
            .expect_err("memory.grow past the configured page cap must trap");
        assert!(
            matches!(error, RuntimeError::Execution(_)),
            "memory limiter should reject growth as an execution trap: {error:?}"
        );
    }
}
