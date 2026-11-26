//! Relocation of a clangd index from a donor worktree to a target worktree by rewriting absolute paths.

pub mod cdb_relocation;
pub mod donor_selection;
pub mod index_relocation;
pub mod path_map;
pub mod reindex_prediction;
pub mod shard_relocation;
pub mod string_map_order;
pub mod worktree_relocation;
