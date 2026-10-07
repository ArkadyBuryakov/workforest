//! Makefile targets as scripts.
//!
//! Where `make` is installed and the worktree root holds a makefile, every
//! target it defines is runnable as `wf make TARGET` — the same machinery
//! `wf run` uses, so a target is recorded, stopped, and counted like any
//! other script. Its script name is the target prefixed with `make:`,
//! which is what job records, `wf stop --make`, and `wf list --json` show;
//! nothing in `scripts` is shadowed, and the two namespaces never collide.
//!
//! Targets are discovered by reading the makefile (and what it includes)
//! rather than by asking make: listing what a project can run must not
//! evaluate `$(shell ...)`. The parse is therefore approximate by design —
//! it finds the plain rules a human would call and leaves generated ones
//! alone — and it never gates `wf make`, which passes the target to make
//! untouched.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use crate::config::{Config, ScriptSpec};
use crate::errors::{Error, Result};
use crate::util;

pub const PREFIX: &str = "make:";
/// The names GNU make looks for, in its own order.
pub const MAKEFILE_NAMES: [&str; 3] = ["GNUmakefile", "makefile", "Makefile"];
const MAX_INCLUDE_DEPTH: usize = 8;

pub fn script_name(target: &str) -> String {
    format!("{PREFIX}{target}")
}

/// The target a `make:` script name addresses, else None.
pub fn target_of(name: &str) -> Option<&str> {
    name.strip_prefix(PREFIX).filter(|target| !target.is_empty())
}

pub fn makefile_path(root: &Path) -> Option<PathBuf> {
    MAKEFILE_NAMES.iter().map(|name| root.join(name)).find(|candidate| candidate.is_file())
}

fn make_installed() -> bool {
    util::which("make").is_some()
}

/// Whether `wf make` has anything to offer here.
pub fn available(root: &Path) -> bool {
    make_installed() && makefile_path(root).is_some()
}

pub fn require(root: &Path) -> Result<()> {
    require_with(root, make_installed())
}

fn require_with(root: &Path, make_installed: bool) -> Result<()> {
    if !make_installed {
        return Err(Error::new("make is not installed"));
    }
    if makefile_path(root).is_none() {
        return Err(Error::new(format!(
            "no makefile in {} (looked for {})",
            root.display(),
            MAKEFILE_NAMES.join(", ")
        )));
    }
    Ok(())
}

/// The targets one makefile line defines, ignoring everything that is not
/// a plain rule: recipes and comments, variable assignments (`:=` and
/// friends), pattern rules, `$(...)`-built names, and make's own
/// dot-targets (`.PHONY`).
pub fn rule_targets(line: &str) -> Vec<&str> {
    if line.is_empty() || line.starts_with([' ', '\t', '#']) {
        return Vec::new();
    }
    let Some((head, rest)) = line.split_once(':') else {
        return Vec::new();
    };
    if head.contains(['=', '#']) {
        return Vec::new();
    }
    if rest.trim_start_matches(':').starts_with('=') {
        return Vec::new(); // `X := v`, `X ::= v`, `X :::= v`
    }
    head.split_whitespace()
        .filter(|target| !target.starts_with('.') && !target.contains(['%', '$']))
        .collect()
}

/// The files an `include`/`-include`/`sinclude` line pulls in; a path
/// built from variables is not one we can resolve, so it is skipped.
fn included(line: &str, directory: &Path) -> Vec<PathBuf> {
    let Some(rest) = line.strip_prefix(['-', 's']).unwrap_or(line).strip_prefix("include") else {
        return Vec::new();
    };
    if !rest.starts_with(char::is_whitespace) {
        return Vec::new();
    }
    rest.split_whitespace()
        .filter(|word| !word.contains('$'))
        .map(|word| directory.join(word))
        .collect()
}

fn collect(path: &Path, found: &mut Vec<String>, seen: &mut HashSet<PathBuf>, depth: usize) {
    let resolved = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    if depth > MAX_INCLUDE_DEPTH || !seen.insert(resolved) {
        return;
    }
    // An `-include` of something not generated yet, or unreadable, is skipped.
    let Ok(bytes) = fs::read(path) else {
        return;
    };
    let text = String::from_utf8_lossy(&bytes);
    let directory = path.parent().unwrap_or(Path::new(""));
    for line in util::splitlines(&text) {
        for target in rule_targets(line) {
            if !found.iter().any(|known| known == target) {
                found.push(target.to_string());
            }
        }
        for file in included(line, directory) {
            collect(&file, found, seen, depth + 1);
        }
    }
}

