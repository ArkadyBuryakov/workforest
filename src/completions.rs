//! `workforest --complete TOPIC` backend.
//!
//! Completion must never break the shell: any error yields an empty
//! candidate list, and everything stays on stdout as plain lines. Most
//! topics emit bare names; `commands`, `openers`, `branches` and
//! `unlockable` emit `NAME<TAB>DESCRIPTION` so shells that can render
//! descriptions (zsh) do, while others take field 1.

use crate::cli;
use crate::commands::{self, Context};
use crate::config::{Config, load_config};
use crate::errors::Result;
use crate::git::{self, Worktree};
use crate::integrations::claude::Claude;
use crate::util::one_line;
use crate::{launch, makefile};

pub const TOPICS: [&str; 9] = [
    "commands",
    "branches",
    "worktrees",
    "lockable",
    "unlockable",
    "scripts",
    "make",
    "openers",
    "claude-sessions",
];

pub fn complete(topic: &str) -> Vec<String> {
    candidates(topic).unwrap_or_default()
}

fn candidates(topic: &str) -> Result<Vec<String>> {
    Ok(match topic {
        "commands" => commands(Claude::standard().available()),
        "branches" => branches()?,
        "worktrees" => managed()?.iter().map(Worktree::name).collect(),
        "lockable" => lockable(&managed()?),
        "unlockable" => unlockable(&managed()?),
        "scripts" => scripts(&config()),
        "make" => {
            // The makefile targets `wf make` offers here, in the makefile's
            // own order (its default goal first); empty where make or the
            // makefile is missing, or the `make` config hides them.
            let ctx = commands::build_context(None)?;
            makefile::visible_targets(&ctx.config, &ctx.cwd_root)
        }
        "openers" => openers(&config()),
        "claude-sessions" => claude_sessions(&Claude::standard())?,
        _ => Vec::new(),
    })
}

/// The merged config where we stand, or the global layers outside a
/// repository; nothing at all rather than an error.
fn config() -> Config {
    commands::build_context(None)
        .map(|ctx| ctx.config)
        .or_else(|_| load_config(None))
        .unwrap_or_default()
}

fn commands(claude: bool) -> Vec<String> {
    let mut known: Vec<&(&str, &str)> =
        cli::SUBCOMMAND_HELP.iter().filter(|(name, _)| claude || *name != "claude").collect();
    known.sort_unstable();
    known.into_iter().map(|(name, help)| format!("{name}\t{help}")).collect()
}

fn lockable(managed: &[Worktree]) -> Vec<String> {
    managed.iter().filter(|worktree| worktree.locked.is_none()).map(Worktree::name).collect()
}

fn unlockable(managed: &[Worktree]) -> Vec<String> {
    // one line each, whatever the reason holds: a tab or newline in it
    // would break the line protocol
    managed
        .iter()
        .filter_map(|worktree| {
            let reason = one_line(worktree.locked.as_deref()?);
            let shown = if reason.is_empty() { "locked" } else { &reason };
            Some(format!("{}\t{shown}", worktree.name()))
        })
        .collect()
}

fn scripts(config: &Config) -> Vec<String> {
    let mut names: Vec<String> = config
        .scripts
        .iter()
        .filter(|(_, spec)| !spec.hidden)
        .map(|(name, _)| name.clone())
        .collect();
    names.sort_unstable();
    names
}

fn openers(config: &Config) -> Vec<String> {
    let mut names: Vec<&String> = config.openers.keys().collect();
    names.sort_unstable();
    names
        .into_iter()
        .filter_map(|name| {
            // collapse whitespace: a tab/newline in an opener command
            // would break the line protocol
            let described = launch::describe_opener(config, name).ok()?;
            Some(format!("{name}\t{}", one_line(&described)))
        })
        .collect()
}

/// `NAME<TAB>LOCATION` lines, minus branches already checked out: local
/// branches by their bare name (location lists their remotes too),
/// remote-only branches remote-qualified — the form `wf create` resolves
/// unambiguously.
fn branches() -> Result<Vec<String>> {
    let root = git::repo_root(None)?;
    let taken: Vec<String> = git::list_worktrees(Some(&root))?
        .into_iter()
        .filter_map(|worktree| worktree.branch)
        .collect();
    let local = git::local_branches(&root)?;
    let remote_map = git::remote_branches(&root)?;
    let mut lines = Vec::new();
    for branch in local.iter().filter(|branch| !taken.contains(branch)) {
        let mut location = vec!["local"];
        location.extend(remote_map.get(branch).into_iter().flatten().map(String::as_str));
        lines.push(format!("{branch}\t{}", location.join(", ")));
    }
    for (branch, remotes) in &remote_map {
        if !taken.contains(branch) && !local.contains(branch) {
            lines.extend(remotes.iter().map(|remote| format!("{remote}/{branch}\t{remote}")));
        }
    }
    Ok(lines)
}

