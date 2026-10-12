//! Claude Code session integration: copy a session from the main
//! worktree's project dir into the current worktree's, rewriting cwd
//! fields.
//!
//! EXPERIMENTAL: this reads and writes Claude Code's private on-disk state
//! (~/.claude/projects layout, session .jsonl format, history.jsonl), none
//! of which is a stable interface — any Claude Code update may break it.
//!
//! Pure file operations — no external binary. Lines are rewritten by JSON
//! parsing, never by string substitution.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::errors::{Error, Result};
use crate::util::{self, repr};
use crate::{git, output};

const DESCRIPTION_LIMIT: usize = 80;
const NO_DESCRIPTION: &str = "(no description)";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Session {
    pub id: String,
    pub description: String,
}

/// Claude Code's state directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Claude {
    dir: PathBuf,
}

impl Claude {
    /// `~/.claude`.
    pub fn standard() -> Self {
        Self { dir: util::home_dir().join(".claude") }
    }

    pub fn at(dir: &Path) -> Self {
        Self { dir: dir.to_path_buf() }
    }

    /// Feature gate: the integration is invisible without the directory.
    pub fn available(&self) -> bool {
        self.dir.is_dir()
    }

    fn history(&self) -> PathBuf {
        self.dir.join("history.jsonl")
    }

    /// Claude Code encodes a project path by replacing '/' and '.' with '-'.
    pub fn project_dir(&self, path: &Path) -> PathBuf {
        let encoded = path.to_string_lossy().replace(['/', '.'], "-");
        self.dir.join("projects").join(encoded)
    }

    fn history_entries(&self) -> Vec<serde_json::Map<String, Value>> {
        let Ok(bytes) = fs::read(self.history()) else {
            return Vec::new();
        };
        util::splitlines(&String::from_utf8_lossy(&bytes))
            .into_iter()
            .filter_map(|line| match serde_json::from_str(line) {
                Ok(Value::Object(entry)) => Some(entry),
                _ => None,
            })
            .collect()
    }

    /// Sessions of the main worktree's project, with history descriptions.
    pub fn list_sessions(&self, main: &Path) -> Vec<Session> {
        let mut ids: Vec<String> = fs::read_dir(self.project_dir(main))
            .map(|entries| {
                entries
                    .flatten()
                    .filter_map(|entry| {
                        entry
                            .file_name()
                            .to_string_lossy()
                            .strip_suffix(".jsonl")
                            .map(str::to_string)
                    })
                    .collect()
            })
            .unwrap_or_default();
        ids.sort();
        if ids.is_empty() {
            return Vec::new();
        }
        let main_text = main.to_string_lossy();
        let mut descriptions: Vec<(String, String)> = Vec::new();
        for entry in self.history_entries() {
            if entry.get("project").and_then(Value::as_str) != Some(&main_text) {
                continue;
            }
            let Some(id) = entry.get("sessionId").and_then(Value::as_str) else {
                continue;
            };
            if descriptions.iter().any(|(known, _)| known == id) {
                continue;
            }
            let display =
                entry.get("display").and_then(Value::as_str).filter(|text| !text.is_empty());
            let mut text = display.unwrap_or(NO_DESCRIPTION).to_string();
            if text.chars().count() > DESCRIPTION_LIMIT {
                text = text.chars().take(DESCRIPTION_LIMIT - 3).collect::<String>() + "...";
            }
            descriptions.push((id.to_string(), text));
        }
        ids.into_iter()
            .map(|id| {
                let description = descriptions
                    .iter()
                    .find(|(known, _)| *known == id)
                    .map_or_else(|| NO_DESCRIPTION.to_string(), |(_, text)| text.clone());
                Session { id, description }
            })
            .collect()
    }

    /// Sessions not yet copied into the current worktree's project dir.
    pub fn list_new_sessions(&self, main: &Path, current: &Path) -> Vec<Session> {
        let current_dir = self.project_dir(current);
        self.list_sessions(main)
            .into_iter()
            .filter(|session| !current_dir.join(format!("{}.jsonl", session.id)).is_file())
            .collect()
    }

