//! Native rendering addon execution bridge — loads a cdylib and drives it as a
//! [`FilterNode`] within the existing scheduler.
//!
//! The addon exports a single `extern "C"` symbol:
//!   - `pipeline_api() -> *const PipelineApi`
//!
//! The `PipelineApi` contains C-ABI function pointers to create, update, and
//! destroy the instance. The engine passes a `*mut PipelineHost` to `create()`.
//! The addon calls into `PipelineHost` to publish render artifacts.

use std::ffi::c_void;
use std::path::PathBuf;
use wgpu::*;

use crate::addon::schema::{ParamMap, ParamValue};
use crate::native::runner::NativeRunner;
use crate::native::handshake::{RENDER_FAMILY, RENDER_ABI_V1};
use crate::runtime::{
    FilterNode, FrameContext, HostApi, NodeHandle, ArtifactHandle,
    artifact::{RenderArtifact, InstanceSchema, SemanticRows, SemanticRow, SemanticValue, SemanticField},
    context::SignalContext,
};
use crate::signal::SignalSnapshot;

// ---- C‑ABI types ------------------------------------------------------------

#[repr(C)]
pub struct PipelineHost {
    pub engine_ctx: *mut c_void,
    pub publish_instances: unsafe extern "C" fn(
        host: *mut PipelineHost,
        instance_id: *const u8,
        instance_id_len: usize,
        schema_id: u64,
        rows_data: *const f32,
        rows_data_len: usize,
    ),
    pub get_param_f32: unsafe extern "C" fn(
        host: *mut PipelineHost,
        name: *const u8,
        name_len: usize,
        out: *mut f32,
    ) -> u8,
    pub timing: unsafe extern "C" fn(
        host: *mut PipelineHost,
        dt: *mut f32,
        elapsed: *mut f32,
    ),
    pub publish_instances_v2: unsafe extern "C" fn(
        host: *mut PipelineHost,
        artifact_handle: u32,
        schema_id: u64,
        rows_data: *const f32,
        rows_data_len: usize,
    ),
}

#[repr(C)]
pub struct PipelineApi {
    pub create: extern "C" fn(host: *mut PipelineHost) -> *mut c_void,
    pub update: extern "C" fn(instance: *mut c_void),
    pub destroy: extern "C" fn(instance: *mut c_void),
}

// ---- callback state --------------------------------------------------------

struct ExecutionContext<'a> {
    node_handle: NodeHandle,
    host_api: &'a mut HostApi,
    schema: &'a InstanceSchema,
    values: &'a ParamMap,
    dt: f32,
    elapsed: f32,
}

// ---- C callbacks -----------------------------------------------------------

