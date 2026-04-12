//! Cache v3: content-addressable, repo-identity-aware index cache.
//!
//! Two-layer design:
//!   - Layer A (global per user): content store keyed by `ContentHash`.
//!   - Layer B (per worktree): SQLite-WAL manifest mapping rel_path → ContentHash.
//!
//! See `docs/cache-v3-design.md` for the full design.

pub mod content_store;
pub mod hash;
pub mod identity;
pub mod layout;
pub mod manifest;
pub mod metrics;
pub mod registry;
pub mod state;
pub mod types;

pub use hash::word_hash_fnv1a;

pub use content_store::ContentStore;
pub use identity::{detect_identity, RepoIdentity, WorktreeKey};
pub use manifest::{Manifest, ManifestRow};
pub use metrics::ScanMetrics;
pub use registry::{RegisteredWorktree, Registry};
pub use state::{build_row, CacheOps, CacheState};
pub use types::{ContentHash, FileUnit, ParserVersion};

/// Version tag stamped on every `FileUnit`. Bump when the tokenizer or
/// symbol-extraction output changes so old entries become cache-misses.
pub const PARSER_VERSION: ParserVersion = 1;