    pub fn copy_session(&self, session_id: &str, main: &Path, current: &Path) -> Result<()> {
        let (src_dir, dst_dir) = (self.project_dir(main), self.project_dir(current));
        let src = src_dir.join(format!("{session_id}.jsonl"));
        if !src.is_file() {
            return Err(Error::new(format!(
                "session {} not found in {}",
                repr(session_id),
                src_dir.display()
            )));
        }
        let dst = dst_dir.join(format!("{session_id}.jsonl"));
        if dst.exists() {
            output::warn(&format!(
                "session {} already exists in {}",
                repr(session_id),
                dst_dir.display()
            ));
            return Ok(());
        }
        let failed = |error: std::io::Error| {
            Error::new(format!(
                "cannot copy session {}: {}",
                repr(session_id),
                util::os_error_text(&error)
            ))
        };
        fs::create_dir_all(&dst_dir).map_err(failed)?;

        let text = String::from_utf8_lossy(&fs::read(&src).map_err(failed)?).into_owned();
        let rewritten: Vec<String> = util::splitlines(&text)
            .into_iter()
            .map(|line| rewrite_line(line, main, current))
            .collect();
        fs::write(&dst, rewritten.join("\n") + "\n").map_err(failed)?;

        let assets = src_dir.join(session_id);
        if assets.is_dir() {
            copy_tree(&assets, &dst_dir.join(session_id)).map_err(failed)?;
        }

        // Append a rewritten history entry so Claude Code discovers the copy.
        let matching = self
            .history_entries()
            .into_iter()
            .find(|entry| entry.get("sessionId").and_then(Value::as_str) == Some(session_id));
        if let Some(mut entry) = matching {
            entry.insert("project".into(), current.to_string_lossy().into_owned().into());
            let mut history =
                OpenOptions::new().append(true).open(self.history()).map_err(failed)?;
            writeln!(history, "{}", util::json_compact(&entry)).map_err(failed)?;
        }

        output::success(&format!("copied session {} to {}", repr(session_id), dst_dir.display()));
        Ok(())
    }
}

fn rewrite_line(line: &str, main: &Path, current: &Path) -> String {
    match serde_json::from_str::<Value>(line) {
        Ok(Value::Object(mut entry))
            if entry.get("cwd").and_then(Value::as_str) == Some(&main.to_string_lossy()) =>
        {
            entry.insert("cwd".into(), current.to_string_lossy().into_owned().into());
            util::json_compact(&entry)
        }
        _ => line.to_string(),
    }
}

/// Copy a directory tree into `dst`, merging into what is already there.
fn copy_tree(src: &Path, dst: &Path) -> std::io::Result<()> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let target = dst.join(entry.file_name());
        if entry.path().is_dir() {
            copy_tree(&entry.path(), &target)?;
        } else {
            fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}

