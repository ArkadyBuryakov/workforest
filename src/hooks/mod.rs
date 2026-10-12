//! Creation hooks (symlinks, setup scripts) and named-script execution.
//!
//! Scripts get exactly one environment variable family, WF_*, and run via
//! `$SHELL -c` (`sh -c` fallback). Their stdout is routed to our stderr so
//! the cd protocol on stdout stays clean.
//!
//! A `wf run` command gets a process group of its own so that it can be
//! stopped as a whole — by `wf stop`, by an `exclusive` script starting
//! elsewhere, or by a signal forwarded from us. On a terminal the group is
//! made the foreground one, as a shell would, so Ctrl-C and Ctrl-Z reach
//! the command directly; we reclaim the terminal when it ends. A
//! `background` script runs the same way under a detached supervisor (a
//! fork of us) with its output in a log file. The `cleanup` command then
//! runs however the command ended, and only afterwards is the job record
//! removed (that removal is what a stopper waits for).
//!
//! A group (`bulk`, `pipeline`) is run by a supervisor — a fork of us that
//! leads the process group and takes the terminal exactly as a command
//! would, so everything above applies to it unchanged. Inside, a pipeline
//! runs its members one after another like consecutive `wf run`s; a bulk
//! forks a runner per member, gives each an output channel of its own, and
//! relays their lines to its stderr prefixed with the member's name.
//! Members keep their own records, cleanup, and `exclusive` semantics:
//! `wf stop MEMBER` works while a group runs it.
//!
//! The process machinery — fork, process groups, the terminal — is in
//! `run.rs`; what is decided from plain data is here.

mod run;

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::io::Write;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};

use indexmap::IndexMap;
use nix::sys::signal::Signal;

use crate::config::{Config, ScriptSpec};
use crate::errors::{Error, ErrorKind, Result};
use crate::util::{self, Env, repr, shell_quote};
use crate::{git, jobs, makefile, output};

pub use run::{die_by, flush_coverage, run_snippet};

pub const EXCLUDE_FILE_NAME: &str = "workforest.exclude";
const SIGINT: i32 = Signal::SIGINT as i32;

/// The environment a script runs in: ours, plus the WF_* family.
pub fn script_env(
    base: &Env,
    main: &Path,
    worktree: &Path,
    worktrees_dir: &Path,
    branch: Option<&str>,
) -> Env {
    let mut env = base.clone();
    env.insert("WF_MAIN".into(), main.into());
    env.insert("WF_NAME".into(), util::file_name(main).into());
    env.insert("WF_WORKTREE".into(), worktree.into());
    env.insert("WF_WORKTREES_DIR".into(), worktrees_dir.into());
    env.insert("WF_BRANCH".into(), branch.unwrap_or_default().into());
    env
}

/// Symlink configured repo-root-relative paths from main into the
/// worktree; returns the created relative paths.
pub fn create_symlinks(config: &Config, main: &Path, worktree: &Path) -> Result<Vec<String>> {
    let failed = |path: &Path, error: std::io::Error| {
        Error::new(format!("cannot symlink {}: {}", path.display(), util::os_error_text(&error)))
    };
    let mut created = Vec::new();
    for rel in &config.symlinks {
        let rel = rel.trim_matches('/');
        if rel.is_empty() {
            continue;
        }
        let (src, dst) = (main.join(rel), worktree.join(rel));
        if !src.exists() {
            output::warn(&format!("symlink source does not exist, skipping: {}", src.display()));
            continue;
        }
        if dst.exists() && !dst.is_symlink() {
            output::warn(&format!(
                "destination exists and is not a symlink, skipping: {}",
                dst.display()
            ));
            continue;
        }
        if let Some(parent) = dst.parent() {
            fs::create_dir_all(parent).map_err(|error| failed(&dst, error))?;
        }
        if dst.is_symlink() {
            fs::remove_file(&dst).map_err(|error| failed(&dst, error))?;
        }
        symlink(&src, &dst).map_err(|error| failed(&dst, error))?;
        output::success(&format!("symlinked {rel} -> {}", src.display()));
        created.push(rel.to_string());
    }
    if !created.is_empty() {
        exclude_from_git(worktree, &created)?;
    }
    Ok(created)
}

