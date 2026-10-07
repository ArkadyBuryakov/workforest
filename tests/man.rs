//! Man pages: hand-written roff, kept honest against the CLI definition
//! and laid out the way the wheel's data directory ships them.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use workforest::cli;
use workforest::errors;

fn man_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("packaging/pypi/data/share/man")
}

fn page() -> String {
    fs::read_to_string(man_dir().join("man1/workforest.1")).unwrap()
}

/// A literal the way the pages spell it (hyphens as `\-`).
fn roff(text: &str) -> String {
    text.replace('-', "\\-")
}

/// Every subcommand — `claude copy-session` included — with its options
/// as the command line spells them.
fn flags_by_command() -> Vec<(String, Vec<String>)> {
    let mut found = Vec::new();
    let command = cli::build_command(true);
    let mut pending: Vec<&clap::Command> = command.get_subcommands().collect();
    while let Some(sub) = pending.pop() {
        let mut flags = Vec::new();
        for arg in sub.get_arguments() {
            flags.extend(arg.get_long().map(|long| format!("--{long}")));
            flags.extend(arg.get_short().map(|short| format!("-{short}")));
        }
        found.push((sub.get_name().to_string(), flags));
        pending.extend(sub.get_subcommands());
    }
    found
}

#[test]
fn every_subcommand_has_a_section() {
    let page = page();
    for (name, _) in cli::SUBCOMMAND_HELP {
        assert!(page.contains(&format!("\n.SS {name}")), "no .SS section for {name:?}");
    }
    // and the page describes no command the CLI does not have
    let sections: BTreeSet<&str> = page
        .lines()
        .skip_while(|line| *line != ".SH COMMANDS")
        .skip(1)
        .take_while(|line| !line.starts_with(".SH"))
        .filter_map(|line| line.strip_prefix(".SS "))
        .map(|heading| heading.split_whitespace().next().unwrap())
        .collect();
    let commands: BTreeSet<&str> = cli::SUBCOMMAND_HELP.iter().map(|(name, _)| *name).collect();
    assert_eq!(sections, commands);
}

#[test]
fn every_option_is_documented() {
    let page = page();
    let commands = flags_by_command();
    assert!(commands.iter().any(|(name, _)| name == "copy-session"));
    for (name, flags) in commands {
        for flag in flags.iter().filter(|flag| !["-h", "--help"].contains(&flag.as_str())) {
            assert!(page.contains(&roff(flag)), "{name} {flag} missing from workforest.1");
        }
    }
}

#[test]
fn no_phantom_options() {
    // Every `--long` option the page mentions exists on some subcommand.
    let mut known: BTreeSet<String> =
        flags_by_command().into_iter().flat_map(|(_, flags)| flags).collect();
    known.extend(["--version".to_string(), "--help".to_string()]);
    // `.B \-\-name` / `\-\-name` inside running text
    let documented: BTreeSet<String> = page()
        .replace("\\-", "-")
        .split_whitespace()
        .map(|token| token.trim_matches(|c| "[](),.;'\"|".contains(c)).to_string())
        .filter(|token| {
            token.strip_prefix("--").is_some_and(|name| {
                !name.is_empty() && name.chars().all(|c| c.is_ascii_alphabetic() || c == '-')
            })
        })
        .collect();
    let phantom: Vec<&String> = documented.difference(&known).collect();
    assert!(phantom.is_empty(), "documented but not accepted: {phantom:?}");
    assert!(documented.len() > 10, "the scan found the options at all: {documented:?}");
}

#[test]
fn exit_codes_match_the_errors_module() {
    let page = page();
    let section = page.split(".SH EXIT STATUS").nth(1).unwrap().split(".SH").next().unwrap();
    let codes = [
        errors::EXIT_OK,
        errors::EXIT_ERROR,
        errors::EXIT_USAGE,
        errors::EXIT_CANCELLED,
        errors::EXIT_CONFIG,
    ];
    for code in codes {
        assert!(section.contains(&format!("\n.B {code}\n")), "exit code {code}");
    }
    let listed = section.lines().filter(|line| {
        line.strip_prefix(".B ").is_some_and(|code| code.chars().all(|c| c.is_ascii_digit()))
    });
    assert_eq!(listed.count(), codes.len(), "a code the CLI never returns");
}

#[test]
fn the_tui_needs_no_other_program() {
    for name in ["man1/workforest.1", "man5/workforest.5"] {
        let text = fs::read_to_string(man_dir().join(name)).unwrap();
        assert!(!text.contains("fzf"), "{name} still mentions fzf");
    }
}

#[test]
fn wf_is_a_link_to_workforest() {
    for section in ["1", "5"] {
        let link =
            fs::read_to_string(man_dir().join(format!("man{section}/wf.{section}"))).unwrap();
        assert_eq!(link, format!(".so man{section}/workforest.{section}\n"));
    }
}

#[test]
fn the_pages_are_where_the_wheel_ships_them_from() {
    // maturin puts `<data>/data/**` under the install prefix, so the pages
    // land in share/man/manN — which is also where shell-init looks.
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let pyproject = fs::read_to_string(root.join("pyproject.toml")).unwrap();
    assert!(pyproject.contains("data = \"packaging/pypi\""), "tool.maturin.data moved");
    let mut shipped = Vec::new();
    for section in ["man1", "man5"] {
        for entry in fs::read_dir(man_dir().join(section)).unwrap() {
            shipped.push(format!("{section}/{}", entry.unwrap().file_name().to_string_lossy()));
        }
    }
    shipped.sort();
    assert_eq!(shipped, ["man1/wf.1", "man1/workforest.1", "man5/wf.5", "man5/workforest.5"]);
}

#[test]
fn roff_is_clean() {
    // groff with every warning enabled must stay silent: the pages are
    // read by mandoc on macOS too, which is stricter than groff's defaults.
    if Command::new("groff").arg("--version").output().is_err() {
        return; // groff not installed
    }
    for name in ["man1/workforest.1", "man5/workforest.5"] {
        let result = Command::new("groff")
            .args(["-man", "-Tutf8", "-ww", "-z"])
            .arg(man_dir().join(name))
            .output()
            .unwrap();
        assert!(result.status.success(), "{name}");
        assert_eq!(String::from_utf8_lossy(&result.stderr), "", "{name}");
    }
}
