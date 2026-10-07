//! The terminal side of the interactive mode: the alternate screen, raw
//! keys, and the loop between them and the [`App`].
//!
//! Drawn on stderr: stdout belongs to the shell wrapper, which is waiting
//! there for the directive the picked command prints.

use std::io::{self, Stderr};

use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::event::{self, Event, KeyEventKind};
use ratatui::crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::crossterm::{cursor, execute};

use super::app::{App, Mode, Row, Step};
use super::view;
use crate::errors::{Error, Result};

/// The screen while the interface is up; whatever was there comes back
/// when this goes — on every way out, an error included.
struct Screen {
    terminal: Terminal<CrosstermBackend<Stderr>>,
}

impl Screen {
    fn enter() -> io::Result<Self> {
        enable_raw_mode()?;
        let mut stderr = io::stderr();
        if let Err(error) = execute!(stderr, EnterAlternateScreen) {
            let _ = disable_raw_mode();
            return Err(error);
        }
        Ok(Self { terminal: Terminal::new(CrosstermBackend::new(stderr))? })
    }
}

impl Drop for Screen {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(io::stderr(), LeaveAlternateScreen, cursor::Show);
    }
}

fn terminal_error(error: io::Error) -> Error {
    Error::new(format!("the TUI lost its terminal: {error}"))
}

/// Show the interface until something is picked (its name) or the user
/// leaves (None). `load` gives a mode's rows; it runs for the mode the
/// app starts in, and again whenever the mode changes.
pub fn interact(
    app: &mut App,
    load: &mut dyn FnMut(Mode) -> Result<Vec<Row>>,
) -> Result<Option<String>> {
    app.set_rows(load(app.mode())?);
    let mut screen = Screen::enter().map_err(terminal_error)?;
    loop {
        screen.terminal.draw(|frame| view::draw(frame, app)).map_err(terminal_error)?;
        let key = match event::read().map_err(terminal_error)? {
            Event::Key(key) if key.kind != KeyEventKind::Release => key,
            _ => continue, // a resize: draw again
        };
        match app.handle_key(key) {
            Step::Stay => {}
            Step::Reload => app.set_rows(load(app.mode())?),
            Step::Quit => return Ok(None),
            Step::Accept(name) => return Ok(Some(name)),
        }
    }
}
