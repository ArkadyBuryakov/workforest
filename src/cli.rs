//! clap front-end: error → exit-code mapping and the sole writer to
//! stdout.

use std::env;
use std::io::Write;

use clap::error::ErrorKind as ClapErrorKind;
use clap::{Arg, ArgAction, ArgMatches, Command};
use nix::sys::signal::Signal;

use crate::commands::{self, OpenWith, Outcome};
use crate::errors::{EXIT_OK, ErrorKind, Result};
use crate::integrations::claude::{self, Claude};
use crate::{completions, hooks, output, shellinit, tui, util};

/// Single source for subcommand names and their one-line help:
/// `build_command()` and the `commands` completion topic both read from
/// here.
pub const SUBCOMMAND_HELP: [(&str, &str); 16] = [
    ("create", "create (or reuse) a worktree for a branch and open it"),
    ("open", "open an existing worktree"),
    ("list", "list managed worktrees"),
    ("delete", "delete worktree(s)"),
    ("checkout", "delete a worktree and check its branch out in main"),
    ("lock", "lock a worktree against delete, checkout and prune"),
    ("unlock", "unlock a locked worktree"),
    ("prune", "drop the records of worktrees whose directory is gone"),
    ("run", "run a named script from the merged config"),
    ("make", "run a makefile target (like `make TARGET` at the worktree root)"),
    ("stop", "stop a running script (this worktree's instances, or --all)"),
    ("tui", "interactive mode"),
    ("init", "write a commented .workforest.yaml starter"),
    ("config", "show the merged configuration and its sources"),
    ("shell-init", "print the wf shell wrapper (eval in your shell rc)"),
    ("claude", "Claude Code integration (experimental: may break on any Claude Code update)"),
];

/// Marks a stdout line as a directive for the wf shell wrapper to eval.
/// The unit-separator control byte cannot appear in data output (listings,
/// dumps), so the wrapper never mistakes data for something to execute.
pub const SHELL_DIRECTIVE_PREFIX: &str = "\x1f";

/// Subcommands whose arguments after the first positional belong to the
/// script, flags and all.
const PASSTHROUGH: [&str; 2] = ["run", "make"];

fn help(name: &str) -> &'static str {
    SUBCOMMAND_HELP.iter().find(|(known, _)| *known == name).map_or("", |(_, help)| help)
}

fn subcommand(name: &'static str) -> Command {
    Command::new(name).about(help(name))
}

fn flag(name: &'static str, text: &'static str) -> Arg {
    Arg::new(name).long(name).action(ArgAction::SetTrue).help(text)
}

fn opener_args(command: Command) -> Command {
    command
        .arg(Arg::new("opener").short('o').long("opener").help("opener name or shell command"))
        .arg(
            Arg::new("wrap")
                .short('w')
                .long("wrap")
                .value_name("WRAPPER")
                .help("run the opener through this `wrappers` entry ('' for none)"),
        )
        .arg(
            Arg::new("path")
                .short('p')
                .long("path")
                .help("path inside the worktree, passed to the opener as $WF_TARGET"),
        )
}

fn script_args(command: Command, what: &'static str, appended: &'static str) -> Command {
    let background = if what == "SCRIPT" {
        "detach, with output to a log file (before SCRIPT; after it, it belongs to ARGS)"
    } else {
        "detach, with output to a log file (before TARGET; after it, it belongs to ARGS)"
    };
    command
        .arg(flag("background", background).short('b'))
        .arg(Arg::new("script").value_name(what).required(true))
        .arg(
            Arg::new("args")
                .value_name("ARGS")
                .num_args(0..)
                .trailing_var_arg(true)
                .allow_hyphen_values(true)
                .help(appended),
        )
}

