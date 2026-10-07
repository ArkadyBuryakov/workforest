//! Small text helpers shared across modules. Several reproduce a Python
//! builtin exactly (`repr`, `shlex.quote`, `str.splitlines`,
//! `json.dumps`): messages and machine output are part of the interface.

use std::collections::BTreeMap;
use std::env;
use std::ffi::{OsStr, OsString};
use std::fs::{self, File, OpenOptions};
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;
use serde_json::ser::{CompactFormatter, Formatter, PrettyFormatter};

/// A string the way messages quote it: single quotes, unless it holds one
/// and no double quote; backslash escapes for what would not print.
pub fn repr(text: &str) -> String {
    let quote = if text.contains('\'') && !text.contains('"') { '"' } else { '\'' };
    let mut out = String::with_capacity(text.len() + 2);
    out.push(quote);
    for ch in text.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            c if c.is_control() || (c.is_whitespace() && c != ' ') => {
                let code = c as u32;
                if code < 0x100 {
                    out.push_str(&format!("\\x{code:02x}"));
                } else if code < 0x10000 {
                    out.push_str(&format!("\\u{code:04x}"));
                } else {
                    out.push_str(&format!("\\U{code:08x}"));
                }
            }
            c => out.push(c),
        }
    }
    out.push(quote);
    out
}

/// `repr` of a path's text.
pub fn repr_path(path: &Path) -> String {
    repr(&path.to_string_lossy())
}

/// One shell word: unchanged when it is made of safe characters only,
/// single-quoted otherwise.
pub fn shell_quote(text: &str) -> String {
    let safe = |c: char| c.is_ascii_alphanumeric() || "_@%+=:,./-".contains(c);
    if !text.is_empty() && text.chars().all(safe) {
        return text.to_string();
    }
    format!("'{}'", text.replace('\'', "'\"'\"'"))
}

pub fn shell_quote_path(path: &Path) -> String {
    shell_quote(&path.to_string_lossy())
}

/// Whitespace runs collapsed to one space: a lock reason may hold tabs
/// and newlines, and every line-oriented output is one record per line.
pub fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Lines without their terminators; every line boundary Python's
/// `str.splitlines` knows ends one.
pub fn splitlines(text: &str) -> Vec<&str> {
    let mut lines = Vec::new();
    let mut start = 0;
    let mut chars = text.char_indices().peekable();
    while let Some((index, ch)) = chars.next() {
        let is_break = matches!(
            ch,
            '\n' | '\r'
                | '\x0b'
                | '\x0c'
                | '\x1c'
                | '\x1d'
                | '\x1e'
                | '\u{85}'
                | '\u{2028}'
                | '\u{2029}'
        );
        if !is_break {
            continue;
        }
        lines.push(&text[start..index]);
        start = index + ch.len_utf8();
        if ch == '\r' && matches!(chars.peek(), Some((_, '\n'))) {
            chars.next();
            start += 1;
        }
    }
    if start < text.len() {
        lines.push(&text[start..]);
    }
    lines
}

/// A number of seconds the way `%g` prints it: no trailing `.0`.
pub fn format_seconds(value: f64) -> String {
    if value.fract() == 0.0 && value.abs() < 1e15 {
        format!("{}", value as i64)
    } else {
        format!("{value}")
    }
}

/// The last path component, or "" for a root.
pub fn file_name(path: &Path) -> String {
    path.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default()
}

/// A process environment as data: what a child gets, and what decisions
/// that depend on the environment (`$SHELL`, `$EDITOR`) are made from.
pub type Env = BTreeMap<OsString, OsString>;

pub fn current_env() -> Env {
    env::vars_os().collect()
}

/// A variable of `env` that is set and not empty.
pub fn env_get(env: &Env, name: &str) -> Option<String> {
    env.get(OsStr::new(name))
        .filter(|value| !value.is_empty())
        .map(|value| value.to_string_lossy().into_owned())
}

/// The shell config snippets run through: `$SHELL`, else `sh`.
pub fn user_shell(env: &Env) -> String {
    env_get(env, "SHELL").unwrap_or_else(|| "sh".to_string())
}