/// Hide the given root-relative paths from git status in this worktree
/// only, via a per-worktree core.excludesFile seeded with the user's
/// global excludes (so overriding the file loses nothing).
pub fn exclude_from_git(worktree: &Path, rel_paths: &[String]) -> Result<()> {
    let exclude_file = git::git_dir(worktree)?.join(EXCLUDE_FILE_NAME);
    git::set_config(worktree, "extensions.worktreeConfig", "true", false)?;
    git::set_config(worktree, "core.excludesFile", &exclude_file.to_string_lossy(), true)?;

    let mut lines =
        vec!["# Managed by workforest: symlinks from the `symlinks` config key".to_string()];
    let global_excludes = git::global_excludes_file();
    if global_excludes.is_file() {
        lines.push(format!(
            "# --- snapshot of global core.excludesFile ({}), \
             taken at worktree creation; later edits there do not apply here ---",
            global_excludes.display()
        ));
        let text = fs::read(&global_excludes).unwrap_or_default();
        lines.push(String::from_utf8_lossy(&text).trim_end_matches('\n').to_string());
        lines.push("# --- workforest symlinks ---".to_string());
    }
    lines.extend(rel_paths.iter().map(|rel| format!("/{rel}")));
    fs::write(&exclude_file, lines.join("\n") + "\n").map_err(|error| {
        Error::new(format!(
            "cannot write {}: {}",
            exclude_file.display(),
            util::os_error_text(&error)
        ))
    })?;
    output::success(&format!("excluded {} symlink(s) from git in this worktree", rel_paths.len()));
    Ok(())
}

/// Run setup_scripts in order; failures warn but do not abort. Returns the
/// number of failed scripts.
pub fn run_setup_scripts(config: &Config, worktree: &Path, env: &Env) -> Result<usize> {
    let mut failures = 0;
    for snippet in &config.setup_scripts {
        output::success(&format!("running setup script: {snippet}"));
        if run_snippet(snippet, worktree, env)? != 0 {
            output::warn(&format!("setup script failed: {snippet}"));
            failures += 1;
        }
    }
    Ok(failures)
}

/// How a run ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct JobResult {
    /// Exit status, or -N when killed by signal N.
    pub code: i32,
    pub stopped_by: Option<String>,
}

/// One `wf run` invocation, resolved: the script, its final command text
/// (empty for a group), and where it runs and is recorded. `tty` is
/// whether it may take the terminal's foreground: a bulk's members may
/// not, since only one group can have it.
#[derive(Debug, Clone)]
pub(crate) struct Job<'a> {
    pub config: &'a Config,
    pub spec: ScriptSpec,
    pub name: String,
    pub snippet: String,
    pub cwd: PathBuf,
    pub env: Env,
    /// Where the record and the log live.
    pub common_dir: PathBuf,
    pub tty: bool,
}

impl Job<'_> {
    pub fn record_path(&self, pid: i32) -> PathBuf {
        jobs::record_path(&self.common_dir, &self.name, &self.cwd, pid)
    }

    /// The log of the instance owned by `pid`: the detached supervisor,
    /// which names its own log from the inside and is named from the
    /// outside by the `wf run` that forked it.
    pub fn log_path(&self, pid: i32) -> PathBuf {
        jobs::log_path(&self.common_dir, &self.name, &self.cwd, pid)
    }
}

pub(crate) fn run_cleanup(spec: &ScriptSpec, name: &str, cwd: &Path, env: &Env) -> Result<()> {
    let Some(cleanup) = &spec.cleanup else {
        return Ok(());
    };
    output::success(&format!("running cleanup for {}: {cleanup}", repr(name)));
    let code = run_snippet(cleanup, cwd, env)?;
    if code != 0 {
        output::warn(&format!("cleanup for {} failed with exit code {code}", repr(name)));
    }
    Ok(())
}