/// Every target the worktree's makefile defines, in file order (make's
/// default goal first); empty where there is no makefile.
pub fn targets(root: &Path) -> Vec<String> {
    let mut found = Vec::new();
    if let Some(path) = makefile_path(root) {
        collect(&path, &mut found, &mut HashSet::new(), 0);
    }
    found
}

/// The targets offered by name — completions, editor lists. `wf make`
/// itself is not restricted by these: hiding a target keeps it out of the
/// way, it does not take it away.
pub fn visible_targets(config: &Config, root: &Path) -> Vec<String> {
    visible_targets_with(config, root, make_installed())
}

fn visible_targets_with(config: &Config, root: &Path, make_installed: bool) -> Vec<String> {
    let spec = &config.make;
    if spec.hidden || !make_installed {
        return Vec::new();
    }
    let all = targets(root);
    if !spec.show_scripts.is_empty() {
        return all.into_iter().filter(|target| spec.show_scripts.contains(target)).collect();
    }
    all.into_iter().filter(|target| !spec.hide_scripts.contains(target)).collect()
}

/// The synthetic `scripts` entry a target runs as.
pub fn spec_for(config: &Config, target: &str) -> ScriptSpec {
    ScriptSpec {
        exclusive: config.make.exclusive_scripts.iter().any(|name| name == target),
        ..ScriptSpec::command(format!("make {}", util::shell_quote(target)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::MakeSpec;
    use crate::testing::Sandbox;

    const MAKEFILE: &str = "\
# Dev targets.
-include Makefile.local

VERSION := 1.0
PREFIX ?= /usr/local
FLAGS ::= -g

.PHONY: build test

build: deps
\t@echo building
test: FLAGS = -O0
test:
\t@echo testing $(VERSION)
lint check:
\t@echo both
%.o: %.c
\t@echo pattern
$(PREFIX)/bin/tool:
\t@echo generated
";

    fn with_makefile(text: &str) -> Sandbox {
        let sandbox = Sandbox::new();
        fs::write(sandbox.path().join("Makefile"), text).unwrap();
        sandbox
    }

    fn make(spec: MakeSpec) -> Config {
        Config { make: spec, ..Config::default() }
    }

    fn names(items: &[&str]) -> Vec<String> {
        items.iter().map(|item| item.to_string()).collect()
    }

    #[test]
    fn finds_plain_rules_in_file_order() {
        // Rules only: a prerequisite (`build: deps`) is not a definition.
        let sandbox = with_makefile(MAKEFILE);
        assert_eq!(targets(sandbox.path()), ["build", "test", "lint", "check"]);
    }

    #[test]
    fn lines_that_define_no_target() {
        for line in [
            "\t@echo recipe",
            "# comment: not a rule",
            "VERSION := 1.0",
            "VERSION ::= 1.0",
            "VERSION :::= 1.0",
            "PREFIX ?= /usr/local",
            "",
            "no colon here",
            ".PHONY: build",
            "%.o: %.c",
            "$(PREFIX)/bin/tool:",
            "  indented: rule",
            "a # b: c",
        ] {
            assert!(rule_targets(line).is_empty(), "{line:?}");
        }
    }

    #[test]
    fn double_colon_and_order_only_rules() {
        assert_eq!(rule_targets("clean:: extra"), ["clean"]);
        assert_eq!(rule_targets("app: | build-dir"), ["app"]);
    }

    #[test]
    fn follows_includes_relative_to_the_makefile() {
        // In make's own order: an included file is read where it is named.
        let sandbox = with_makefile("include extra.mk\nlocal:\n\t@echo local\nsinclude s.mk\n");
        fs::write(sandbox.path().join("extra.mk"), "included:\n\t@echo included\n").unwrap();
        fs::write(sandbox.path().join("s.mk"), "silent:\n").unwrap();
        assert_eq!(targets(sandbox.path()), ["included", "local", "silent"]);
    }

    #[test]
    fn missing_include_and_variable_paths_are_skipped() {
        let sandbox =
            with_makefile("-include gone.mk $(GENERATED)\nincludes:\nlocal:\n\t@echo local\n");
        assert_eq!(targets(sandbox.path()), ["includes", "local"]);
    }

    #[test]
    fn include_cycles_end() {
        let sandbox = with_makefile("include a.mk\nroot:\n\t@echo root\n");
        fs::write(sandbox.path().join("a.mk"), "include Makefile\na:\n\t@echo a\n").unwrap();
        assert_eq!(targets(sandbox.path()), ["a", "root"]);
    }

    #[test]
    fn include_chains_stop_at_a_depth() {
        let sandbox = with_makefile("include 1.mk\n");
        for index in 1..12 {
            let text = format!("t{index}:\ninclude {}.mk\n", index + 1);
            fs::write(sandbox.path().join(format!("{index}.mk")), text).unwrap();
        }
        assert_eq!(targets(sandbox.path()).len(), MAX_INCLUDE_DEPTH);
    }

    #[test]
    fn no_makefile_no_targets() {
        let sandbox = Sandbox::new();
        assert!(targets(sandbox.path()).is_empty());
        assert_eq!(makefile_path(sandbox.path()), None);
        assert!(!available(sandbox.path()));
    }

    #[test]
    fn gnu_makefile_wins_over_makefile() {
        let sandbox = with_makefile("fallback:\n\t@echo fallback\n");
        fs::write(sandbox.path().join("GNUmakefile"), "preferred:\n\t@echo preferred\n").unwrap();
        assert_eq!(targets(sandbox.path()), ["preferred"]);
    }

    #[test]
    fn names_round_trip() {
        assert_eq!(script_name("check"), "make:check");
        assert_eq!(target_of("make:check"), Some("check"));
        for name in ["check", "make:", "makecheck", ""] {
            assert_eq!(target_of(name), None, "{name:?}");
        }
    }

    #[test]
    fn visibility() {
        let sandbox = with_makefile(MAKEFILE);
        let root = sandbox.path();
        let visible = |spec: MakeSpec| visible_targets_with(&make(spec), root, true);
        assert_eq!(visible(MakeSpec::default()), ["build", "test", "lint", "check"]);
        assert!(visible(MakeSpec { hidden: true, ..MakeSpec::default() }).is_empty());
        assert_eq!(
            visible(MakeSpec { hide_scripts: names(&["build", "lint"]), ..MakeSpec::default() }),
            ["test", "check"]
        );
        // show_scripts wins over hide_scripts
        assert_eq!(
            visible(MakeSpec {
                show_scripts: names(&["check", "test"]),
                hide_scripts: names(&["check"]),
                ..MakeSpec::default()
            }),
            ["test", "check"]
        );
        assert!(visible_targets_with(&Config::default(), root, false).is_empty());
        fs::remove_file(root.join("Makefile")).unwrap();
        assert!(visible(MakeSpec::default()).is_empty());
        assert_eq!(visible_targets(&Config::default(), root), Vec::<String>::new());
    }

    #[test]
    fn spec_quotes_the_target_and_knows_exclusive_ones() {
        let spec = spec_for(&Config::default(), "a target");
        assert_eq!(spec.command.as_deref(), Some("make 'a target'"));
        assert!(!spec.exclusive);
        let config = make(MakeSpec { exclusive_scripts: names(&["dev"]), ..MakeSpec::default() });
        assert!(spec_for(&config, "dev").exclusive);
        assert!(!spec_for(&config, "check").exclusive);
    }

    #[test]
    fn require_says_what_is_missing() {
        let sandbox = Sandbox::new();
        let error = require_with(sandbox.path(), true).unwrap_err();
        assert_eq!(
            error.message,
            format!(
                "no makefile in {} (looked for GNUmakefile, makefile, Makefile)",
                sandbox.path().display()
            )
        );
        assert_eq!(
            require_with(sandbox.path(), false).unwrap_err().message,
            "make is not installed"
        );
        fs::write(sandbox.path().join("makefile"), "a:\n").unwrap();
        assert_eq!(require_with(sandbox.path(), true), Ok(()));
        assert_eq!(require(sandbox.path()).is_ok(), make_installed());
    }
}
