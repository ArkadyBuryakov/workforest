//! Typed subprocess wrappers around git plumbing.
//!
//! The only module that spawns git. Consumers get typed results; worktree
//! data comes from `--porcelain -z` output, never from parsing the
//! human-readable form.

use std::path::{Path, PathBuf};
use std::process::Command;

use indexmap::IndexMap;

use crate::errors::{Error, Result};
use crate::util;

/// A finished git process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitOutput {
    /// The exit status; -1 when git died by a signal.
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

fn command() -> Command {
    #[allow(unused_mut)]
    let mut command = Command::new("git");
    // Unit tests share one process environment: git gets its isolation
    // here instead (no user or system config, a fixed identity).
    #[cfg(test)]
    command
        .env("GIT_CONFIG_GLOBAL", crate::testing::gitconfig())
        .env("GIT_CONFIG_SYSTEM", "/dev/null");
    command
}

/// Run git and return what it said; a failure is an error when `check`.
pub fn run_git(args: &[&str], cwd: Option<&Path>, check: bool) -> Result<GitOutput> {
    let mut command = command();
    command.args(args);
    if let Some(cwd) = cwd {
        command.current_dir(cwd);
    }
    let described = || format!("git {}", args.join(" "));
    let output = command.output().map_err(|error| {
        Error::git(format!("`{}` failed: {}", described(), util::os_error_text(&error)))
    })?;
    let result = GitOutput {
        code: output.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    };
    if check && result.code != 0 {
        let detail = [result.stderr.trim(), result.stdout.trim(), "unknown error"]
            .into_iter()
            .find(|text| !text.is_empty())
            .unwrap_or_default();
        return Err(Error::git(format!("`{}` failed: {detail}", described())));
    }
    Ok(result)
}

pub fn git_output(args: &[&str], cwd: Option<&Path>) -> Result<String> {
    Ok(run_git(args, cwd, true)?.stdout.trim().to_string())
}

