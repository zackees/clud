use std::fmt;
use std::path::Path;

use wasmi::{Caller, Engine, Extern, Linker, Module, Store};

#[derive(Default)]
struct HostState {
    stdout: Vec<u8>,
}

#[derive(Debug)]
pub struct WasmRuntimeError(String);

impl fmt::Display for WasmRuntimeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for WasmRuntimeError {}

impl From<std::io::Error> for WasmRuntimeError {
    fn from(value: std::io::Error) -> Self {
        Self(value.to_string())
    }
}

impl From<wasmi::Error> for WasmRuntimeError {
    fn from(value: wasmi::Error) -> Self {
        Self(value.to_string())
    }
}

pub fn run_file(module_path: &str, invoke: &str) -> Result<i32, WasmRuntimeError> {
    let wasm = std::fs::read(module_path)?;
    run_bytes(module_path, &wasm, invoke)
}

fn run_bytes(module_path: &str, wasm: &[u8], invoke: &str) -> Result<i32, WasmRuntimeError> {
    let engine = Engine::default();
    let module = Module::new(&engine, wasm)
        .map_err(|error| WasmRuntimeError(format!("failed to load '{module_path}': {error}")))?;
    let mut store = Store::new(&engine, HostState::default());
    let mut linker = Linker::new(&engine);

    linker
        .func_wrap(
            "host",
            "log",
            |mut caller: Caller<'_, HostState>, ptr: i32, len: i32| -> Result<(), wasmi::Error> {
                let memory = caller
                    .get_export("memory")
                    .and_then(Extern::into_memory)
                    .ok_or_else(|| wasmi::Error::new("guest did not export memory"))?;
                let start = usize::try_from(ptr)
                    .map_err(|_| wasmi::Error::new("host.log received a negative pointer"))?;
                let byte_len = usize::try_from(len)
                    .map_err(|_| wasmi::Error::new("host.log received a negative length"))?;
                let end = start
                    .checked_add(byte_len)
                    .ok_or_else(|| wasmi::Error::new("host.log pointer overflow"))?;
                let bytes = {
                    let data = memory.data(&caller);
                    let range = data
                        .get(start..end)
                        .ok_or_else(|| wasmi::Error::new("host.log pointer was out of bounds"))?;
                    range.to_vec()
                };
                caller.data_mut().stdout.extend_from_slice(&bytes);
                Ok(())
            },
        )
        .map_err(|error| WasmRuntimeError(format!("failed to register host imports: {error}")))?;

    let pre = linker
        .instantiate(&mut store, &module)
        .map_err(|error| WasmRuntimeError(format!("failed to instantiate guest: {error}")))?;
    let instance = pre
        .start(&mut store)
        .map_err(|error| WasmRuntimeError(format!("failed to start guest: {error}")))?;
    let exit_code = if let Ok(function) = instance.get_typed_func::<(), i32>(&store, invoke) {
        function.call(&mut store, ()).map_err(|error| {
            WasmRuntimeError(format!("wasm function '{invoke}' failed: {error}"))
        })?
    } else if let Ok(function) = instance.get_typed_func::<(), ()>(&store, invoke) {
        function.call(&mut store, ()).map_err(|error| {
            WasmRuntimeError(format!("wasm function '{invoke}' failed: {error}"))
        })?;
        0
    } else {
        return Err(WasmRuntimeError(format!(
            "failed to resolve exported zero-argument function '{invoke}' in '{}'",
            Path::new(module_path).display()
        )));
    };
    let output = String::from_utf8_lossy(&store.data().stdout);
    if !output.is_empty() {
        print!("{output}");
    }

    Ok(exit_code)
}
