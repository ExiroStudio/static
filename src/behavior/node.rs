//! The behavior↔engine execution contract.
//!
//! A behavior is anything implementing [`BehaviorNode`]: it reads the latest
//! source frame (CPU only) and publishes signals. It cannot reach the GPU, a
//! queue, a texture, the runtime, or `build()` — the context types simply do
//! not expose them, so misuse is unrepresentable rather than merely discouraged.
//!
//! The context exposes the full contract surface — `frame`, `publish`, `config`,
//! `timing`. The one builtin behavior (`time`) only uses `timing`, so the
//! frame/config accessors are `#[allow(dead_code)]` here; frame-consuming
//! behaviors (Phase 3) exercise the rest.

use crate::runtime::ResolvedConfig;
use crate::signal::{SignalId, SignalPublisher, SignalSchema, SignalValue};

/// A read-only, CPU-side view of the latest source frame. No GPU handles.
#[allow(dead_code)] // contract surface for frame-consuming behaviors (Phase 3)
pub struct FrameView<'a> {
    width: u32,
    height: u32,
    rgba: &'a [u8],
}

#[allow(dead_code)] // accessors used by frame-consuming behaviors (Phase 3)
impl<'a> FrameView<'a> {
    pub(crate) fn new(width: u32, height: u32, rgba: &'a [u8]) -> Self {
        Self {
            width,
            height,
            rgba,
        }
    }

    pub fn width(&self) -> u32 {
        self.width
    }
    pub fn height(&self) -> u32 {
        self.height
    }
    /// Tightly-packed RGBA8, `width * height * 4` bytes.
    pub fn rgba(&self) -> &[u8] {
        self.rgba
    }
}

/// Per-tick timing handed to every behavior.
#[derive(Clone, Copy, Debug)]
pub struct Timing {
    /// Seconds since the previous tick.
    #[allow(dead_code)] // contract surface; `time` uses only `elapsed`
    pub dt: f32,
    /// Seconds since the runtime started.
    pub elapsed: f32,
}

/// Context at `start`: resolve published signal names to ids and read config.
/// No frame, no publish, no GPU.
pub struct BehaviorStartCtx<'a> {
    schema: &'a SignalSchema,
    config: ResolvedConfig<'a>,
}

impl<'a> BehaviorStartCtx<'a> {
    pub(crate) fn new(schema: &'a SignalSchema, config: ResolvedConfig<'a>) -> Self {
        Self { schema, config }
    }

    /// The signal schema — resolve published names to a [`SignalId`] once here.
    pub fn schema(&self) -> &SignalSchema {
        self.schema
    }

    #[allow(dead_code)] // contract surface; `time` resolves only via `schema`
    pub fn config(&self) -> &ResolvedConfig<'a> {
        &self.config
    }
}

/// Per-`update` context. Exposes ONLY frame, publish, config, timing.
pub struct BehaviorCtx<'a> {
    frame: Option<FrameView<'a>>,
    publisher: &'a mut SignalPublisher,
    config: ResolvedConfig<'a>,
    timing: Timing,
}

impl<'a> BehaviorCtx<'a> {
    pub(crate) fn new(
        frame: Option<FrameView<'a>>,
        publisher: &'a mut SignalPublisher,
        config: ResolvedConfig<'a>,
        timing: Timing,
    ) -> Self {
        Self {
            frame,
            publisher,
            config,
            timing,
        }
    }

    /// The latest source frame, if one has been captured.
    #[allow(dead_code)] // contract surface for frame-consuming behaviors (Phase 3)
    pub fn frame(&self) -> Option<&FrameView<'a>> {
        self.frame.as_ref()
    }

    /// Stage a signal for this tick. The scheduler commits all staged signals
    /// atomically once per tick — behaviors never trigger a buffer swap.
    pub fn publish(&mut self, id: SignalId, value: SignalValue) {
        self.publisher.set(id, value);
    }

    #[allow(dead_code)] // contract surface; `time` uses only `timing`
    pub fn config(&self) -> &ResolvedConfig<'a> {
        &self.config
    }

    pub fn timing(&self) -> Timing {
        self.timing
    }
}

/// A live behavior. Owned and driven entirely by the behavior thread.
///
/// `Send` so instances can be constructed on the engine thread and moved onto
/// the behavior thread (and across the reload channel).
pub trait BehaviorNode: Send {
    /// Resolve published signal ids and load any resources. Runs on the
    /// behavior thread, so model/asset loading never blocks rendering.
    fn start(&mut self, ctx: &mut BehaviorStartCtx);

    /// Analyze the frame and publish signals. Called every tick while enabled.
    fn update(&mut self, ctx: &mut BehaviorCtx);

    /// Release resources. Called on reload and shutdown.
    fn stop(&mut self);
}