/// A file with no name: open for reading and writing, gone when closed.
pub fn anonymous_file() -> io::Result<File> {
    let nanos =
        SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |elapsed| elapsed.subsec_nanos());
    for attempt in 0..100u32 {
        let name = format!("workforest-{}-{nanos}-{attempt}", std::process::id());
        let path = env::temp_dir().join(name);
        match OpenOptions::new().read(true).write(true).create_new(true).open(&path) {
            Ok(file) => {
                fs::remove_file(&path)?;
                return Ok(file);
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::other("cannot create a temporary file"))
}

/// The name of signal number `signum` (`SIGTERM`), or `signal N` for one
/// that has none.
pub fn signal_name(signum: i32) -> String {
    nix::sys::signal::Signal::try_from(signum)
        .map_or_else(|_| format!("signal {signum}"), |signal| signal.as_str().to_string())
}

/// An environment variable that is set and not empty.
pub fn env_nonempty(name: &str) -> Option<String> {
    env::var_os(name)
        .filter(|value| !value.is_empty())
        .map(|value| value.to_string_lossy().into_owned())
}

/// The user's home directory: `$HOME`, else the password database's.
pub fn home_dir() -> PathBuf {
    if let Some(home) = env_nonempty("HOME") {
        return PathBuf::from(home);
    }
    nix::unistd::User::from_uid(nix::unistd::getuid())
        .ok()
        .flatten()
        .map(|user| user.dir)
        .unwrap_or_else(|| PathBuf::from("/"))
}

/// `~` and `~user` at the start of a path, expanded; anything else, and a
/// user that does not exist, is left as written.
pub fn expand_user(path: &str) -> PathBuf {
    let Some(rest) = path.strip_prefix('~') else {
        return PathBuf::from(path);
    };
    let (user, tail) = rest.split_once('/').map_or((rest, ""), |(user, tail)| (user, tail));
    let home = if user.is_empty() {
        home_dir()
    } else {
        match nix::unistd::User::from_name(user) {
            Ok(Some(found)) => found.dir,
            _ => return PathBuf::from(path),
        }
    };
    if tail.is_empty() { home } else { home.join(tail) }
}

/// Lexical normalization: `.` dropped, `..` resolved against what precedes
/// it, never touching the filesystem.
pub fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => match out.components().next_back() {
                Some(Component::Normal(_)) => {
                    out.pop();
                }
                Some(Component::RootDir) => {}
                _ => out.push(".."),
            },
            other => out.push(other),
        }
    }
    if out.as_os_str().is_empty() {
        out.push(".");
    }
    out
}

/// The executable `name` resolves to on `$PATH`.
pub fn which(name: &str) -> Option<PathBuf> {
    which_in(name, &env::var_os("PATH")?)
}

pub fn which_in(name: &str, path: &std::ffi::OsStr) -> Option<PathBuf> {
    env::split_paths(path).map(|dir| dir.join(name)).find(|candidate| {
        candidate
            .metadata()
            .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
    })
}

/// What `strerror` says for an I/O error, without Rust's "(os error N)".
pub fn os_error_text(error: &io::Error) -> String {
    match error.raw_os_error() {
        Some(code) => nix::errno::Errno::from_raw(code).desc().to_string(),
        None => error.to_string(),
    }
}

/// Escapes what Python's `json.dumps` escapes by default: everything
/// outside printable ASCII becomes `\uXXXX`.
struct Ascii<F>(F);

impl<F: Formatter> Formatter for Ascii<F> {
    fn write_string_fragment<W: ?Sized + io::Write>(
        &mut self,
        writer: &mut W,
        fragment: &str,
    ) -> io::Result<()> {
        for ch in fragment.chars() {
            if ch.is_ascii() && ch != '\x7f' {
                writer.write_all(&[ch as u8])?;
            } else {
                let mut units = [0u16; 2];
                for unit in ch.encode_utf16(&mut units) {
                    write!(writer, "\\u{unit:04x}")?;
                }
            }
        }
        Ok(())
    }

