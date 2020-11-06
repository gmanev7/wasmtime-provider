use std::error::Error;
use wapc::{ModuleState, WapcFunctions, WebAssemblyEngineProvider, HOST_NAMESPACE};
use wasmtime::{Engine, Extern, ExternType, Func, Instance, Module, Store};

use std::sync::Arc;

#[macro_use]
extern crate log;

mod callbacks;

macro_rules! call {
    ($func:expr, $($p:expr),*) => {
      match $func.call(&[$($p.into()),*]) {
        Ok(result) => {
          let result: i32 = result[0].i32().unwrap();
          result
        }
        Err(e) => {
            error!("Failure invoking guest module handler: {:?}", e);
            0
        }
      }
    }
}

/// A waPC engine provider that encapsulates the Wasmtime WebAssembly runtime
#[derive(Clone)]
pub struct WasmtimeEngineProvider {
    host: Option<Arc<ModuleState>>,
    module: Module,
}

impl WasmtimeEngineProvider {
    pub fn new(buf: &[u8]) -> Result<Self, Box<dyn Error>> {
        Ok(WasmtimeEngineProvider {
            host: None,
            module: Self::load_module(buf)?,
        })
    }

    fn load_module(buf: &[u8]) -> Result<Module, Box<dyn Error>> {
        let engine = Engine::default();
        Ok(Module::new(&engine, buf)?)
    }
}

impl WebAssemblyEngineProvider for WasmtimeEngineProvider {
    fn init(&mut self, host: Arc<ModuleState>) -> Result<(), Box<dyn Error>> {
        self.host = Some(host);
        Ok(())
    }

    fn call(&mut self, op_length: i32, msg_length: i32) -> Result<i32, Box<dyn Error>> {
        debug_assert!(self.initialized());
        let instance = self.instantiate()?;
        let guest_call_fn = guest_call_fn(&instance)?;

        // Note that during this call, the guest should, through the functions
        // it imports from the host, set the guest error and response

        let callresult: i32 = call!(guest_call_fn, op_length, msg_length);

        Ok(callresult)
    }

    fn replace(&mut self, buf: &[u8]) -> Result<(), Box<dyn Error>> {
        debug_assert!(self.initialized());
        info!(
            "HOT SWAP - Replacing existing WebAssembly module with new buffer, {} bytes",
            buf.len()
        );

        self.module = Self::load_module(buf)?;
        Ok(())
    }
}

impl WasmtimeEngineProvider {
    fn initialized(&self) -> bool {
        self.host.is_some()
    }

    fn instantiate(&self) -> Result<Instance, Box<dyn Error>> {
        debug_assert!(self.initialized());
        let host = self.host.as_ref().unwrap().clone();
        let engine = self.module.engine();
        let store = Store::new(engine);
        let imports = arrange_imports(&self.module, host, store.clone());
        let instance = wasmtime::Instance::new(&store, &self.module, imports?.as_slice())?;
        initialize(&instance)?;
        Ok(instance)
    }
}

fn initialize(instance: &Instance) -> Result<(), Box<dyn Error>> {
    for starter in wapc::WapcFunctions::REQUIRED_STARTS.iter() {
        if let Some(ext) = instance.get_export(starter) {
            ext.into_func().unwrap().call(&[])?;
        }
    }
    Ok(())
}

/// wasmtime requires that the list of callbacks be "zippable" with the list
/// of module imports. In order to ensure that both lists are in the same
/// order, we have to loop through the module imports and instantiate the
/// corresponding callback. We **cannot** rely on a predictable import order
/// in the wasm module
fn arrange_imports(
    module: &Module,
    host: Arc<ModuleState>,
    store: Store,
) -> Result<Vec<Extern>, Box<dyn Error>> {
    Ok(module
        .imports()
        .filter_map(|imp| {
            if let ExternType::Func(_) = imp.ty() {
                match imp.module() {
                    HOST_NAMESPACE => {
                        Some(callback_for_import(imp.name(), host.clone(), store.clone()))
                    }
                    other => panic!("import module `{}` was not found", other), //TODO: get rid of panic
                }
            } else {
                None
            }
        })
        .collect())
}

fn callback_for_import(import: &str, host: Arc<ModuleState>, store: Store) -> Extern {
    match import {
        WapcFunctions::HOST_CONSOLE_LOG => callbacks::console_log_func(&store, host.clone()).into(),
        WapcFunctions::HOST_CALL => callbacks::host_call_func(&store, host.clone()).into(),
        WapcFunctions::GUEST_REQUEST_FN => {
            callbacks::guest_request_func(&store, host.clone()).into()
        }
        WapcFunctions::HOST_RESPONSE_FN => {
            callbacks::host_response_func(&store, host.clone()).into()
        }
        WapcFunctions::HOST_RESPONSE_LEN_FN => {
            callbacks::host_response_len_func(&store, host.clone()).into()
        }
        WapcFunctions::GUEST_RESPONSE_FN => {
            callbacks::guest_response_func(&store, host.clone()).into()
        }
        WapcFunctions::GUEST_ERROR_FN => callbacks::guest_error_func(&store, host.clone()).into(),
        WapcFunctions::HOST_ERROR_FN => callbacks::host_error_func(&store, host.clone()).into(),
        WapcFunctions::HOST_ERROR_LEN_FN => {
            callbacks::host_error_len_func(&store, host.clone()).into()
        }
        _ => unreachable!(),
    }
}

// Called once, then the result is cached. This returns a `Func` that corresponds
// to the `__guest_call` export
fn guest_call_fn(instance: &Instance) -> Result<Func, Box<dyn Error>> {
    if let Some(func) = instance.get_func(WapcFunctions::GUEST_CALL) {
        Ok(func)
    } else {
        Err("Guest module did not export __guest_call function!".into())
    }
}
