use async_trait::async_trait;
use wasmtime::*;
use crate::runtime::driver::RuntimeDriver;
use crate::runner::{TickOutcome, Handshake, Capabilities, ABI_VERSION, HostApi};
use crate::behavior::node::Timing;
use crate::addon::error::{AddonError, Result};

/// WASM driver using `wasmtime` for sandboxed execution.
pub struct WasmDriver {
    engine: Engine,
    module: Option<Module>,
    store: Option<Store<WasmCtx>>,
    instance: Option<Instance>,
}

pub struct WasmCtx {
    // Current timing info for the WASM guest
    pub timing: Timing,
}

impl Default for WasmDriver {
    fn default() -> Self {
        Self {
            engine: Engine::default(),
            module: None,
            store: None,
            instance: None,
        }
    }
}

#[async_trait]
impl RuntimeDriver for WasmDriver {
    fn kind(&self) -> &'static str {
        "wasm"
    }

    async fn load(&mut self, entry_point: &str) -> Result<()> {
        let module = Module::from_file(&self.engine, entry_point)
            .map_err(|e| AddonError::Runtime(format!("Failed to load WASM: {}", e)))?;
        self.module = Some(module);
        Ok(())
    }

    async fn bind(&mut self, _host: &mut dyn HostApi) -> Result<Handshake> {
        // Instantiate the module here
        let mut linker = Linker::new(&self.engine);
        
        // --- Host Function: log ---
        linker.func_wrap("env", "host_log", |mut _caller: Caller<'_, WasmCtx>, level: i32, ptr: i32, len: i32| {
            // Memory reading would happen here...
            println!("WASM Log (level {}): TODO read memory at {}..{}", level, ptr, ptr + len);
        }).unwrap();

        let ctx = WasmCtx { timing: Timing { dt: 0.0, elapsed: 0.0 } };
        let mut store = Store::new(&self.engine, ctx);
        
        let module = self.module.as_ref().ok_or(AddonError::Runtime("No module loaded".into()))?;
        let instance = linker.instantiate(&mut store, module)
            .map_err(|e| AddonError::Runtime(format!("Failed to instantiate WASM: {}", e)))?;

        self.store = Some(store);
        self.instance = Some(instance);

        Ok(Handshake {
            version: ABI_VERSION,
            caps: Capabilities::default(),
            publish: Vec::new(),
            consume: Vec::new(),
        })
    }

    async fn start(&mut self, _host: &mut dyn HostApi) -> Result<()> {
        if let (Some(store), Some(instance)) = (self.store.as_mut(), self.instance.as_ref()) {
            let start_fn = instance.get_typed_func::<(), ()>(&mut *store, "start")
                .map_err(|_| AddonError::Runtime("Missing start function in WASM".into()))?;
            start_fn.call(&mut *store, ())
                .map_err(|e| AddonError::Runtime(format!("WASM start failed: {}", e)))?;
        }
        Ok(())
    }

    async fn tick(&mut self, _host: &mut dyn HostApi, timing: Timing) -> TickOutcome {
        if let (Some(store), Some(instance)) = (self.store.as_mut(), self.instance.as_ref()) {
            store.data_mut().timing = timing;
            
            if let Ok(tick_fn) = instance.get_typed_func::<(f32, f32), i32>(&mut *store, "tick") {
                match tick_fn.call(&mut *store, (timing.dt, timing.elapsed)) {
                    Ok(0) => return TickOutcome::Ok,
                    Ok(err) => return TickOutcome::Faulted(format!("WASM tick error code: {}", err)),
                    Err(e) => return TickOutcome::Faulted(format!("WASM execution failed: {}", e)),
                }
            }
        }
        TickOutcome::Ok
    }

    async fn stop(&mut self, _host: &mut dyn HostApi) -> Result<()> {
        self.instance = None;
        self.store = None;
        Ok(())
    }
}
