//! On-disk word index for memory-efficient reference lookups.
//!
//! Three-file format: words.v2.bin (string tables), files.v2.bin (per-file
//! occurrences), index.v2.bin (inverted posting lists).

mod format;
pub mod persistence;

pub use format::{IndexEntry, WordDirectory, WordIndex, WordIndexBuilder};
pub use persistence::{IndexMeta, collect_file_mtimes, index_dir_for_project};