/// Killed by SIGINT — or, as a shell ends after its child was, exit
/// 128+SIGINT: Ctrl-C reached the command either way.
pub(crate) fn interrupted(code: i32) -> bool {
    code == -SIGINT || code == 128 + SIGINT
}

/// What went wrong, or None for success or an interruption.
pub(crate) fn failure(result: &JobResult, name: &str) -> Option<String> {
    if result.code == 0 || interrupted(result.code) {
        return None;
    }
    if result.code < 0 {
        let mut message =
            format!("script {} was killed by {}", repr(name), util::signal_name(-result.code));
        if let Some(by) = result.stopped_by.as_deref().filter(|by| !by.is_empty()) {
            message.push_str(&format!(" (stopped by {by})"));
        }
        return Some(message);
    }
    Some(format!("script {} failed with exit code {}", repr(name), result.code))
}

/// An interrupted command ends us the way Ctrl-C would, so a shell loop
/// around `wf run` aborts.
pub(crate) fn raise_for(result: &JobResult, name: &str) -> Result<()> {
    if interrupted(result.code) {
        output::warn(&format!("script {} was interrupted", repr(name)));
        return Err(Error::interrupted());
    }
    match failure(result, name) {
        None => Ok(()),
        Some(message) if result.code < 0 => {
            Err(Error::of(ErrorKind::ScriptKilled(-result.code), message))
        }
        Some(message) => Err(Error::new(message)),
    }
}

/// The last lines of a log, or "" when there is nothing to read (the
/// supervisor died before it could open one).
pub(crate) fn log_tail(log_path: &Path, lines: usize) -> String {
    let Ok(bytes) = fs::read(log_path) else {
        return String::new();
    };
    let text = String::from_utf8_lossy(&bytes);
    let all = util::splitlines(text.trim());
    all[all.len().saturating_sub(lines)..].join("\n")
}

// --- groups -----------------------------------------------------------------

/// A bulk member's runner process and the read end of its output channel;
/// `fd` is -1 once the channel is closed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Runner {
    pub name: String,
    pub pid: i32,
    pub fd: i32,
    /// Its outcome once reaped: exit status, or -N for signal N.
    pub code: Option<i32>,
}

const PALETTE: [&str; 6] = ["\x1b[36m", "\x1b[35m", "\x1b[34m", "\x1b[32m", "\x1b[33m", "\x1b[91m"];
const RESET: &str = "\x1b[0m";

/// Turns a bulk member's raw output into lines prefixed with its name,
/// padded so the prefixes line up and colored when stderr is a terminal.
/// Bytes are kept until their line completes; a pty's `\r\n` becomes `\n`.
#[derive(Debug)]
pub(crate) struct Prefixer {
    names: Vec<String>,
    color: bool,
    pending: HashMap<String, Vec<u8>>,
}

impl Prefixer {
    pub fn new(names: &[String], color: bool) -> Self {
        Self { names: names.to_vec(), color, pending: HashMap::new() }
    }

    pub fn prefix(&self, name: &str) -> String {
        let width = self.names.iter().map(|name| name.chars().count()).max().unwrap_or(0);
        let label = format!("{name:<width$} | ");
        if !self.color {
            return label;
        }
        let index = self.names.iter().position(|known| known == name).unwrap_or(0);
        format!("{}{label}{RESET}", PALETTE[index % PALETTE.len()])
    }

    /// Complete lines out of `data` (and what was pending), prefixed.
    pub fn feed(&mut self, name: &str, data: &[u8]) -> String {
        let mut buffered = self.pending.remove(name).unwrap_or_default();
        buffered.extend_from_slice(data);
        let mut lines: Vec<&[u8]> = buffered.split(|byte| *byte == b'\n').collect();
        let rest = lines.pop().unwrap_or_default();
        let prefix = self.prefix(name);
        let out = lines
            .iter()
            .map(|line| {
                let line = line.strip_suffix(b"\r").unwrap_or(line);
                format!("{prefix}{}\n", String::from_utf8_lossy(line))
            })
            .collect();
        if !rest.is_empty() {
            self.pending.insert(name.to_string(), rest.to_vec());
        }
        out
    }

