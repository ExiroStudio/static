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
    FilterNode, FrameContext, HostApi,
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
}

#[repr(C)]
pub struct PipelineApi {
    pub create: extern "C" fn(host: *mut PipelineHost) -> *mut c_void,
    pub update: extern "C" fn(instance: *mut c_void),
    pub destroy: extern "C" fn(instance: *mut c_void),
}

// ---- callback state --------------------------------------------------------

struct PipelineCallbackState<'a> {
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
    unsafe {
        let state = &mut *((*host).engine_ctx as *mut PipelineCallbackState);
        let inst_id_str = std::str::from_utf8_unchecked(std::slice::from_raw_parts(instance_id, instance_id_len));
        
        let float_stride = state.schema.fields.iter().map(|f| match f {
            SemanticField::Position2 => 2,
            SemanticField::Position3 => 3,
            SemanticField::ColorRgba => 4,
            SemanticField::UvQuad => 4,
            SemanticField::CustomFloat(_) => 1,
        }).sum::<usize>();

        if float_stride == 0 {
            eprintln!("[native] warning: float_stride is 0 for schema_id {}", schema_id);
            return;
        }

        let row_count = rows_data_len / float_stride;
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

        if let Err(e) = state.host_api.publish_artifact(inst_id_str.to_string(), artifact, state.host_api.epoch()) {
            eprintln!("[native] failed to publish artifact: {:?}", e);
        }
    }
}

unsafe extern "C" fn cb_get_param_f32(
    host: *mut PipelineHost,
    name: *const u8,
    name_len: usize,
    out: *mut f32,
) -> u8 {
    unsafe {
        let state = &*((*host).engine_ctx as *const PipelineCallbackState);
        let name_str = std::str::from_utf8_unchecked(std::slice::from_raw_parts(name, name_len));
        if let Some(ParamValue::F32(v)) = state.values.get(name_str) {
            *out = *v as f32;
            1
        } else {
            0
        }
    }
}

unsafe extern "C" fn cb_timing(
    host: *mut PipelineHost,
    dt: *mut f32,
    elapsed: *mut f32,
) {
    unsafe {
        let state = &*((*host).engine_ctx as *const PipelineCallbackState);
        if !dt.is_null() {
            *dt = state.dt;
        }
        if !elapsed.is_null() {
            *elapsed = state.elapsed;
        }
    }
}

// ---- NativePipelineBridge --------------------------------------------------

pub struct NativePipelineBridge {
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
    ) -> Self {
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
        );

        let host = Box::new(PipelineHost {
            engine_ctx: std::ptr::null_mut(),
            publish_instances: cb_publish_instances,
            get_param_f32: cb_get_param_f32,
            timing: cb_timing,
        });

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
            return;
        }

        let dt = host.dt();
        let elapsed = host.time();
        let mut cb_state = PipelineCallbackState {
            host_api: host,
            schema: &self.schema,
            values: &self.values,
            dt,
            elapsed,
        };

        self.host.engine_ctx = &mut cb_state as *mut PipelineCallbackState as *mut c_void;
        let result = self.runner.update();
        self.host.engine_ctx = std::ptr::null_mut();

        if let Err(e) = result {
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
