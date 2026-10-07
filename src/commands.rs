//! Core commands. Each returns an [`Outcome`] — a shell directive, text
//! for stdout, or nothing — and never prints to stdout itself: `cli.rs` is
//! the sole stdout writer.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;

use serde_json::{Map, Value as Json, json};

use crate::config::{self, Config, PROJECT_LOCAL_DIRS, load_config, resolve_worktrees_dir};
use crate::errors::{Error, ErrorKind, Result};
use crate::git::{self, Worktree};
use crate::launch::{self, ShellAction, Target};
use crate::util::{self, Env, one_line, repr};
use crate::{hooks, makefile, output};

const PROJECT_TEMPLATE: &str = include_str!("workforest/templates/project.yaml");

/// What a command leaves for `cli.rs` to print.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// A directive for the wf shell wrapper.
    Shell(ShellAction),
    /// Data for stdout.
    Text(String),
    Nothing,
}

impl From<Option<ShellAction>> for Outcome {
    fn from(action: Option<ShellAction>) -> Self {
        action.map_or(Outcome::Nothing, Outcome::Shell)
    }
}

#[derive(Debug, Clone)]
pub struct Context {
    /// Repo root of the invocation directory.
    pub cwd_root: PathBuf,
    /// Main worktree ($WF_MAIN).
    pub main: PathBuf,
    pub config: Config,
    pub worktrees_dir: PathBuf,
    /// The environment we were started in.
    pub env: Env,
}

pub fn build_context(cwd: Option<&Path>) -> Result<Context> {
    let cwd_root = git::repo_root(cwd)?;
    let main = git::main_worktree(Some(&cwd_root))?;
    let config = load_config(Some(&main))?;
    let worktrees_dir = resolve_worktrees_dir(&config, &main)?;
    Ok(Context { cwd_root, main, config, worktrees_dir, env: util::current_env() })
}

fn is_managed(ctx: &Context, worktree: &Worktree) -> bool {
    !worktree.is_main && worktree.path.parent() == Some(&ctx.worktrees_dir)
}

/// Worktrees located directly inside the resolved worktrees dir — the only
/// ones we list, complete, or delete.
pub fn managed_worktrees(ctx: &Context) -> Result<Vec<Worktree>> {
    Ok(git::list_worktrees(Some(&ctx.main))?
        .into_iter()
        .filter(|worktree| is_managed(ctx, worktree))
        .collect())
}

pub fn find_managed(ctx: &Context, name: &str) -> Result<Worktree> {
    managed_worktrees(ctx)?.into_iter().find(|worktree| worktree.name() == name).ok_or_else(|| {
        Error::new(format!("worktree {} not found in {}", repr(name), ctx.worktrees_dir.display()))
    })
}

fn locked_error(worktree: &Worktree) -> Error {
    let reason = match worktree.locked.as_deref().filter(|reason| !reason.is_empty()) {
        Some(reason) => format!(" ({})", one_line(reason)),
        None => String::new(),
    };
    let name = worktree.name();
    Error::new(format!("worktree {} is locked{reason} — run: wf unlock {name}", repr(&name)))
}

fn stale_error(worktree: &Worktree, then: &str) -> Error {
    let name = worktree.name();
    if worktree.locked.is_some() {
        // `prune` skips a locked record, so naming it alone would be a dead end.
        return Error::new(format!(
            "worktree {} is stale and locked — run: wf unlock {name}, then wf prune",
            repr(&name)
        ));
    }
    Error::new(format!("worktree {} is stale — run: wf prune{then}", repr(&name)))
}

/// The record as git has it now — it may have been locked, unlocked or
/// pruned by someone else since we listed it.
fn registered(ctx: &Context, worktree: &Worktree) -> Result<Option<Worktree>> {
    Ok(git::list_worktrees(Some(&ctx.main))?.into_iter().find(|other| other.path == worktree.path))
}

fn remove(ctx: &Context, worktree: &Worktree) -> Result<()> {
    let Err(error) = git::worktree_remove(&ctx.main, &worktree.path, true) else {
        return Ok(());
    };
    // Locked since the check: say so in our words, not git's `fatal:`.
    match registered(ctx, worktree)? {
        Some(current) if current.locked.is_some() => Err(locked_error(&current)),
        _ => Err(error),
    }
}

/// `git worktree prune`, returning the records it dropped.
fn prune(ctx: &Context) -> Result<Vec<Worktree>> {
    let before = git::list_worktrees(Some(&ctx.main))?;
    git::worktree_prune(&ctx.main)?;
    let left = git::list_worktrees(Some(&ctx.main))?;
    Ok(before
        .into_iter()
        .filter(|worktree| !left.iter().any(|kept| kept.path == worktree.path))
        .collect())
}

