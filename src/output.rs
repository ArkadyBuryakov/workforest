//! Human-facing output and interaction.
//!
//! Everything here writes to stderr: stdout is reserved for machine output
//! (shell directives, porcelain listings, completions) and is owned by
//! `cli.rs`.

use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::env;
use std::io::{self, IsTerminal, Write};

use nix::sys::signal::{SaFlags, SigAction, SigHandler, SigSet, Signal, sigaction};

use crate::errors::{Error, Result};
use crate::util::repr;

const RED: &str = "\x1b[0;31m";
const GREEN: &str = "\x1b[0;32m";
const YELLOW: &str = "\x1b[0;33m";
const RESET: &str = "\x1b[0m";

// All output comes from the thread that runs the command (worker threads
// only spawn git), so the per-process state is per-thread: under `cargo
// test` every test then has its own, like the process it stands for.
thread_local! {
    static WARNED: RefCell<HashSet<String>> = RefCell::new(HashSet::new());
    static QUIET: Cell<bool> = const { Cell::new(false) };
    #[cfg(test)]
    static CAPTURED: RefCell<Option<String>> = const { RefCell::new(None) };
}

/// The NO_COLOR / CLICOLOR_FORCE / isatty policy for everything we color;
/// stderr is the stream that matters, since that is where we write.
pub fn colors_enabled() -> bool {
    color_policy(
        env::var_os("NO_COLOR").is_some_and(|v| !v.is_empty()),
        env::var_os("CLICOLOR_FORCE").is_some_and(|v| !v.is_empty()),
        io::stderr().is_terminal(),
    )
}

fn color_policy(no_color: bool, force: bool, tty: bool) -> bool {
    if no_color {
        return false;
    }
    force || tty
}

fn write_raw(text: &str) {
    #[cfg(test)]
    {
        let captured = CAPTURED.with_borrow_mut(|captured| match captured {
            Some(buffer) => {
                buffer.push_str(text);
                true
            }
            None => false,
        });
        if captured {
            return;
        }
    }
    let mut stderr = io::stderr();
    let _ = stderr.write_all(text.as_bytes());
    let _ = stderr.flush();
}

fn emit(text: &str, color: &str) {
    if colors_enabled() {
        write_raw(&format!("{color}{text}{RESET}\n"));
    } else {
        write_raw(&format!("{text}\n"));
    }
}

pub fn info(text: &str) {
    write_raw(&format!("{text}\n"));
}

pub fn success(text: &str) {
    emit(text, GREEN);
}

pub fn warn(text: &str) {
    if !QUIET.get() {
        emit(text, YELLOW);
    }
}

/// `warn()`, unless this process has said exactly this already — for what
/// is noticed on every pass over the same input (some commands load the
/// configuration more than once).
pub fn warn_once(text: &str) {
    if WARNED.with_borrow_mut(|warned| warned.insert(text.to_string())) {
        warn(text);
    }
}

/// Drop every warning from here on. For `--complete`: its stderr lands in
/// the middle of the line the user is typing, and a warning is no reason
/// to offer no candidates.
pub fn quiet() {
    QUIET.set(true);
}

pub fn error(text: &str) {
    if colors_enabled() {
        write_raw(&format!("{RED}Error:{RESET} {text}\n"));
    } else {
        write_raw(&format!("Error: {text}\n"));
    }
}

/// True when we may prompt the user.
pub fn interactive() -> bool {
    io::stdin().is_terminal() && io::stderr().is_terminal()
}

/// What reading one line of an answer came to.
#[derive(Debug, PartialEq, Eq)]
pub enum Answer {
    Line(String),
    /// Ctrl-D.
    Eof,
    /// Ctrl-C.
    Interrupted,
}

extern "C" fn note_interrupt(_signum: i32) {}

/// One line from stdin. Ctrl-C must cancel the question instead of killing
/// us mid-prompt, so SIGINT is caught — without SA_RESTART, which is what
/// lets it interrupt the read — for as long as we wait.
fn read_answer() -> Answer {
    let action =
        SigAction::new(SigHandler::Handler(note_interrupt), SaFlags::empty(), SigSet::empty());
    // SAFETY: the handler does nothing, which is async-signal-safe.
    let previous = unsafe { sigaction(Signal::SIGINT, &action) }.ok();
    let mut line = Vec::new();
    let mut byte = [0u8; 1];
    let answer = loop {
        match nix::unistd::read(io::stdin(), &mut byte) {
            Ok(0) if line.is_empty() => break Answer::Eof,
            Ok(0) => break Answer::Line(String::from_utf8_lossy(&line).into_owned()),
            Ok(_) if byte[0] == b'\n' => {
                break Answer::Line(String::from_utf8_lossy(&line).into_owned());
            }
            Ok(_) => line.push(byte[0]),
            Err(nix::errno::Errno::EINTR) => break Answer::Interrupted,
            Err(_) => break Answer::Eof,
        }
    };
    if let Some(previous) = previous {
        // SAFETY: restores the disposition that was in place before.
        let _ = unsafe { sigaction(Signal::SIGINT, &previous) };
    }
    answer
}

/// Ask for a line of free-text input on the terminal.
///
/// Callers must check `interactive()` first and provide a non-interactive
/// path (an error with guidance) of their own.
pub fn ask(question: &str) -> Result<String> {
    ask_with(question, read_answer)
}

