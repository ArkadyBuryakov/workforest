//! Interactive mode: pick a mode, a worktree or branch, and an opener,
//! then run the command they add up to.
//!
//! Mode tabs (←/→, alt-h/alt-l), an opener carousel for CREATE/OPEN
//! (ctrl-←/ctrl-→), type to filter, Enter accepts (in CREATE a
//! non-matching query becomes a new branch), Esc quits, DELETE stays in
//! the loop for bulk cleanup.
//!
//! `app.rs` (state) and `view.rs` (drawing) know nothing of a terminal and
//! are unit-tested; `term.rs` is the loop that connects them to one.

mod app;
mod term;
mod view;

use crate::commands::{self, Context, OpenWith, Outcome};
use crate::errors::{Error, Result};
use crate::integrations::claude::Claude;
use crate::{completions, output, util};
pub use app::{Mode, Opener, Row};

pub fn available_modes(ctx: &Context, claude: &Claude) -> Vec<Mode> {
    let mut modes = Mode::BASE.to_vec();
    if claude.available() && ctx.cwd_root != ctx.main {
        modes.push(Mode::Claude);
    }
    modes
}

/// Config `openers` keys, or the derived fallback pair — default opener
/// and $SHELL.
pub fn opener_carousel(ctx: &Context) -> Vec<Opener> {
    if !ctx.config.openers.is_empty() {
        return ctx
            .config
            .openers
            .keys()
            .map(|name| Opener { label: name.clone(), arg: Some(name.clone()) })
            .collect();
    }
    let mut entries = vec![Opener { label: "edit".into(), arg: None }];
    if let Some(shell) = util::env_get(&ctx.env, "SHELL") {
        entries.push(Opener { label: "shell".into(), arg: Some(shell) });
    }
    entries
}

/// What a mode offers: branches to create a worktree for, worktrees with
/// their state, or sessions to copy.
pub fn rows(ctx: &Context, mode: Mode, claude: &Claude) -> Result<Vec<Row>> {
    Ok(match mode {
        // NAME<TAB>LOCATION lines, as the shell completes them
        Mode::Create => completions::complete("branches")
            .iter()
            .map(|line| {
                let (name, location) = line.split_once('\t').unwrap_or((line, ""));
                Row::new(name, location, "")
            })
            .collect(),
        Mode::Claude => claude
            .list_new_sessions(&ctx.main, &ctx.cwd_root)
            .iter()
            .map(|session| Row::new(&session.id, &session.description, ""))
            .collect(),
        Mode::Open | Mode::Checkout | Mode::Delete => {
            commands::inspect(commands::managed_worktrees(ctx)?)
                .iter()
                // A stale worktree has no directory to open or to check
                // out from; `delete` is what clears it.
                .filter(|listed| mode == Mode::Delete || !listed.state.stale)
                .map(|listed| {
                    Row::new(&listed.worktree.name(), listed.branch_label(), &listed.state.label())
                })
                .collect()
        }
    })
}

pub fn execute(
    ctx: &Context,
    mode: Mode,
    selection: &str,
    opener: Option<&str>,
    claude: &Claude,
) -> Result<Outcome> {
    let with = OpenWith { opener, ..OpenWith::default() };
    match mode {
        Mode::Create => commands::cmd_create(ctx, Some(selection), with, false, false),
        Mode::Open => commands::cmd_open(ctx, Some(selection), with),
        Mode::Checkout => commands::cmd_checkout(ctx, selection, false),
        Mode::Delete => commands::cmd_delete(ctx, &[selection.to_string()], false, None),
        Mode::Claude => {
            claude.copy_session(selection, &ctx.main, &ctx.cwd_root).map(|()| Outcome::Nothing)
        }
    }
}

/// The mode to start in: the one asked for when there is such a mode,
/// else OPEN when there is something to open, else CREATE.
fn initial_mode(asked: Option<&str>, modes: &[Mode], has_worktrees: bool) -> Mode {
    asked
        .and_then(Mode::from_name)
        .filter(|mode| modes.contains(mode))
        .unwrap_or(if has_worktrees { Mode::Open } else { Mode::Create })
}

