//! Relocates a clangd background index between git worktrees.

mod atomic_write;
pub mod cli;
pub mod format;
pub mod git;
pub mod relocate;