    /// A final partial line, terminated.
    pub fn flush(&mut self, name: &str) -> String {
        if self.pending.get(name).is_some_and(|rest| !rest.is_empty()) {
            self.feed(name, b"\n")
        } else {
            String::new()
        }
    }
}

fn describe_outcome(code: i32) -> String {
    if code < 0 {
        format!("was killed by {}", util::signal_name(-code))
    } else {
        format!("failed with exit code {code}")
    }
}

/// The group's outcome from its members': success only when all
/// succeeded; an interruption wins (Ctrl-C stays Ctrl-C, and each runner
/// has already said so), else any other signal death, else the first
/// failing member's status.
pub(crate) fn bulk_outcome(name: &str, runners: &[Runner]) -> i32 {
    let failed: Vec<(&str, i32)> = runners
        .iter()
        .filter_map(|runner| Some((runner.name.as_str(), runner.code.filter(|code| *code != 0)?)))
        .collect();
    if failed.iter().any(|(_, code)| interrupted(*code)) {
        return -SIGINT;
    }
    for (member, code) in &failed {
        output::error(&format!(
            "member {} of {} {}",
            repr(member),
            repr(name),
            describe_outcome(*code)
        ));
    }
    failed
        .iter()
        .map(|(_, code)| *code)
        .find(|code| *code < 0)
        .or_else(|| failed.first().map(|(_, code)| *code))
        .unwrap_or(0)
}

// --- entry points -------------------------------------------------------------

/// The cleanup of an instance whose own `wf run` is gone, in that
/// instance's worktree and with its WF_* values.
fn orphan_cleanup(spec: &ScriptSpec, name: &str, env: &Env, record: &jobs::JobRecord) {
    let worktree = Path::new(&record.worktree);
    if !worktree.is_dir() {
        output::warn(&format!(
            "skipping cleanup for {}: {} no longer exists",
            repr(name),
            worktree.display()
        ));
        return;
    }
    let mut env = env.clone();
    env.insert("WF_WORKTREE".into(), record.worktree.clone().into());
    env.insert("WF_BRANCH".into(), record.branch.clone().into());
    if let Err(error) = run_cleanup(spec, name, worktree, &env) {
        output::warn(&error.message);
    }
}

/// A `scripts` entry, or — for a `make:TARGET` name — the synthetic entry
/// that target runs as. A configured name always wins, so a `scripts` key
/// spelled `make:...` is never shadowed.
pub(crate) fn resolve_script(config: &Config, name: &str) -> Result<ScriptSpec> {
    if let Some(spec) = config.scripts.get(name) {
        return Ok(spec.clone());
    }
    if let Some(target) = makefile::target_of(name) {
        return Ok(makefile::spec_for(config, target));
    }
    let mut available: Vec<&str> = config.scripts.keys().map(String::as_str).collect();
    available.sort_unstable();
    let available =
        if available.is_empty() { "none defined".to_string() } else { available.join(", ") };
    Err(Error::new(format!("no script named {} (available: {available})", repr(name))))
}

/// The entry's own, else — for a group — the longest any member may need
/// (its members are stopped through it), else the global one.
pub(crate) fn stop_timeout(config: &Config, spec: &ScriptSpec) -> f64 {
    if let Some(own) = spec.stop_timeout {
        return own.as_f64();
    }
    spec.members()
        .iter()
        .filter_map(|member| config.scripts.get(member))
        .map(|member| stop_timeout(config, member))
        .reduce(f64::max)
        .unwrap_or(config.stop_timeout.as_f64())
}

fn stop_jobs(config: &Config, name: &str, found: &[jobs::Job], by: &str, env: &Env) -> Result<()> {
    let spec = resolve_script(config, name)?;
    let timeout = stop_timeout(config, &spec);
    for job in found {
        jobs::stop(job, by, timeout, Some(&|record| orphan_cleanup(&spec, name, env, record)));
    }
    Ok(())
}

