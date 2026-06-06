//! [`FrameBridge`] — realizes `request_frame`/`change_frame_tier`/`read_frame`,
//! preserving the frozen Step-2.6 frame semantics.
//!
//! * `read_frame` — **borrow** of the host-owned latest frame, **pinned for the
//!   tick** (multiple reads return the *same* frame), **latest snapshot** across
//!   ticks. No copy of pixel buffers, no ownership transfer, no buffering, no GPU
//!   objects (only `FrameRef` metadata crosses).
//! * tier subscription is an atomic select; the host owns/resizes all tiers.
//!
//! Step 4 carries `FrameRef` **metadata only** (the pixel-view per transport is
//! later); `width` doubles as a sequence discriminator in tests.

use crate::runner::host::{FrameRef, FrameTier};

pub struct FrameBridge {
    tier: FrameTier,
    /// Newest frame the host has published for the current tier.
    latest: Option<FrameRef>,
    /// The frame pinned at tick start — what `read_frame` returns all tick.
    pinned: Option<FrameRef>,
}

impl FrameBridge {
    pub fn new() -> Self {
        Self {
            tier: FrameTier::R320x180,
            latest: None,
            pinned: None,
        }
    }

    /// Subscribe to a tier (host grants; the host owns the buffers).
    pub fn request_frame(&mut self, tier: FrameTier) -> bool {
        self.tier = tier;
        true
    }
    /// Lock-free tier switch (the host pre-allocates tiers).
    pub fn change_frame_tier(&mut self, tier: FrameTier) -> bool {
        self.tier = tier;
        true
    }
    pub fn tier(&self) -> FrameTier {
        self.tier
    }

    /// Host-side feed: the newest downscaled frame for the current tier.
    pub fn push(&mut self, width: u32, height: u32) {
        self.latest = Some(FrameRef {
            width,
            height,
            tier: self.tier,
        });
    }

    /// Pin the latest at tick start — read_frame is consistent for the whole tick.
    pub fn begin_tick(&mut self) {
        self.pinned = self.latest;
    }

    /// Borrow the pinned latest frame (metadata). Same value for repeated reads
    /// within a tick; `None` if none has been published.
    pub fn read_frame(&self) -> Option<FrameRef> {
        self.pinned
    }
}

impl Default for FrameBridge {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn multiple_reads_in_a_tick_return_the_same_frame() {
        let mut f = FrameBridge::new();
        f.request_frame(FrameTier::R320x180);
        f.push(100, 90); // host publishes frame #100
        f.begin_tick();
        let a = f.read_frame();
        let b = f.read_frame();
        assert_eq!(a, b, "repeated reads → identical pinned frame");
        assert_eq!(a.unwrap().width, 100);
    }

    #[test]
    fn latest_wins_only_after_the_next_tick() {
        let mut f = FrameBridge::new();
        f.push(100, 90);
        f.begin_tick();
        assert_eq!(f.read_frame().unwrap().width, 100);
        // Host pushes a newer frame mid-tick — read stays pinned to #100.
        f.push(200, 90);
        assert_eq!(f.read_frame().unwrap().width, 100, "pinned within the tick");
        // Next tick samples the latest.
        f.begin_tick();
        assert_eq!(f.read_frame().unwrap().width, 200, "latest-wins across ticks");
    }

    #[test]
    fn no_frame_yet_is_none_and_tier_select_is_lock_free() {
        let mut f = FrameBridge::new();
        f.begin_tick();
        assert!(f.read_frame().is_none());
        assert!(f.change_frame_tier(FrameTier::R160x90));
        assert_eq!(f.tier(), FrameTier::R160x90);
    }
}
