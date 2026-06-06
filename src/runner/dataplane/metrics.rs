//! [`MetricsBridge`] — realizes `metrics()`, preserving the frozen Step-2.6 rule:
//! **addon-local snapshot only**. One bridge per addon (no cross-addon view), no
//! engine metrics, no realtime stream — a point-in-time `Copy` of this addon's
//! own counters, updated host-side by the supervisor.

use crate::runner::host::Metrics;

pub struct MetricsBridge {
    /// This addon's own metrics. Never another addon's, never the engine's.
    own: Metrics,
}

impl MetricsBridge {
    pub fn new() -> Self {
        Self {
            own: Metrics::default(),
        }
    }

    /// Host/supervisor updates this addon's metrics (cpu/mem/tick/fps/over_budget).
    pub fn update(&mut self, metrics: Metrics) {
        self.own = metrics;
    }

    /// Addon-local snapshot (a copy). No stream, no cross-addon data.
    pub fn snapshot(&self) -> Metrics {
        self.own
    }
}

impl Default for MetricsBridge {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_is_fresh_and_a_copy() {
        let mut m = MetricsBridge::new();
        assert_eq!(m.snapshot(), Metrics::default());
        m.update(Metrics {
            cpu: 12.5,
            memory_bytes: 1024,
            tick_us: 800.0,
            fps: 30.0,
            over_budget: 2,
        });
        let snap = m.snapshot();
        assert_eq!(snap.cpu, 12.5);
        assert_eq!(snap.over_budget, 2);
        // It's a copy — mutating the bridge afterwards doesn't change the taken snapshot.
        m.update(Metrics::default());
        assert_eq!(snap.cpu, 12.5, "prior snapshot is an independent copy");
        assert_eq!(m.snapshot(), Metrics::default(), "fresh read reflects the update");
    }

    #[test]
    fn each_bridge_is_isolated_to_one_addon() {
        // Two addons → two bridges → no shared/cross visibility.
        let mut a = MetricsBridge::new();
        let b = MetricsBridge::new();
        a.update(Metrics {
            fps: 144.0,
            ..Metrics::default()
        });
        assert_eq!(a.snapshot().fps, 144.0);
        assert_eq!(b.snapshot().fps, 0.0, "addon B never sees addon A's metrics");
    }
}