/// The command line as clap sees it; the `claude` subcommand exists only
/// where Claude Code does.
pub fn build_command(with_claude: bool) -> Command {
    let mut command = Command::new("workforest")
        .about("Git worktree forest management")
        .version(env!("CARGO_PKG_VERSION"))
        .disable_version_flag(true)
        .arg(Arg::new("version").long("version").action(ArgAction::Version).help("Print version"))
        .subcommand_required(true)
        .infer_long_args(true)
        .subcommand(
            opener_args(subcommand("create").arg(
                Arg::new("branch").help("branch name or REMOTE/BRANCH (default: current branch)"),
            ))
            .arg(flag("no-hooks", "skip symlinks and setup scripts"))
            .arg(flag("no-open", "create only, do not open")),
        )
        .subcommand(opener_args(
            subcommand("open").arg(Arg::new("name").help("worktree directory name")),
        ))
        .subcommand(
            subcommand("list")
                .arg(flag("porcelain", "stable tab-separated output").conflicts_with("json"))
                .arg(flag("json", "the whole forest (main checkout included) as JSON")),
        )
        .subcommand(
            subcommand("delete")
                .arg(Arg::new("names").value_name("NAME").num_args(1..).required(true))
                .arg(flag("force", "skip the dirty-worktree confirmation"))
                .arg(flag("delete-branch", "also delete the branch").conflicts_with("keep-branch"))
                .arg(flag("keep-branch", "never delete the branch")),
        )
        .subcommand(
            subcommand("checkout")
                .arg(Arg::new("name").value_name("NAME").required(true))
                .arg(flag("force", "skip the dirty-worktree confirmation")),
        )
        .subcommand(
            subcommand("lock").arg(Arg::new("name").value_name("NAME").required(true)).arg(
                Arg::new("reason")
                    .long("reason")
                    .value_name("TEXT")
                    .help("why, shown wherever the lock is"),
            ),
        )
        .subcommand(subcommand("unlock").arg(Arg::new("name").value_name("NAME").required(true)))
        .subcommand(
            subcommand("prune").arg(flag("dry-run", "only say what would be pruned").short('n')),
        )
        .subcommand(script_args(
            subcommand("run"),
            "SCRIPT",
            "appended (shell-quoted) to the script command",
        ))
        .subcommand(script_args(
            subcommand("make"),
            "TARGET",
            "appended (shell-quoted) to the make command",
        ))
        .subcommand(
            subcommand("stop")
                .arg(Arg::new("script").value_name("SCRIPT").required(true))
                .arg(flag("all", "every worktree's instances, not just this one's"))
                .arg(flag("make", "SCRIPT is a makefile target (`make:SCRIPT`)")),
        )
        .subcommand(
            subcommand("tui")
                .arg(Arg::new("mode").help("initial mode (create/open/checkout/delete)")),
        )
        .subcommand(subcommand("init").arg(flag(
            "local",
            "place it in .vscode/ or .idea/ (untracked per-developer override)",
        )))
        .subcommand(subcommand("config").arg(flag("json", "the merged configuration as JSON")))
        .subcommand(
            subcommand("shell-init")
                .arg(Arg::new("shell").value_parser(["bash", "zsh"]).help("default: from $SHELL")),
        );
    if with_claude {
        command = command.subcommand(
            subcommand("claude").subcommand_required(true).subcommand(
                Command::new("copy-session")
                    .about("copy a session from the main worktree into this one")
                    .arg(Arg::new("session_id").value_name("SESSION_ID").required(true)),
            ),
        );
    }
    command
}

/// Split off what belongs to the script: for `run`/`make`, everything
/// after the first positional is its argument list, untouched — a `-b`
/// there is the script's, not ours.
fn split_passthrough(args: &[String]) -> (Vec<String>, Vec<String>) {
    if !args.first().is_some_and(|first| PASSTHROUGH.contains(&first.as_str())) {
        return (args.to_vec(), Vec::new());
    }
    let Some(offset) = args.iter().skip(1).position(|arg| !arg.starts_with('-')) else {
        return (args.to_vec(), Vec::new());
    };
    let (ours, theirs) = args.split_at(offset + 2);
    // A `--` straight after the script only says "arguments follow".
    let theirs = theirs.strip_prefix(&["--".to_string()]).unwrap_or(theirs);
    (ours.to_vec(), theirs.to_vec())
}

fn text<'a>(matches: &'a ArgMatches, name: &str) -> Option<&'a str> {
    matches.get_one::<String>(name).map(String::as_str)
}

fn open_with(matches: &ArgMatches) -> OpenWith<'_> {
    OpenWith {
        opener: text(matches, "opener"),
        wrap: text(matches, "wrap"),
        path: text(matches, "path"),
    }
}