/// A worktree by name when it is one of ours, by path otherwise.
fn label(ctx: &Context, worktree: &Worktree) -> String {
    if is_managed(ctx, worktree) { worktree.name() } else { worktree.path.display().to_string() }
}

fn labels(ctx: &Context, worktrees: &[Worktree]) -> String {
    worktrees.iter().map(|worktree| label(ctx, worktree)).collect::<Vec<_>>().join(", ")
}

fn warn_left_behind(worktree: &Worktree) {
    output::warn(&format!(
        "left {} in place: it is no longer a worktree, \
         and whatever is in it is yours to keep or remove",
        worktree.path.display()
    ));
}

/// Drop one stale, unlocked record, never touching files.
///
/// With the directory gone git removes just that record. With the
/// directory still there (its `.git` file is what went missing) git
/// refuses, and only a repository-wide prune clears it — so say what else
/// went, and that the directory stays.
fn forget(ctx: &Context, worktree: &Worktree) -> Result<()> {
    if registered(ctx, worktree)?.is_none() {
        return Ok(()); // already gone, e.g. pruned along with an earlier one
    }
    if !worktree.path.exists() {
        return remove(ctx, worktree);
    }
    let pruned = prune(ctx)?;
    let others: Vec<Worktree> =
        pruned.iter().filter(|other| other.path != worktree.path).cloned().collect();
    if others.len() == pruned.len() {
        // Still there: locked since the check is the one way prune skips it.
        let current = registered(ctx, worktree)?;
        return Err(locked_error(current.as_ref().unwrap_or(worktree)));
    }
    warn_left_behind(worktree);
    if !others.is_empty() {
        output::warn(&format!(
            "also pruned the other stale worktree records: {}",
            labels(ctx, &others)
        ));
    }
    Ok(())
}

/// `create` found a stale record in its way: git does not prune on
/// `worktree add`, so do it — unless a lock says to keep the record.
fn clear_stale(ctx: &Context, worktree: &Worktree) -> Result<()> {
    if worktree.locked.is_some() {
        if is_managed(ctx, worktree) {
            return Err(stale_error(worktree, ""));
        }
        let path = worktree.path.display();
        return Err(Error::new(format!(
            "a stale, locked worktree record at {path} is in the way — \
             run: git worktree unlock {path}, then wf prune"
        )));
    }
    forget(ctx, worktree)?;
    output::warn(&format!("pruned the stale worktree record at {}", worktree.path.display()));
    Ok(())
}

pub fn short_branch_name(branch: &str) -> &str {
    branch.rsplit('/').next().unwrap_or(branch)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ResolvedBranch {
    /// Local branch name.
    branch: String,
    /// Remote ref the branch should track, if any.
    track: Option<String>,
}

/// Resolve BRANCH or REMOTE/BRANCH to a local branch and its remote ref.
///
/// Precedence: exact local branch, explicit REMOTE/BRANCH, a branch known
/// to exactly one remote; anything else names a new local branch.
fn resolve_branch(ctx: &Context, spec: &str) -> Result<ResolvedBranch> {
    if git::branch_exists(spec, &ctx.main) {
        return Ok(ResolvedBranch { branch: spec.to_string(), track: None });
    }
    let remote_map = git::remote_branches(&ctx.main)?;
    for remote in git::remotes_longest_first(&ctx.main)? {
        let Some(branch) = spec.strip_prefix(&format!("{remote}/")) else {
            continue;
        };
        if !remote_map.get(branch).is_some_and(|carriers| carriers.contains(&remote)) {
            return Err(Error::new(format!(
                "branch {} not found on remote {}",
                repr(branch),
                repr(&remote)
            )));
        }
        let mut branch = branch.to_string();
        while git::branch_exists(&branch, &ctx.main) {
            // The obvious local name is taken (possibly tracking a
            // different remote), so the new branch needs a name of its own.
            if !output::interactive() {
                return Err(Error::new(format!(
                    "branch {} already exists locally; `wf create {branch}` to use it",
                    repr(&branch)
                )));
            }
            branch = output::ask(&format!(
                "branch {} already exists locally; local name for {spec}:",
                repr(&branch)
            ))?;
            if branch.is_empty() {
                return Err(Error::cancelled("cancelled"));
            }
        }
        return Ok(ResolvedBranch { branch, track: Some(spec.to_string()) });
    }
    let carriers = remote_map.get(spec).cloned().unwrap_or_default();
    if carriers.len() > 1 {
        return Err(Error::new(format!(
            "branch {} exists on multiple remotes ({}); pick one, e.g. `wf create {}/{spec}`",
            repr(spec),
            carriers.join(", "),
            carriers[0]
        )));
    }
    let track = carriers.first().map(|remote| format!("{remote}/{spec}"));
    Ok(ResolvedBranch { branch: spec.to_string(), track })
}

fn script_env(ctx: &Context, worktree: &Path, branch: Option<&str>) -> Env {
    hooks::script_env(&ctx.env, &ctx.main, worktree, &ctx.worktrees_dir, branch)
}

/// How to open what `create`/`open` arrive at.
#[derive(Debug, Clone, Copy, Default)]
pub struct OpenWith<'a> {
    pub opener: Option<&'a str>,
    pub wrap: Option<&'a str>,
    pub path: Option<&'a str>,
}

