use std::path::Path;
use std::panic::{catch_unwind, AssertUnwindSafe};
use crate::native::handshake::HandshakeData;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunnerError {
    Load(String),
    Handshake(String),
    Symbol(String),
}

impl std::fmt::Display for RunnerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RunnerError::Load(e) => write!(f, "Load error: {e}"),
            RunnerError::Handshake(e) => write!(f, "Handshake error: {e}"),
            RunnerError::Symbol(e) => write!(f, "Symbol error: {e}"),
        }
    }
}

impl std::error::Error for RunnerError {}

pub struct NativeRunner {
    lib: Option<libloading::Library>,
    instance: *mut std::ffi::c_void,
    // Opaque lifecycle entry points resolved from dynamic library
    create_fn: Option<unsafe extern "C" fn(*mut std::ffi::c_void) -> *mut std::ffi::c_void>,
    update_fn: Option<unsafe extern "C" fn(*mut std::ffi::c_void)>,
    destroy_fn: Option<unsafe extern "C" fn(*mut std::ffi::c_void)>,
    faulted: bool,
}

impl NativeRunner {
    pub fn new() -> Self {
        Self {
            lib: None,
            instance: std::ptr::null_mut(),
            create_fn: None,
            update_fn: None,
            destroy_fn: None,
            faulted: false,
        }
    }

    pub fn is_faulted(&self) -> bool {
        self.faulted
    }

    pub fn load(&mut self, path: &Path, expected_family: u32, expected_abi: u32) -> Result<(), RunnerError> {
        let result = catch_unwind(AssertUnwindSafe(|| unsafe {
            let lib = libloading::Library::new(path)
                .map_err(|e| RunnerError::Load(format!("dlopen failed: {e}")))?;

            // Resolve handshake
            let get_handshake: libloading::Symbol<unsafe extern "C" fn() -> HandshakeData> = lib
                .get(b"get_handshake")
                .map_err(|e| RunnerError::Symbol(format!("missing get_handshake: {e}")))?;

            let handshake = get_handshake();
            if handshake.handshake_abi != super::handshake::HANDSHAKE_ABI_VERSION {
                return Err(RunnerError::Handshake(format!(
                    "handshake ABI mismatch: expected {}, got {}",
                    super::handshake::HANDSHAKE_ABI_VERSION,
                    handshake.handshake_abi
                )));
            }
            if handshake.runtime_family != expected_family {
                return Err(RunnerError::Handshake(format!(
                    "runtime family mismatch: expected 0x{:08X}, got 0x{:08X}",
                    expected_family, handshake.runtime_family
                )));
            }
            if handshake.runtime_abi != expected_abi {
                return Err(RunnerError::Handshake(format!(
                    "runtime ABI mismatch: expected 0x{:08X}, got 0x{:08X}",
                    expected_abi, handshake.runtime_abi
                )));
            }

            // Resolve direct entry points
            let create_sym: libloading::Symbol<unsafe extern "C" fn(*mut std::ffi::c_void) -> *mut std::ffi::c_void> = lib
                .get(b"create_instance")
                .map_err(|e| RunnerError::Symbol(format!("missing create_instance: {e}")))?;
            let update_sym: libloading::Symbol<unsafe extern "C" fn(*mut std::ffi::c_void)> = lib
                .get(b"update_instance")
                .map_err(|e| RunnerError::Symbol(format!("missing update_instance: {e}")))?;
            let destroy_sym: libloading::Symbol<unsafe extern "C" fn(*mut std::ffi::c_void)> = lib
                .get(b"destroy_instance")
                .map_err(|e| RunnerError::Symbol(format!("missing destroy_instance: {e}")))?;

            // Transmute to static lifetime function pointers
            let create_fn = std::mem::transmute(*create_sym);
            let update_fn = std::mem::transmute(*update_sym);
            let destroy_fn = std::mem::transmute(*destroy_sym);

            Ok((lib, create_fn, update_fn, destroy_fn))
        }));

        match result {
            Ok(Ok((lib, create, update, destroy))) => {
                self.lib = Some(lib);
                self.create_fn = Some(create);
                self.update_fn = Some(update);
                self.destroy_fn = Some(destroy);
                Ok(())
            }
            Ok(Err(e)) => {
                self.faulted = true;
                Err(e)
            }
            Err(_) => {
                self.faulted = true;
                Err(RunnerError::Load("panic during library load".into()))
            }
        }
    }

    pub fn start(&mut self, host_ptr: *mut std::ffi::c_void) -> Result<(), String> {
        if self.faulted {
            return Err("runner is in faulted state".into());
        }
        let create_fn = self.create_fn.ok_or_else(|| "runner not loaded".to_string())?;

        let result = catch_unwind(AssertUnwindSafe(|| unsafe {
            create_fn(host_ptr)
        }));

        match result {
            Ok(instance) => {
                if instance.is_null() {
                    self.faulted = true;
                    Err("create_instance returned null".into())
                } else {
                    self.instance = instance;
                    Ok(())
                }
            }
            Err(_) => {
                self.faulted = true;
                Err("panic in create_instance".into())
            }
        }
    }

    pub fn update(&mut self) -> Result<(), String> {
        if self.faulted || self.instance.is_null() {
            return Err("runner is faulted or not running".into());
        }
        let update_fn = self.update_fn.ok_or_else(|| "runner not loaded".to_string())?;
        let instance = self.instance;

        let result = catch_unwind(AssertUnwindSafe(|| unsafe {
            update_fn(instance)
        }));

        match result {
            Ok(()) => Ok(()),
            Err(_) => {
                self.faulted = true;
                Err("panic in update_instance".into())
            }
        }
    }

    pub fn stop(&mut self) {
        if self.instance.is_null() || self.faulted {
            return;
        }
        if let Some(destroy_fn) = self.destroy_fn {
            let instance = self.instance;
            let _ = catch_unwind(AssertUnwindSafe(|| unsafe {
                destroy_fn(instance);
            }));
        }
        self.instance = std::ptr::null_mut();
    }

    pub fn unload(&mut self) {
        self.stop();
        self.create_fn = None;
        self.update_fn = None;
        self.destroy_fn = None;
        self.lib = None;
    }
}

impl Drop for NativeRunner {
    fn drop(&mut self) {
        self.unload();
    }
}
