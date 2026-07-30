use std::sync::Arc;
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PixelFormat {
    Rgba8Unorm,
    Bgra8Unorm,
    Nv12,
    Yuy2,
    I420,
    Uyvy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ColorSpace {
    Srgb,
    Bt601,
    Bt709,
    Bt2020,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MemoryLocation {
    Cpu,
    GpuZeroCopy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FrameRequestKey {
    pub resolution: (u32, u32),
    pub pixel_format: PixelFormat,
    pub color_space: ColorSpace,
    pub memory_type: MemoryLocation,
    pub frame_epoch: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendKind {
    D3D11,
    DmaBuf,
    IoSurface,
    Metal,
    VulkanExport,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NativeHandle {
    pub backend_kind: BackendKind,
    pub resource_id: std::num::NonZeroU64,
    pub sync_metadata: u64,
}

pub struct PooledBuffer {
    pub data: Vec<u8>,
}

impl AsRef<[u8]> for PooledBuffer {
    fn as_ref(&self) -> &[u8] {
        &self.data
    }
}

pub enum FramePacket {
    Cpu {
        pixels: Arc<PooledBuffer>,
        format: PixelFormat,
        frame_epoch: u64,
    },
    Gpu {
        handle: NativeHandle,
        format: PixelFormat,
        frame_epoch: u64,
    },
}

#[derive(Debug)]
pub enum BackendError {
    InitFailed(String),
    UnsupportedFormat(PixelFormat),
    PermissionDenied(String),
    PlatformNotSupported,
    DeviceLost,
}

pub struct BackendConfig {
    pub width: u32,
    pub height: u32,
    pub format: PixelFormat,
}

pub trait OutputBackend {
    fn new(config: &BackendConfig) -> Result<Self, BackendError> where Self: Sized;
    fn start(&mut self) -> Result<(), BackendError>;
    fn submit_frame(&mut self, packet: Arc<FramePacket>, timestamp: Duration) -> Result<(), BackendError>;
    fn stop(&mut self) -> Result<(), BackendError>;
    fn cleanup(self) -> Result<(), BackendError>;
}
