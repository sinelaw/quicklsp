//! On-disk word index for memory-efficient reference lookups.
//!
//! Stores all identifier occurrences in a sorted, seek-based index file.
//! Only the word directory (word → file offset) is kept in memory.
//! Reference lookups require one seek + sequential read of matching entries.

mod format;
pub mod persistence;

pub use format::{IndexEntry, WordDirectory, WordIndex, WordIndexBuilder};
pub use persistence::{
    IndexMeta, collect_file_mtimes, compute_content_hash, index_dir_for_project, index_filename,
};
