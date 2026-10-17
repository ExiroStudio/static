use std::collections::HashMap;
use std::sync::Arc;
use crate::output::types::{FrameRequestKey, FramePacket, PooledBuffer};

pub struct FrameDistributionService {
    cache: HashMap<FrameRequestKey, Arc<FramePacket>>,
}

impl FrameDistributionService {
    pub fn new() -> Self {
        Self {
            cache: HashMap::new(),
        }
    }

    pub fn clear_cache(&mut self) {
        self.cache.clear();
    }

    pub fn request_frame(&mut self, key: FrameRequestKey) -> Arc<FramePacket> {
        if let Some(packet) = self.cache.get(&key) {
            return packet.clone();
        }

        let new_packet = match key.memory_type {
            crate::output::types::MemoryLocation::Cpu => {
                let buffer = PooledBuffer { data: vec![0; 1024] }; 
                Arc::new(FramePacket::Cpu {
                    pixels: Arc::new(buffer),
                    format: key.pixel_format,
                    frame_epoch: key.frame_epoch,
                })
            }
            crate::output::types::MemoryLocation::GpuZeroCopy => {
                Arc::new(FramePacket::Gpu {
                    handle: crate::output::types::NativeHandle {
                        backend_kind: crate::output::types::BackendKind::VulkanExport,
                        resource_id: std::num::NonZeroU64::new(1).unwrap(),
                        sync_metadata: 0,
                    },
                    format: key.pixel_format,
                    frame_epoch: key.frame_epoch,
                })
            }
        };

        self.cache.insert(key, new_packet.clone());
        new_packet
    }
}