fn ask_with(question: &str, read: impl FnOnce() -> Answer) -> Result<String> {
    write_raw(&format!("{question} "));
    match read() {
        Answer::Line(line) => Ok(line.trim().to_string()),
        Answer::Eof | Answer::Interrupted => {
            write_raw("\n");
            Err(Error::cancelled("cancelled"))
        }
    }
}

/// Ask a y/N question on the terminal.
///
/// Fails as cancelled when there is no terminal to ask on — callers that
/// support an explicit flag (--force) must check `interactive()` first and
/// take that path instead. Ctrl-C aborts the whole command (it never means
/// "no"); Ctrl-D declines.
pub fn confirm(question: &str) -> Result<bool> {
    confirm_with(question, interactive(), read_answer)
}

fn confirm_with(question: &str, interactive: bool, read: impl FnOnce() -> Answer) -> Result<bool> {
    if !interactive {
        return Err(Error::cancelled(format!(
            "cannot prompt ({}): not a terminal; use --force",
            repr(question)
        )));
    }
    write_raw(&format!("{question} [y/N] "));
    match read() {
        Answer::Line(line) => Ok(matches!(line.trim().to_lowercase().as_str(), "y" | "yes")),
        Answer::Eof => {
            write_raw("\n");
            Ok(false)
        }
        Answer::Interrupted => {
            write_raw("\n");
            Err(Error::cancelled("cancelled"))
        }
    }
}

/// Collect what the closure writes instead of sending it to stderr.
#[cfg(test)]
pub fn capture<T>(body: impl FnOnce() -> T) -> (T, String) {
    let outer = CAPTURED.replace(Some(String::new()));
    let result = body();
    let text = CAPTURED.replace(outer).unwrap_or_default();
    (result, text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::errors::ErrorKind;

    fn line(text: &str) -> impl FnOnce() -> Answer {
        let text = text.to_string();
        move || Answer::Line(text)
    }

    #[test]
    fn confirm_yes() {
        let (answer, shown) = capture(|| confirm_with("Delete?", true, line("y")));
        assert_eq!(answer, Ok(true));
        assert_eq!(shown, "Delete? [y/N] ");
        assert_eq!(capture(|| confirm_with("Delete?", true, line(" YES "))).0, Ok(true));
    }

    #[test]
    fn confirm_empty_answer_declines() {
        assert_eq!(capture(|| confirm_with("Delete?", true, line(""))).0, Ok(false));
        assert_eq!(capture(|| confirm_with("Delete?", true, line("yep"))).0, Ok(false));
    }

    #[test]
    fn confirm_ctrl_c_aborts_the_command_not_just_the_question() {
        let (answer, shown) = capture(|| confirm_with("Delete?", true, || Answer::Interrupted));
        assert_eq!(answer.unwrap_err().kind, ErrorKind::Cancelled);
        assert!(shown.ends_with('\n'));
    }

    #[test]
    fn confirm_ctrl_d_declines() {
        assert_eq!(capture(|| confirm_with("Delete?", true, || Answer::Eof)).0, Ok(false));
    }

    #[test]
    fn confirm_without_a_terminal_fails_with_the_force_hint() {
        let error = confirm_with("Delete?", false, || unreachable!()).unwrap_err();
        assert_eq!(error.kind, ErrorKind::Cancelled);
        assert_eq!(error.message, "cannot prompt ('Delete?'): not a terminal; use --force");
    }

    #[test]
    fn ask_returns_the_stripped_line() {
        let (answer, shown) = capture(|| ask_with("Name?", line("  answer  ")));
        assert_eq!(answer.unwrap(), "answer");
        assert_eq!(shown, "Name? ");
    }

    #[test]
    fn ask_ctrl_c_and_ctrl_d_cancel() {
        for answer in [Answer::Interrupted, Answer::Eof] {
            let error = capture(|| ask_with("Name?", || answer)).0.unwrap_err();
            assert_eq!(error.kind, ErrorKind::Cancelled);
            assert_eq!(error.message, "cancelled");
        }
    }

    #[test]
    fn color_policy_prefers_no_color() {
        assert!(!color_policy(true, true, true));
        assert!(color_policy(false, true, false));
        assert!(color_policy(false, false, true));
        assert!(!color_policy(false, false, false));
    }

    #[test]
    fn warn_once_says_a_thing_once() {
        let ((), shown) = capture(|| {
            warn_once("a");
            warn_once("a");
            warn_once("b");
        });
        assert_eq!(shown.matches('a').count(), 1);
        assert_eq!(shown.matches('b').count(), 1);
    }

    #[test]
    fn quiet_drops_warnings_only() {
        let ((), shown) = capture(|| {
            quiet();
            warn("hidden");
            warn_once("hidden too");
            info("plain");
        });
        assert_eq!(shown, "plain\n");
    }

    #[test]
    fn levels_write_their_text() {
        let ((), shown) = capture(|| {
            success("ok");
            error("bad");
        });
        // Under `cargo test` colors depend on the terminal: compare the text.
        let plain = shown.replace(GREEN, "").replace(RED, "").replace(RESET, "");
        assert_eq!(plain, "ok\nError: bad\n");
    }
}