/// The interactive loop.
pub fn run(asked_mode: Option<&str>) -> Result<Outcome> {
    let ctx = commands::build_context(None)?;
    if !output::interactive() {
        return Err(Error::new(
            "the TUI needs a terminal; all actions are also available as plain subcommands",
        ));
    }
    let claude = Claude::standard();
    let modes = available_modes(&ctx, &claude);
    let start = initial_mode(asked_mode, &modes, !commands::managed_worktrees(&ctx)?.is_empty());
    let mut app = app::App::new(modes, start, opener_carousel(&ctx));
    loop {
        let picked = term::interact(&mut app, &mut |mode| rows(&ctx, mode, &claude))?;
        let Some(selection) = picked else {
            return Ok(Outcome::Nothing);
        };
        let outcome = execute(&ctx, app.mode(), &selection, app.opener_arg().as_deref(), &claude)?;
        if app.mode() != Mode::Delete {
            return Ok(outcome);
        }
        // bulk cleanup: stay in the loop
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Config, OpenerSpec};
    use crate::output::capture;
    use crate::testing::{Repo, Sandbox};
    use std::fs;
    use std::path::Path;

    fn context(repo: &Repo, cwd: &Path, config: Config) -> Context {
        Context {
            cwd_root: cwd.to_path_buf(),
            main: repo.path.clone(),
            worktrees_dir: repo.path.parent().unwrap().join("worktrees").join("api"),
            config,
            env: [("SHELL", "/bin/sh"), ("EDITOR", "stub-editor")]
                .into_iter()
                .map(|(name, value)| (name.into(), value.into()))
                .collect(),
        }
    }

    fn no_claude() -> Claude {
        Claude::at(Path::new("/no/such/claude"))
    }

    fn create(ctx: &Context, branch: &str) {
        capture(|| commands::cmd_create(ctx, Some(branch), OpenWith::default(), false, true))
            .0
            .unwrap();
    }

    fn names(rows: &[Row]) -> Vec<&str> {
        rows.iter().map(|row| row.name.as_str()).collect()
    }

    #[test]
    fn config_openers_make_the_carousel_in_their_own_order() {
        let sandbox = Sandbox::new();
        let repo = sandbox.repo("api");
        let config = Config {
            openers: [("win", "x"), ("git", "lazygit"), ("edit", "$EDITOR")]
                .into_iter()
                .map(|(name, command)| (name.to_string(), OpenerSpec::command(command)))
                .collect(),
            ..Config::default()
        };
        let carousel = opener_carousel(&context(&repo, &repo.path, config));
        let labels: Vec<(&str, Option<&str>)> =
            carousel.iter().map(|opener| (opener.label.as_str(), opener.arg.as_deref())).collect();
        assert_eq!(labels, [("win", Some("win")), ("git", Some("git")), ("edit", Some("edit"))]);
    }

    #[test]
    fn without_openers_the_carousel_is_the_default_and_the_shell() {
        let sandbox = Sandbox::new();
        let repo = sandbox.repo("api");
        let mut ctx = context(&repo, &repo.path, Config::default());
        assert_eq!(
            opener_carousel(&ctx),
            [
                Opener { label: "edit".into(), arg: None },
                Opener { label: "shell".into(), arg: Some("/bin/sh".into()) }
            ]
        );
        ctx.env.remove(std::ffi::OsStr::new("SHELL"));
        assert_eq!(opener_carousel(&ctx), [Opener { label: "edit".into(), arg: None }]);
    }

    #[test]
    fn claude_mode_only_in_a_non_main_worktree_with_claude_around() {
        let sandbox = Sandbox::new();
        let repo = sandbox.repo("api");
        let claude_dir = sandbox.path().join(".claude");
        fs::create_dir(&claude_dir).unwrap();
        let claude = Claude::at(&claude_dir);
        let main = context(&repo, &repo.path, Config::default());
        assert_eq!(available_modes(&main, &claude), Mode::BASE);
        let inner = context(&repo, &main.worktrees_dir.join("feat"), Config::default());
        assert_eq!(available_modes(&inner, &claude).last(), Some(&Mode::Claude));
        assert_eq!(available_modes(&inner, &no_claude()), Mode::BASE);
    }

    #[test]
    fn worktree_rows_carry_branch_and_state() {
        let sandbox = Sandbox::new();
        let repo = sandbox.repo("api");
        let ctx = context(&repo, &repo.path, Config::default());
        create(&ctx, "feature/feat");
        create(&ctx, "gone");
        repo.make_dirty(&ctx.worktrees_dir.join("feat"));
        crate::git::worktree_lock(&repo.path, &ctx.worktrees_dir.join("feat"), None).unwrap();
        fs::remove_dir_all(ctx.worktrees_dir.join("gone")).unwrap();

        let feat = Row::new("feat", "feature/feat", "dirty locked");
        // a stale worktree is only offered for delete
        for mode in [Mode::Open, Mode::Checkout] {
            assert_eq!(rows(&ctx, mode, &no_claude()).unwrap(), std::slice::from_ref(&feat));
        }
        let mut deletable = rows(&ctx, Mode::Delete, &no_claude()).unwrap();
        deletable.sort_by(|a, b| a.name.cmp(&b.name));
        assert_eq!(deletable, [feat, Row::new("gone", "gone", "stale")]);
    }

    #[test]
    fn claude_rows_are_the_sessions_not_copied_yet() {
        let sandbox = Sandbox::new();
        let repo = sandbox.repo("api");
        let claude = Claude::at(&sandbox.path().join(".claude"));
        let ctx = context(&repo, &sandbox.path().join("dev/worktrees/api/feat"), Config::default());
        let project = claude.project_dir(&repo.path);
        fs::create_dir_all(&project).unwrap();
        fs::write(project.join("abc.jsonl"), "{}\n").unwrap();
        assert_eq!(
            rows(&ctx, Mode::Claude, &claude).unwrap(),
            [Row::new("abc", "(no description)", "")]
        );
        let (outcome, shown) = capture(|| execute(&ctx, Mode::Claude, "abc", None, &claude));
        assert_eq!(outcome, Ok(Outcome::Nothing));
        assert!(shown.starts_with("copied session 'abc' to "), "{shown}");
        assert!(rows(&ctx, Mode::Claude, &claude).unwrap().is_empty());
    }

    #[test]
    fn executing_runs_the_command_the_mode_stands_for() {
        let sandbox = Sandbox::new();
        let repo = sandbox.repo("api");
        let ctx = context(&repo, &repo.path, Config::default());
        let run = |mode, selection: &str, opener: Option<&str>| {
            capture(|| execute(&ctx, mode, selection, opener, &no_claude())).0
        };
        let Outcome::Shell(action) = run(Mode::Create, "feat", Some("my-opener")).unwrap() else {
            panic!("create opens what it made");
        };
        assert!(action.script.ends_with(" /bin/sh -c my-opener"), "{}", action.script);
        let Outcome::Shell(action) = run(Mode::Open, "feat", None).unwrap() else {
            panic!("open opens");
        };
        assert!(action.script.ends_with(" /bin/sh -c stub-editor"), "{}", action.script);
        assert!(names(&rows(&ctx, Mode::Open, &no_claude()).unwrap()).contains(&"feat"));

        assert_eq!(run(Mode::Delete, "feat", None), Ok(Outcome::Nothing));
        assert!(run(Mode::Delete, "feat", None).is_err());
        run(Mode::Create, "feat2", Some("true")).unwrap();
        let Outcome::Shell(action) = run(Mode::Checkout, "feat2", None).unwrap() else {
            panic!("checkout moves the shell to main");
        };
        assert_eq!(action.script, format!("cd {}", repo.path.display()));
    }

    #[test]
    fn the_first_mode_is_the_one_asked_for_or_the_useful_one() {
        let modes = Mode::BASE.to_vec();
        assert_eq!(initial_mode(Some("delete"), &modes, true), Mode::Delete);
        assert_eq!(initial_mode(Some("claude"), &modes, true), Mode::Open); // not available here
        assert_eq!(initial_mode(Some("nope"), &modes, false), Mode::Create);
        assert_eq!(initial_mode(None, &modes, true), Mode::Open);
        assert_eq!(initial_mode(None, &modes, false), Mode::Create);
    }
}
