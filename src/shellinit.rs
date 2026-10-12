//! `workforest shell-init [bash|zsh]`: print the wf wrapper + completions
//! (+ a $MANPATH entry for installs whose man pages man(1) would not
//! find).

use std::env;
use std::path::{Path, PathBuf};

use crate::errors::{Error, Result};
use crate::util::{self, Env, shell_quote_path};

const WRAPPER: &str = include_str!("../resources/shell/workforest.sh");
const COMPLETION_BASH: &str = include_str!("../resources/shell/completion.bash");
const COMPLETION_ZSH: &str = include_str!("../resources/shell/completion.zsh");

/// Prefixes whose share/man is on every man(1) default search path already.
const SYSTEM_PREFIXES: [&str; 2] = ["/usr", "/usr/local"];
const PAGE: &str = "share/man/man1/workforest.1";

pub fn detect_shell(env: &Env) -> Result<&'static str> {
    let shell = util::env_get(env, "SHELL").map(|path| util::file_name(Path::new(&path)));
    match shell.as_deref() {
        Some("bash") => Ok("bash"),
        Some("zsh") => Ok("zsh"),
        other => Err(Error::new(format!(
            "cannot detect shell from $SHELL ({}); pass one explicitly: workforest shell-init bash|zsh",
            other.filter(|name| !name.is_empty()).unwrap_or("unset")
        ))),
    }
}

/// Shell lines putting PREFIX/share/man on $MANPATH, or "" when man(1)
/// searches it anyway (system prefixes) or our pages are not there (a
/// package manager relocated them).
///
/// The PyPI wheel ships the pages as share/man data, so a `uv tool`/pipx
/// environment holds them where no default man path looks. Idempotent for
/// repeated evals; the trailing colon when MANPATH was unset means "then
/// the system default" to both man-db and BSD/macOS man.
pub fn manpath_snippet(prefix: &Path) -> String {
    if SYSTEM_PREFIXES.iter().any(|system| Path::new(system) == prefix)
        || !prefix.join(PAGE).is_file()
    {
        return String::new();
    }
    let quoted = shell_quote_path(&prefix.join("share").join("man"));
    format!(
        "# workforest's man pages live in its own install directory, off the default\n\
         # man path; a trailing colon keeps the system path when MANPATH was unset.\n\
         case \":${{MANPATH-}}:\" in\n\
         \x20   *\":\"{quoted}\":\"*) ;;\n\
         \x20   *) export MANPATH={quoted}\":${{MANPATH-}}\" ;;\n\
         esac\n"
    )
}

/// `PREFIX` of a `PREFIX/bin/workforest`.
fn prefix_of(executable: &Path) -> Option<PathBuf> {
    Some(executable.parent()?.parent()?.to_path_buf())
}

/// The prefix whose share/man needs a $MANPATH entry, if any: where this
/// executable really lives (a tool environment's `bin/` is reached through
/// a symlink on $PATH) — unless man(1) finds the pages on its own, next to
/// the $PATH entry we were started through (how a package manager that
/// links both `bin/` and `share/man/` into one prefix lays things out).
fn manpath_prefix(executable: &Path, on_path: Option<&Path>) -> Option<PathBuf> {
    let real = executable.canonicalize().ok()?;
    if let Some(link) = on_path
        && link.canonicalize().ok().as_deref() == Some(&real)
        && prefix_of(link).is_some_and(|prefix| prefix.join(PAGE).is_file())
    {
        return None;
    }
    prefix_of(&real)
}