    fn begin_array<W: ?Sized + io::Write>(&mut self, writer: &mut W) -> io::Result<()> {
        self.0.begin_array(writer)
    }

    fn end_array<W: ?Sized + io::Write>(&mut self, writer: &mut W) -> io::Result<()> {
        self.0.end_array(writer)
    }

    fn begin_array_value<W: ?Sized + io::Write>(
        &mut self,
        writer: &mut W,
        first: bool,
    ) -> io::Result<()> {
        self.0.begin_array_value(writer, first)
    }

    fn end_array_value<W: ?Sized + io::Write>(&mut self, writer: &mut W) -> io::Result<()> {
        self.0.end_array_value(writer)
    }

    fn begin_object<W: ?Sized + io::Write>(&mut self, writer: &mut W) -> io::Result<()> {
        self.0.begin_object(writer)
    }

    fn end_object<W: ?Sized + io::Write>(&mut self, writer: &mut W) -> io::Result<()> {
        self.0.end_object(writer)
    }

    fn begin_object_key<W: ?Sized + io::Write>(
        &mut self,
        writer: &mut W,
        first: bool,
    ) -> io::Result<()> {
        self.0.begin_object_key(writer, first)
    }

    fn begin_object_value<W: ?Sized + io::Write>(&mut self, writer: &mut W) -> io::Result<()> {
        self.0.begin_object_value(writer)
    }

    fn end_object_value<W: ?Sized + io::Write>(&mut self, writer: &mut W) -> io::Result<()> {
        self.0.end_object_value(writer)
    }
}

fn to_json<T: Serialize, F: Formatter>(value: &T, formatter: F) -> String {
    let mut out = Vec::new();
    let mut serializer = serde_json::Serializer::with_formatter(&mut out, Ascii(formatter));
    value.serialize(&mut serializer).expect("in-memory JSON never fails");
    String::from_utf8(out).expect("the formatter writes ASCII")
}

/// `json.dumps(value, indent=2)`.
pub fn json_pretty<T: Serialize>(value: &T) -> String {
    to_json(value, PrettyFormatter::with_indent(b"  "))
}