/// CLI entry: must run from a non-main worktree.
pub fn cmd_copy_session(session_id: &str) -> Result<()> {
    let root = git::repo_root(None)?;
    let main = git::main_worktree(Some(&root))?;
    if root == main {
        return Err(Error::new("copy-session must run from a non-main worktree"));
    }
    Claude::standard().copy_session(session_id, &main, &root)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::output::capture;
    use crate::testing::Sandbox;
    use serde_json::json;

    const MAIN: &str = "/home/u/dev/api";
    const WORKTREE: &str = "/home/u/dev/worktrees/api/feat";

    fn claude(sandbox: &Sandbox) -> Claude {
        Claude::at(&sandbox.path().join(".claude"))
    }

    /// Create a session file (and history entry) for the main worktree.
    fn seed(claude: &Claude, id: &str, display: Option<&str>, spaced: bool) {
        let project = claude.project_dir(Path::new(MAIN));
        fs::create_dir_all(&project).unwrap();
        let lines = if spaced {
            vec![
                format!("{{\"cwd\": \"{MAIN}\", \"type\": \"user\", \"text\": \"hello\"}}"),
                "{\"type\": \"meta\"}".to_string(),
            ]
        } else {
            vec![
                json!({"cwd": MAIN, "type": "user", "text": "hello"}).to_string(),
                json!({"type": "meta"}).to_string(),
                "not-json-at-all".to_string(),
            ]
        };
        fs::write(project.join(format!("{id}.jsonl")), lines.join("\n") + "\n").unwrap();
        if let Some(display) = display {
            let entry = json!({"sessionId": id, "project": MAIN, "display": display});
            let mut history =
                OpenOptions::new().create(true).append(true).open(claude.history()).unwrap();
            writeln!(history, "{entry}").unwrap();
        }
    }

    fn copy(claude: &Claude, id: &str) -> (Result<()>, String) {
        capture(|| claude.copy_session(id, Path::new(MAIN), Path::new(WORKTREE)))
    }

    fn copied_lines(claude: &Claude, id: &str) -> Vec<String> {
        let path = claude.project_dir(Path::new(WORKTREE)).join(format!("{id}.jsonl"));
        fs::read_to_string(path).unwrap().lines().map(str::to_string).collect()
    }

    #[test]
    fn gated_on_the_directory() {
        let sandbox = Sandbox::new();
        let claude = claude(&sandbox);
        assert!(!claude.available());
        fs::create_dir(&claude.dir).unwrap();
        assert!(claude.available());
        assert!(Claude::standard().dir.ends_with(".claude"));
    }

    #[test]
    fn project_dir_encoding() {
        let claude = Claude::at(Path::new("/c"));
        assert_eq!(
            claude.project_dir(Path::new("/home/u/dev/api.v2")),
            Path::new("/c/projects/-home-u-dev-api-v2")
        );
    }

    #[test]
    fn lists_sessions_with_descriptions() {
        let sandbox = Sandbox::new();
        let claude = claude(&sandbox);
        seed(&claude, "s1", Some("short one"), false);
        seed(&claude, "s1", Some("a later entry for the same session"), false);
        seed(&claude, "s2", Some(&"x".repeat(100)), false);
        seed(&claude, "s3", None, false);
        seed(&claude, "s4", Some(""), false);
        // noise the listing has to step over
        let mut history = OpenOptions::new().append(true).open(claude.history()).unwrap();
        writeln!(history, "not json\n[1]\n{{\"project\": \"/elsewhere\", \"sessionId\": \"s3\", \"display\": \"other\"}}\n{{\"project\": \"{MAIN}\"}}").unwrap();

        let sessions = claude.list_sessions(Path::new(MAIN));
        let found: Vec<(&str, &str)> = sessions
            .iter()
            .map(|session| (session.id.as_str(), session.description.as_str()))
            .collect();
        assert_eq!(found[0], ("s1", "short one"));
        assert_eq!(found[1].1.len(), 80);
        assert!(found[1].1.ends_with("xxx..."));
        assert_eq!(found[2], ("s3", "(no description)"));
        assert_eq!(found[3], ("s4", "(no description)"));
    }

    #[test]
    fn empty_without_project_dir_or_sessions() {
        let sandbox = Sandbox::new();
        let claude = claude(&sandbox);
        assert!(claude.list_sessions(Path::new(MAIN)).is_empty());
        fs::create_dir_all(claude.project_dir(Path::new(MAIN))).unwrap();
        assert!(claude.list_sessions(Path::new(MAIN)).is_empty());
    }

    #[test]
    fn new_sessions_filter_already_copied() {
        let sandbox = Sandbox::new();
        let claude = claude(&sandbox);
        seed(&claude, "old", Some("x"), false);
        seed(&claude, "new", Some("y"), false);
        let copied = claude.project_dir(Path::new(WORKTREE));
        fs::create_dir_all(&copied).unwrap();
        fs::write(copied.join("old.jsonl"), "{}\n").unwrap();
        let fresh = claude.list_new_sessions(Path::new(MAIN), Path::new(WORKTREE));
        assert_eq!(fresh, [Session { id: "new".into(), description: "y".into() }]);
    }

    #[test]
    fn rewrites_cwd_by_parsing_whatever_the_spacing() {
        let sandbox = Sandbox::new();
        let claude = claude(&sandbox);
        seed(&claude, "abc-123", Some("fix the login bug"), false);
        let (result, shown) = copy(&claude, "abc-123");
        assert_eq!(result, Ok(()));
        assert!(shown.starts_with("copied session 'abc-123' to "), "{shown}");
        let lines = copied_lines(&claude, "abc-123");
        assert_eq!(
            lines[0],
            format!("{{\"cwd\":\"{WORKTREE}\",\"type\":\"user\",\"text\":\"hello\"}}")
        );
        assert_eq!(lines[1], "{\"type\":\"meta\"}"); // untouched
        assert_eq!(lines[2], "not-json-at-all"); // non-JSON copied verbatim

        seed(&claude, "sp", Some("spaced"), true);
        assert_eq!(copy(&claude, "sp").0, Ok(()));
        let lines = copied_lines(&claude, "sp");
        let first: Value = serde_json::from_str(&lines[0]).unwrap();
        assert_eq!(first["cwd"], WORKTREE);
        assert_eq!(lines[1], "{\"type\": \"meta\"}"); // a line that needs no change keeps its bytes
    }

    #[test]
    fn appends_a_rewritten_history_entry() {
        let sandbox = Sandbox::new();
        let claude = claude(&sandbox);
        seed(&claude, "abc-123", Some("fix the login bug"), false);
        copy(&claude, "abc-123").0.unwrap();
        let entries = claude.history_entries();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0]["project"], MAIN); // original kept
        assert_eq!(entries[1]["project"], WORKTREE);
        assert_eq!(entries[1]["sessionId"], "abc-123");
        assert_eq!(entries[1]["display"], "fix the login bug");
    }

    #[test]
    fn a_session_without_history_copies_without_one() {
        let sandbox = Sandbox::new();
        let claude = claude(&sandbox);
        seed(&claude, "quiet", None, false);
        assert_eq!(copy(&claude, "quiet").0, Ok(()));
        assert!(!claude.history().exists());
    }

    #[test]
    fn copies_the_session_assets_dir() {
        let sandbox = Sandbox::new();
        let claude = claude(&sandbox);
        seed(&claude, "abc-123", Some("x"), false);
        let assets = claude.project_dir(Path::new(MAIN)).join("abc-123");
        fs::create_dir_all(assets.join("nested")).unwrap();
        fs::write(assets.join("note.txt"), "asset\n").unwrap();
        fs::write(assets.join("nested/deep.txt"), "deep\n").unwrap();
        copy(&claude, "abc-123").0.unwrap();
        let copied = claude.project_dir(Path::new(WORKTREE)).join("abc-123");
        assert_eq!(fs::read_to_string(copied.join("note.txt")).unwrap(), "asset\n");
        assert_eq!(fs::read_to_string(copied.join("nested/deep.txt")).unwrap(), "deep\n");
    }

    #[test]
    fn already_copied_warns_and_unknown_errors() {
        let sandbox = Sandbox::new();
        let claude = claude(&sandbox);
        seed(&claude, "abc-123", Some("x"), false);
        copy(&claude, "abc-123").0.unwrap();
        let (again, shown) = copy(&claude, "abc-123");
        assert_eq!(again, Ok(()));
        assert!(shown.starts_with("session 'abc-123' already exists in "), "{shown}");
        assert_eq!(claude.history_entries().len(), 2, "no second history entry");

        let error = copy(&claude, "ghost").0.unwrap_err();
        assert!(error.message.starts_with("session 'ghost' not found in "), "{error}");
    }
}