unsafe extern "C" fn cb_publish_instances(
    host: *mut PipelineHost,
    instance_id: *const u8,
    instance_id_len: usize,
    schema_id: u64,
    rows_data: *const f32,
    rows_data_len: usize,
) {
    let start_time = std::time::Instant::now();
    unsafe {
        let host_ptr_val = host;
        let engine_ctx_val = if !host.is_null() { (*host).engine_ctx } else { std::ptr::null_mut() };

        crate::trace_log!("FFI", "ENTER CALLBACK");
        crate::trace_log!("FFI", "  callback name: cb_publish_instances");
        crate::trace_log!("FFI", "  arguments: instance_id_len = {}, schema_id = {}, rows_data_len = {}", instance_id_len, schema_id, rows_data_len);
        crate::trace_log!("FFI", "  host ptr: {:p}", host_ptr_val);
        crate::trace_log!("FFI", "  engine_ctx ptr: {:p}", engine_ctx_val);

        if host.is_null() {
            crate::trace_log!("WARNING", "cb_publish_instances: host pointer is null");
            crate::trace_log!("FFI", "EXIT CALLBACK");
            crate::trace_log!("FFI", "  success: false");
            crate::trace_log!("FFI", "  duration: {:?}", start_time.elapsed());
            return;
        }
        if engine_ctx_val.is_null() {
            crate::trace_log!("WARNING", "cb_publish_instances: engine_ctx pointer is null");
            crate::trace_log!("FFI", "EXIT CALLBACK");
            crate::trace_log!("FFI", "  success: false");
            crate::trace_log!("FFI", "  duration: {:?}", start_time.elapsed());
            return;
        }
        let state = &mut *(engine_ctx_val as *mut ExecutionContext);

        if instance_id.is_null() {
            crate::trace_log!("WARNING", "cb_publish_instances: instance_id pointer is null");
            crate::trace_log!("FFI", "EXIT CALLBACK");
            crate::trace_log!("FFI", "  success: false");
            crate::trace_log!("FFI", "  duration: {:?}", start_time.elapsed());
            return;
        }
        let inst_id_str = std::str::from_utf8_unchecked(std::slice::from_raw_parts(instance_id, instance_id_len));

        let node_handle = state.node_handle;
        let art_handle = ArtifactHandle::new(crate::runtime::hash_str(inst_id_str));

        if rows_data.is_null() {
            crate::trace_log!("WARNING", "cb_publish_instances: rows_data pointer is null");
            crate::trace_log!("FFI", "EXIT CALLBACK");
            crate::trace_log!("FFI", "  success: false");
            crate::trace_log!("FFI", "  duration: {:?}", start_time.elapsed());
            return;
        }

        let float_stride = state.schema.fields.iter().map(|f| match f {
            SemanticField::Position2 => 2,
            SemanticField::Position3 => 3,
            SemanticField::ColorRgba => 4,
            SemanticField::UvQuad => 4,
            SemanticField::CustomFloat(_) => 1,
        }).sum::<usize>();

        if float_stride == 0 {
            crate::trace_log!("WARNING", "cb_publish_instances: float_stride is 0 for schema_id {}", schema_id);
            crate::trace_log!("FFI", "EXIT CALLBACK");
            crate::trace_log!("FFI", "  success: false");
            crate::trace_log!("FFI", "  duration: {:?}", start_time.elapsed());
            return;
        }

        let row_count = rows_data_len / float_stride;
        let byte_size = rows_data_len * std::mem::size_of::<f32>();

        let mut rows = Vec::with_capacity(row_count);
        let slice = std::slice::from_raw_parts(rows_data, rows_data_len);

        for r in 0..row_count {
            let offset = r * float_stride;
            let mut values = Vec::with_capacity(state.schema.fields.len());
            let mut cursor = 0;
            for f in &state.schema.fields {
                match f {
                    SemanticField::Position2 => {
                        let x = slice[offset + cursor];
                        let y = slice[offset + cursor + 1];
                        values.push(SemanticValue::Vec2([x, y]));
                        cursor += 2;
                    }
                    SemanticField::Position3 => {
                        let x = slice[offset + cursor];
                        let y = slice[offset + cursor + 1];
                        let z = slice[offset + cursor + 2];
                        values.push(SemanticValue::Vec3([x, y, z]));
                        cursor += 3;
                    }
                    SemanticField::ColorRgba => {
                        let r = slice[offset + cursor];
                        let g = slice[offset + cursor + 1];
                        let b = slice[offset + cursor + 2];
                        let a = slice[offset + cursor + 3];
                        values.push(SemanticValue::Vec4([r, g, b, a]));
                        cursor += 4;
                    }
                    SemanticField::UvQuad => {
                        let u = slice[offset + cursor];
                        let v = slice[offset + cursor + 1];
                        let w = slice[offset + cursor + 2];
                        let h = slice[offset + cursor + 3];
                        values.push(SemanticValue::Vec4([u, v, w, h]));
                        cursor += 4;
                    }
                    SemanticField::CustomFloat(_) => {
                        let val = slice[offset + cursor];
                        values.push(SemanticValue::Float(val));
                        cursor += 1;
                    }
                }
            }
            rows.push(SemanticRow { values });
        }

        let artifact = RenderArtifact::Instances {
            schema: InstanceSchema {
                schema_id: state.schema.schema_id,
                fields: state.schema.fields.clone(),
            },
            rows: SemanticRows {
                schema_id: state.schema.schema_id,
                rows,
            },
        };

        let result = state.host_api.publish_artifact_v2(node_handle, art_handle, artifact, state.host_api.epoch());
        let success = result.is_ok();
        if let Err(e) = &result {
            crate::trace_log!("WARNING", "cb_publish_instances publish_artifact_v2 failed: {:?}", e);
        }

        crate::trace_log!("FFI", "EXIT CALLBACK");
        crate::trace_log!("FFI", "  success: {}", success);
        crate::trace_log!("FFI", "  duration: {:?}", start_time.elapsed());
    }
}