/// `json.dumps(value, separators=(",", ":"))`.
pub fn json_compact<T: Serialize>(value: &T) -> String {
    to_json(value, CompactFormatter)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn repr_matches_python() {
        assert_eq!(repr("feat"), "'feat'");
        assert_eq!(repr("it's"), "\"it's\"");
        assert_eq!(repr("it's \"x\""), "'it\\'s \"x\"'");
        assert_eq!(repr("a\nb\tc\\"), "'a\\nb\\tc\\\\'");
        assert_eq!(repr("\x1b[0m"), "'\\x1b[0m'");
        assert_eq!(repr("\u{2028}"), "'\\u2028'");
        assert_eq!(repr("é"), "'é'");
        assert_eq!(repr_path(Path::new("/a b")), "'/a b'");
    }

    #[test]
    fn shell_quote_matches_shlex() {
        assert_eq!(shell_quote(""), "''");
        assert_eq!(shell_quote("a-b_c/d.e:f,g@h%i+j=k"), "a-b_c/d.e:f,g@h%i+j=k");
        assert_eq!(shell_quote("a b"), "'a b'");
        assert_eq!(shell_quote("it's"), "'it'\"'\"'s'");
        assert_eq!(shell_quote("$HOME"), "'$HOME'");
        assert_eq!(shell_quote("é"), "'é'");
        assert_eq!(shell_quote_path(Path::new("/x/y z")), "'/x/y z'");
    }

    #[test]
    fn one_line_collapses_whitespace() {
        assert_eq!(one_line("  a\tb\n\nc  "), "a b c");
        assert_eq!(one_line(""), "");
    }

    #[test]
    fn splitlines_knows_every_boundary() {
        assert_eq!(splitlines("a\nb\r\nc\rd"), ["a", "b", "c", "d"]);
        assert_eq!(splitlines("a\n"), ["a"]);
        assert_eq!(splitlines("a\n\nb"), ["a", "", "b"]);
        assert_eq!(splitlines(""), Vec::<&str>::new());
        assert_eq!(splitlines("a\u{2028}b\x0cc"), ["a", "b", "c"]);
    }

    #[test]
    fn seconds_drop_a_trailing_zero() {
        assert_eq!(format_seconds(30.0), "30");
        assert_eq!(format_seconds(0.5), "0.5");
    }

    #[test]
    fn normalize_is_lexical() {
        assert_eq!(normalize(Path::new("/a/b/../c/./d")), Path::new("/a/c/d"));
        assert_eq!(normalize(Path::new("/../a")), Path::new("/a"));
        assert_eq!(normalize(Path::new("../a/..")), Path::new(".."));
        assert_eq!(normalize(Path::new("a/..")), Path::new("."));
        assert_eq!(normalize(Path::new("/a//b/")), Path::new("/a/b"));
    }

    #[test]
    fn expand_user_leaves_other_paths_alone() {
        assert_eq!(expand_user("/a/~"), Path::new("/a/~"));
        assert_eq!(expand_user("~no-such-user-here/x"), Path::new("~no-such-user-here/x"));
        assert_eq!(expand_user("~"), home_dir());
        assert_eq!(expand_user("~/x"), home_dir().join("x"));
        assert_eq!(
            expand_user("~root"),
            nix::unistd::User::from_name("root").unwrap().unwrap().dir
        );
    }

    #[test]
    fn which_finds_only_executables() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("plain"), "").unwrap();
        let tool = dir.path().join("tool");
        std::fs::write(&tool, "").unwrap();
        std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(which_in("tool", dir.path().as_os_str()), Some(tool));
        assert_eq!(which_in("plain", dir.path().as_os_str()), None);
        assert_eq!(which_in("missing", dir.path().as_os_str()), None);
    }

    #[test]
    fn os_error_text_is_strerror() {
        let error = io::Error::from_raw_os_error(2);
        assert_eq!(os_error_text(&error), "No such file or directory");
        assert_eq!(os_error_text(&io::Error::other("custom")), "custom");
    }

    #[test]
    fn json_is_ascii_like_pythons() {
        let value =
            json!({"name": "é\u{1F600}\x7f", "list": [], "map": {}, "n": [1, 2.0, null, true]});
        assert_eq!(
            json_compact(&value),
            [
                r#"{"name":""#,
                "\\u00e9",
                "\\ud83d",
                "\\ude00",
                "\\u007f",
                r#"","list":[],"map":{},"n":[1,2.0,null,true]}"#
            ]
            .concat()
        );
        assert_eq!(
            json_pretty(&json!({"a": {"b": [1, "x\n"]}, "c": {}})),
            "{\n  \"a\": {\n    \"b\": [\n      1,\n      \"x\\n\"\n    ]\n  },\n  \"c\": {}\n}"
        );
    }

    #[test]
    fn environments_are_data() {
        let mut env = Env::new();
        assert_eq!(user_shell(&env), "sh");
        env.insert("SHELL".into(), "".into());
        assert_eq!((env_get(&env, "SHELL"), user_shell(&env)), (None, "sh".to_string()));
        env.insert("SHELL".into(), "/bin/zsh".into());
        assert_eq!(user_shell(&env), "/bin/zsh");
        assert!(current_env().contains_key(OsStr::new("PATH")));
    }

    #[test]
    fn an_anonymous_file_reads_back_what_was_written() {
        use std::io::{Read, Seek, Write};
        let mut file = anonymous_file().unwrap();
        file.write_all(b"hello").unwrap();
        file.rewind().unwrap();
        let mut text = String::new();
        file.read_to_string(&mut text).unwrap();
        assert_eq!(text, "hello");
    }

    #[test]
    fn signal_names() {
        assert_eq!(signal_name(15), "SIGTERM");
        assert_eq!(signal_name(2), "SIGINT");
        assert_eq!(signal_name(999), "signal 999");
    }

    #[test]
    fn file_name_and_env() {
        assert_eq!(file_name(Path::new("/a/b")), "b");
        assert_eq!(file_name(Path::new("/")), "");
        assert_eq!(env_nonempty("WORKFOREST_SURELY_UNSET_VARIABLE"), None);
    }
}
