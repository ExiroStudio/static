//! The core engine: a minimal realtime signal runtime.
//!
//! The engine owns the GPU context and the webcam *source*, and drives a
//! [`PipelineRuntime`]. It does **not** own any specific look: the render chain
//! is loaded from `pipeline.json` and executed as
//!
//! ```text
//!   webcam (source) ──▶ pipeline nodes ──▶ window (sink)
//! ```
//!
//! The DotRenderer and CRT are builtin *addons*, registered through the runtime
//! like any other. Nothing here special-cases either of them.

pub mod gpu;
pub mod image;
pub mod video;

use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use wgpu::*;
use winit::window::Window;

use crate::addon::pipeline::{PipelineConfig, SinkConfig, SourceConfig};
use crate::addons::{CrtAddon, DotRendererAddon};
use crate::camera::WebcamCapture;
use crate::runtime::{PipelineRuntime, SINK_WINDOW, SOURCE_WEBCAM};

use gpu::GpuContext;
use video::VideoTexture;

/// Where the engine looks for the pipeline definition, relative to the working
/// directory. Missing or invalid → the built-in default pipeline is used.
const PIPELINE_PATH: &str = "pipeline.json";

pub struct Engine {
    gpu: GpuContext,

    /// The webcam source: frames stream into this texture every frame.
    video: VideoTexture,

    runtime: PipelineRuntime,

    frame_buf: Vec<u8>,
    start: Instant,
}

impl Engine {
    pub async fn new(window: Arc<Window>, cam_width: u32, cam_height: u32) -> Self {
        let gpu = GpuContext::new(window).await;

        let video = VideoTexture::new(&gpu.device, cam_width, cam_height);

        let mut runtime =
            PipelineRuntime::new(&gpu.device, gpu.config.format, gpu.config.width, gpu.config.height);

        // Register builtin addons through the same path an external addon would
        // use. The runtime does not know what these *are* — only that they exist.
        runtime
            .register_builtin::<DotRendererAddon>()
            .expect("register dot-renderer");
        runtime
            .register_builtin::<CrtAddon>()
            .expect("register crt");

        // The webcam is the source; hand its view to the runtime.
        runtime.set_source(&gpu.device, &video.view);

        // Load the pipeline definition, validate it against the registry, and
        // build the runtime pipeline — all before the first frame renders.
        let config = load_pipeline_config();
        runtime
            .build(&gpu.device, &config)
            .expect("failed to build pipeline from pipeline.json");

        Self {
            gpu,
            video,
            runtime,
            frame_buf: Vec::new(),
            start: Instant::now(),
        }
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        self.gpu.resize(width, height);
        self.runtime
            .resize(&self.gpu.device, self.gpu.config.width, self.gpu.config.height);
    }

    pub fn render(&mut self, camera: &WebcamCapture) {
        // Stream the newest webcam frame to the GPU (the source).
        if camera.copy_latest(&mut self.frame_buf) && !self.frame_buf.is_empty() {
            self.video.upload(&self.gpu.queue, &self.frame_buf);
        }

        let frame = match self.gpu.surface.get_current_texture() {
            Ok(frame) => frame,
            Err(SurfaceError::Lost | SurfaceError::Outdated) => {
                self.gpu.surface.configure(&self.gpu.device, &self.gpu.config);
                return;
            }
            Err(_) => return,
        };
        let view = frame.texture.create_view(&TextureViewDescriptor::default());

        let time = self.start.elapsed().as_secs_f32();
        self.runtime
            .render(&self.gpu.device, &self.gpu.queue, &view, time);

        frame.present();
    }
}

/// Load `pipeline.json`, falling back to the built-in default pipeline if it is
/// missing or unreadable. Structural problems in an existing file are surfaced
/// (panic) rather than silently swallowed, since the user clearly intended to
/// drive the pipeline from disk.
fn load_pipeline_config() -> PipelineConfig {
    let path = Path::new(PIPELINE_PATH);
    if path.exists() {
        match PipelineConfig::load(path) {
            Ok(config) => {
                println!("[engine] loaded pipeline from {PIPELINE_PATH}");
                return config;
            }
            Err(e) => panic!("[engine] failed to load {PIPELINE_PATH}: {e}"),
        }
    }
    println!("[engine] no {PIPELINE_PATH} found — using default pipeline");
    default_pipeline()
}

/// The default pipeline: webcam → dot-renderer → crt → window.
fn default_pipeline() -> PipelineConfig {
    let mut config = PipelineConfig::new(
        SourceConfig {
            kind: SOURCE_WEBCAM.into(),
            config: serde_json::Value::Object(Default::default()),
        },
        SinkConfig {
            kind: SINK_WINDOW.into(),
            config: serde_json::Value::Object(Default::default()),
        },
    );
    config.add_node("dot-renderer", None);
    config.add_node("crt", None);
    config
}
