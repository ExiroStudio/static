use std::sync::Arc;
use std::time::Duration;
use crate::output::types::{OutputBackend, BackendConfig, BackendError, FramePacket};

pub struct V4l2Backend {
    // fd: i32
}

impl OutputBackend for V4l2Backend {
    fn new(_config: &BackendConfig) -> Result<Self, BackendError> {
        Ok(Self {})
    }
    fn start(&mut self) -> Result<(), BackendError> { Ok(()) }
    fn submit_frame(&mut self, _packet: Arc<FramePacket>, _timestamp: Duration) -> Result<(), BackendError> { Ok(()) }
    fn stop(&mut self) -> Result<(), BackendError> { Ok(()) }
    fn cleanup(self) -> Result<(), BackendError> { Ok(()) }
}
