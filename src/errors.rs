//! Error type. `cli.rs` maps these to messages and exit codes.

use std::fmt;

pub const EXIT_OK: i32 = 0;
pub const EXIT_ERROR: i32 = 1;
pub const EXIT_USAGE: i32 = 2;
pub const EXIT_CANCELLED: i32 = 3;
pub const EXIT_CONFIG: i32 = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    /// Operational error; the message is shown to the user.
    Error,
    Usage,
    Cancelled,
    Config,
    /// A `wf run` command died by this signal; exits 128+N like a shell would.
    ScriptKilled(i32),
    Git,
    NotARepo,
    /// Ctrl-C reached the command we ran: we end the way it did, by SIGINT,
    /// so a shell loop around us aborts.
    Interrupted,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error {
    pub kind: ErrorKind,
    pub message: String,
}

pub type Result<T> = std::result::Result<T, Error>;

impl Error {
    pub fn new(message: impl Into<String>) -> Self {
        Self::of(ErrorKind::Error, message)
    }

    pub fn of(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self { kind, message: message.into() }
    }

    pub fn usage(message: impl Into<String>) -> Self {
        Self::of(ErrorKind::Usage, message)
    }

    pub fn cancelled(message: impl Into<String>) -> Self {
        Self::of(ErrorKind::Cancelled, message)
    }

    pub fn config(message: impl Into<String>) -> Self {
        Self::of(ErrorKind::Config, message)
    }

    pub fn git(message: impl Into<String>) -> Self {
        Self::of(ErrorKind::Git, message)
    }

    pub fn not_a_repo() -> Self {
        Self::of(ErrorKind::NotARepo, "Not inside a git repository")
    }

    pub fn interrupted() -> Self {
        Self::of(ErrorKind::Interrupted, "interrupted")
    }

    /// GitError and its NotARepoError subclass, in the Python hierarchy.
    pub fn is_git(&self) -> bool {
        matches!(self.kind, ErrorKind::Git | ErrorKind::NotARepo)
    }

    pub fn exit_code(&self) -> i32 {
        match self.kind {
            ErrorKind::Error | ErrorKind::Git | ErrorKind::NotARepo => EXIT_ERROR,
            ErrorKind::Usage => EXIT_USAGE,
            ErrorKind::Cancelled => EXIT_CANCELLED,
            ErrorKind::Config => EXIT_CONFIG,
            ErrorKind::ScriptKilled(signum) => 128 + signum,
            ErrorKind::Interrupted => 128 + nix::sys::signal::Signal::SIGINT as i32,
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Error {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_codes() {
        assert_eq!(Error::new("x").exit_code(), 1);
        assert_eq!(Error::git("x").exit_code(), 1);
        assert_eq!(Error::not_a_repo().exit_code(), 1);
        assert_eq!(Error::usage("x").exit_code(), 2);
        assert_eq!(Error::cancelled("x").exit_code(), 3);
        assert_eq!(Error::config("x").exit_code(), 4);
        assert_eq!(Error::of(ErrorKind::ScriptKilled(15), "x").exit_code(), 143);
        assert_eq!(Error::interrupted().exit_code(), 130);
    }

    #[test]
    fn not_a_repo_is_a_git_error_with_a_fixed_message() {
        let error = Error::not_a_repo();
        assert!(error.is_git());
        assert_eq!(error.to_string(), "Not inside a git repository");
        assert!(!Error::new("x").is_git());
    }
}
