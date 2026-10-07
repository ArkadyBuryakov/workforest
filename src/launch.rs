//! Opener resolution, wrapper composition, and cd-protocol emission.
//!
//! No tool names appear here: what to run comes from config (`openers`,
//! `wrappers`) and the $VISUAL/$EDITOR contract; where to run it is the
//! user's terminal (a [`ShellAction`]) unless what spawns is `background`.
//!
//! Every entry is a plain shell command, exactly like `scripts`: it runs
//! via `$SHELL -c` with the WF_* family in the environment, so expansion,
//! quoting, and word splitting are the shell's, never ours — `"$WF_X"` is
//! one argument, bare `$WF_X` word-splits, and there is no workforest
//! template syntax to escape. A wrapper additionally gets the opener
//! command unexpanded as $WF_COMMAND and runs it through a shell of its
//! own, e.g. `kitty ... $SHELL -c "$WF_COMMAND"`.

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::io::{Read, Seek};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use crate::config::{CommandSpec, Config, OpenerSpec};
use crate::errors::{Error, Result};
use crate::output;
use crate::util::{self, Env, repr, shell_quote, shell_quote_path};

/// One tool's shell-session activation state that must not leak into a
/// spawned window. An empty bin subdir means the prefix variable is itself
/// the PATH entry.
struct Activation {
    prefix_var: &'static str,
    /// Subdirs under the prefix that activation put on PATH.
    bin_subdirs: &'static [&'static str],
    /// Variables set alongside the prefix.
    companion_vars: &'static [&'static str],
}

const ACTIVATION_STATE: [Activation; 5] = [
    Activation {
        prefix_var: "VIRTUAL_ENV",
        bin_subdirs: &["bin"],
        companion_vars: &["VIRTUAL_ENV_PROMPT"],
    },
    Activation {
        prefix_var: "CONDA_PREFIX",
        bin_subdirs: &["bin"],
        companion_vars: &["CONDA_DEFAULT_ENV", "CONDA_PROMPT_MODIFIER", "CONDA_SHLVL"],
    },
    Activation { prefix_var: "NVM_BIN", bin_subdirs: &[""], companion_vars: &["NVM_INC"] },
    Activation { prefix_var: "GEM_HOME", bin_subdirs: &["bin"], companion_vars: &["GEM_PATH"] },
    Activation {
        prefix_var: "MY_RUBY_HOME",
        bin_subdirs: &["bin"],
        companion_vars: &["RUBY_VERSION"],
    },
];

/// How long a background command gets to fail: long enough to catch
/// argv/env/display errors, short enough to be imperceptible next to a
/// window opening.
pub const GRACE: Duration = Duration::from_millis(300);

/// A directive for the wf shell wrapper; the only thing `cli.rs` prints to
/// stdout for create/open/checkout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellAction {
    pub script: String,
}

pub fn cd_action(path: &Path) -> ShellAction {
    ShellAction { script: format!("cd {}", shell_quote_path(path)) }
}

/// What a launch spawns: the opener's own command, or its wrapper with the
/// opener command riding along as $WF_COMMAND.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedOpener {
    /// Still unexpanded; its `background` decides where.
    pub run: CommandSpec,
    /// The opener command when `run` is a wrapper.
    pub inner: Option<String>,
}

/// -o VALUE (or `opener`) → `openers` entry, else the value itself as a
/// shell command; default $VISUAL → $EDITOR.
fn opener_spec(config: &Config, opener_arg: Option<&str>, env: &Env) -> Result<OpenerSpec> {
    let value = opener_arg.filter(|value| !value.is_empty()).unwrap_or(&config.opener);
    if !value.is_empty() {
        return Ok(config
            .openers
            .get(value)
            .cloned()
            .unwrap_or_else(|| OpenerSpec::command(value)));
    }
    ["VISUAL", "EDITOR"]
        .into_iter()
        .find_map(|name| util::env_get(env, name))
        .map(OpenerSpec::command)
        .ok_or_else(|| {
            Error::new(
                "no opener: pass -o, set `opener` in a config file, or export $VISUAL/$EDITOR",
            )
        })
}

/// The command an opener entry runs — its own, or its `from` target's with
/// `background` inherited unless overridden — and its wrapper name.
fn effective(config: &Config, spec: &OpenerSpec) -> Result<(CommandSpec, Option<String>)> {
    let base = match &spec.from {
        None => Some(spec),
        Some(from) => config.openers.get(from),
    };
    let Some((base, command)) = base.and_then(|base| Some((base, base.command.clone()?))) else {
        // load_config validates this; only a hand-built Config gets here
        return Err(Error::new(format!(
            "opener 'from: {}' has no command",
            spec.from.as_deref().unwrap_or_default()
        )));
    };
    let background = spec.background.or(base.background).unwrap_or(false);
    Ok((CommandSpec { command, background }, spec.wrap.clone()))
}