fn open(ctx: &Context, worktree: &Path, branch: Option<&str>, with: OpenWith) -> Result<Outcome> {
    let target = Target { main: &ctx.main, worktree, worktrees_dir: &ctx.worktrees_dir, branch };
    Ok(launch::launch(&ctx.config, &target, with.opener, with.wrap, with.path, &ctx.env)?.into())
}

pub fn cmd_create(
    ctx: &Context,
    branch: Option<&str>,
    with: OpenWith,
    no_hooks: bool,
    no_open: bool,
) -> Result<Outcome> {
    let spec = match branch.filter(|branch| !branch.is_empty()) {
        Some(branch) => branch.to_string(),
        None => {
            let current = git::current_branch(&ctx.cwd_root)?;
            if current == "HEAD" {
                return Err(Error::new("detached HEAD: specify a branch name"));
            }
            current
        }
    };
    let resolved = resolve_branch(ctx, &spec)?;
    let branch = resolved.branch.as_str();

    let mut existing = git::find_branch_worktree(branch, &ctx.main)?;
    if let Some(stale) = existing.as_ref().filter(|worktree| git::is_stale(worktree)) {
        clear_stale(ctx, stale)?;
        existing = None;
    }
    let worktree_path = if let Some(existing) = existing {
        output::warn(&format!(
            "branch {} already checked out at {}",
            repr(branch),
            existing.path.display()
        ));
        existing.path
    } else {
        let worktree_path = ctx.worktrees_dir.join(short_branch_name(branch));
        let mut occupant = git::list_worktrees(Some(&ctx.main))?
            .into_iter()
            .find(|worktree| worktree.path == worktree_path);
        if let Some(stale) = occupant.as_ref().filter(|worktree| git::is_stale(worktree)) {
            clear_stale(ctx, stale)?;
            occupant = None;
        }
        if occupant.is_some() {
            // Same directory name, different branch (feat/x vs fix/x):
            // never silently reuse another branch's worktree.
            return Err(Error::new(format!(
                "{} already holds a different branch; \
                 remove it first or use a different branch name",
                worktree_path.display()
            )));
        }
        if worktree_path.exists() {
            return Err(Error::new(format!(
                "directory exists but is not a worktree: {}",
                worktree_path.display()
            )));
        }
        fs::create_dir_all(&ctx.worktrees_dir).map_err(|error| {
            Error::new(format!(
                "cannot create {}: {}",
                ctx.worktrees_dir.display(),
                util::os_error_text(&error)
            ))
        })?;
        git::worktree_add(&ctx.main, &worktree_path, branch, resolved.track.as_deref())?;
        output::success(&format!(
            "created worktree for {} at {}",
            repr(branch),
            worktree_path.display()
        ));
        if !no_hooks {
            let env = script_env(ctx, &worktree_path, Some(branch));
            hooks::create_symlinks(&ctx.config, &ctx.main, &worktree_path)?;
            let failures = hooks::run_setup_scripts(&ctx.config, &worktree_path, &env)?;
            if failures > 0 {
                output::warn(&format!("{failures} setup script(s) failed"));
            }
        }
        worktree_path
    };

    if no_open {
        return Ok(Outcome::Nothing);
    }
    open(ctx, &worktree_path, Some(branch), with)
}

