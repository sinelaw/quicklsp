//! Test-visible counters for objective-validation integration tests.
//!
//! Always compiled (not test-gated) so integration tests in `tests/` can
//! observe them through `Workspace::metrics()`. Cost is ~40 bytes per
//! `Workspace` and one relaxed atomic increment per counted event.

use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

#[derive(Default, Debug)]
pub struct ScanMetrics {
    pub files_stat_called: AtomicU64,
    pub files_bytes_read: AtomicU64,
    pub files_blake3_hashed: AtomicU64,
    pub files_parsed: AtomicU64,
    pub layer_a_writes: AtomicU64,
    pub layer_a_hits: AtomicU64,
    pub manifest_rows_copied: AtomicU64,
}

impl ScanMetrics {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn reset(&self) {
        self.files_stat_called.store(0, Relaxed);
        self.files_bytes_read.store(0, Relaxed);
        self.files_blake3_hashed.store(0, Relaxed);
        self.files_parsed.store(0, Relaxed);
        self.layer_a_writes.store(0, Relaxed);
        self.layer_a_hits.store(0, Relaxed);
        self.manifest_rows_copied.store(0, Relaxed);
    }

    pub fn snapshot(&self) -> MetricsSnapshot {
        MetricsSnapshot {
            files_stat_called: self.files_stat_called.load(Relaxed),
            files_bytes_read: self.files_bytes_read.load(Relaxed),
            files_blake3_hashed: self.files_blake3_hashed.load(Relaxed),
            files_parsed: self.files_parsed.load(Relaxed),
            layer_a_writes: self.layer_a_writes.load(Relaxed),
            layer_a_hits: self.layer_a_hits.load(Relaxed),
            manifest_rows_copied: self.manifest_rows_copied.load(Relaxed),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MetricsSnapshot {
    pub files_stat_called: u64,
    pub files_bytes_read: u64,
    pub files_blake3_hashed: u64,
    pub files_parsed: u64,
    pub layer_a_writes: u64,
    pub layer_a_hits: u64,
    pub manifest_rows_copied: u64,
}