unsafe extern "C" fn cb_publish_instances_v2(
    host: *mut PipelineHost,
    artifact_handle: u32,
    schema_id: u64,
    rows_data: *const f32,
    rows_data_len: usize,
) {
    let start_time = std::time::Instant::now();
    unsafe {
        let host_ptr_val = host;
        let engine_ctx_val = if !host.is_null() { (*host).engine_ctx } else { std::ptr::null_mut() };

        crate::trace_log!("FFI", "ENTER CALLBACK");
        crate::trace_log!("FFI", "  callback name: cb_publish_instances_v2");
        crate::trace_log!("FFI", "  arguments: artifact_handle = {}, schema_id = {}, rows_data_len = {}", artifact_handle, schema_id, rows_data_len);
        crate::trace_log!("FFI", "  host ptr: {:p}", host_ptr_val);
        crate::trace_log!("FFI", "  engine_ctx ptr: {:p}", engine_ctx_val);

        if host.is_null() {
            crate::trace_log!("WARNING", "cb_publish_instances_v2: host pointer is null");
            crate::trace_log!("FFI", "EXIT CALLBACK");
            crate::trace_log!("FFI", "  success: false");
            crate::trace_log!("FFI", "  duration: {:?}", start_time.elapsed());
            return;
        }
        if engine_ctx_val.is_null() {
            crate::trace_log!("WARNING", "cb_publish_instances_v2: engine_ctx pointer is null");
            crate::trace_log!("FFI", "EXIT CALLBACK");
            crate::trace_log!("FFI", "  success: false");
            crate::trace_log!("FFI", "  duration: {:?}", start_time.elapsed());
            return;
        }
        let state = &mut *(engine_ctx_val as *mut ExecutionContext);

        let node_handle = state.node_handle;
        let art_handle = ArtifactHandle::new(artifact_handle);

        if rows_data.is_null() {
            crate::trace_log!("WARNING", "cb_publish_instances_v2: rows_data pointer is null");
            crate::trace_log!("FFI", "EXIT CALLBACK");
            crate::trace_log!("FFI", "  success: false");
            crate::trace_log!("FFI", "  duration: {:?}", start_time.elapsed());
            return;
        }

        let float_stride = state.schema.fields.iter().map(|f| match f {
            SemanticField::Position2 => 2,
            SemanticField::Position3 => 3,
            SemanticField::ColorRgba => 4,
            SemanticField::UvQuad => 4,
            SemanticField::CustomFloat(_) => 1,
        }).sum::<usize>();

        if float_stride == 0 {
            crate::trace_log!("WARNING", "cb_publish_instances_v2: float_stride is 0 for schema_id {}", schema_id);
            crate::trace_log!("FFI", "EXIT CALLBACK");
            crate::trace_log!("FFI", "  success: false");
            crate::trace_log!("FFI", "  duration: {:?}", start_time.elapsed());
            return;
        }

        let row_count = rows_data_len / float_stride;
        let byte_size = rows_data_len * std::mem::size_of::<f32>();

        let mut rows = Vec::with_capacity(row_count);
        let data_slice = std::slice::from_raw_parts(rows_data, rows_data_len);

        for r in 0..row_count {
            let offset = r * float_stride;
            let mut values = Vec::with_capacity(state.schema.fields.len());
            let mut cursor = 0;
            for f in &state.schema.fields {
                match f {
                    SemanticField::Position2 => {
                        let x = data_slice[offset + cursor];
                        let y = data_slice[offset + cursor + 1];
                        values.push(SemanticValue::Vec2([x, y]));
                        cursor += 2;
                    }
                    SemanticField::Position3 => {
                        let x = data_slice[offset + cursor];
                        let y = data_slice[offset + cursor + 1];
                        let z = data_slice[offset + cursor + 2];
                        values.push(SemanticValue::Vec3([x, y, z]));
                        cursor += 3;
                    }
                    SemanticField::ColorRgba => {
                        let r_val = data_slice[offset + cursor];
                        let g = data_slice[offset + cursor + 1];
                        let b = data_slice[offset + cursor + 2];
                        let a = data_slice[offset + cursor + 3];
                        values.push(SemanticValue::Vec4([r_val, g, b, a]));
                        cursor += 4;
                    }
                    SemanticField::UvQuad => {
                        let u = data_slice[offset + cursor];
                        let v = data_slice[offset + cursor + 1];
                        let w = data_slice[offset + cursor + 2];
                        let h = data_slice[offset + cursor + 3];
                        values.push(SemanticValue::Vec4([u, v, w, h]));
                        cursor += 4;
                    }
                    SemanticField::CustomFloat(_) => {
                        let val = data_slice[offset + cursor];
                        values.push(SemanticValue::Float(val));
                        cursor += 1;
                    }
                }
            }
            rows.push(SemanticRow { values });
        }

        let artifact = RenderArtifact::Instances {
            schema: InstanceSchema {
                schema_id: state.schema.schema_id,
                fields: state.schema.fields.clone(),
            },
            rows: SemanticRows {
                schema_id: state.schema.schema_id,
                rows,
            },
        };

        let result = state.host_api.publish_artifact_v2(node_handle, art_handle, artifact, state.host_api.epoch());
        let success = result.is_ok();
        if let Err(e) = &result {
            crate::trace_log!("WARNING", "cb_publish_instances_v2 publish_artifact_v2 failed: {:?}", e);
        }

        crate::trace_log!("FFI", "EXIT CALLBACK");
        crate::trace_log!("FFI", "  success: {}", success);
        crate::trace_log!("FFI", "  duration: {:?}", start_time.elapsed());
    }
}

