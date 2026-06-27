pub const HANDSHAKE_ABI_VERSION: u32 = 1;

// FourCC Constants
pub const BEHAVIOR_FAMILY: u32 = 0x42454841; // "BEHA"
pub const RENDER_FAMILY: u32 = 0x52454e44;   // "REND"

pub const BEHAVIOR_ABI_V1: u32 = 0x42414231; // "BAB1"
pub const RENDER_ABI_V1: u32 = 0x52414231;   // "RAB1"

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HandshakeData {
    pub runtime_family: u32,
    pub handshake_abi: u32,
    pub runtime_abi: u32,
}