pub fn repo_root(cwd: Option<&Path>) -> Result<PathBuf> {
    match run_git(&["rev-parse", "--show-toplevel"], cwd, false) {
        Ok(result) if result.code == 0 => Ok(PathBuf::from(result.stdout.trim())),
        _ => Err(Error::not_a_repo()),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Worktree {
    pub path: PathBuf,
    pub head: String,
    /// Short name; None when detached or bare.
    pub branch: Option<String>,
    pub is_main: bool,
    // Git's own flags, each with its reason: None when absent, "" when git
    // gave none (a lock without --reason is a bare `locked`).
    pub locked: Option<String>,
    pub prunable: Option<String>,
}

impl Worktree {
    pub fn name(&self) -> String {
        util::file_name(&self.path)
    }
}

/// Prunable per git, or the admin link is gone. A locked stale record is
/// not reported prunable, so git's own flag is not enough; and the
/// directory existing is not enough either — without its `.git` file it is
/// a plain directory git would answer for from whatever repository
/// encloses it. One stat, no git spawn.
pub fn is_stale(worktree: &Worktree) -> bool {
    if worktree.is_main {
        return false;
    }
    worktree.prunable.is_some() || !worktree.path.join(".git").exists()
}

/// Parse `git worktree list --porcelain -z` output.
///
/// Records are groups of NUL-terminated attribute lines separated by an
/// empty entry; a value (a lock reason) may itself contain newlines.
/// Unknown attributes are ignored. The first record is the main worktree —
/// a git guarantee.
pub fn parse_worktree_porcelain(data: &str) -> Vec<Worktree> {
    let mut worktrees = Vec::new();
    let mut record: Vec<(&str, &str)> = Vec::new();
    for token in data.split('\0') {
        if token.is_empty() {
            if !record.is_empty() {
                worktrees.push(record_to_worktree(&record, worktrees.is_empty()));
                record.clear();
            }
            continue;
        }
        record.push(token.split_once(' ').unwrap_or((token, "")));
    }
    if !record.is_empty() {
        worktrees.push(record_to_worktree(&record, worktrees.is_empty()));
    }
    worktrees
}

fn record_to_worktree(record: &[(&str, &str)], is_main: bool) -> Worktree {
    let get = |name: &str| {
        record.iter().rev().find(|(key, _)| *key == name).map(|(_, value)| value.to_string())
    };
    Worktree {
        path: PathBuf::from(get("worktree").unwrap_or_default()),
        head: get("HEAD").unwrap_or_default(),
        branch: get("branch")
            .map(|branch| branch.strip_prefix("refs/heads/").unwrap_or(&branch).to_string()),
        is_main,
        locked: get("locked"),
        prunable: get("prunable"),
    }
}

pub fn list_worktrees(cwd: Option<&Path>) -> Result<Vec<Worktree>> {
    match run_git(&["worktree", "list", "--porcelain", "-z"], cwd, false) {
        Ok(result) if result.code == 0 => Ok(parse_worktree_porcelain(&result.stdout)),
        _ => Err(Error::not_a_repo()),
    }
}

pub fn main_worktree(cwd: Option<&Path>) -> Result<PathBuf> {
    list_worktrees(cwd)?
        .into_iter()
        .next()
        .map(|worktree| worktree.path)
        .ok_or_else(Error::not_a_repo)
}

/// Short branch name, or "HEAD" when detached.
pub fn current_branch(cwd: &Path) -> Result<String> {
    git_output(&["rev-parse", "--abbrev-ref", "HEAD"], Some(cwd))
}

fn lines(text: String) -> Vec<String> {
    util::splitlines(&text).into_iter().map(str::to_string).collect()
}

pub fn local_branches(cwd: &Path) -> Result<Vec<String>> {
    Ok(lines(git_output(&["for-each-ref", "--format=%(refname:short)", "refs/heads"], Some(cwd))?))
}

pub fn remotes(cwd: &Path) -> Result<Vec<String>> {
    Ok(lines(git_output(&["remote"], Some(cwd))?))
}

/// The remote names, longest first: a remote named "up/stream" must win
/// over "up" when a ref is matched against them.
pub fn remotes_longest_first(cwd: &Path) -> Result<Vec<String>> {
    let mut names = remotes(cwd)?;
    names.sort_by_key(|name| std::cmp::Reverse(name.chars().count()));
    Ok(names)
}

/// Map of branch name -> remotes that have it, across all remotes.
pub fn remote_branches(cwd: &Path) -> Result<IndexMap<String, Vec<String>>> {
    let names = remotes_longest_first(cwd)?;
    let out = git_output(&["for-each-ref", "--format=%(refname)", "refs/remotes"], Some(cwd))?;
    Ok(group_remote_refs(&util::splitlines(&out), &names))
}

fn group_remote_refs(refs: &[&str], remotes: &[String]) -> IndexMap<String, Vec<String>> {
    let mut branches: IndexMap<String, Vec<String>> = IndexMap::new();
    for full in refs {
        let short = full.strip_prefix("refs/remotes/").unwrap_or(full);
        for remote in remotes {
            if let Some(name) = short.strip_prefix(&format!("{remote}/")) {
                if name != "HEAD" {
                    branches.entry(name.to_string()).or_default().push(remote.clone());
                }
                break;
            }
        }
    }
    branches
}

pub fn branch_exists(branch: &str, cwd: &Path) -> bool {
    let reference = format!("refs/heads/{branch}");
    run_git(&["show-ref", "--verify", "--quiet", &reference], Some(cwd), false)
        .is_ok_and(|result| result.code == 0)
}

pub fn find_branch_worktree(branch: &str, cwd: &Path) -> Result<Option<Worktree>> {
    Ok(list_worktrees(Some(cwd))?
        .into_iter()
        .find(|worktree| worktree.branch.as_deref() == Some(branch)))
}

/// Empty string means clean.
pub fn status_porcelain(path: &Path) -> Result<String> {
    Ok(run_git(&["status", "--porcelain"], Some(path), true)?
        .stdout
        .trim_end_matches('\n')
        .to_string())
}

/// Add a worktree: create `branch` tracking the `track` remote ref, check
/// out the existing local branch, or create a brand-new branch.
pub fn worktree_add(repo: &Path, path: &Path, branch: &str, track: Option<&str>) -> Result<()> {
    let path = path.to_string_lossy();
    let args: Vec<&str> = if let Some(track) = track {
        vec!["worktree", "add", "--track", "-b", branch, &path, track]
    } else if branch_exists(branch, repo) {
        vec!["worktree", "add", &path, branch]
    } else {
        vec!["worktree", "add", "-b", branch, &path]
    };
    run_git(&args, Some(repo), true).map(drop)
}

pub fn worktree_remove(repo: &Path, path: &Path, force: bool) -> Result<()> {
    let path = path.to_string_lossy();
    let mut args = vec!["worktree", "remove"];
    if force {
        args.push("--force");
    }
    args.push(&path);
    run_git(&args, Some(repo), true).map(drop)
}

pub fn worktree_lock(repo: &Path, path: &Path, reason: Option<&str>) -> Result<()> {
    let path = path.to_string_lossy();
    let mut args = vec!["worktree", "lock"];
    if let Some(reason) = reason.filter(|reason| !reason.is_empty()) {
        args.extend(["--reason", reason]);
    }
    args.push(&path);
    run_git(&args, Some(repo), true).map(drop)
}

pub fn worktree_unlock(repo: &Path, path: &Path) -> Result<()> {
    run_git(&["worktree", "unlock", &path.to_string_lossy()], Some(repo), true).map(drop)
}

/// Drop every stale record that is not locked. Repository-wide: git has no
/// way to prune one record. What went is the difference between two
/// `list_worktrees` calls, never git's human-readable `-v` output — and a
/// dry run is the `prunable` flag of the listing, so it needs no spawn.
pub fn worktree_prune(repo: &Path) -> Result<()> {
    run_git(&["worktree", "prune"], Some(repo), true).map(drop)
}

pub fn checkout(path: &Path, branch: &str) -> Result<()> {
    run_git(&["checkout", branch], Some(path), true).map(drop)
}

pub fn delete_branch(repo: &Path, branch: &str) -> Result<()> {
    run_git(&["branch", "-D", branch], Some(repo), true).map(drop)
}

/// Per-worktree git dir (.git/worktrees/<name> for linked worktrees).
pub fn git_dir(worktree: &Path) -> Result<PathBuf> {
    git_output(&["rev-parse", "--absolute-git-dir"], Some(worktree)).map(PathBuf::from)
}

/// The repository's shared git dir (.git of the main checkout), the same
/// from every worktree.
pub fn git_common_dir(worktree: &Path) -> Result<PathBuf> {
    git_output(&["rev-parse", "--path-format=absolute", "--git-common-dir"], Some(worktree))
        .map(PathBuf::from)
}

pub fn set_config(worktree: &Path, key: &str, value: &str, per_worktree: bool) -> Result<()> {
    let mut args = vec!["config"];
    if per_worktree {
        args.push("--worktree");
    }
    args.extend([key, value]);
    run_git(&args, Some(worktree), true).map(drop)
}

/// The user's global core.excludesFile, following git's own default.
pub fn global_excludes_file() -> PathBuf {
    let configured = run_git(&["config", "--global", "--get", "core.excludesFile"], None, false)
        .map(|result| result.stdout.trim().to_string())
        .unwrap_or_default();
    if !configured.is_empty() {
        return util::expand_user(&configured);
    }
    let base = match util::env_nonempty("XDG_CONFIG_HOME") {
        Some(xdg) => PathBuf::from(xdg),
        None => util::home_dir().join(".config"),
    };
    base.join("git").join("ignore")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::errors::ErrorKind;
    use crate::testing::{Repo, Sandbox};
    use std::fs;

    // Recorded `git worktree list --porcelain -z` shapes (unit layer: no git).
    const MAIN_ONLY: &str = "worktree /dev/api\0HEAD abc123\0branch refs/heads/main\0\0";
    const WITH_LINKED: &str = "worktree /dev/api\0HEAD abc123\0branch refs/heads/main\0\0\
        worktree /dev/worktrees/api/feat\0HEAD def456\0branch refs/heads/feature/feat\0\0";
    const DETACHED: &str = "worktree /dev/api\0HEAD abc123\0detached\0\0";
    const BARE: &str = "worktree /dev/api.git\0bare\0\0";
    const LOCKED_AND_UNKNOWN: &str = "worktree /dev/api\0HEAD abc123\0branch refs/heads/main\0\0\
        worktree /dev/worktrees/api/x\0HEAD def456\0branch refs/heads/x\0\
        locked because reasons\0prunable gone\0future-attribute value\0\0";

    #[test]
    fn porcelain_main_only() {
        assert_eq!(
            parse_worktree_porcelain(MAIN_ONLY),
            [Worktree {
                path: "/dev/api".into(),
                head: "abc123".into(),
                branch: Some("main".into()),
                is_main: true,
                locked: None,
                prunable: None,
            }]
        );
    }

    #[test]
    fn porcelain_linked_worktree_and_main_flag() {
        let worktrees = parse_worktree_porcelain(WITH_LINKED);
        assert_eq!(worktrees.iter().map(|w| w.is_main).collect::<Vec<_>>(), [true, false]);
        assert_eq!(worktrees[1].branch.as_deref(), Some("feature/feat"));
        assert_eq!(worktrees[1].name(), "feat");
    }

    #[test]
    fn porcelain_detached_and_bare() {
        assert_eq!(parse_worktree_porcelain(DETACHED)[0].branch, None);
        let bare = &parse_worktree_porcelain(BARE)[0];
        assert_eq!((bare.branch.as_deref(), bare.head.as_str()), (None, ""));
    }

    #[test]
    fn porcelain_unknown_attributes_ignored_and_reasons_kept() {
        let worktrees = parse_worktree_porcelain(LOCKED_AND_UNKNOWN);
        assert_eq!(worktrees.len(), 2);
        assert_eq!(worktrees[1].branch.as_deref(), Some("x"));
        assert_eq!(
            (worktrees[0].locked.as_deref(), worktrees[0].prunable.as_deref()),
            (None, None)
        );
        assert_eq!(worktrees[1].locked.as_deref(), Some("because reasons"));
        assert_eq!(worktrees[1].prunable.as_deref(), Some("gone"));
    }

    #[test]
    fn porcelain_bare_lock_is_an_empty_reason_not_an_absent_lock() {
        let data = format!("{MAIN_ONLY}worktree /dev/worktrees/api/x\0HEAD def456\0locked\0\0");
        assert_eq!(parse_worktree_porcelain(&data)[1].locked.as_deref(), Some(""));
    }

    #[test]
    fn porcelain_lock_reason_may_span_lines() {
        let data = format!("{MAIN_ONLY}worktree /dev/worktrees/api/x\0HEAD d\0locked one\ntwo\0\0");
        assert_eq!(parse_worktree_porcelain(&data)[1].locked.as_deref(), Some("one\ntwo"));
    }

    #[test]
    fn porcelain_empty_and_unterminated() {
        assert_eq!(parse_worktree_porcelain(""), []);
        assert_eq!(parse_worktree_porcelain("worktree /dev/api\0HEAD abc").len(), 1);
    }

    fn record(path: &Path, locked: Option<&str>, prunable: Option<&str>) -> Worktree {
        Worktree {
            path: path.to_path_buf(),
            head: "abc".into(),
            branch: Some("x".into()),
            is_main: false,
            locked: locked.map(str::to_string),
            prunable: prunable.map(str::to_string),
        }
    }

    fn linked(sandbox: &Sandbox) -> PathBuf {
        let path = sandbox.path().join("x");
        fs::create_dir(&path).unwrap();
        fs::write(path.join(".git"), "gitdir: elsewhere\n").unwrap();
        path
    }

    #[test]
    fn stale_is_one_stat_on_the_admin_link() {
        let sandbox = Sandbox::new();
        let gone = sandbox.path().join("gone");
        assert!(is_stale(&record(&gone, None, None)));
        // locked and gone is stale though git does not say prunable
        assert!(is_stale(&record(&gone, Some("usb"), None)));

        let healthy = linked(&sandbox);
        assert!(!is_stale(&record(&healthy, None, None)));
        assert!(!is_stale(&record(&healthy, Some(""), None)));
        // git's own flag is enough, whatever the reason says
        assert!(is_stale(&record(&healthy, None, Some(""))));

        fs::remove_file(healthy.join(".git")).unwrap();
        assert!(is_stale(&record(&healthy, None, None)));
    }

    #[test]
    fn main_is_never_stale() {
        let main = Worktree { is_main: true, ..record(Path::new("/nowhere/bare.git"), None, None) };
        assert!(!is_stale(&main));
    }

    #[test]
    fn remote_refs_group_by_branch_longest_remote_first() {
        let remotes = ["up/stream".to_string(), "origin".to_string(), "up".to_string()];
        let refs = [
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/main",
            "refs/remotes/up/main",
            "refs/remotes/up/stream/feat",
            "refs/remotes/elsewhere/x",
        ];
        let grouped = group_remote_refs(&refs, &remotes);
        assert_eq!(grouped["main"], ["origin", "up"]);
        assert_eq!(grouped["feat"], ["up/stream"]);
        assert_eq!(grouped.len(), 2);
    }

    #[test]
    fn repo_root_inside_and_outside() {
        let sandbox = Sandbox::new();
        let repo = sandbox.repo("api");
        assert_eq!(repo_root(Some(&repo.path)).unwrap(), repo.path);
        let sub = repo.path.join("src");
        fs::create_dir(&sub).unwrap();
        assert_eq!(repo_root(Some(&sub)).unwrap(), repo.path);

        let outside = sandbox.path().join("not-a-repo");
        fs::create_dir(&outside).unwrap();
        assert_eq!(repo_root(Some(&outside)).unwrap_err().kind, ErrorKind::NotARepo);
        assert_eq!(list_worktrees(Some(&outside)).unwrap_err().kind, ErrorKind::NotARepo);
        assert_eq!(
            repo_root(Some(&sandbox.path().join("missing"))).unwrap_err().kind,
            ErrorKind::NotARepo
        );
    }

    #[test]
    fn list_worktrees_main_and_current_branch() {
        let sandbox = Sandbox::new();
        let repo = sandbox.repo("api");
        let worktrees = list_worktrees(Some(&repo.path)).unwrap();
        assert_eq!(worktrees.len(), 1);
        assert!(worktrees[0].is_main);
        assert_eq!(worktrees[0].path, repo.path);
        assert_eq!(worktrees[0].branch.as_deref(), Some("main"));
        assert_eq!(main_worktree(Some(&repo.path)).unwrap(), repo.path);
        assert_eq!(current_branch(&repo.path).unwrap(), "main");
    }

    fn sorted(mut names: Vec<String>) -> Vec<String> {
        names.sort();
        names
    }

    #[test]
    fn branches_local_and_remote() {
        let sandbox = Sandbox::new();
        let repo = sandbox.repo_with_origin("api");
        repo.add_branch("feature/local");
        repo.add_remote_only_branch("remote-only", "origin");
        assert_eq!(sorted(local_branches(&repo.path).unwrap()), ["feature/local", "main"]);
        assert_eq!(remotes(&repo.path).unwrap(), ["origin"]);
        let remote = remote_branches(&repo.path).unwrap();
        assert_eq!(
            sorted(remote.keys().cloned().collect()),
            ["feature/local", "main", "remote-only"]
        );
        assert!(remote.values().all(|carriers| carriers == &["origin"]));
        assert!(branch_exists("feature/local", &repo.path));
        assert!(!branch_exists("remote-only", &repo.path));
    }

    #[test]
    fn remote_branches_cover_all_remotes() {
        let sandbox = Sandbox::new();
        let repo = sandbox.repo_with_origin("api");
        repo.add_remote("upstream");
        repo.add_branch("shared");
        repo.git(&["push", "-q", "upstream", "shared"]);
        repo.add_remote_only_branch("upstream-only", "upstream");
        let branches = remote_branches(&repo.path).unwrap();
        assert_eq!(branches["shared"], ["origin", "upstream"]);
        assert_eq!(branches["upstream-only"], ["upstream"]);
    }

    #[test]
    fn a_repository_without_remotes_has_no_remote_branches() {
        let sandbox = Sandbox::new();
        let repo = sandbox.repo("api");
        assert!(remotes(&repo.path).unwrap().is_empty());
        assert!(remote_branches(&repo.path).unwrap().is_empty());
    }

    #[test]
    fn status_porcelain_is_empty_when_clean() {
        let sandbox = Sandbox::new();
        let repo = sandbox.repo("api");
        assert_eq!(status_porcelain(&repo.path).unwrap(), "");
        repo.make_dirty(&repo.path);
        assert!(status_porcelain(&repo.path).unwrap().contains("dirty.txt"));
    }

    fn target(sandbox: &Sandbox, name: &str) -> PathBuf {
        sandbox.path().join("wt").join(name)
    }

    #[test]
    fn worktree_add_new_and_existing_branch() {
        let sandbox = Sandbox::new();
        let repo = sandbox.repo("api");
        let feat = target(&sandbox, "feat");
        worktree_add(&repo.path, &feat, "feat", None).unwrap();
        assert!(feat.is_dir());
        assert_eq!(find_branch_worktree("feat", &repo.path).unwrap().unwrap().path, feat);
        assert!(branch_exists("feat", &repo.path));
        assert_eq!(find_branch_worktree("nope", &repo.path).unwrap(), None);

        repo.add_branch("existing");
        let existing = target(&sandbox, "existing");
        worktree_add(&repo.path, &existing, "existing", None).unwrap();
        assert_eq!(current_branch(&existing).unwrap(), "existing");
    }

    #[test]
    fn worktree_add_tracking_remote() {
        let sandbox = Sandbox::new();
        let repo = sandbox.repo_with_origin("api");
        repo.add_remote_only_branch("remote-feat", "origin");
        let path = target(&sandbox, "remote-feat");
        worktree_add(&repo.path, &path, "remote-feat", Some("origin/remote-feat")).unwrap();
        assert_eq!(current_branch(&path).unwrap(), "remote-feat");
        let upstream = repo.git(&["rev-parse", "--abbrev-ref", "remote-feat@{upstream}"]);
        assert_eq!(upstream, "origin/remote-feat");
    }

    #[test]
    fn worktree_remove_and_delete_branch() {
        let sandbox = Sandbox::new();
        let repo = sandbox.repo("api");
        let gone = target(&sandbox, "gone");
        worktree_add(&repo.path, &gone, "gone", None).unwrap();
        worktree_remove(&repo.path, &gone, false).unwrap();
        assert!(!gone.exists());
        assert_eq!(find_branch_worktree("gone", &repo.path).unwrap(), None);
        delete_branch(&repo.path, "gone").unwrap();
        assert!(!branch_exists("gone", &repo.path));
    }

    #[test]
    fn worktree_remove_dirty_needs_force() {
        let sandbox = Sandbox::new();
        let repo = sandbox.repo("api");
        let dirty = target(&sandbox, "dirty");
        worktree_add(&repo.path, &dirty, "dirty", None).unwrap();
        repo.make_dirty(&dirty);
        assert!(worktree_remove(&repo.path, &dirty, false).unwrap_err().is_git());
        worktree_remove(&repo.path, &dirty, true).unwrap();
        assert!(!dirty.exists());
    }

    fn flags(repo: &Repo) -> Vec<(String, Option<String>, bool)> {
        let mut flags: Vec<_> = list_worktrees(Some(&repo.path))
            .unwrap()
            .into_iter()
            .filter(|w| !w.is_main)
            .map(|w| (w.name(), w.locked.clone(), w.prunable.is_some()))
            .collect();
        flags.sort();
        flags.reverse(); // "kept" before "gone"
        flags
    }

    #[test]
    fn lock_unlock_prune() {
        let sandbox = Sandbox::new();
        let repo = sandbox.repo("api");
        let (kept, gone) = (target(&sandbox, "kept"), target(&sandbox, "gone"));
        worktree_add(&repo.path, &kept, "kept", None).unwrap();
        worktree_add(&repo.path, &gone, "gone", None).unwrap();

        worktree_lock(&repo.path, &kept, Some("on a\nusb drive")).unwrap();
        assert_eq!(flags(&repo)[0], ("kept".into(), Some("on a\nusb drive".into()), false));
        assert!(worktree_lock(&repo.path, &kept, None).unwrap_err().is_git());
        worktree_unlock(&repo.path, &kept).unwrap();
        worktree_lock(&repo.path, &kept, None).unwrap();
        assert_eq!(flags(&repo)[0], ("kept".into(), Some(String::new()), false));
        worktree_unlock(&repo.path, &kept).unwrap();
        assert!(worktree_unlock(&repo.path, &kept).unwrap_err().is_git());

        fs::remove_dir_all(&gone).unwrap();
        assert_eq!(flags(&repo), [("kept".into(), None, false), ("gone".into(), None, true)]);
        worktree_prune(&repo.path).unwrap();
        assert_eq!(flags(&repo), [("kept".into(), None, false)]);
    }

    #[test]
    fn checkout_switches_and_errors_carry_command_and_stderr() {
        let sandbox = Sandbox::new();
        let repo = sandbox.repo("api");
        repo.add_branch("other");
        checkout(&repo.path, "other").unwrap();
        assert_eq!(current_branch(&repo.path).unwrap(), "other");
        let error = checkout(&repo.path, "no-such-branch").unwrap_err();
        assert_eq!(error.kind, ErrorKind::Git);
        assert!(error.message.starts_with("`git checkout no-such-branch` failed: "), "{error}");
    }

    #[test]
    fn git_dirs_and_config() {
        let sandbox = Sandbox::new();
        let repo = sandbox.repo("api");
        let feat = target(&sandbox, "feat");
        worktree_add(&repo.path, &feat, "feat", None).unwrap();
        let common = repo.path.join(".git");
        assert_eq!(git_common_dir(&feat).unwrap(), common);
        assert_eq!(git_common_dir(&repo.path).unwrap(), common);
        assert_eq!(git_dir(&feat).unwrap(), common.join("worktrees").join("feat"));

        set_config(&feat, "extensions.worktreeConfig", "true", false).unwrap();
        set_config(&feat, "core.excludesFile", "/x/y", true).unwrap();
        assert_eq!(git_output(&["config", "core.excludesFile"], Some(&feat)).unwrap(), "/x/y");
        assert_eq!(
            run_git(&["config", "core.excludesFile"], Some(&repo.path), false).unwrap().code,
            1
        );
    }

    #[test]
    fn a_failure_without_output_is_an_unknown_error() {
        let sandbox = Sandbox::new();
        let repo = sandbox.repo("api");
        let error = run_git(&["config", "no.such"], Some(&repo.path), true).unwrap_err();
        assert_eq!(error.message, "`git config no.such` failed: unknown error");
    }
}