unsafe extern "C" fn cb_get_param_f32(
    host: *mut PipelineHost,
    name: *const u8,
    name_len: usize,
    out: *mut f32,
) -> u8 {
    let start_time = std::time::Instant::now();
    unsafe {
        let host_ptr_val = host;
        let engine_ctx_val = if !host.is_null() { (*host).engine_ctx } else { std::ptr::null_mut() };

        crate::trace_log!("FFI", "ENTER CALLBACK");
        crate::trace_log!("FFI", "  callback name: cb_get_param_f32");
        crate::trace_log!("FFI", "  arguments: name_len = {}", name_len);
        crate::trace_log!("FFI", "  host ptr: {:p}", host_ptr_val);
        crate::trace_log!("FFI", "  engine_ctx ptr: {:p}", engine_ctx_val);

        if host.is_null() {
            crate::trace_log!("WARNING", "cb_get_param_f32: host pointer is null");
            crate::trace_log!("FFI", "EXIT CALLBACK");
            crate::trace_log!("FFI", "  success: false");
            crate::trace_log!("FFI", "  duration: {:?}", start_time.elapsed());
            return 0;
        }
        if engine_ctx_val.is_null() {
            crate::trace_log!("WARNING", "cb_get_param_f32: engine_ctx pointer is null");
            crate::trace_log!("FFI", "EXIT CALLBACK");
            crate::trace_log!("FFI", "  success: false");
            crate::trace_log!("FFI", "  duration: {:?}", start_time.elapsed());
            return 0;
        }
        let state = &*(engine_ctx_val as *const ExecutionContext);

        if name.is_null() {
            crate::trace_log!("WARNING", "cb_get_param_f32: name pointer is null");
            crate::trace_log!("FFI", "EXIT CALLBACK");
            crate::trace_log!("FFI", "  success: false");
            crate::trace_log!("FFI", "  duration: {:?}", start_time.elapsed());
            return 0;
        }
        let name_str = std::str::from_utf8_unchecked(std::slice::from_raw_parts(name, name_len));

        if out.is_null() {
            crate::trace_log!("WARNING", "cb_get_param_f32: out pointer is null");
            crate::trace_log!("FFI", "EXIT CALLBACK");
            crate::trace_log!("FFI", "  success: false");
            crate::trace_log!("FFI", "  duration: {:?}", start_time.elapsed());
            return 0;
        }

        if let Some(ParamValue::F32(v)) = state.values.get(name_str) {
            *out = *v as f32;
            crate::trace_log!("FFI", "EXIT CALLBACK");
            crate::trace_log!("FFI", "  success: true");
            crate::trace_log!("FFI", "  duration: {:?}", start_time.elapsed());
            1
        } else {
            crate::trace_log!("WARNING", "cb_get_param_f32: param '{}' not found in ExecutionContext values", name_str);
            crate::trace_log!("FFI", "EXIT CALLBACK");
            crate::trace_log!("FFI", "  success: false");
            crate::trace_log!("FFI", "  duration: {:?}", start_time.elapsed());
            0
        }
    }
}