fn dispatch(name: &str, matches: &ArgMatches, passthrough: &[String]) -> Result<Outcome> {
    let context = || commands::build_context(None);
    let required = |name: &str| text(matches, name).unwrap_or_default();
    let background = || matches.get_flag("background").then_some(true);
    match name {
        "create" => commands::cmd_create(
            &context()?,
            text(matches, "branch"),
            open_with(matches),
            matches.get_flag("no-hooks"),
            matches.get_flag("no-open"),
        ),
        "open" => commands::cmd_open(&context()?, text(matches, "name"), open_with(matches)),
        "list" => {
            commands::cmd_list(&context()?, matches.get_flag("porcelain"), matches.get_flag("json"))
        }
        "delete" => {
            let names: Vec<String> =
                matches.get_many::<String>("names").unwrap_or_default().cloned().collect();
            let delete_branch = if matches.get_flag("delete-branch") {
                Some(true)
            } else if matches.get_flag("keep-branch") {
                Some(false)
            } else {
                None
            };
            commands::cmd_delete(&context()?, &names, matches.get_flag("force"), delete_branch)
        }
        "checkout" => {
            commands::cmd_checkout(&context()?, required("name"), matches.get_flag("force"))
        }
        "lock" => commands::cmd_lock(&context()?, required("name"), text(matches, "reason")),
        "unlock" => commands::cmd_unlock(&context()?, required("name")),
        "prune" => commands::cmd_prune(&context()?, matches.get_flag("dry-run")),
        "run" => commands::cmd_run(&context()?, required("script"), passthrough, background()),
        "make" => commands::cmd_make(&context()?, required("script"), passthrough, background()),
        "stop" => commands::cmd_stop(
            &context()?,
            required("script"),
            matches.get_flag("all"),
            matches.get_flag("make"),
        ),
        "init" => commands::cmd_init(&context()?, matches.get_flag("local")),
        "config" => commands::cmd_config_show(matches.get_flag("json")),
        "shell-init" => {
            shellinit::shell_init(text(matches, "shell"), &util::current_env()).map(Outcome::Text)
        }
        "tui" => tui::run(text(matches, "mode")),
        "claude" => {
            let session = matches.subcommand().and_then(|(_, sub)| text(sub, "session_id"));
            claude::cmd_copy_session(session.unwrap_or_default()).map(|()| Outcome::Nothing)
        }
        _ => Ok(Outcome::Nothing),
    }
}

fn write_stdout(text: &str) {
    let mut stdout = std::io::stdout();
    let _ = stdout.write_all(text.as_bytes());
    let _ = stdout.flush();
}

fn emit(outcome: &Outcome) {
    match outcome {
        Outcome::Shell(action) => {
            write_stdout(&format!("{SHELL_DIRECTIVE_PREFIX}{}\n", action.script));
        }
        Outcome::Text(text) if !text.is_empty() => write_stdout(&format!("{text}\n")),
        Outcome::Text(_) | Outcome::Nothing => {}
    }
}

