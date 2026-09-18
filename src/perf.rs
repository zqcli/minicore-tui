//! Structural performance counters (spec §25.1).
//!
//! These counters exist to prove *structure* in tests — how many times a
//! layout was rebuilt, how many historical text bytes were cloned, whether a
//! tool lookup scanned all blocks — never to claim timing or RSS. Every
//! counter is incremented at the real execution point, so a test that reads
//! zero is evidence that the path was not taken, not that a constant was
//! returned.
//!
//! The counters are thread-local: the UI loop and each test thread count
//! independently, so parallel tests cannot pollute each other's snapshots.
//! The cost is one relaxed atomic increment per event.

use std::sync::atomic::{AtomicU64, Ordering};

/// One counter set per thread. Tests snapshot before/after one scenario.
#[derive(Debug, Default)]
pub struct PerfCounters {
    /// Times a durable/whole conversation layout was actually rebuilt (cache
    /// misses and live-tail composition), not cache hits.
    pub layout_calls: AtomicU64,
    /// Historical row bytes cloned into a freshly owned `Vec<Line>`.
    pub historical_text_bytes_cloned: AtomicU64,
    /// Owned transcript rows materialized for one prepared conversation.
    pub viewport_rows_materialized: AtomicU64,
    /// `Composer::content()` full joins (the buffer joined into a String).
    pub composer_full_joins: AtomicU64,
    /// Tool lookups served by the projection index.
    pub tool_index_lookups: AtomicU64,
    /// Tool lookups that fell back to scanning every block. Must stay zero in
    /// the projection path.
    pub tool_linear_scans: AtomicU64,
    /// Bytes currently retained by the shared history window (budget input).
    pub history_body_bytes: AtomicU64,
}

impl PerfCounters {
    pub const fn new() -> Self {
        Self {
            layout_calls: AtomicU64::new(0),
            historical_text_bytes_cloned: AtomicU64::new(0),
            viewport_rows_materialized: AtomicU64::new(0),
            composer_full_joins: AtomicU64::new(0),
            tool_index_lookups: AtomicU64::new(0),
            tool_linear_scans: AtomicU64::new(0),
            history_body_bytes: AtomicU64::new(0),
        }
    }

    pub fn reset(&self) {
        for counter in self.counters() {
            counter.store(0, Ordering::Relaxed);
        }
    }

    pub fn snapshot(&self) -> PerfSnapshot {
        PerfSnapshot {
            layout_calls: self.layout_calls.load(Ordering::Relaxed),
            historical_text_bytes_cloned: self.historical_text_bytes_cloned.load(Ordering::Relaxed),
            viewport_rows_materialized: self.viewport_rows_materialized.load(Ordering::Relaxed),
            composer_full_joins: self.composer_full_joins.load(Ordering::Relaxed),
            tool_index_lookups: self.tool_index_lookups.load(Ordering::Relaxed),
            tool_linear_scans: self.tool_linear_scans.load(Ordering::Relaxed),
            history_body_bytes: self.history_body_bytes.load(Ordering::Relaxed),
        }
    }

    fn counters(&self) -> [&AtomicU64; 7] {
        [
            &self.layout_calls,
            &self.historical_text_bytes_cloned,
            &self.viewport_rows_materialized,
            &self.composer_full_joins,
            &self.tool_index_lookups,
            &self.tool_linear_scans,
            &self.history_body_bytes,
        ]
    }
}

/// A plain snapshot of [`PerfCounters`] for assertions and reporting.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PerfSnapshot {
    pub layout_calls: u64,
    pub historical_text_bytes_cloned: u64,
    pub viewport_rows_materialized: u64,
    pub composer_full_joins: u64,
    pub tool_index_lookups: u64,
    pub tool_linear_scans: u64,
    pub history_body_bytes: u64,
}

/// The counter to touch, named so call sites cannot silently count the wrong
/// field.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Counter {
    LayoutCalls,
    HistoricalTextBytesCloned,
    ViewportRowsMaterialized,
    ComposerFullJoins,
    ToolIndexLookups,
    ToolLinearScans,
    HistoryBodyBytes,
}

thread_local! {
    static PERF: PerfCounters = const { PerfCounters::new() };
}

/// Runs `f` with this thread's counters.
pub fn with<R>(f: impl FnOnce(&PerfCounters) -> R) -> R {
    PERF.with(f)
}

/// This thread's current counters.
pub fn snapshot() -> PerfSnapshot {
    with(PerfCounters::snapshot)
}

/// Resets this thread's counters.
pub fn reset() {
    with(PerfCounters::reset);
}

/// Adds `delta` to `counter` at the real execution point.
pub fn add(counter: Counter, delta: u64) {
    if delta == 0 {
        return;
    }
    with(|perf| {
        let target = match counter {
            Counter::LayoutCalls => &perf.layout_calls,
            Counter::HistoricalTextBytesCloned => &perf.historical_text_bytes_cloned,
            Counter::ViewportRowsMaterialized => &perf.viewport_rows_materialized,
            Counter::ComposerFullJoins => &perf.composer_full_joins,
            Counter::ToolIndexLookups => &perf.tool_index_lookups,
            Counter::ToolLinearScans => &perf.tool_linear_scans,
            Counter::HistoryBodyBytes => &perf.history_body_bytes,
        };
        target.fetch_add(delta, Ordering::Relaxed);
    });
}

/// Increments one counter by one.
pub fn count(counter: Counter) {
    add(counter, 1);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counters_count_real_events_and_reset() {
        reset();
        count(Counter::LayoutCalls);
        add(Counter::ToolLinearScans, 2);
        let before_second = snapshot();
        assert_eq!(before_second.layout_calls, 1);
        assert_eq!(before_second.tool_linear_scans, 2);
        assert_eq!(before_second.composer_full_joins, 0);
        reset();
        assert_eq!(snapshot(), PerfSnapshot::default());
    }
}
