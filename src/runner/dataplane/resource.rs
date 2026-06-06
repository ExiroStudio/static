//! [`ResourceBridge`] — realizes `request_resource`/`release_resource`,
//! preserving the frozen Step-2.6 resource semantics.
//!
//! * host-owned; the addon only ever holds an **opaque** [`ResourceHandle`].
//! * **lazy** + content-addressed by id: the same id returns the same handle
//!   while live (refcounted, shared).
//! * `release` is **idempotent** (releasing twice / an unknown handle is a no-op).
//! * a reload bumps the **epoch**, invalidating all handles.
//!
//! No fd, path, or mmap ownership ever crosses — only the `u32` handle.

use std::collections::HashMap;

use crate::runner::host::ResourceHandle;

pub struct ResourceBridge {
    next: u32,
    epoch: u32,
    /// id → (handle, refcount). Content-addressed cache.
    by_id: HashMap<String, (ResourceHandle, u32)>,
    /// handle → id (reverse lookup for release).
    by_handle: HashMap<u32, String>,
}

impl ResourceBridge {
    pub fn new() -> Self {
        Self {
            next: 1, // 0 reserved as "never issued"
            epoch: 0,
            by_id: HashMap::new(),
            by_handle: HashMap::new(),
        }
    }

    /// Lazily resolve `id` to an opaque handle; identical ids share a handle
    /// (refcount++). The host owns the underlying resource.
    pub fn request(&mut self, id: &str) -> Option<ResourceHandle> {
        if let Some((handle, rc)) = self.by_id.get_mut(id) {
            *rc += 1;
            return Some(*handle);
        }
        let handle = ResourceHandle(self.next);
        self.next += 1;
        self.by_id.insert(id.to_string(), (handle, 1));
        self.by_handle.insert(handle.0, id.to_string());
        Some(handle)
    }

    /// Idempotent release. Drops one reference; frees the entry at zero. An
    /// unknown/stale handle is a no-op.
    pub fn release(&mut self, handle: ResourceHandle) {
        let Some(id) = self.by_handle.get(&handle.0).cloned() else {
            return; // unknown/already-released → no-op
        };
        if let Some((_, rc)) = self.by_id.get_mut(&id) {
            *rc -= 1;
            if *rc == 0 {
                self.by_id.remove(&id);
                self.by_handle.remove(&handle.0);
            }
        }
    }

    /// Live reference count for `id` (diagnostics/tests).
    pub fn refcount(&self, id: &str) -> u32 {
        self.by_id.get(id).map(|(_, rc)| *rc).unwrap_or(0)
    }

    pub fn epoch(&self) -> u32 {
        self.epoch
    }

    /// Reload: invalidate every handle (epoch bump). Addons must re-request.
    pub fn invalidate(&mut self) {
        self.epoch += 1;
        self.by_id.clear();
        self.by_handle.clear();
    }
}

impl Default for ResourceBridge {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_id_shares_a_handle_and_refcounts() {
        let mut r = ResourceBridge::new();
        let h1 = r.request("model.onnx").unwrap();
        let h2 = r.request("model.onnx").unwrap();
        assert_eq!(h1, h2, "content-addressed: same id → same handle");
        assert_eq!(r.refcount("model.onnx"), 2);
        let other = r.request("weights.bin").unwrap();
        assert_ne!(other, h1);
    }

    #[test]
    fn release_is_idempotent() {
        let mut r = ResourceBridge::new();
        let h = r.request("m").unwrap();
        r.request("m").unwrap(); // rc = 2
        r.release(h);
        assert_eq!(r.refcount("m"), 1, "one ref remains");
        r.release(h);
        assert_eq!(r.refcount("m"), 0, "freed at zero");
        // Idempotent: releasing again, and an unknown handle, are no-ops.
        r.release(h);
        r.release(ResourceHandle(9999));
        assert_eq!(r.refcount("m"), 0);
    }

    #[test]
    fn reload_invalidates_all_handles() {
        let mut r = ResourceBridge::new();
        let _ = r.request("m").unwrap();
        assert_eq!(r.refcount("m"), 1);
        r.invalidate();
        assert_eq!(r.epoch(), 1);
        assert_eq!(r.refcount("m"), 0, "handles invalid after reload");
        // A fresh request after reload works.
        assert!(r.request("m").is_some());
    }
}