/// `wf stop`: stop the script's instance in this worktree, or in every
/// worktree of the project. Stale records are dropped, not counted.
pub fn stop_script(
    config: &Config,
    name: &str,
    cwd: &Path,
    env: &Env,
    everywhere: bool,
) -> Result<()> {
    resolve_script(config, name)?;
    let common_dir = git::git_common_dir(cwd)?;
    let mut running = Vec::new();
    for job in jobs::jobs_for(&common_dir, name) {
        if jobs::classify(&job.record) == jobs::JobState::Stale {
            let _ = fs::remove_file(&job.path);
        } else if everywhere || Path::new(&job.record.worktree) == cwd {
            running.push(job);
        }
    }
    let here = repr(&util::file_name(cwd));
    if running.is_empty() {
        let place =
            if everywhere { "anywhere in this project".to_string() } else { format!("in {here}") };
        return Err(Error::new(format!("{} is not running {place}", repr(name))));
    }
    stop_jobs(config, name, &running, &format!("`wf stop` in {here}"), env)
}

/// Which scripts of this project are running, by worktree path — the
/// records of every worktree, since they share the common git dir.
pub fn running_scripts(cwd: &Path) -> Result<IndexMap<PathBuf, BTreeMap<String, usize>>> {
    Ok(jobs::running_scripts(&git::git_common_dir(cwd)?))
}

pub(crate) fn start_message(job: &Job) -> String {
    let what = if job.spec.command.is_some() {
        job.snippet.clone()
    } else {
        job.spec.members().join(", ")
    };
    format!("running {} in {}: {what}", repr(&job.name), job.cwd.display())
}

/// Resolve a script and clear the way for it: an `exclusive` one first
/// stops every running instance in the project (their cleanup included);
/// any other simply joins the instances already running here.
pub(crate) fn prepare<'a>(
    config: &'a Config,
    name: &str,
    cwd: &Path,
    env: &Env,
    extra_args: &[String],
    tty: bool,
) -> Result<Job<'a>> {
    let spec = resolve_script(config, name)?;
    let mut snippet = spec.command.clone().unwrap_or_default();
    if !extra_args.is_empty() {
        if spec.command.is_none() {
            return Err(Error::new(format!(
                "{} is a group of scripts and takes no arguments",
                repr(name)
            )));
        }
        let quoted: Vec<String> = extra_args.iter().map(|arg| shell_quote(arg)).collect();
        snippet = format!("{snippet} {}", quoted.join(" "));
    }
    let common_dir = git::git_common_dir(cwd)?;
    if spec.exclusive {
        let by = format!("`wf run {name}` in {}", repr(&util::file_name(cwd)));
        stop_jobs(config, name, &jobs::jobs_for(&common_dir, name), &by, env)?;
    }
    // Any number of instances may share a worktree, each recorded and
    // logged under the pid of the run that owns it; what dead ones left
    // behind goes now.
    jobs::prune(&common_dir, name, cwd);
    Ok(Job {
        config,
        spec,
        name: name.to_string(),
        snippet,
        cwd: cwd.to_path_buf(),
        env: env.clone(),
        common_dir,
        tty,
    })
}

/// Run a `scripts` entry from the merged config; fail when it does.
///
/// `extra_args` are shell-quoted and appended to the command, so `wf run
/// make check -j2` runs `make check -j2` for a script defined as `make`; a
/// group takes none. `background` overrides the entry's own setting.
pub fn run_named_script(
    config: &Config,
    name: &str,
    cwd: &Path,
    env: &Env,
    extra_args: &[String],
    background: Option<bool>,
) -> Result<()> {
    let job = prepare(config, name, cwd, env, extra_args, true)?;
    if background.unwrap_or(job.spec.background) {
        return run::start_background(&job);
    }
    output::success(&start_message(&job));
    raise_for(&run::run_to_completion(&job)?, name)
}

/// Relay what a writer of lines produces to stderr.
pub(crate) fn stderr_sink() -> impl Write {
    std::io::stderr()
}

#[cfg(test)]
mod tests;
