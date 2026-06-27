//! Native addon execution bridge — loads a cdylib and drives it as a
//! [`BehaviorNode`] within the existing scheduler.
//!
//! The addon exports a single `extern "C"` symbol:
//!   - `behavior_api() -> *const BehaviorApi`
//!
//! The `BehaviorApi` contains C-ABI function pointers to create, update, and
//! destroy the instance. The engine passes a `*mut NativeHost` to `create()`.
//! The addon calls into `NativeHost` to read frames and publish signals.

use std::collections::BTreeMap;
use std::ffi::c_void;
use std::path::PathBuf;

use crate::addon::schema::{ParamMap, ParamSpec};
use crate::behavior::node::{BehaviorCtx, BehaviorNode, BehaviorStartCtx};
use crate::behavior::BehaviorInit;
use crate::signal::{SignalId, SignalSpec, SignalValue};

// ---- C‑ABI types ------------------------------------------------------------

#[repr(C)]
pub struct FfiFrame {
    pub width: u32,
    pub height: u32,
    pub data: *const u8,
    pub len: usize,
    pub valid: u8,
}

#[repr(C)]
pub struct NativeHost {
    pub engine_ctx: *mut c_void,
    pub read_frame: unsafe extern "C" fn(host: *mut NativeHost, out: *mut FfiFrame),
    pub publish_f32: unsafe extern "C" fn(host: *mut NativeHost, name: *const u8, name_len: usize, value: f32),
    pub publish_bool: unsafe extern "C" fn(host: *mut NativeHost, name: *const u8, name_len: usize, value: u8),
    pub publish_vec2: unsafe extern "C" fn(host: *mut NativeHost, name: *const u8, name_len: usize, x: f32, y: f32),
    pub publish_vec3: unsafe extern "C" fn(host: *mut NativeHost, name: *const u8, name_len: usize, x: f32, y: f32, z: f32),
    pub get_param_f32: unsafe extern "C" fn(host: *mut NativeHost, name: *const u8, name_len: usize, out: *mut f32) -> u8,
    pub timing: unsafe extern "C" fn(host: *mut NativeHost, dt: *mut f32, elapsed: *mut f32),
}

#[repr(C)]
pub struct BehaviorApi {
    pub create: extern "C" fn(host: *mut NativeHost) -> *mut c_void,
    pub update: extern "C" fn(instance: *mut c_void),
    pub destroy: extern "C" fn(instance: *mut c_void),
}

// ---- callback state --------------------------------------------------------

/// Mutable state passed as `engine_ctx` through the host struct.
struct CallbackState<'a> {
    staged: Vec<(String, SignalValue)>,
    frame_width: u32,
    frame_height: u32,
    frame_data: &'a [u8],
    has_frame: bool,
    values: &'a ParamMap,
    dt: f32,
    elapsed: f32,
}

// ---- C callbacks -----------------------------------------------------------

unsafe extern "C" fn cb_read_frame(host: *mut NativeHost, out: *mut FfiFrame) {
    unsafe {
        let state = &*((*host).engine_ctx as *const CallbackState);
        let frame = &mut *out;
        if state.has_frame && !state.frame_data.is_empty() {
            frame.width = state.frame_width;
            frame.height = state.frame_height;
            frame.data = state.frame_data.as_ptr();
            frame.len = state.frame_data.len();
            frame.valid = 1;
            eprintln!("[native] frame available: {}x{} ({} bytes)", frame.width, frame.height, frame.len);
        } else {
            frame.width = 0;
            frame.height = 0;
            frame.data = std::ptr::null();
            frame.len = 0;
            frame.valid = 0;
            eprintln!("[native] frame empty");
        }
    }
}

unsafe extern "C" fn cb_publish_f32(host: *mut NativeHost, name: *const u8, name_len: usize, value: f32) {
    unsafe {
        let state = &mut *((*host).engine_ctx as *mut CallbackState);
        let name = std::str::from_utf8_unchecked(std::slice::from_raw_parts(name, name_len));
        eprintln!("[native] publish f32: {} = {}", name, value);
        state.staged.push((name.to_string(), SignalValue::F32(value)));
    }
}

unsafe extern "C" fn cb_publish_bool(host: *mut NativeHost, name: *const u8, name_len: usize, value: u8) {
    unsafe {
        let state = &mut *((*host).engine_ctx as *mut CallbackState);
        let name = std::str::from_utf8_unchecked(std::slice::from_raw_parts(name, name_len));
        let b = value != 0;
        eprintln!("[native] publish bool: {} = {}", name, b);
        state.staged.push((name.to_string(), SignalValue::Bool(b)));
    }
}

unsafe extern "C" fn cb_publish_vec2(host: *mut NativeHost, name: *const u8, name_len: usize, x: f32, y: f32) {
    unsafe {
        let state = &mut *((*host).engine_ctx as *mut CallbackState);
        let name = std::str::from_utf8_unchecked(std::slice::from_raw_parts(name, name_len));
        eprintln!("[native] publish vec2: {} = [{}, {}]", name, x, y);
        state.staged.push((name.to_string(), SignalValue::Vec2([x, y])));
    }
}

unsafe extern "C" fn cb_publish_vec3(host: *mut NativeHost, name: *const u8, name_len: usize, x: f32, y: f32, z: f32) {
    unsafe {
        let state = &mut *((*host).engine_ctx as *mut CallbackState);
        let name = std::str::from_utf8_unchecked(std::slice::from_raw_parts(name, name_len));
        eprintln!("[native] publish vec3: {} = [{}, {}, {}]", name, x, y, z);
        state.staged.push((name.to_string(), SignalValue::Vec3([x, y, z])));
    }
}