unsafe extern "C" fn cb_timing(
    host: *mut PipelineHost,
    dt: *mut f32,
    elapsed: *mut f32,
) {
    let start_time = std::time::Instant::now();
    unsafe {
        let host_ptr_val = host;
        let engine_ctx_val = if !host.is_null() { (*host).engine_ctx } else { std::ptr::null_mut() };

        crate::trace_log!("FFI", "ENTER CALLBACK");
        crate::trace_log!("FFI", "  callback name: cb_timing");
        crate::trace_log!("FFI", "  arguments: dt_ptr = {:p}, elapsed_ptr = {:p}", dt, elapsed);
        crate::trace_log!("FFI", "  host ptr: {:p}", host_ptr_val);
        crate::trace_log!("FFI", "  engine_ctx ptr: {:p}", engine_ctx_val);

        if host.is_null() {
            crate::trace_log!("WARNING", "cb_timing: host pointer is null");
            crate::trace_log!("FFI", "EXIT CALLBACK");
            crate::trace_log!("FFI", "  success: false");
            crate::trace_log!("FFI", "  duration: {:?}", start_time.elapsed());
            return;
        }
        if engine_ctx_val.is_null() {
            crate::trace_log!("WARNING", "cb_timing: engine_ctx pointer is null");
            crate::trace_log!("FFI", "EXIT CALLBACK");
            crate::trace_log!("FFI", "  success: false");
            crate::trace_log!("FFI", "  duration: {:?}", start_time.elapsed());
            return;
        }
        let state = &*(engine_ctx_val as *const ExecutionContext);

        let mut success = true;
        if dt.is_null() {
            crate::trace_log!("WARNING", "cb_timing: dt pointer is null");
            success = false;
        } else {
            *dt = state.dt;
        }

        if elapsed.is_null() {
            crate::trace_log!("WARNING", "cb_timing: elapsed pointer is null");
            success = false;
        } else {
            *elapsed = state.elapsed;
        }

        crate::trace_log!("FFI", "EXIT CALLBACK");
        crate::trace_log!("FFI", "  success: {}", success);
        crate::trace_log!("FFI", "  duration: {:?}", start_time.elapsed());
    }
}

// ---- NativePipelineBridge --------------------------------------------------

pub struct NativePipelineBridge {
    node_handle: NodeHandle,
    entry_path: PathBuf,
    runner: NativeRunner,
    host: Box<PipelineHost>,
    schema: InstanceSchema,
    values: ParamMap,
    inner_node: Box<dyn FilterNode>,
}

unsafe impl Send for NativePipelineBridge {}