pub fn cmd_open(ctx: &Context, name: Option<&str>, with: OpenWith) -> Result<Outcome> {
    let worktree = match name.filter(|name| !name.is_empty()) {
        Some(name) => find_managed(ctx, name)?,
        // No name, but standing in a managed worktree: that's the one.
        None => managed_worktrees(ctx)?
            .into_iter()
            .find(|worktree| worktree.path == ctx.cwd_root)
            .ok_or_else(|| {
                Error::usage("worktree name required (or run inside a managed worktree)")
            })?,
    };
    if git::is_stale(&worktree) {
        // A lock never blocks opening; a missing directory has to.
        let again = match worktree.branch.as_deref().filter(|branch| !branch.is_empty()) {
            Some(branch) => format!(", then wf create {branch}"),
            None => String::new(),
        };
        return Err(stale_error(&worktree, &again));
    }
    open(ctx, &worktree.path, worktree.branch.as_deref(), with)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeState {
    /// None when stale: never asked.
    pub dirty: Option<bool>,
    /// The lock reason, "" when none was given.
    pub locked: Option<String>,
    pub stale: bool,
}

impl WorktreeState {
    /// `clean`, `dirty` or `stale`, plus `locked`.
    pub fn label(&self) -> String {
        let word = if self.stale {
            "stale"
        } else if self.dirty == Some(true) {
            "dirty"
        } else {
            "clean"
        };
        if self.locked.is_some() { format!("{word} locked") } else { word.to_string() }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListedWorktree {
    pub worktree: Worktree,
    pub state: WorktreeState,
}

impl ListedWorktree {
    /// Why the record is stale, None when it is not. Git gives no reason
    /// for a stale record that is locked, so there is a stock one.
    pub fn prunable(&self) -> Option<String> {
        if !self.state.stale {
            return None;
        }
        Some(
            self.worktree
                .prunable
                .clone()
                .filter(|reason| !reason.is_empty())
                .unwrap_or_else(|| "the working tree is missing or is no longer a worktree".into()),
        )
    }

    pub fn branch_label(&self) -> &str {
        self.worktree.branch.as_deref().filter(|branch| !branch.is_empty()).unwrap_or("(detached)")
    }
}

fn inspect_one(worktree: Worktree) -> ListedWorktree {
    let mut stale = git::is_stale(&worktree);
    let mut dirty = None;
    if !stale {
        // Only ever in a directory that is a worktree: anywhere else git
        // would fail, or answer for the repository enclosing it.
        match git::status_porcelain(&worktree.path) {
            Ok(changes) => dirty = Some(!changes.is_empty()),
            Err(_) => stale = true, // went away, or broke, since the listing
        }
    }
    let state = WorktreeState { dirty, locked: worktree.locked.clone(), stale };
    ListedWorktree { worktree, state }
}

/// One `git status` per live worktree; subprocess-bound, so run them
/// together.
pub fn inspect(worktrees: Vec<Worktree>) -> Vec<ListedWorktree> {
    let next = AtomicUsize::new(0);
    let results: Mutex<Vec<Option<ListedWorktree>>> = Mutex::new(vec![None; worktrees.len()]);
    thread::scope(|scope| {
        for _ in 0..worktrees.len().min(8) {
            scope.spawn(|| {
                loop {
                    let index = next.fetch_add(1, Ordering::SeqCst);
                    let Some(worktree) = worktrees.get(index) else {
                        break;
                    };
                    let listed = inspect_one(worktree.clone());
                    results.lock().expect("no worker panics")[index] = Some(listed);
                }
            });
        }
    });
    results.into_inner().expect("no worker panics").into_iter().flatten().collect()
}

type Running = indexmap::IndexMap<PathBuf, std::collections::BTreeMap<String, usize>>;

fn worktree_json(listed: &ListedWorktree, running: &Running) -> Json {
    let worktree = &listed.worktree;
    // scripts running there: name → live instances
    let scripts: Map<String, Json> = running
        .get(&worktree.path)
        .map(|counts| counts.iter().map(|(name, count)| (name.clone(), json!(count))).collect())
        .unwrap_or_default();
    json!({
        "name": worktree.name(),
        "branch": worktree.branch, // null when detached
        "path": worktree.path.to_string_lossy(),
        "dirty": listed.state.dirty, // null when stale: never asked
        "locked": listed.state.locked, // the reason ("" for none); null when not locked
        "prunable": listed.prunable(), // why it is stale; null when it is not
        "running": scripts,
    })
}

/// The whole forest for programs (editor integrations): the main checkout
/// in the same shape as the worktrees, plus where they live and what is
/// running in each.
fn list_json(ctx: &Context) -> Result<String> {
    let mut everything = git::list_worktrees(Some(&ctx.main))?.into_iter();
    let main = everything.next().ok_or_else(Error::not_a_repo)?;
    let rest = everything.filter(|worktree| is_managed(ctx, worktree));
    let mut listed = inspect(std::iter::once(main).chain(rest).collect()).into_iter();
    let main = listed.next().ok_or_else(Error::not_a_repo)?;
    let running = hooks::running_scripts(&ctx.main)?;
    let data = json!({
        "main": worktree_json(&main, &running),
        "worktrees_dir": ctx.worktrees_dir.to_string_lossy(),
        "worktrees": listed.map(|listed| worktree_json(&listed, &running)).collect::<Vec<_>>(),
    });
    Ok(util::json_pretty(&data))
}

/// A git flag as one TSV field: empty when absent, else the flag's own
/// name and, after a space, its reason on one line — never empty for a
/// flag that is set, whatever the reason.
fn flag_column(flag: &str, reason: Option<&str>) -> String {
    match reason {
        None => String::new(),
        Some(reason) => format!("{flag} {}", one_line(reason)).trim_end().to_string(),
    }
}

fn porcelain_row(listed: &ListedWorktree) -> String {
    let (worktree, state) = (&listed.worktree, &listed.state);
    let dirty = match state.dirty {
        None => "",
        Some(true) => "1",
        Some(false) => "0",
    };
    [
        worktree.name(),
        worktree.branch.clone().unwrap_or_default(),
        worktree.path.to_string_lossy().into_owned(),
        dirty.to_string(),
        flag_column("locked", state.locked.as_deref()),
        flag_column("prunable", listed.prunable().as_deref()),
    ]
    .join("\t")
}

pub fn cmd_list(ctx: &Context, porcelain: bool, as_json: bool) -> Result<Outcome> {
    if as_json {
        return list_json(ctx).map(Outcome::Text);
    }
    let listing = inspect(managed_worktrees(ctx)?);
    if listing.is_empty() {
        if !porcelain {
            output::info(&format!(
                "no worktrees in {} (create one with: wf create BRANCH)",
                ctx.worktrees_dir.display()
            ));
        }
        return Ok(Outcome::Nothing);
    }
    if porcelain {
        return Ok(Outcome::Text(listing.iter().map(porcelain_row).collect::<Vec<_>>().join("\n")));
    }
    let width = |text: &dyn Fn(&ListedWorktree) -> String| {
        listing.iter().map(|listed| text(listed).chars().count()).max().unwrap_or(0)
    };
    let name_width = width(&|listed| listed.worktree.name());
    let branch_width = width(&|listed| listed.branch_label().to_string());
    let state_width = width(&|listed| listed.state.label());
    let rows: Vec<String> = listing
        .iter()
        .map(|listed| {
            format!(
                "{:<name_width$}  {:<branch_width$}  {:<state_width$}  {}",
                listed.worktree.name(),
                listed.branch_label(),
                listed.state.label(),
                listed.worktree.path.display()
            )
        })
        .collect();
    Ok(Outcome::Text(rows.join("\n")))
}

/// Shared delete/checkout guard for uncommitted changes.
fn confirm_dirty(worktree: &Worktree, question: &str, force: bool) -> Result<()> {
    if force {
        return Ok(());
    }
    let changes = git::status_porcelain(&worktree.path)?;
    if changes.is_empty() {
        return Ok(());
    }
    let lines = util::splitlines(&changes);
    output::warn(&format!("worktree {} has uncommitted changes:", repr(&worktree.name())));
    for line in lines.iter().take(10) {
        output::info(&format!("  {line}"));
    }
    if lines.len() > 10 {
        output::info("  ...");
    }
    if !output::confirm(question)? {
        return Err(Error::cancelled("cancelled"));
    }
    Ok(())
}

pub fn cmd_delete(
    ctx: &Context,
    names: &[String],
    force: bool,
    delete_branch: Option<bool>,
) -> Result<Outcome> {
    let mut result = Outcome::Nothing;
    // Resolve every name first: a typo must fail the batch before anything
    // is deleted, not strand it half-done.
    let worktrees: Vec<Worktree> =
        names.iter().map(|name| find_managed(ctx, name)).collect::<Result<_>>()?;
    // The lock check belongs to the same pre-pass. --force never overrides
    // a lock: it answers "uncommitted changes", and one flag for both is
    // how the worktree locked against deletion gets deleted.
    if let Some(locked) = worktrees.iter().find(|worktree| worktree.locked.is_some()) {
        return Err(locked_error(locked));
    }
    for worktree in &worktrees {
        let name = repr(&worktree.name());
        if git::is_stale(worktree) {
            // Nothing to ask about and no files of ours to remove: only
            // the record goes.
            forget(ctx, worktree)?;
            output::success(&format!("pruned stale worktree {name}"));
        } else {
            confirm_dirty(worktree, "Delete anyway?", force)?;
            remove(ctx, worktree)?;
            output::success(&format!("deleted worktree {name}"));
        }
        if worktree.path == ctx.cwd_root {
            // The shell is standing in the directory we just removed —
            // move it back to the main checkout.
            result = Outcome::Shell(launch::cd_action(&ctx.main));
        }
        let Some(branch) = worktree.branch.as_deref() else {
            continue;
        };
        let mut decision = delete_branch;
        if decision.is_none() && output::interactive() {
            decision = Some(output::confirm(&format!("Also delete branch {}?", repr(branch)))?);
        }
        if decision == Some(true) {
            match git::delete_branch(&ctx.main, branch) {
                Ok(()) => output::success(&format!("deleted branch {}", repr(branch))),
                Err(error) => output::warn(&error.message),
            }
        }
    }
    Ok(result)
}

pub fn cmd_checkout(ctx: &Context, name: &str, force: bool) -> Result<Outcome> {
    let worktree = find_managed(ctx, name)?;
    if git::is_stale(&worktree) {
        return Err(stale_error(&worktree, ""));
    }
    if worktree.locked.is_some() {
        return Err(locked_error(&worktree));
    }
    let Some(branch) = worktree.branch.as_deref() else {
        return Err(Error::new(format!(
            "cannot determine branch for worktree {} (detached HEAD)",
            repr(name)
        )));
    };
    confirm_dirty(
        &worktree,
        "Delete worktree and checkout its branch in the main repo anyway?",
        force,
    )?;
    remove(ctx, &worktree)?;
    output::success(&format!("deleted worktree {}", repr(name)));
    git::checkout(&ctx.main, branch)?;
    output::success(&format!("checked out {} in {}", repr(branch), ctx.main.display()));
    Ok(Outcome::Shell(launch::cd_action(&ctx.main)))
}

/// Lock a worktree against `delete`, `checkout` and `prune`. A second lock
/// is an error, as in git: it would silently replace the reason.
pub fn cmd_lock(ctx: &Context, name: &str, reason: Option<&str>) -> Result<Outcome> {
    let worktree = find_managed(ctx, name)?;
    let mut current = Some(worktree.clone());
    if worktree.locked.is_none() {
        match git::worktree_lock(&ctx.main, &worktree.path, reason) {
            Ok(()) => {
                output::success(&format!("locked worktree {}", repr(name)));
                return Ok(Outcome::Nothing);
            }
            Err(error) => {
                current = registered(ctx, &worktree)?; // locked by someone else meanwhile?
                if current.as_ref().is_none_or(|current| current.locked.is_none()) {
                    return Err(error);
                }
            }
        }
    }
    let held = current
        .and_then(|current| current.locked)
        .filter(|reason| !reason.is_empty())
        .map(|reason| format!(" ({})", one_line(&reason)))
        .unwrap_or_default();
    Err(Error::new(format!(
        "worktree {} is already locked{held} — run: wf unlock {name} to lock it anew",
        repr(name)
    )))
}

pub fn cmd_unlock(ctx: &Context, name: &str) -> Result<Outcome> {
    let worktree = find_managed(ctx, name)?;
    if worktree.locked.is_some() {
        match git::worktree_unlock(&ctx.main, &worktree.path) {
            Ok(()) => {
                output::success(&format!("unlocked worktree {}", repr(name)));
                return Ok(Outcome::Nothing);
            }
            Err(error) => {
                // unlocked by someone else meanwhile?
                let current = registered(ctx, &worktree)?;
                if current.is_none_or(|current| current.locked.is_some()) {
                    return Err(error);
                }
            }
        }
    }
    Err(Error::new(format!("worktree {} is not locked", repr(name))))
}

fn records(count: usize) -> String {
    format!("{count} stale worktree record{}", if count == 1 { "" } else { "s" })
}

/// Drop the records of worktrees that are gone. Repository-wide — git
/// cannot prune one record — so every record removed is named, ours or
/// not; and a locked one is kept and named too, rather than reporting
/// nothing to do while a broken row sits in the listing.
pub fn cmd_prune(ctx: &Context, dry_run: bool) -> Result<Outcome> {
    let stale: Vec<Worktree> =
        git::list_worktrees(Some(&ctx.main))?.into_iter().filter(git::is_stale).collect();
    let (held, loose): (Vec<Worktree>, Vec<Worktree>) =
        stale.into_iter().partition(|worktree| worktree.locked.is_some());
    let gone = if dry_run { loose } else { prune(ctx)? };
    if !gone.is_empty() {
        let names = labels(ctx, &gone);
        if dry_run {
            output::info(&format!("would prune {}: {names}", records(gone.len())));
        } else {
            output::success(&format!("pruned {}: {names}", records(gone.len())));
            for worktree in gone.iter().filter(|worktree| worktree.path.exists()) {
                warn_left_behind(worktree);
            }
        }
    } else if held.is_empty() {
        output::info("no stale worktree records");
    }
    if !held.is_empty() {
        let verb = if held.len() == 1 { "is" } else { "are" };
        output::warn(&format!(
            "{} {verb} locked: {} — unlock to prune",
            records(held.len()),
            labels(ctx, &held)
        ));
    }
    Ok(Outcome::Nothing)
}

fn current_script_env(ctx: &Context) -> Result<Env> {
    let branch = git::current_branch(&ctx.cwd_root)?;
    Ok(script_env(
        ctx,
        &ctx.cwd_root,
        Some(&branch).filter(|branch| *branch != "HEAD").map(|b| &**b),
    ))
}

pub fn cmd_run(
    ctx: &Context,
    name: &str,
    extra_args: &[String],
    background: Option<bool>,
) -> Result<Outcome> {
    let env = current_script_env(ctx)?;
    hooks::run_named_script(&ctx.config, name, &ctx.cwd_root, &env, extra_args, background)?;
    Ok(Outcome::Nothing)
}

/// `wf make TARGET`: `make TARGET` at the worktree root, run as the script
/// `make:TARGET` so it is recorded, stoppable and counted like any other.
/// The target is make's to accept or reject — a hidden one still runs, and
/// one this build generates is never in our list.
pub fn cmd_make(
    ctx: &Context,
    target: &str,
    extra_args: &[String],
    background: Option<bool>,
) -> Result<Outcome> {
    makefile::require(&ctx.cwd_root)?;
    cmd_run(ctx, &makefile::script_name(target), extra_args, background)
}

pub fn cmd_stop(ctx: &Context, name: &str, everywhere: bool, make: bool) -> Result<Outcome> {
    let name = if make { makefile::script_name(name) } else { name.to_string() };
    let env = current_script_env(ctx)?;
    hooks::stop_script(&ctx.config, &name, &ctx.cwd_root, &env, everywhere)?;
    Ok(Outcome::Nothing)
}

pub fn cmd_init(ctx: &Context, local: bool) -> Result<Outcome> {
    let directory = if local {
        PROJECT_LOCAL_DIRS
            .iter()
            .map(|candidate| ctx.main.join(candidate))
            .find(|directory| directory.is_dir())
            .ok_or_else(|| {
                let dirs: Vec<String> =
                    PROJECT_LOCAL_DIRS.iter().map(|directory| format!("{directory}/")).collect();
                Error::new(format!(
                    "--local needs an IDE settings folder ({}) in {}",
                    dirs.join(" or "),
                    ctx.main.display()
                ))
            })?
    } else {
        ctx.main.clone()
    };
    let target = directory.join(config::PROJECT_BASENAMES[0]);
    if target.exists() {
        return Err(Error::new(format!("{} already exists", target.display())));
    }
    fs::write(&target, PROJECT_TEMPLATE).map_err(|error| {
        Error::new(format!("cannot write {}: {}", target.display(), util::os_error_text(&error)))
    })?;
    output::success(&format!(
        "scaffolded {} (all keys commented out; see man 5 workforest)",
        target.display()
    ));
    Ok(Outcome::Nothing)
}

pub fn cmd_config_show(as_json: bool) -> Result<Outcome> {
    let config = match build_context(None) {
        Ok(ctx) => ctx.config,
        Err(error) if error.kind == ErrorKind::NotARepo => load_config(None)?,
        Err(error) => return Err(error),
    };
    Ok(Outcome::Text(show_config(&config, as_json)))
}

fn show_config(config: &Config, as_json: bool) -> String {
    if as_json {
        let sources: Vec<Json> = config
            .sources
            .iter()
            .map(|source| json!({"layer": source.layer, "path": source.path.to_string_lossy()}))
            .collect();
        return util::json_pretty(
            &json!({"config": config.as_value().to_json(), "sources": sources}),
        );
    }
    let dump = config::dump::dump(&config.as_value());
    let mut lines = vec![dump.trim_end_matches('\n').to_string(), String::new()];
    lines.push("# sources (low -> high):".to_string());
    if config.sources.is_empty() {
        lines.push("#   (built-in defaults only)".to_string());
    }
    for source in &config.sources {
        lines.push(format!("#   {}: {}", source.layer, source.path.display()));
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ConfigSource;

    fn worktree(name: &str, locked: Option<&str>, prunable: Option<&str>) -> Worktree {
        Worktree {
            path: PathBuf::from("/wt").join(name),
            head: "abc".into(),
            branch: Some(format!("feature/{name}")),
            is_main: false,
            locked: locked.map(str::to_string),
            prunable: prunable.map(str::to_string),
        }
    }

    fn listed(worktree: Worktree, dirty: Option<bool>, stale: bool) -> ListedWorktree {
        let state = WorktreeState { dirty, locked: worktree.locked.clone(), stale };
        ListedWorktree { worktree, state }
    }

    #[test]
    fn short_branch_name_is_the_last_segment() {
        assert_eq!(short_branch_name("feature/auth/login"), "login");
        assert_eq!(short_branch_name("main"), "main");
    }

    #[test]
    fn state_labels() {
        let label = |dirty, locked: Option<&str>, stale| {
            WorktreeState { dirty, locked: locked.map(str::to_string), stale }.label()
        };
        assert_eq!(label(Some(false), None, false), "clean");
        assert_eq!(label(Some(true), None, false), "dirty");
        assert_eq!(label(None, None, true), "stale");
        assert_eq!(label(Some(true), Some(""), false), "dirty locked");
        assert_eq!(label(None, Some("usb"), true), "stale locked");
    }

    #[test]
    fn porcelain_rows_are_six_tab_separated_fields() {
        let clean = listed(worktree("a", None, None), Some(false), false);
        assert_eq!(porcelain_row(&clean), "a\tfeature/a\t/wt/a\t0\t\t");
        let locked = listed(worktree("b", Some("on a\n\tusb  drive"), None), Some(true), false);
        assert_eq!(porcelain_row(&locked), "b\tfeature/b\t/wt/b\t1\tlocked on a usb drive\t");
        let bare_lock = listed(worktree("c", Some(""), None), Some(false), false);
        assert_eq!(porcelain_row(&bare_lock), "c\tfeature/c\t/wt/c\t0\tlocked\t");
        let stale = listed(
            worktree("d", None, Some("gitdir file points to non-existent location")),
            None,
            true,
        );
        assert_eq!(
            porcelain_row(&stale),
            "d\tfeature/d\t/wt/d\t\t\tprunable gitdir file points to non-existent location"
        );
        let stale_locked = listed(worktree("e", Some("x"), None), None, true);
        assert_eq!(
            porcelain_row(&stale_locked),
            "e\tfeature/e\t/wt/e\t\tlocked x\tprunable the working tree is missing or is no longer a worktree"
        );
        let detached =
            listed(Worktree { branch: None, ..worktree("f", None, None) }, Some(false), false);
        assert_eq!(porcelain_row(&detached), "f\t\t/wt/f\t0\t\t");
        assert_eq!(detached.branch_label(), "(detached)");
    }

    #[test]
    fn json_rows_carry_every_field() {
        let mut running = Running::new();
        running.entry(PathBuf::from("/wt/a")).or_default().insert("dev".into(), 2);
        let row =
            worktree_json(&listed(worktree("a", Some(""), None), Some(true), false), &running);
        assert_eq!(
            row.to_string(),
            r#"{"name":"a","branch":"feature/a","path":"/wt/a","dirty":true,"locked":"","prunable":null,"running":{"dev":2}}"#
        );
        let stale = listed(Worktree { branch: None, ..worktree("b", None, None) }, None, true);
        assert_eq!(
            worktree_json(&stale, &running).to_string(),
            r#"{"name":"b","branch":null,"path":"/wt/b","dirty":null,"locked":null,"prunable":"the working tree is missing or is no longer a worktree","running":{}}"#
        );
    }

    #[test]
    fn inspecting_nothing_and_missing_directories() {
        assert!(inspect(Vec::new()).is_empty());
        let many: Vec<Worktree> =
            (0..20).map(|index| worktree(&format!("w{index}"), None, None)).collect();
        let listed = inspect(many.clone());
        assert_eq!(listed.len(), 20);
        // in order, and every missing directory reads as stale, never asked
        assert!(listed.iter().zip(&many).all(|(listed, worktree)| listed.worktree == *worktree));
        assert!(listed.iter().all(|listed| listed.state.stale && listed.state.dirty.is_none()));
    }

    #[test]
    fn record_counts_are_singular_and_plural() {
        assert_eq!(records(1), "1 stale worktree record");
        assert_eq!(records(2), "2 stale worktree records");
    }

    #[test]
    fn config_is_shown_with_its_sources() {
        let defaults = show_config(&Config::default(), false);
        assert!(
            defaults.starts_with("worktrees_dir: $WF_MAIN/../worktrees/$WF_NAME\nopener: ''\n")
        );
        assert!(defaults.ends_with(
            "  exclusive_scripts: []\n\n# sources (low -> high):\n#   (built-in defaults only)"
        ));

        let config = Config {
            sources: vec![
                ConfigSource {
                    layer: "user",
                    path: "/home/u/.config/workforest/config.yaml".into(),
                },
                ConfigSource { layer: "project", path: "/dev/api/.workforest.yaml".into() },
            ],
            ..Config::default()
        };
        assert!(show_config(&config, false).ends_with(
            "# sources (low -> high):\n#   user: /home/u/.config/workforest/config.yaml\n#   project: /dev/api/.workforest.yaml"
        ));
        let json: Json = serde_json::from_str(&show_config(&config, true)).unwrap();
        assert_eq!(json["config"]["stop_timeout"], 30.0);
        assert_eq!(
            json["sources"][1],
            json!({"layer": "project", "path": "/dev/api/.workforest.yaml"})
        );
        assert!(
            show_config(&config, true).starts_with("{\n  \"config\": {\n    \"worktrees_dir\": ")
        );
    }
}