/// Run one invocation; returns the exit code.
pub fn main(argv: &[String]) -> i32 {
    // No args → TUI.
    let args = if argv.is_empty() { vec!["tui".to_string()] } else { argv.to_vec() };

    if args[0] == "--complete" {
        output::quiet(); // candidates only: a config warning waits for a real command
        let topic = args.get(1).map_or("", String::as_str);
        write_stdout(
            &completions::complete(topic)
                .iter()
                .map(|line| format!("{line}\n"))
                .collect::<String>(),
        );
        return EXIT_OK;
    }

    let (parsed, passthrough) = split_passthrough(&args);
    let command = build_command(Claude::standard().available());
    let matches = match command
        .try_get_matches_from(std::iter::once("workforest".to_string()).chain(parsed))
    {
        Ok(matches) => matches,
        // clap prints --help/--version to stdout and usage errors to stderr
        Err(error) => {
            let _ = error.print();
            let informational =
                matches!(error.kind(), ClapErrorKind::DisplayHelp | ClapErrorKind::DisplayVersion);
            return if informational { EXIT_OK } else { error.exit_code() };
        }
    };
    let Some((name, sub)) = matches.subcommand() else {
        return EXIT_OK;
    };

    match dispatch(name, sub, &passthrough) {
        Ok(outcome) => {
            emit(&outcome);
            EXIT_OK
        }
        Err(error) if error.kind == ErrorKind::Interrupted => {
            // Die by SIGINT instead of exiting normally: a parent shell
            // decides whether to abort a loop by how the child died
            // (WIFSIGNALED), not by its exit code. Explicit prompt cancels
            // still exit EXIT_CANCELLED.
            output::info("");
            hooks::die_by(Signal::SIGINT as i32);
            error.exit_code() // only if SIGINT is blocked
        }
        Err(error) => {
            if env::var_os("WORKFOREST_DEBUG").is_some_and(|value| !value.is_empty()) {
                output::info(&format!("{error:?}"));
            }
            output::error(&error.message);
            error.exit_code()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(items: &[&str]) -> Vec<String> {
        items.iter().map(|item| item.to_string()).collect()
    }

    fn parse(args: &[&str]) -> std::result::Result<ArgMatches, clap::Error> {
        let (parsed, _) = split_passthrough(&strings(args));
        build_command(true)
            .try_get_matches_from(std::iter::once("workforest".to_string()).chain(parsed))
    }

    #[test]
    fn every_subcommand_has_its_help_line() {
        let command = build_command(true);
        let names: Vec<&str> = command.get_subcommands().map(Command::get_name).collect();
        assert_eq!(names, SUBCOMMAND_HELP.map(|(name, _)| name));
        for sub in command.get_subcommands() {
            assert_eq!(sub.get_about().unwrap().to_string(), help(sub.get_name()));
        }
        assert!(build_command(false).find_subcommand("claude").is_none());
        build_command(true).debug_assert();
    }

    #[test]
    fn everything_after_the_script_belongs_to_it() {
        let split = |args: &[&str]| split_passthrough(&strings(args));
        assert_eq!(
            split(&["run", "test", "-b", "--x", "y"]),
            (strings(&["run", "test"]), strings(&["-b", "--x", "y"]))
        );
        assert_eq!(
            split(&["run", "-b", "test", "-b"]),
            (strings(&["run", "-b", "test"]), strings(&["-b"]))
        );
        assert_eq!(
            split(&["make", "check", "-j2"]),
            (strings(&["make", "check"]), strings(&["-j2"]))
        );
        assert_eq!(split(&["run", "-b"]), (strings(&["run", "-b"]), strings(&[])));
        assert_eq!(split(&["run", "x", "--", "-b"]), (strings(&["run", "x"]), strings(&["-b"])));
        assert_eq!(
            split(&["run", "x", "a", "--", "b"]),
            (strings(&["run", "x"]), strings(&["a", "--", "b"]))
        );
        assert_eq!(
            split(&["stop", "dev", "--all"]),
            (strings(&["stop", "dev", "--all"]), strings(&[]))
        );
        assert_eq!(split(&[]), (strings(&[]), strings(&[])));
    }

    #[test]
    fn run_takes_background_only_before_the_script() {
        let matches = parse(&["run", "-b", "dev", "-b"]).unwrap();
        let (name, sub) = matches.subcommand().unwrap();
        assert_eq!(
            (name, text(sub, "script"), sub.get_flag("background")),
            ("run", Some("dev"), true)
        );
        let matches = parse(&["run", "dev", "-b"]).unwrap();
        assert!(!matches.subcommand().unwrap().1.get_flag("background"));
        assert_eq!(parse(&["run"]).unwrap_err().exit_code(), 2);
    }

    #[test]
    fn usage_errors_exit_2_and_help_exits_0() {
        for args in [
            &["delete"][..],
            &["nope"],
            &["list", "--porcelain", "--json"],
            &["delete", "x", "--delete-branch", "--keep-branch"],
            &["shell-init", "fish"],
            &["claude"],
            &["checkout"],
            &["list", "--bogus"],
        ] {
            let error = parse(args).unwrap_err();
            assert_eq!(error.exit_code(), 2, "{args:?}");
            assert!(error.use_stderr(), "{args:?}");
        }
        for args in [&["--help"][..], &["create", "--help"], &["--version"]] {
            let error = parse(args).unwrap_err();
            assert!(!error.use_stderr(), "{args:?}");
            assert_eq!(error.exit_code(), 0, "{args:?}");
        }
        assert_eq!(
            parse(&["--version"]).unwrap_err().to_string(),
            format!("workforest {}\n", env!("CARGO_PKG_VERSION"))
        );
    }

    #[test]
    fn long_options_may_be_abbreviated() {
        let matches = parse(&["list", "--porc"]).unwrap();
        assert!(matches.subcommand().unwrap().1.get_flag("porcelain"));
        let matches = parse(&["lock", "x", "--reason=on a usb drive"]).unwrap();
        assert_eq!(text(matches.subcommand().unwrap().1, "reason"), Some("on a usb drive"));
    }

    #[test]
    fn opener_flags_and_an_empty_wrap() {
        let matches =
            parse(&["create", "feat", "-o", "code", "-w", "", "-p", "src", "--no-open"]).unwrap();
        let sub = matches.subcommand().unwrap().1;
        let with = open_with(sub);
        assert_eq!((with.opener, with.wrap, with.path), (Some("code"), Some(""), Some("src")));
        assert!(sub.get_flag("no-open") && !sub.get_flag("no-hooks"));
        let bare = parse(&["open"]).unwrap();
        assert_eq!(text(bare.subcommand().unwrap().1, "name"), None);
    }
}