/// Resolve the opener to what spawns. --wrap beats the opener's own
/// `wrap`; an empty value means none.
pub fn resolve_opener(
    config: &Config,
    opener_arg: Option<&str>,
    wrap_arg: Option<&str>,
    env: &Env,
) -> Result<ResolvedOpener> {
    let (command, own_wrap) = effective(config, &opener_spec(config, opener_arg, env)?)?;
    let wrap = wrap_arg.map(str::to_string).or(own_wrap).filter(|wrap| !wrap.is_empty());
    let Some(wrap) = wrap else {
        return Ok(ResolvedOpener { run: command, inner: None });
    };
    let Some(wrapper) = config.wrappers.get(&wrap) else {
        let mut known: Vec<&str> = config.wrappers.keys().map(String::as_str).collect();
        known.sort_unstable();
        let known = if known.is_empty() { "none defined".to_string() } else { known.join(", ") };
        return Err(Error::new(format!("unknown wrapper {} (available: {known})", repr(&wrap))));
    };
    Ok(ResolvedOpener { run: wrapper.clone(), inner: Some(command.command) })
}

/// One-line summary for completions and the TUI: the command text, plus
/// the wrapper when there is one.
pub fn describe_opener(config: &Config, name: &str) -> Result<String> {
    let spec = config.openers.get(name).cloned().unwrap_or_else(|| OpenerSpec::command(name));
    let (command, wrap) = effective(config, &spec)?;
    Ok(match wrap.filter(|wrap| !wrap.is_empty()) {
        Some(wrap) => format!("{} via {wrap}", command.command),
        None => command.command,
    })
}

/// Where a launch happens and what it is about.
#[derive(Debug, Clone, Copy)]
pub struct Target<'a> {
    pub main: &'a Path,
    pub worktree: &'a Path,
    pub worktrees_dir: &'a Path,
    pub branch: Option<&'a str>,
}

/// The full WF_* family for a launch: the scripts' structural five, the
/// launch-only WF_TARGET and WF_TITLE, and WF_ENV — all of the above as
/// shell-quoted assignments, for a command string that crosses into a
/// process that does not inherit this environment (a tmux server, ssh):
/// `"export $WF_ENV; $WF_COMMAND"` re-creates the family on the far side.
pub fn launch_vars(target: &Target, path_target: &str) -> Vec<(String, String)> {
    let text = |path: &Path| path.to_string_lossy().into_owned();
    let name = util::file_name(target.main);
    let mut family = vec![
        ("WF_MAIN".to_string(), text(target.main)),
        ("WF_NAME".to_string(), name.clone()),
        ("WF_WORKTREES_DIR".to_string(), text(target.worktrees_dir)),
        ("WF_WORKTREE".to_string(), text(target.worktree)),
        ("WF_BRANCH".to_string(), target.branch.unwrap_or_default().to_string()),
        ("WF_TARGET".to_string(), path_target.to_string()),
        ("WF_TITLE".to_string(), format!("{name}: {}", util::file_name(target.worktree))),
    ];
    family.push(("WF_ENV".to_string(), assignments(&family)));
    family
}

/// `NAME=value ...`, each value shell-quoted.
fn assignments(variables: &[(String, String)]) -> String {
    variables
        .iter()
        .map(|(name, value)| format!("{name}={}", shell_quote(value)))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Open a worktree: a ShellAction for the user's terminal, or a detached
/// spawn (returning None) when what spawns is `background`.
///
/// The launch cwd is always the worktree root; -p only sets WF_TARGET, the
/// opener's argument.
pub fn launch(
    config: &Config,
    target: &Target,
    opener_arg: Option<&str>,
    wrap_arg: Option<&str>,
    path_arg: Option<&str>,
    env: &Env,
) -> Result<Option<ShellAction>> {
    let resolved = resolve_opener(config, opener_arg, wrap_arg, env)?;
    let path_target = path_arg.filter(|path| !path.is_empty()).unwrap_or(".");
    let mut variables = launch_vars(target, path_target);
    if let Some(inner) = resolved.inner {
        variables.push(("WF_COMMAND".to_string(), inner));
    }
    let command = &resolved.run.command;
    if resolved.run.background {
        spawn_background(command, &variables, target.worktree, env)?;
        output::success(&format!("opened {} in the background", util::file_name(target.worktree)));
        return Ok(None);
    }
    // Prefix assignments scope WF_* to the child shell alone — nothing
    // leaks into (or goes stale in) the user's interactive shell — and
    // that child shell, not workforest, expands the command with WF_* in
    // its environment.
    let runner = format!("{} -c {}", shell_quote(&util::user_shell(env)), shell_quote(command));
    Ok(Some(ShellAction {
        script: format!(
            "cd {} && {} {runner}",
            shell_quote_path(target.worktree),
            assignments(&variables)
        ),
    }))
}

fn is_conda_stack(name: &str) -> bool {
    name.strip_prefix("CONDA_PREFIX_")
        .is_some_and(|digits| !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit()))
}

/// A path the way a PATH entry spells it: no doubled or trailing slashes.
fn tidy(path: &str, sub: &str) -> String {
    let base: PathBuf = Path::new(path).components().collect();
    base.join(sub).to_string_lossy().into_owned()
}