unsafe extern "C" fn cb_get_param_f32(host: *mut NativeHost, name: *const u8, name_len: usize, out: *mut f32) -> u8 {
    unsafe {
        let state = &*((*host).engine_ctx as *const CallbackState);
        let name = std::str::from_utf8_unchecked(std::slice::from_raw_parts(name, name_len));
        if let Some(crate::addon::schema::ParamValue::F32(v)) = state.values.get(name) {
            *out = *v as f32;
            1
        } else {
            0
        }
    }
}

unsafe extern "C" fn cb_timing(host: *mut NativeHost, dt: *mut f32, elapsed: *mut f32) {
    unsafe {
        let state = &*((*host).engine_ctx as *const CallbackState);
        if !dt.is_null() {
            *dt = state.dt;
        }
        if !elapsed.is_null() {
            *elapsed = state.elapsed;
        }
    }
}

// ---- NativeBehaviorBridge --------------------------------------------------

use crate::native::runner::NativeRunner;
use crate::native::handshake::{BEHAVIOR_FAMILY, BEHAVIOR_ABI_V1};

pub struct NativeBehaviorBridge {
    entry_path: PathBuf,
    runner: NativeRunner,
    host: Box<NativeHost>,
    signal_ids: Vec<(String, Option<SignalId>)>,
    values: ParamMap,
}

unsafe impl Send for NativeBehaviorBridge {}

impl NativeBehaviorBridge {
    pub fn new(entry_path: PathBuf, values: ParamMap) -> Self {
        // Create the host struct. Its memory location must be stable because
        // the addon might store the pointer passed to `create()`. We Box it.
        let host = Box::new(NativeHost {
            engine_ctx: std::ptr::null_mut(),
            read_frame: cb_read_frame,
            publish_f32: cb_publish_f32,
            publish_bool: cb_publish_bool,
            publish_vec2: cb_publish_vec2,
            publish_vec3: cb_publish_vec3,
            get_param_f32: cb_get_param_f32,
            timing: cb_timing,
        });

        Self {
            entry_path,
            runner: NativeRunner::new(),
            host,
            signal_ids: Vec::new(),
            values,
        }
    }
}

impl BehaviorNode for NativeBehaviorBridge {
    fn start(&mut self, ctx: &mut BehaviorStartCtx) {
        eprintln!("[engine] [native] loading {:?}", self.entry_path);

        for (id, name, _) in ctx.schema().iter() {
            self.signal_ids.push((name.to_string(), Some(id)));
        }

        let host_ptr = self.host.as_mut() as *mut NativeHost as *mut std::ffi::c_void;

        match self.runner.load(&self.entry_path, BEHAVIOR_FAMILY, BEHAVIOR_ABI_V1) {
            Ok(()) => {
                if let Err(e) = self.runner.start(host_ptr) {
                    eprintln!("[engine] [native] start failed: {e}");
                } else {
                    eprintln!("[engine] [native] loaded {:?} successfully", self.entry_path);
                }
            }
            Err(e) => {
                eprintln!("[engine] [native] failed to load {:?}: {e}", self.entry_path);
            }
        }
    }

    fn update(&mut self, ctx: &mut BehaviorCtx) {
        if self.runner.is_faulted() {
            return;
        }

        eprintln!("[native] update enter");

        let (has_frame, fw, fh) = match ctx.frame() {
            Some(f) => (true, f.width(), f.height()),
            None => (false, 0, 0),
        };
        let frame_data: &[u8] = match ctx.frame() {
            Some(f) => f.rgba(),
            None => &[],
        };

        let mut cb_state = CallbackState {
            staged: Vec::new(),
            frame_width: fw,
            frame_height: fh,
            frame_data,
            has_frame,
            values: &self.values,
            dt: ctx.timing().dt,
            elapsed: ctx.timing().elapsed,
        };

        // Wire the current tick's data into the host struct.
        self.host.engine_ctx = &mut cb_state as *mut CallbackState as *mut c_void;

        let result = self.runner.update();

        // Invalidate pointer.
        self.host.engine_ctx = std::ptr::null_mut();

        if let Err(e) = result {
            eprintln!("[engine] [native] addon update failed: {e} — disabling");
            return;
        }

        eprintln!("[native] draining {} staged signals", cb_state.staged.len());
        for (name, value) in cb_state.staged.drain(..) {
            if let Some((_, Some(id))) = self.signal_ids.iter().find(|(n, _)| n == &name) {
                eprintln!("[native] store commit: signal {} (id {:?}) = {:?}", name, id, value);
                ctx.publish(*id, value);
            } else {
                eprintln!("[native] lookup failed: signal {} not found in registry", name);
            }
        }

        eprintln!("[native] update exit");
    }

    fn stop(&mut self) {
        self.runner.unload();
        eprintln!("[engine] [native] unloaded {:?}", self.entry_path);
    }
}

// ---- factory ---------------------------------------------------------------

pub fn native_init(
    instance_id: String,
    entry_path: PathBuf,
    publish: Vec<SignalSpec>,
    specs: BTreeMap<String, ParamSpec>,
    values: ParamMap,
    enabled: bool,
) -> BehaviorInit {
    BehaviorInit {
        instance_id,
        node: Box::new(NativeBehaviorBridge::new(entry_path, values.clone())),
        publish,
        specs,
        values,
        enabled,
    }
}
