use async_trait::async_trait;
use libloading::{Library, Symbol};
use crate::runtime::driver::RuntimeDriver;
use crate::runner::{TickOutcome, Handshake, Capabilities, ABI_VERSION, HostApi};
use crate::behavior::node::Timing;
use crate::addon::error::{AddonError, Result};

/// Native driver using `libloading` to execute compiled code.
pub struct NativeDriver {
    lib: Option<Library>,
    capabilities: Capabilities,
}

impl Default for NativeDriver {
    fn default() -> Self {
        Self {
            lib: None,
            capabilities: Capabilities::default(),
        }
    }
}

#[async_trait]
impl RuntimeDriver for NativeDriver {
    fn kind(&self) -> &'static str {
        "native"
    }

    async fn load(&mut self, entry_point: &str) -> Result<()> {
        unsafe {
            let lib = Library::new(entry_point).map_err(|e| AddonError::Runtime(format!("Failed to load library: {}", e)))?;
            
            // Check ABI version
            let get_abi: Symbol<extern "C" fn() -> u16> = lib.get(b"addon_abi_version")
                .map_err(|_| AddonError::Runtime("Missing addon_abi_version symbol".into()))?;
            
            if get_abi() != ABI_VERSION {
                return Err(AddonError::Runtime(format!("ABI version mismatch: engine={} addon={}", ABI_VERSION, get_abi())));
            }

            self.lib = Some(lib);
        }
        Ok(())
    }

    async fn bind(&mut self, _host: &mut dyn HostApi) -> Result<Handshake> {
        // In native mode, we delegate binding to the addon's own init.
        // For now, return a placeholder handshake until the C-ABI is finalized.
        Ok(Handshake {
            version: ABI_VERSION,
            caps: self.capabilities,
            publish: Vec::new(),
            consume: Vec::new(),
        })
    }

    async fn start(&mut self, _host: &mut dyn HostApi) -> Result<()> {
        if let Some(ref lib) = self.lib {
            unsafe {
                let start_fn: Symbol<extern "C" fn() -> i32> = lib.get(b"addon_start")
                    .map_err(|_| AddonError::Runtime("Missing addon_start symbol".into()))?;
                if start_fn() != 0 {
                    return Err(AddonError::Runtime("addon_start failed".into()));
                }
            }
        }
        Ok(())
    }

    async fn tick(&mut self, _host: &mut dyn HostApi, timing: Timing) -> TickOutcome {
        if let Some(ref lib) = self.lib {
            unsafe {
                if let Ok(tick_fn) = lib.get::<extern "C" fn(f32, f32) -> i32>(b"addon_tick") {
                    let res = tick_fn(timing.dt, timing.elapsed);
                    if res == 0 {
                        return TickOutcome::Ok;
                    } else {
                        return TickOutcome::Faulted(format!("Addon tick returned error: {}", res));
                    }
                }
            }
        }
        TickOutcome::Ok
    }

    async fn stop(&mut self, _host: &mut dyn HostApi) -> Result<()> {
        if let Some(ref lib) = self.lib {
            unsafe {
                if let Ok(stop_fn) = lib.get::<extern "C" fn() -> i32>(b"addon_stop") {
                    stop_fn();
                }
            }
        }
        self.lib = None;
        Ok(())
    }
}
