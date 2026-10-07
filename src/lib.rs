//! Workforest — git worktree forest management.

pub mod config;
pub mod errors;
pub mod git;
pub mod jobs;
pub mod makefile;
pub mod output;
pub mod util;

#[cfg(test)]
mod testing;
