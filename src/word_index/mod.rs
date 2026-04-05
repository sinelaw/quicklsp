//! On-disk word index for memory-efficient reference lookups.
//!
//! Append-only log format: index.log (single file, all data).

mod format;
pub mod log;
pub mod persistence;

pub use format::IndexEntry;
pub use log::{LogIndex, LogWriter, OccEntry, FileData};
pub use persistence::{IndexMeta, collect_file_mtimes, index_dir_for_project};
