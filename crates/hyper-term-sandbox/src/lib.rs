//! Operating-system enforcement backends for explicit Agent operations.
//!
//! This crate is intentionally separate from the PTY and renderer layers. A
//! backend either compiles an enforced launch plan or fails closed; it never
//! silently falls back to an ordinary process spawn.

mod lima;
mod macos;
mod worktree;

pub use lima::*;
pub use macos::*;
pub use worktree::*;