/// Straight from git's listing — no directory is visited, so a stale
/// record cannot empty the candidates.
fn managed() -> Result<Vec<Worktree>> {
    commands::managed_worktrees(&commands::build_context(None)?)
}

fn claude_sessions(claude: &Claude) -> Result<Vec<String>> {
    if !claude.available() {
        return Ok(Vec::new());
    }
    let ctx: Context = commands::build_context(None)?;
    if ctx.cwd_root == ctx.main {
        return Ok(Vec::new());
    }
    Ok(claude
        .list_new_sessions(&ctx.main, &ctx.cwd_root)
        .into_iter()
        .map(|session| session.id)
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{CommandSpec, OpenerSpec, ScriptSpec};
    use std::path::PathBuf;

    fn worktree(name: &str, locked: Option<&str>) -> Worktree {
        Worktree {
            path: PathBuf::from("/wt").join(name),
            head: String::new(),
            branch: Some(name.into()),
            is_main: false,
            locked: locked.map(str::to_string),
            prunable: None,
        }
    }

    #[test]
    fn commands_are_sorted_and_described_and_claude_is_gated() {
        let hidden = commands(false);
        assert!(hidden.contains(
            &"create\tcreate (or reuse) a worktree for a branch and open it".to_string()
        ));
        assert!(hidden.iter().all(|line| !line.starts_with("claude\t")));
        let names: Vec<&str> = hidden.iter().map(|line| line.split('\t').next().unwrap()).collect();
        let mut sorted = names.clone();
        sorted.sort_unstable();
        assert_eq!(names, sorted);
        assert_eq!(names.len(), cli::SUBCOMMAND_HELP.len() - 1);
        for new in ["lock", "unlock", "prune"] {
            assert!(names.contains(&new));
        }
        assert!(
            commands(true).iter().any(|line| line.starts_with("claude\tClaude Code integration"))
        );
    }

    #[test]
    fn lock_offers_the_unlocked_and_unlock_the_locked_one_line_each() {
        let managed = [
            worktree("a", None),
            worktree("b", Some("on a\n\tusb drive")),
            worktree("c", Some("")),
        ];
        assert_eq!(lockable(&managed), ["a"]);
        assert_eq!(unlockable(&managed), ["b\ton a usb drive", "c\tlocked"]);
    }

    #[test]
    fn scripts_are_sorted_without_the_hidden_ones() {
        let config = Config {
            scripts: [
                ("test", ScriptSpec::command("x")),
                ("step", ScriptSpec { hidden: true, ..ScriptSpec::command("x") }),
                ("build", ScriptSpec::command("x")),
            ]
            .into_iter()
            .map(|(name, spec)| (name.to_string(), spec))
            .collect(),
            ..Config::default()
        };
        assert_eq!(scripts(&config), ["build", "test"]);
    }

    #[test]
    fn openers_are_described_on_one_line() {
        let config = Config {
            openers: [
                (
                    "win",
                    OpenerSpec {
                        from: Some("edit".into()),
                        wrap: Some("kitty".into()),
                        ..OpenerSpec::default()
                    },
                ),
                ("edit", OpenerSpec::command("$EDITOR\t\"$WF_TARGET\"\n")),
                ("broken", OpenerSpec { from: Some("gone".into()), ..OpenerSpec::default() }),
            ]
            .into_iter()
            .map(|(name, spec)| (name.to_string(), spec))
            .collect(),
            wrappers: [(
                "kitty".to_string(),
                CommandSpec { command: "k".into(), background: true },
            )]
            .into_iter()
            .collect(),
            ..Config::default()
        };
        assert_eq!(
            openers(&config),
            ["edit\t$EDITOR \"$WF_TARGET\"", "win\t$EDITOR \"$WF_TARGET\" via kitty"]
        );
    }

    #[test]
    fn an_unknown_topic_and_a_missing_claude_dir_offer_nothing() {
        assert!(complete("no-such-topic").is_empty());
        assert!(complete("").is_empty());
        let nowhere = Claude::at(std::path::Path::new("/no/such/claude/dir"));
        assert_eq!(claude_sessions(&nowhere), Ok(Vec::new()));
        assert_eq!(TOPICS.len(), 9);
    }
}