pub fn shell_init(shell: Option<&str>, env: &Env) -> Result<String> {
    let shell = match shell {
        Some(shell) => shell,
        None => detect_shell(env)?,
    };
    let completion = if shell == "bash" { COMPLETION_BASH } else { COMPLETION_ZSH };
    let manpath = env::current_exe()
        .ok()
        .and_then(|executable| manpath_prefix(&executable, util::which("workforest").as_deref()))
        .map(|prefix| manpath_snippet(&prefix))
        .unwrap_or_default();
    let parts = [WRAPPER, manpath.as_str(), completion];
    Ok(parts.into_iter().filter(|part| !part.is_empty()).collect::<Vec<_>>().join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::Sandbox;
    use std::fs;
    use std::os::unix::fs::symlink;

    fn shell(path: &str) -> Env {
        [("SHELL".into(), path.into())].into_iter().collect()
    }

    /// A prefix with `bin/workforest` and, optionally, the man page.
    fn prefix(root: &Path, pages: bool) -> PathBuf {
        fs::create_dir_all(root.join("bin")).unwrap();
        fs::write(root.join("bin/workforest"), "").unwrap();
        if pages {
            fs::create_dir_all(root.join("share/man/man1")).unwrap();
            fs::write(root.join(PAGE), ".TH X 1\n").unwrap();
        }
        root.to_path_buf()
    }

    #[test]
    fn detects_the_shell_from_the_environment() {
        assert_eq!(detect_shell(&shell("/usr/bin/zsh")), Ok("zsh"));
        assert_eq!(detect_shell(&shell("/bin/bash")), Ok("bash"));
        assert_eq!(
            detect_shell(&shell("/usr/bin/fish")).unwrap_err().message,
            "cannot detect shell from $SHELL (fish); pass one explicitly: workforest shell-init bash|zsh"
        );
        assert!(detect_shell(&Env::new()).unwrap_err().message.contains("$SHELL (unset)"));
    }

    #[test]
    fn output_is_the_wrapper_then_the_completion() {
        let bash = shell_init(Some("bash"), &Env::new()).unwrap();
        assert!(bash.starts_with("# Workforest shell integration"));
        assert!(
            bash.contains("workforest() {") && bash.contains("complete -F _workforest_complete")
        );
        assert!(!bash.contains("compdef"));
        let zsh = shell_init(None, &shell("/usr/bin/zsh")).unwrap();
        assert!(zsh.contains("compdef _workforest_complete workforest wf"));
        assert!(shell_init(None, &shell("/usr/bin/fish")).is_err());
    }

    #[test]
    fn snippet_for_a_prefix_that_holds_the_pages() {
        let sandbox = Sandbox::new();
        let root = prefix(&sandbox.path().join("tool env"), true);
        let snippet = manpath_snippet(&root);
        let quoted = format!("'{}/share/man'", root.display());
        assert!(snippet.contains(&format!("    *\":\"{quoted}\":\"*) ;;\n")), "{snippet}");
        assert!(
            snippet.contains(&format!("    *) export MANPATH={quoted}\":${{MANPATH-}}\" ;;\n"))
        );
        assert!(snippet.starts_with("# workforest's man pages") && snippet.ends_with("esac\n"));
    }

    #[test]
    fn nothing_without_pages_or_for_a_system_prefix() {
        let sandbox = Sandbox::new();
        assert_eq!(manpath_snippet(&prefix(sandbox.path(), false)), "");
        // /usr/share/man is on every default man path; never touch MANPATH for it
        assert_eq!(manpath_snippet(Path::new("/usr")), "");
        assert_eq!(manpath_snippet(Path::new("/usr/local")), "");
    }

    #[test]
    fn the_prefix_is_where_the_executable_really_lives() {
        let sandbox = Sandbox::new();
        let tool = prefix(&sandbox.path().join("tools/workforest"), true);
        let local = sandbox.path().join("local");
        fs::create_dir_all(local.join("bin")).unwrap();
        let link = local.join("bin/workforest");
        symlink(tool.join("bin/workforest"), &link).unwrap();
        // started through the symlink on $PATH: the pages are not beside it
        assert_eq!(manpath_prefix(&link, Some(&link)), Some(tool.clone()));
        assert_eq!(manpath_prefix(&link, None), Some(tool.clone()));
        // another install on $PATH says nothing about this one
        let other = prefix(&sandbox.path().join("other"), true);
        assert_eq!(manpath_prefix(&link, Some(&other.join("bin/workforest"))), Some(tool));
        assert_eq!(manpath_prefix(&sandbox.path().join("missing"), None), None);
    }

    #[test]
    fn pages_beside_the_path_entry_need_no_manpath() {
        // bin/ and share/man/ linked into one prefix, the real files elsewhere
        let sandbox = Sandbox::new();
        let cellar = prefix(&sandbox.path().join("cellar/workforest/1.0"), true);
        let linked = sandbox.path().join("brew");
        fs::create_dir_all(linked.join("bin")).unwrap();
        fs::create_dir_all(linked.join("share/man/man1")).unwrap();
        let link = linked.join("bin/workforest");
        symlink(cellar.join("bin/workforest"), &link).unwrap();
        symlink(cellar.join(PAGE), linked.join(PAGE)).unwrap();
        assert_eq!(manpath_prefix(&link, Some(&link)), None);
    }

    #[test]
    fn shipped_scripts_are_valid_syntax() {
        let sandbox = Sandbox::new();
        for (shell, script) in [("bash", "init.bash"), ("zsh", "init.zsh")] {
            if util::which(shell).is_none() {
                continue;
            }
            let path = sandbox.path().join(script);
            fs::write(&path, shell_init(Some(shell), &Env::new()).unwrap()).unwrap();
            let check = std::process::Command::new(shell).arg("-n").arg(&path).output().unwrap();
            assert!(check.status.success(), "{}", String::from_utf8_lossy(&check.stderr));
        }
    }
}