impl NativePipelineBridge {
    pub fn new(
        node_handle: NodeHandle,
        device: &Device,
        host_layout: &BindGroupLayout,
        image_layout: &BindGroupLayout,
        format: TextureFormat,
        entry_path: PathBuf,
        label: &str,
        shader_src: &str,
        params: &[f32],
        signals: &SignalContext,
        layout_plan: &crate::runtime::packing::LayoutPlan,
        schema: InstanceSchema,
        values: ParamMap,
        vertex_entry: &str,
        fragment_entry: &str,
    ) -> Self {
        println!("[NATIVE] Bridge creation: node_handle = NodeHandle({}), entry_path = {:?}, label = '{}'", node_handle.raw(), entry_path, label);
        println!("[NATIVE] NodeHandle injected: NodeHandle({})", node_handle.raw());
        let inner_node = crate::addons::instanced_external_node(
            device,
            host_layout,
            image_layout,
            format,
            label,
            shader_src,
            params,
            signals,
            layout_plan,
            vertex_entry,
            fragment_entry,
        );

        let host = Box::new(PipelineHost {
            engine_ctx: std::ptr::null_mut(),
            publish_instances: cb_publish_instances,
            get_param_f32: cb_get_param_f32,
            timing: cb_timing,
            publish_instances_v2: cb_publish_instances_v2,
        });
        println!("[NATIVE] PipelineHost created at address: {:p}", &*host as *const PipelineHost);

        let mut runner = NativeRunner::new();
        let host_ptr = &*host as *const PipelineHost as *mut PipelineHost as *mut std::ffi::c_void;
        match runner.load(&entry_path, RENDER_FAMILY, RENDER_ABI_V1) {
            Ok(()) => {
                if let Err(e) = runner.start(host_ptr) {
                    eprintln!("[engine] [native] pipeline start failed: {e}");
                } else {
                    eprintln!("[engine] [native] pipeline loaded {:?} successfully", entry_path);
                }
            }
            Err(e) => {
                eprintln!("[engine] [native] pipeline load failed {:?}: {e}", entry_path);
            }
        }

        Self {
            node_handle,
            entry_path,
            runner,
            host,
            schema,
            values,
            inner_node,
        }
    }
}

impl FilterNode for NativePipelineBridge {
    fn update(&mut self, host: &mut HostApi) {
        if self.runner.is_faulted() {
            crate::trace_log!("WARNING", "Native runner is faulted for NodeHandle({}); skipping update", self.node_handle.raw());
            return;
        }

        let dt = host.dt();
        let elapsed = host.time();
        let epoch = host.epoch();
        let mut cb_state = ExecutionContext {
            node_handle: self.node_handle,
            host_api: host,
            schema: &self.schema,
            values: &self.values,
            dt,
            elapsed,
        };

        let ctx_ptr = &mut cb_state as *mut ExecutionContext as *mut c_void;
        self.host.engine_ctx = ctx_ptr;

        let addon_name = self.entry_path.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("unknown_addon");

        crate::trace_log!("NATIVE", "ENTER NODE");
        crate::trace_log!("NATIVE", "  NodeHandle: NodeHandle({})", self.node_handle.raw());
        crate::trace_log!("NATIVE", "  Addon: {}", addon_name);
        crate::trace_log!("NATIVE", "  ExecutionContext ptr: {:p}", ctx_ptr);

        crate::trace_log!("NATIVE", "ExecutionContext");
        crate::trace_log!("NATIVE", "  address: {:p}", ctx_ptr);
        crate::trace_log!("NATIVE", "  NodeHandle: NodeHandle({})", self.node_handle.raw());
        crate::trace_log!("NATIVE", "  Epoch: {}", epoch.raw());

        let start_time = std::time::Instant::now();
        let result = self.runner.update();
        let duration = start_time.elapsed();

        let count_before = cb_state.host_api.staged_count();

        crate::trace_log!("NATIVE", "EXIT NODE");
        crate::trace_log!("NATIVE", "  duration: {:?}", duration);
        crate::trace_log!("NATIVE", "  published_count: {}", count_before);

        self.host.engine_ctx = std::ptr::null_mut();
        crate::trace_log!("NATIVE", "ExecutionContext released");

        if let Err(e) = result {
            crate::trace_log!("WARNING", "Native pipeline update failed for NodeHandle({}): {} — disabling", self.node_handle.raw(), e);
            eprintln!("[engine] [native] pipeline update failed: {e} — disabling");
        }
    }

    fn prepare(&mut self, queue: &Queue, signals: &SignalSnapshot) {
        self.inner_node.prepare(queue, signals);
    }

    fn process(&self, ctx: &mut FrameContext) {
        self.inner_node.process(ctx);
    }
}

impl Drop for NativePipelineBridge {
    fn drop(&mut self) {
        self.runner.unload();
    }
}
