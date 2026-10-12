//! Workforest — git worktree forest management.

pub mod cli;
pub mod commands;
pub mod completions;
pub mod config;
pub mod errors;
pub mod git;
pub mod hooks;
pub mod integrations;
pub mod jobs;
pub mod launch;
pub mod makefile;
pub mod output;
pub mod shellinit;
pub mod tui;
pub mod util;

#[cfg(test)]
mod testing;
