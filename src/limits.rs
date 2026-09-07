//! Bounds from `docs/api.md` §2.2. Every one of them is enforced, not advisory.

pub const MAX_BODY_BYTES: usize = 2 * 1024 * 1024;
pub const MAX_IMPORT_BYTES: usize = 64 * 1024 * 1024;
pub const MAX_PACKAGE_RECORDS: usize = 500;
pub const MAX_SLOTS: usize = 64;
pub const MAX_ROLES: usize = 8;
pub const MAX_TAGS: usize = 32;
pub const MAX_BODY_CHARS: usize = 65_536;
pub const MAX_CAPTURE_CHARS: usize = 262_144;
pub const MAX_PREFETCH_RECORDS: usize = 5_000;
pub const MAX_SNAPSHOT_NODES: usize = 5_000;
pub const DEFAULT_SNAPSHOT_NODES: usize = 500;
pub const MAX_SEARCH_LIMIT: usize = 200;
pub const DEFAULT_SEARCH_LIMIT: usize = 20;
pub const MAX_LIST_LIMIT: usize = 1_000;
pub const DEFAULT_LIST_LIMIT: usize = 200;
pub const MAX_CAPTURE_LIST_LIMIT: usize = 200;
pub const DEFAULT_CAPTURE_LIST_LIMIT: usize = 50;
pub const MAX_EMBEDDING_DIM: usize = 4_096;
pub const MAX_EXPORT_RECORDS: usize = 5_000;

pub fn clamp(requested: Option<usize>, default: usize, max: usize) -> usize {
    requested.unwrap_or(default).clamp(1, max)
}