/// Drop venv/conda/nvm/rvm activation inherited from the invoking shell.
///
/// A spawned window is a fresh context: the tools' `deactivate`
/// counterparts are shell functions that don't exist there, so inherited
/// activation is unremovable and points at the wrong worktree's
/// environment. Prompt-hook managers (direnv, mise, asdf) re-derive their
/// state in the new shell and need no help; nix-shell is left alone
/// because on NixOS its PATH entries are indistinguishable from the system
/// PATH.
pub fn scrub_activation_state(env: &Env) -> Env {
    let mut env = env.clone();
    let mut stale_dirs = BTreeSet::new();
    let take = |env: &mut Env, name: &str| {
        env.remove(&OsString::from(name)).map(|value| value.to_string_lossy().into_owned())
    };
    for activation in &ACTIVATION_STATE {
        let prefix = take(&mut env, activation.prefix_var);
        for name in activation.companion_vars {
            take(&mut env, name);
        }
        if let Some(prefix) = prefix.filter(|prefix| !prefix.is_empty()) {
            for sub in activation.bin_subdirs {
                stale_dirs.insert(if sub.is_empty() { prefix.clone() } else { tidy(&prefix, sub) });
            }
        }
    }
    let stacked: Vec<String> = env
        .keys()
        .map(|name| name.to_string_lossy().into_owned())
        .filter(|name| is_conda_stack(name))
        .collect();
    for name in stacked {
        if let Some(prefix) = take(&mut env, &name) {
            stale_dirs.insert(tidy(&prefix, "bin"));
        }
    }
    if let Some(path) = util::env_get(&env, "PATH").filter(|_| !stale_dirs.is_empty()) {
        let kept: Vec<&str> =
            path.split(':').filter(|entry| !stale_dirs.contains(*entry)).collect();
        env.insert("PATH".into(), kept.join(":").into());
    }
    env
}

/// An exit status as one number: the exit code, or -N for a death by
/// signal N.
pub fn exit_code(status: ExitStatus) -> i32 {
    status.code().unwrap_or_else(|| -status.signal().unwrap_or(0))
}

fn describe_exit(code: i32) -> String {
    if code < 0 {
        format!("was killed by {}", util::signal_name(-code))
    } else {
        format!("exited with status {code}")
    }
}

