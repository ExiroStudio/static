use std::collections::HashMap;
use std::sync::RwLock;
use crate::runtime::driver::DriverBox;

/// Factory function type for creating a new driver instance.
pub type DriverFactory = Box<dyn Fn() -> DriverBox + Send + Sync>;

/// Central registry for runtime drivers.
/// Allows registering new execution backends at startup.
pub struct RuntimeRegistry {
    factories: RwLock<HashMap<String, DriverFactory>>,
}

impl RuntimeRegistry {
    pub fn new() -> Self {
        Self {
            factories: RwLock::new(HashMap::new()),
        }
    }

    /// Register a new runtime driver factory.
    pub fn register(&self, name: &str, factory: DriverFactory) {
        let mut lock = self.factories.write().unwrap();
        lock.insert(name.to_string(), factory);
    }

    /// Create a new driver instance of the requested type.
    pub fn create(&self, name: &str) -> Option<DriverBox> {
        let lock = self.factories.read().unwrap();
        lock.get(name).map(|f| f())
    }

    /// List all supported runtime kinds.
    pub fn supported_kinds(&self) -> Vec<String> {
        let lock = self.factories.read().unwrap();
        lock.keys().cloned().collect()
    }
}

impl Default for RuntimeRegistry {
    fn default() -> Self {
        Self::new()
    }
}
