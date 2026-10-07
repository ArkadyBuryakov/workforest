//! Interactive mode (placeholder until the ratatui interface lands).

use crate::commands::Outcome;
use crate::errors::{Error, Result};

pub fn run(_initial_mode: Option<&str>) -> Result<Outcome> {
    Err(Error::new("the TUI is not available yet; all actions are available as plain subcommands"))
}