/// Spawn a `background` command fully detached, via `$SHELL -c`.
///
/// The shell — not workforest — expands the command, with the WF_* family
/// (plus WF_COMMAND, the still-unexpanded opener command, when a wrapper
/// is involved) in its environment. The inherited environment is passed
/// through `scrub_activation_state` first.
///
/// Detached is not silent: stderr goes to an unlinked temp file (a pipe
/// would SIGPIPE a long-lived window once we exit), and a command that
/// dies within the grace period is reported with that stderr instead of
/// failing invisibly. A quick clean exit is fine — clients that hand off
/// to a daemon (`code .`, `kitty @ launch`) look exactly like that.
pub fn spawn_background(
    command: &str,
    variables: &[(String, String)],
    cwd: &Path,
    env: &Env,
) -> Result<()> {
    let shell = util::user_shell(env);
    let cannot_run = |error: std::io::Error| {
        Error::new(format!(
            "cannot run the opener via $SHELL ({}): {}",
            repr(&shell),
            util::os_error_text(&error)
        ))
    };
    let mut full = env.clone();
    full.extend(variables.iter().map(|(name, value)| (name.into(), value.into())));
    let mut stderr_file = util::anonymous_file().map_err(cannot_run)?;
    let mut process = Command::new(&shell);
    process
        .arg("-c")
        .arg(command)
        .current_dir(cwd)
        .env_clear()
        .envs(scrub_activation_state(&full))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(stderr_file.try_clone().map_err(cannot_run)?);
    // SAFETY: setsid is async-signal-safe, and nothing else runs between
    // fork and exec.
    unsafe {
        process.pre_exec(|| nix::unistd::setsid().map(drop).map_err(std::io::Error::from));
    }
    let mut child = process.spawn().map_err(cannot_run)?;
    let deadline = Instant::now() + GRACE;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            // still running: the window is up or coming up
            _ if Instant::now() >= deadline => return Ok(()),
            _ => thread::sleep(Duration::from_millis(5)),
        }
    };
    let code = exit_code(status);
    if code == 0 {
        return Ok(());
    }
    let mut captured = Vec::new();
    let _ = stderr_file.rewind().and_then(|()| stderr_file.read_to_end(&mut captured));
    let text = String::from_utf8_lossy(&captured);
    let lines = util::splitlines(text.trim());
    let tail = &lines[lines.len().saturating_sub(10)..];
    let mut message = format!("opener {} right after launch", describe_exit(code));
    if !tail.is_empty() {
        message.push_str(":\n");
        message.push_str(&tail.join("\n"));
    }
    Err(Error::new(message))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::output::capture;
    use crate::testing::{Recorder, Sandbox};
    use indexmap::IndexMap;
    use std::fs;

    const EDIT: &str = "$EDITOR \"$WF_TARGET\"";

    fn kitty() -> CommandSpec {
        CommandSpec { command: "kitty $SHELL -c \"$WF_COMMAND\"".into(), background: true }
    }

    fn direnv() -> CommandSpec {
        CommandSpec {
            command: "direnv exec \"$WF_WORKTREE\" $SHELL -c \"$WF_COMMAND\"".into(),
            background: false,
        }
    }

    /// SHELL and EDITOR pinned, nothing else: the isolated environment.
    fn env() -> Env {
        env_with(&[])
    }

    fn env_with(extra: &[(&str, &str)]) -> Env {
        let mut env: Env = [("SHELL", "/bin/sh"), ("EDITOR", "stub-editor")]
            .into_iter()
            .chain(extra.iter().copied())
            .map(|(name, value)| (name.into(), value.into()))
            .collect();
        env.insert("PATH".into(), std::env::var_os("PATH").unwrap());
        env
    }

    fn map<T: Clone>(pairs: &[(&str, T)]) -> IndexMap<String, T> {
        pairs.iter().map(|(name, value)| (name.to_string(), value.clone())).collect()
    }

    fn from(target: &str) -> OpenerSpec {
        OpenerSpec { from: Some(target.into()), ..OpenerSpec::default() }
    }

    fn background(command: &str) -> OpenerSpec {
        OpenerSpec { background: Some(true), ..OpenerSpec::command(command) }
    }

    fn wrapped(spec: OpenerSpec, wrap: &str) -> OpenerSpec {
        OpenerSpec { wrap: Some(wrap.into()), ..spec }
    }

    fn openers(pairs: &[(&str, OpenerSpec)]) -> Config {
        Config { openers: map(pairs), ..Config::default() }
    }

    /// An unwrapped resolution: the opener's own command spawns.
    fn resolved(command: &str, background: bool) -> ResolvedOpener {
        ResolvedOpener { run: CommandSpec { command: command.into(), background }, inner: None }
    }

    fn resolve(
        config: &Config,
        opener: Option<&str>,
        wrap: Option<&str>,
    ) -> Result<ResolvedOpener> {
        resolve_opener(config, opener, wrap, &env())
    }

    // --- opener resolution --------------------------------------------------

    #[test]
    fn named_opener_from_config_and_unknown_name_verbatim() {
        let config = openers(&[("edit", OpenerSpec::command(EDIT))]);
        assert_eq!(resolve(&config, Some("edit"), None), Ok(resolved(EDIT, false)));
        assert_eq!(
            resolve(&config, Some("my-tool --flag"), None),
            Ok(resolved("my-tool --flag", false))
        );
    }

    #[test]
    fn config_default_opener() {
        let config =
            Config { opener: "edit".into(), ..openers(&[("edit", OpenerSpec::command(EDIT))]) };
        assert_eq!(resolve(&config, None, None), Ok(resolved(EDIT, false)));
        assert_eq!(resolve(&config, Some(""), None), Ok(resolved(EDIT, false)));
    }

    #[test]
    fn visual_beats_editor_and_editor_is_the_fallback() {
        let config = Config::default();
        let visual = env_with(&[("VISUAL", "visual-tool")]);
        assert_eq!(
            resolve_opener(&config, None, None, &visual),
            Ok(resolved("visual-tool", false))
        );
        assert_eq!(resolve(&config, None, None), Ok(resolved("stub-editor", false)));
    }

    #[test]
    fn no_opener_anywhere_errors() {
        let error = resolve_opener(&Config::default(), None, None, &Env::new()).unwrap_err();
        assert_eq!(
            error.message,
            "no opener: pass -o, set `opener` in a config file, or export $VISUAL/$EDITOR"
        );
    }

    #[test]
    fn background_opener() {
        let config = Config { opener: "code".into(), ..openers(&[("code", background("code ."))]) };
        assert_eq!(resolve(&config, Some("code"), None), Ok(resolved("code .", true)));
        assert_eq!(resolve(&config, None, None), Ok(resolved("code .", true)));
    }

    #[test]
    fn from_reuses_command_and_background() {
        let config = openers(&[
            ("code", background("code .")),
            ("ide", from("code")),
            ("here", OpenerSpec { background: Some(false), ..from("code") }),
        ]);
        assert_eq!(resolve(&config, Some("ide"), None), Ok(resolved("code .", true)));
        assert_eq!(resolve(&config, Some("here"), None), Ok(resolved("code .", false)));
    }

    #[test]
    fn wrappers_carry_the_opener_as_inner() {
        let config = Config {
            wrappers: map(&[("kitty", kitty()), ("direnv", direnv())]),
            ..openers(&[
                ("edit", OpenerSpec::command(EDIT)),
                ("win", wrapped(from("edit"), "kitty")),
                ("env", wrapped(OpenerSpec::command(EDIT), "direnv")),
            ])
        };
        assert_eq!(
            resolve(&config, Some("win"), None),
            Ok(ResolvedOpener { run: kitty(), inner: Some(EDIT.into()) })
        );
        assert_eq!(
            resolve(&config, Some("env"), None),
            Ok(ResolvedOpener { run: direnv(), inner: Some(EDIT.into()) })
        );
    }

    #[test]
    fn wrapper_decides_background() {
        // a background command through an attached wrapper runs attached,
        // and an attached command through a background wrapper runs detached
        let config = Config {
            wrappers: map(&[("direnv", direnv()), ("kitty", kitty())]),
            ..openers(&[("code", background("code .")), ("edit", OpenerSpec::command(EDIT))])
        };
        assert!(!resolve(&config, Some("code"), Some("direnv")).unwrap().run.background);
        assert!(resolve(&config, Some("edit"), Some("kitty")).unwrap().run.background);
    }

    #[test]
    fn wrap_arg_beats_opener_and_empty_means_none() {
        let config = Config {
            wrappers: map(&[("kitty", kitty()), ("direnv", direnv())]),
            ..openers(&[("win", wrapped(OpenerSpec::command(EDIT), "kitty"))])
        };
        assert_eq!(resolve(&config, Some("win"), None).unwrap().run, kitty());
        assert_eq!(resolve(&config, Some("win"), Some("direnv")).unwrap().run, direnv());
        assert_eq!(resolve(&config, Some("win"), Some("")), Ok(resolved(EDIT, false)));
    }

    #[test]
    fn unknown_wrapper_errors() {
        let config = Config {
            wrappers: map(&[("kitty", kitty())]),
            ..openers(&[("win", wrapped(OpenerSpec::command(EDIT), "kity"))])
        };
        assert_eq!(
            resolve(&config, Some("win"), None).unwrap_err().message,
            "unknown wrapper 'kity' (available: kitty)"
        );
        assert_eq!(
            resolve(&Config::default(), Some("x"), Some("kitty")).unwrap_err().message,
            "unknown wrapper 'kitty' (available: none defined)"
        );
    }

    #[test]
    fn from_without_a_command_errors() {
        // load_config rejects these; a hand-built Config still fails cleanly
        let dangling = openers(&[("a", from("b"))]);
        assert_eq!(
            resolve(&dangling, Some("a"), None).unwrap_err().message,
            "opener 'from: b' has no command"
        );
        let chain = openers(&[("a", from("b")), ("b", from("a"))]);
        assert_eq!(
            resolve(&chain, Some("a"), None).unwrap_err().message,
            "opener 'from: b' has no command"
        );
    }

    #[test]
    fn describe() {
        let config = openers(&[
            ("edit", OpenerSpec::command(EDIT)),
            ("win", wrapped(from("edit"), "kitty")),
            ("git", OpenerSpec::command("lazygit")),
        ]);
        assert_eq!(describe_opener(&config, "win").unwrap(), format!("{EDIT} via kitty"));
        assert_eq!(describe_opener(&config, "git").unwrap(), "lazygit");
        assert_eq!(describe_opener(&config, "edit").unwrap(), EDIT);
        assert_eq!(describe_opener(&config, "anything").unwrap(), "anything");
    }

    // --- the WF_* family ----------------------------------------------------

    fn lookup<'a>(variables: &'a [(String, String)], name: &str) -> &'a str {
        &variables.iter().find(|(key, _)| key == name).unwrap().1
    }

    #[test]
    fn full_family() {
        let target = Target {
            main: Path::new("/t/api"),
            worktree: Path::new("/t/worktrees/api/feat"),
            worktrees_dir: Path::new("/t/worktrees/api"),
            branch: Some("feat/x"),
        };
        let variables = launch_vars(&target, "src/foo.py");
        let names: Vec<&str> = variables.iter().map(|(name, _)| name.as_str()).collect();
        assert_eq!(
            names,
            [
                "WF_MAIN",
                "WF_NAME",
                "WF_WORKTREES_DIR",
                "WF_WORKTREE",
                "WF_BRANCH",
                "WF_TARGET",
                "WF_TITLE",
                "WF_ENV"
            ]
        );
        assert_eq!(
            lookup(&variables, "WF_ENV"),
            "WF_MAIN=/t/api WF_NAME=api WF_WORKTREES_DIR=/t/worktrees/api \
             WF_WORKTREE=/t/worktrees/api/feat WF_BRANCH=feat/x WF_TARGET=src/foo.py WF_TITLE='api: feat'"
        );
    }

    #[test]
    fn wf_env_recreates_the_family_elsewhere() {
        // A command string that crosses into a process without our
        // environment (tmux server, ssh) can `export $WF_ENV` to get it
        // back, quoting intact.
        let target = Target {
            main: Path::new("/t/api"),
            worktree: Path::new("/t/o'brien"),
            worktrees_dir: Path::new("/t"),
            branch: None,
        };
        let variables = launch_vars(&target, "src/a b");
        assert_eq!(lookup(&variables, "WF_BRANCH"), ""); // detached
        // the outer shell expands $WF_ENV into the string; a fresh shell
        // (standing in for the one tmux spawns) re-parses it from scratch
        let probe = "sh -c \"export $WF_ENV; printf %s \\\"$WF_TITLE|$WF_TARGET|$WF_BRANCH.\\\"\"";
        let output = Command::new("sh")
            .args(["-c", probe])
            .envs(variables.iter().map(|(name, value)| (name.as_str(), value.as_str())))
            .output()
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&output.stdout), "api: o'brien|src/a b|.");
    }

    // --- activation state ---------------------------------------------------

    fn environment(pairs: &[(&str, &str)]) -> Env {
        pairs.iter().map(|(name, value)| (name.into(), value.into())).collect()
    }

    #[test]
    fn venv_vars_and_path_entry_removed() {
        let env = environment(&[
            ("VIRTUAL_ENV", "/repo/.venv"),
            ("VIRTUAL_ENV_PROMPT", "(.venv)"),
            ("PATH", "/repo/.venv/bin:/usr/local/bin:/usr/bin"),
            ("HOME", "/home/u"),
        ]);
        assert_eq!(
            scrub_activation_state(&env),
            environment(&[("PATH", "/usr/local/bin:/usr/bin"), ("HOME", "/home/u")])
        );
    }

    #[test]
    fn clean_environment_untouched() {
        let env = environment(&[
            ("PATH", "/usr/local/bin:/usr/bin"),
            ("HOME", "/home/u"),
            ("WF_BRANCH", "feat"),
        ]);
        assert_eq!(scrub_activation_state(&env), env);
    }

    #[test]
    fn conda_including_stacked_prefixes() {
        let env = environment(&[
            ("CONDA_PREFIX", "/opt/conda/envs/proj/"),
            ("CONDA_PREFIX_1", "/opt/conda"),
            ("CONDA_PREFIX_X", "kept"),
            ("CONDA_DEFAULT_ENV", "proj"),
            ("CONDA_SHLVL", "2"),
            ("CONDA_PROMPT_MODIFIER", "(proj) "),
            ("PATH", "/opt/conda/envs/proj/bin:/opt/conda/bin:/opt/conda/condabin:/usr/bin"),
        ]);
        // condabin survives so `conda` itself keeps working in the window.
        assert_eq!(
            scrub_activation_state(&env),
            environment(&[("CONDA_PREFIX_X", "kept"), ("PATH", "/opt/conda/condabin:/usr/bin")])
        );
    }

    #[test]
    fn nvm_bin_is_itself_the_path_entry() {
        let env = environment(&[
            ("NVM_BIN", "/home/u/.nvm/versions/node/v22.0.0/bin"),
            ("NVM_INC", "/home/u/.nvm/versions/node/v22.0.0/include/node"),
            ("PATH", "/home/u/.nvm/versions/node/v22.0.0/bin:/usr/bin"),
        ]);
        assert_eq!(scrub_activation_state(&env), environment(&[("PATH", "/usr/bin")]));
    }

    #[test]
    fn rvm_ruby() {
        let env = environment(&[
            ("GEM_HOME", "/home/u/.rvm/gems/ruby-3.3.0"),
            ("GEM_PATH", "/home/u/.rvm/gems/ruby-3.3.0:/home/u/.rvm/gems/ruby-3.3.0@global"),
            ("MY_RUBY_HOME", "/home/u/.rvm/rubies/ruby-3.3.0"),
            ("RUBY_VERSION", "ruby-3.3.0"),
            (
                "PATH",
                "/home/u/.rvm/gems/ruby-3.3.0/bin:/home/u/.rvm/rubies/ruby-3.3.0/bin:/usr/bin",
            ),
        ]);
        assert_eq!(scrub_activation_state(&env), environment(&[("PATH", "/usr/bin")]));
    }

    #[test]
    fn missing_path_is_fine() {
        assert_eq!(
            scrub_activation_state(&environment(&[("VIRTUAL_ENV", "/repo/.venv")])),
            Env::new()
        );
    }

    // --- launching ----------------------------------------------------------

    struct Launch {
        sandbox: Sandbox,
        worktree: PathBuf,
    }

    impl Launch {
        fn new() -> Self {
            let sandbox = Sandbox::new();
            let worktree = sandbox.path().join("worktrees").join("api").join("feat");
            fs::create_dir_all(&worktree).unwrap();
            Self { sandbox, worktree }
        }

        fn run(
            &self,
            config: &Config,
            opener: Option<&str>,
            wrap: Option<&str>,
            path: Option<&str>,
            env: &Env,
        ) -> Result<Option<ShellAction>> {
            let main = self.sandbox.path().join("api");
            let target = Target {
                main: &main,
                worktree: &self.worktree,
                worktrees_dir: self.worktree.parent().unwrap(),
                branch: Some("feat"),
            };
            capture(|| launch(config, &target, opener, wrap, path, env)).0
        }

        fn script(
            &self,
            config: &Config,
            opener: Option<&str>,
            wrap: Option<&str>,
            path: Option<&str>,
        ) -> String {
            self.run(config, opener, wrap, path, &env()).unwrap().unwrap().script
        }
    }

    /// A config whose only wrapper runs `command` detached.
    fn background_wrapper(command: &str) -> Config {
        Config {
            wrappers: map(&[("win", CommandSpec { command: command.into(), background: true })]),
            ..Config::default()
        }
    }

    #[test]
    fn shell_action_by_default() {
        let launch = Launch::new();
        let script = launch.script(&Config::default(), None, None, None);
        let root = launch.sandbox.path().display().to_string();
        let worktree = launch.worktree.display();
        // SHELL is pinned to /bin/sh; the child shell, not workforest,
        // expands the opener command.
        assert!(
            script.starts_with(&format!("cd {worktree} && WF_MAIN={root}/api WF_NAME=api ")),
            "{script}"
        );
        assert!(script.ends_with(" /bin/sh -c stub-editor"), "{script}");
        assert!(script.contains("WF_TARGET=. WF_TITLE='api: feat' WF_ENV='WF_MAIN="), "{script}");
        // WF_* is scoped to the child shell via prefix assignments, not exported.
        assert!(!script.contains("export"));
    }

    #[test]
    fn opener_is_shell_expanded_not_pre_expanded() {
        let launch = Launch::new();
        let script =
            launch.script(&Config::default(), Some("tool \"$WF_TARGET\""), None, Some("src"));
        // cwd stays the worktree root; -p only sets WF_TARGET, and the
        // command reaches the child shell verbatim — no workforest expansion.
        assert!(script.starts_with(&format!("cd {} && ", launch.worktree.display())));
        assert!(script.ends_with(" /bin/sh -c 'tool \"$WF_TARGET\"'"), "{script}");
        assert!(script.contains("WF_TARGET=src"));
    }

    #[test]
    fn shell_syntax_passes_through_verbatim() {
        // tmux braces, $$, && — all shell business, none of ours.
        let opener = "tmux display -p '#{pane_id}' && echo $$";
        let script = Launch::new().script(&Config::default(), Some(opener), None, None);
        assert!(script.ends_with(&format!(" /bin/sh -c {}", shell_quote(opener))), "{script}");
    }

    #[test]
    fn attached_wrapper_runs_in_the_shell() {
        let config = Config {
            wrappers: map(&[(
                "direnv",
                CommandSpec {
                    command: "direnv exec . $SHELL -c \"$WF_COMMAND\"".into(),
                    background: false,
                },
            )]),
            ..Config::default()
        };
        let launch = Launch::new();
        let script = launch.script(&config, Some("the-opener"), Some("direnv"), None);
        // the wrapper is what the child shell runs; the opener rides along
        // as WF_COMMAND, unexpanded, after the rest of the family
        assert!(script.starts_with(&format!("cd {} && WF_MAIN=", launch.worktree.display())));
        assert!(
            script.ends_with(
                " WF_COMMAND=the-opener /bin/sh -c 'direnv exec . $SHELL -c \"$WF_COMMAND\"'"
            ),
            "{script}"
        );
    }

    #[test]
    fn a_shell_without_a_name_is_sh() {
        let script = Launch::new()
            .run(&Config::default(), Some("x"), None, None, &Env::new())
            .unwrap()
            .unwrap()
            .script;
        assert!(script.ends_with(" sh -c x"), "{script}");
    }

    #[test]
    fn background_command_spawns_detached() {
        let launch = Launch::new();
        let recorder = Recorder::new(launch.sandbox.path());
        let config =
            openers(&[("rec", background(&format!("{} --flag", recorder.path.display())))]);
        // nothing on stdout when spawning in the background
        assert_eq!(launch.run(&config, Some("rec"), None, None, &env()), Ok(None));
        let line = &recorder.wait_for_lines(1)[0];
        assert!(line.contains("argv=--flag"), "{line}");
        assert!(line.contains(&format!("cwd={}", launch.worktree.display())), "{line}");
        assert!(line.contains(&format!("wf_worktree={}", launch.worktree.display())), "{line}");
    }

    #[test]
    fn background_wrapper_spawns_detached_with_the_family() {
        let launch = Launch::new();
        let recorder = Recorder::new(launch.sandbox.path());
        let config = background_wrapper(&format!(
            "{} --title \"$WF_TITLE\" -d \"$WF_WORKTREE\" $WF_COMMAND",
            recorder.path.display()
        ));
        assert_eq!(launch.run(&config, Some("the-opener"), Some("win"), None, &env()), Ok(None));
        let line = &recorder.wait_for_lines(1)[0];
        assert!(line.contains("--title api: feat"), "{line}");
        assert!(line.contains(&format!("-d {} the-opener", launch.worktree.display())), "{line}");
    }

    #[test]
    fn wf_command_word_splits_unless_quoted() {
        let launch = Launch::new();
        let recorder = Recorder::new(launch.sandbox.path());
        let unquoted = background_wrapper(&format!("{} $WF_COMMAND", recorder.path.display()));
        launch.run(&unquoted, Some("the-opener --flag"), Some("win"), None, &env()).unwrap();
        assert!(recorder.wait_for_lines(1)[0].contains("argv=the-opener --flag argc=2"));
        let quoted = background_wrapper(&format!("{} \"$WF_COMMAND\"", recorder.path.display()));
        launch.run(&quoted, Some("the-opener --flag"), Some("win"), None, &env()).unwrap();
        assert!(recorder.wait_for_lines(2)[1].contains("argv=the-opener --flag argc=1"));
    }

    #[test]
    fn background_process_sheds_inherited_venv() {
        let launch = Launch::new();
        let recorder = Recorder::new(launch.sandbox.path());
        let venv = launch.sandbox.path().join(".venv");
        let mut activated = env_with(&[("VIRTUAL_ENV", venv.to_str().unwrap())]);
        let path = format!(
            "{}/bin:{}",
            venv.display(),
            activated["PATH".as_ref() as &std::ffi::OsStr].to_string_lossy()
        );
        activated.insert("PATH".into(), path.into());
        let config = background_wrapper(&format!("{} $WF_COMMAND", recorder.path.display()));
        launch.run(&config, Some("x"), Some("win"), None, &activated).unwrap();
        assert!(recorder.wait_for_lines(1)[0].ends_with("virtual_env="));
    }

    #[test]
    fn missing_and_non_executable_background_programs() {
        // The shell reports the missing program (127) through the grace check.
        let launch = Launch::new();
        let missing = background_wrapper("no-such-terminal-xyz $WF_COMMAND");
        let error = launch.run(&missing, Some("x"), Some("win"), None, &env()).unwrap_err();
        assert!(
            error.message.starts_with("opener exited with status 127 right after launch:\n"),
            "{error}"
        );
        assert!(error.message.contains("not found"), "{error}");

        let program = launch.sandbox.path().join("not-executable");
        fs::write(&program, "#!/bin/sh\n").unwrap();
        let denied = background_wrapper(&format!("{} $WF_COMMAND", program.display()));
        let error = launch.run(&denied, Some("x"), Some("win"), None, &env()).unwrap_err();
        assert!(error.message.to_lowercase().contains("permission denied"), "{error}");
    }

    #[test]
    fn a_shell_that_cannot_start_is_named() {
        let launch = Launch::new();
        let broken = env_with(&[("SHELL", "/no/such/shell")]);
        let config = openers(&[("bg", background("true"))]);
        let error = launch.run(&config, Some("bg"), None, None, &broken).unwrap_err();
        assert_eq!(
            error.message,
            "cannot run the opener via $SHELL ('/no/such/shell'): No such file or directory"
        );
    }

    #[test]
    fn quote_in_wf_value_passes_through() {
        // A quote in a WF_* value is just a character in an env var, never
        // a parse error.
        let sandbox = Sandbox::new();
        let recorder = Recorder::new(sandbox.path());
        let variables = [("WF_TITLE".to_string(), "o'brien: feat".to_string())];
        let command = format!("{} --title \"$WF_TITLE\"", recorder.path.display());
        spawn_background(&command, &variables, sandbox.path(), &env()).unwrap();
        assert!(recorder.wait_for_lines(1)[0].contains("--title o'brien: feat"));
    }

    #[test]
    fn cd_action_quotes() {
        assert_eq!(cd_action(Path::new("/tmp/with space")).script, "cd '/tmp/with space'");
    }

    // --- the grace period ---------------------------------------------------

    fn run_background(body: &str) -> (Result<Option<ShellAction>>, String) {
        let launch = Launch::new();
        let program = launch.sandbox.path().join("stub-term");
        crate::testing::write_executable(&program, &format!("#!/bin/sh\n{body}\n"));
        let config = openers(&[("bg", background(program.to_str().unwrap()))]);
        let main = launch.sandbox.path().join("api");
        let target = Target {
            main: &main,
            worktree: &launch.worktree,
            worktrees_dir: launch.worktree.parent().unwrap(),
            branch: Some("feat"),
        };
        capture(|| super::launch(&config, &target, Some("bg"), None, None, &env()))
    }

    #[test]
    fn immediate_failure_reports_status_and_stderr() {
        let error = run_background("echo 'cannot open display' >&2\nexit 2").0.unwrap_err();
        assert_eq!(
            error.message,
            "opener exited with status 2 right after launch:\ncannot open display"
        );
    }

    #[test]
    fn a_silent_failure_and_a_long_stderr() {
        let error = run_background("exit 3").0.unwrap_err();
        assert_eq!(error.message, "opener exited with status 3 right after launch");
        let error =
            run_background("for i in 1 2 3 4 5 6 7 8 9 10 11 12; do echo line$i >&2; done; exit 1")
                .0
                .unwrap_err();
        assert_eq!(error.message.lines().count(), 11);
        assert!(
            error.message.ends_with("line11\nline12") && !error.message.contains("line2\n"),
            "{error}"
        );
    }

    #[test]
    fn death_by_signal_reports_signal_name() {
        // The opener command itself, not a script it runs: a shell that
        // forks for its last command (dash) would outlive the signal and
        // report an exit status instead.
        let launch = Launch::new();
        let config = openers(&[("bg", background("kill -TERM $$"))]);
        let error = launch.run(&config, Some("bg"), None, None, &env()).unwrap_err();
        assert_eq!(error.message, "opener was killed by SIGTERM right after launch");
    }

    #[test]
    fn long_lived_process_and_quick_clean_exit_are_success() {
        // outlived the grace period: spawned fine, nothing on stdout
        assert_eq!(run_background("sleep 5").0, Ok(None));
        // daemon-handoff clients (`code .`) exit 0 immediately; not a failure
        let (result, shown) = run_background("exit 0");
        assert_eq!(result, Ok(None));
        assert!(shown.contains("opened feat in the background"), "{shown}");
    }
}
