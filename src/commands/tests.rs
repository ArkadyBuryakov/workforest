//! commands: end-to-end flows against real throwaway repos, including
//! locked and stale worktrees — no command may crash on one, run git
//! inside one that is not really a worktree, or pass git's own `fatal:`
//! on. Running scripts (`run`, `make`, `stop`) is tested through the
//! binary, in tests/scripts.rs.

use std::fs;
use std::path::{Path, PathBuf};

use super::*;
use crate::config::ConfigSource;
use crate::output::{capture, with_terminal};
use crate::testing::{Repo, Sandbox};

/// A repository and the context of a command started at its root.
struct Fixture {
    sandbox: Sandbox,
    repo: Repo,
    ctx: Context,
}

impl Fixture {
    fn new() -> Self {
        Self::with(Sandbox::repo)
    }

    fn with_origin() -> Self {
        Self::with(Sandbox::repo_with_origin)
    }

    fn with(make: impl Fn(&Sandbox, &str) -> Repo) -> Self {
        let sandbox = Sandbox::new();
        let repo = make(&sandbox, "api");
        let ctx = sandbox.context(&repo, &repo.path);
        Self { sandbox, repo, ctx }
    }

    /// The same repository after its config changed.
    fn reload(&mut self) {
        self.ctx = self.sandbox.context(&self.repo, &self.repo.path);
    }

    /// The context of a command started inside a worktree.
    fn inside(&self, name: &str) -> Context {
        self.sandbox.context(&self.repo, &self.path(name))
    }

    fn path(&self, name: &str) -> PathBuf {
        self.ctx.worktrees_dir.join(name)
    }

    fn create(&self, branch: &str) {
        quiet(|| cmd_create(&self.ctx, Some(branch), OpenWith::default(), false, true)).unwrap();
    }

    fn names(&self) -> Vec<String> {
        managed_worktrees(&self.ctx).unwrap().iter().map(Worktree::name).collect()
    }

    fn upstream_of(&self, branch: &str) -> String {
        self.repo.git(&["rev-parse", "--abbrev-ref", &format!("{branch}@{{upstream}}")])
    }
}

/// Run a command, dropping what it says.
fn quiet<T>(body: impl FnOnce() -> T) -> T {
    capture(body).0
}

/// The message of the error a command failed with — in our words: git's
/// own text must not be what the user reads.
fn message(result: Result<Outcome>) -> String {
    let error = result.unwrap_err();
    assert!(
        !error.message.contains("fatal:") && !error.message.contains("git worktree"),
        "{error}"
    );
    error.message
}

fn script(outcome: Result<Outcome>) -> String {
    match outcome.unwrap() {
        Outcome::Shell(action) => action.script,
        other => panic!("expected a shell directive, got {other:?}"),
    }
}

fn text(outcome: Result<Outcome>) -> String {
    match outcome.unwrap() {
        Outcome::Text(text) => text,
        other => panic!("expected text, got {other:?}"),
    }
}

fn open_with(opener: Option<&'static str>, path: Option<&'static str>) -> OpenWith<'static> {
    OpenWith { opener, path, ..OpenWith::default() }
}

fn make_stale(path: &Path, keep_directory: bool) {
    if keep_directory {
        fs::remove_file(path.join(".git")).unwrap();
    } else {
        fs::remove_dir_all(path).unwrap();
    }
}

/// One worktree per state, named for it: live · held (locked, no reason)
/// · gone (directory removed) · nogit (directory kept, `.git` removed) ·
/// zombie (locked, then removed).
fn forest() -> Fixture {
    let fixture = Fixture::new();
    for name in ["live", "held", "gone", "nogit", "zombie"] {
        fixture.create(name);
    }
    quiet(|| cmd_lock(&fixture.ctx, "held", None)).unwrap();
    quiet(|| cmd_lock(&fixture.ctx, "zombie", Some("on the\nusb\tdrive"))).unwrap();
    make_stale(&fixture.path("gone"), false);
    make_stale(&fixture.path("nogit"), true);
    make_stale(&fixture.path("zombie"), false);
    fixture
}

fn list_json(ctx: &Context) -> Json {
    serde_json::from_str(&text(cmd_list(ctx, false, true))).unwrap()
}

// --- context ----------------------------------------------------------------

#[test]
fn context_from_main() {
    let fixture = Fixture::new();
    assert_eq!(fixture.ctx.main, fixture.repo.path);
    assert_eq!(fixture.ctx.worktrees_dir, fixture.sandbox.path().join("dev/worktrees/api"));
}

#[test]
fn build_context_finds_main_from_inside_a_worktree() {
    let mut fixture = Fixture::new();
    fixture.repo.write_project_config("opener: from-project\n");
    fixture.repo.commit("config");
    fixture.reload();
    fixture.create("feat");
    // the real entry point: git and the worktree layout, whatever the user's own config says
    let inner = build_context(Some(&fixture.path("feat"))).unwrap();
    assert_eq!(inner.main, fixture.repo.path);
    assert_eq!(inner.cwd_root, fixture.path("feat"));
    assert_eq!(inner.config.sources.last().unwrap().layer, "project");
    assert_eq!(build_context(Some(fixture.sandbox.path())).unwrap_err().kind, ErrorKind::NotARepo);
}

// --- create -----------------------------------------------------------------

#[test]
fn create_new_branch() {
    let fixture = Fixture::new();
    let (outcome, shown) = capture(|| {
        cmd_create(&fixture.ctx, Some("feature/cool-thing"), OpenWith::default(), false, false)
    });
    let worktree = fixture.path("cool-thing"); // short name after the last /
    assert!(worktree.is_dir());
    assert_eq!(git::current_branch(&worktree).unwrap(), "feature/cool-thing");
    let script = script(outcome);
    assert!(script.starts_with(&format!("cd {} && WF_MAIN=", worktree.display())), "{script}");
    assert!(script.ends_with(" stub-editor"));
    assert!(script.contains("WF_BRANCH=feature/cool-thing"));
    assert_eq!(
        shown,
        format!("created worktree for 'feature/cool-thing' at {}\n", worktree.display())
    );
}

#[test]
fn create_existing_local_branch() {
    let fixture = Fixture::new();
    fixture.repo.add_branch("existing");
    fixture.create("existing");
    assert_eq!(git::current_branch(&fixture.path("existing")).unwrap(), "existing");
}

#[test]
fn create_remote_branch_tracks_it() {
    let fixture = Fixture::with_origin();
    fixture.repo.add_remote_only_branch("remote-feat", "origin");
    fixture.create("remote-feat");
    assert_eq!(git::current_branch(&fixture.path("remote-feat")).unwrap(), "remote-feat");
    assert_eq!(fixture.upstream_of("remote-feat"), "origin/remote-feat");
}

#[test]
fn create_remote_qualified_branch() {
    let fixture = Fixture::with_origin();
    fixture.repo.add_remote("upstream");
    fixture.repo.add_remote_only_branch("feat", "upstream");
    fixture.create("upstream/feat");
    assert_eq!(git::current_branch(&fixture.path("feat")).unwrap(), "feat");
    assert_eq!(fixture.upstream_of("feat"), "upstream/feat");
}

#[test]
fn a_branch_on_multiple_remotes_needs_qualifying() {
    let fixture = Fixture::with_origin();
    fixture.repo.add_remote("upstream");
    fixture.repo.add_branch("shared");
    fixture.repo.git(&["push", "-q", "upstream", "shared"]);
    fixture.repo.git(&["branch", "-D", "shared"]);
    assert_eq!(
        message(cmd_create(&fixture.ctx, Some("shared"), OpenWith::default(), false, true)),
        "branch 'shared' exists on multiple remotes (origin, upstream); pick one, e.g. `wf create origin/shared`"
    );
    fixture.create("upstream/shared");
    assert_eq!(fixture.upstream_of("shared"), "upstream/shared");
}

#[test]
fn a_branch_missing_on_the_named_remote_errors() {
    let fixture = Fixture::with_origin();
    assert_eq!(
        message(cmd_create(&fixture.ctx, Some("origin/ghost"), OpenWith::default(), false, true)),
        "branch 'ghost' not found on remote 'origin'"
    );
}

#[test]
fn a_remote_branch_whose_local_name_is_taken_asks_for_another() {
    let fixture = Fixture::with_origin();
    fixture.repo.add_remote("upstream");
    fixture.repo.add_branch("feat"); // local, pushed to origin
    fixture.repo.git(&["push", "-q", "upstream", "feat"]);
    let create =
        || cmd_create(&fixture.ctx, Some("upstream/feat"), OpenWith::default(), false, true);

    // off a terminal there is nobody to ask
    assert_eq!(
        message(create()),
        "branch 'feat' already exists locally; `wf create feat` to use it"
    );
    // an empty answer, and Ctrl-D, cancel
    for answers in [&[""][..], &[]] {
        let outcome = quiet(|| with_terminal(answers, create));
        assert_eq!(outcome.unwrap_err().kind, ErrorKind::Cancelled);
    }
    // a taken name is asked about again
    let (outcome, shown) = capture(|| with_terminal(&["feat", "feat-upstream"], create));
    assert_eq!(outcome, Ok(Outcome::Nothing));
    assert_eq!(shown.matches("already exists locally; local name for upstream/feat:").count(), 2);
    assert_eq!(git::current_branch(&fixture.path("feat-upstream")).unwrap(), "feat-upstream");
    assert_eq!(fixture.upstream_of("feat-upstream"), "upstream/feat");
}

#[test]
fn a_branch_already_in_a_worktree_reuses_it() {
    let fixture = Fixture::new();
    fixture.create("feat");
    let (outcome, shown) =
        capture(|| cmd_create(&fixture.ctx, Some("feat"), OpenWith::default(), false, false));
    assert!(script(outcome).contains(&fixture.path("feat").display().to_string()));
    assert_eq!(
        shown,
        format!("branch 'feat' already checked out at {}\n", fixture.path("feat").display())
    );
}

#[test]
fn the_default_branch_is_the_current_one() {
    // the current branch (main) is checked out in the main worktree → reuse
    let fixture = Fixture::new();
    let script =
        script(quiet(|| cmd_create(&fixture.ctx, None, OpenWith::default(), false, false)));
    assert!(script.starts_with(&format!("cd {} && WF_MAIN=", fixture.repo.path.display())));
    assert!(script.ends_with(" stub-editor"));

    fixture.repo.git(&["checkout", "-q", "--detach"]);
    assert_eq!(
        message(cmd_create(&fixture.ctx, Some(""), OpenWith::default(), false, true)),
        "detached HEAD: specify a branch name"
    );
}

#[test]
fn a_short_name_collision_errors() {
    let fixture = Fixture::new();
    fixture.create("feat/x");
    let error = message(cmd_create(&fixture.ctx, Some("fix/x"), OpenWith::default(), false, true));
    assert_eq!(
        error,
        format!(
            "{} already holds a different branch; remove it first or use a different branch name",
            fixture.path("x").display()
        )
    );
}

#[test]
fn an_existing_non_worktree_directory_errors() {
    let fixture = Fixture::new();
    fs::create_dir_all(fixture.path("feat")).unwrap();
    assert_eq!(
        message(cmd_create(&fixture.ctx, Some("feat"), OpenWith::default(), false, true)),
        format!("directory exists but is not a worktree: {}", fixture.path("feat").display())
    );
}

#[test]
fn hooks_run_on_create_unless_skipped() {
    let mut fixture = Fixture::new();
    let out = fixture.sandbox.path().join("hook-ran.txt");
    fixture.repo.write_project_config(&format!(
        "symlinks: ['.env']\nsetup_scripts: ['echo $WF_BRANCH > {}', 'exit 3']\n",
        out.display()
    ));
    fixture.repo.commit("config");
    fs::write(fixture.repo.path.join(".env"), "X=1\n").unwrap(); // untracked, like a real .env
    fixture.reload();

    let (outcome, shown) =
        capture(|| cmd_create(&fixture.ctx, Some("feat"), OpenWith::default(), false, true));
    assert_eq!(outcome, Ok(Outcome::Nothing));
    assert!(fixture.path("feat").join(".env").is_symlink());
    assert_eq!(fs::read_to_string(&out).unwrap(), "feat\n");
    assert_eq!(git::status_porcelain(&fixture.path("feat")).unwrap(), "");
    assert!(shown.ends_with("setup script failed: exit 3\n1 setup script(s) failed\n"), "{shown}");

    fs::remove_file(&out).unwrap();
    quiet(|| cmd_create(&fixture.ctx, Some("bare"), OpenWith::default(), true, true)).unwrap();
    assert!(!fixture.path("bare").join(".env").exists());
    assert!(!out.exists());
}

// --- open -------------------------------------------------------------------

#[test]
fn open_the_worktree_root() {
    let fixture = Fixture::new();
    fixture.create("feat");
    let script = script(cmd_open(&fixture.ctx, Some("feat"), OpenWith::default()));
    assert!(script.starts_with(&format!("cd {} && WF_MAIN=", fixture.path("feat").display())));
    assert!(script.ends_with(" stub-editor"));
}

#[test]
fn a_path_only_sets_the_target() {
    // -p never changes the launch cwd; it only sets WF_TARGET.
    let fixture = Fixture::new();
    fixture.create("feat");
    let script = script(cmd_open(
        &fixture.ctx,
        Some("feat"),
        open_with(Some("tool \"$WF_TARGET\""), Some("README.md")),
    ));
    assert!(script.starts_with(&format!("cd {} && ", fixture.path("feat").display())));
    assert!(script.ends_with(" /bin/sh -c 'tool \"$WF_TARGET\"'"), "{script}");
    assert!(script.contains("WF_TARGET=README.md"));
}

#[test]
fn open_unknown_and_unnamed() {
    let fixture = Fixture::new();
    assert_eq!(
        message(cmd_open(&fixture.ctx, Some("nope"), OpenWith::default())),
        format!("worktree 'nope' not found in {}", fixture.ctx.worktrees_dir.display())
    );
    let unnamed = cmd_open(&fixture.ctx, None, OpenWith::default()).unwrap_err();
    assert_eq!(unnamed.kind, ErrorKind::Usage);
    assert_eq!(unnamed.message, "worktree name required (or run inside a managed worktree)");
    // main is not managed; only worktrees inside worktrees_dir resolve
    assert!(
        message(cmd_open(&fixture.ctx, Some("api"), OpenWith::default())).contains("not found")
    );
}

#[test]
fn open_without_a_name_inside_a_worktree_opens_it() {
    let fixture = Fixture::new();
    fixture.create("feat");
    let script = script(cmd_open(&fixture.inside("feat"), None, OpenWith::default()));
    assert!(script.starts_with(&format!("cd {} && ", fixture.path("feat").display())));
}

#[test]
fn a_stale_worktree_is_not_opened_and_a_locked_one_is() {
    let fixture = forest();
    let open = |name: &str| cmd_open(&fixture.ctx, Some(name), OpenWith::default());
    assert_eq!(
        message(open("gone")),
        "worktree 'gone' is stale — run: wf prune, then wf create gone"
    );
    assert!(message(open("nogit")).contains("is stale"));
    assert_eq!(
        message(open("zombie")),
        "worktree 'zombie' is stale and locked — run: wf unlock zombie, then wf prune"
    );
    // a lock never blocks opening
    assert!(matches!(open("held"), Ok(Outcome::Shell(_))));
}

// --- list -------------------------------------------------------------------

#[test]
fn porcelain_format_is_stable() {
    let fixture = Fixture::new();
    fixture.create("feature/one");
    fixture.create("two");
    fixture.repo.make_dirty(&fixture.path("two"));
    assert_eq!(
        text(cmd_list(&fixture.ctx, true, false)),
        format!(
            "one\tfeature/one\t{}\t0\t\t\ntwo\ttwo\t{}\t1\t\t",
            fixture.path("one").display(),
            fixture.path("two").display()
        )
    );
}

#[test]
fn the_human_listing_is_aligned() {
    let fixture = Fixture::new();
    fixture.create("feat");
    fixture.create("feature/longer-name");
    fixture.repo.make_dirty(&fixture.path("feat"));
    fixture.repo.git_in(&fixture.path("longer-name"), &["checkout", "-q", "--detach"]);
    assert_eq!(
        text(cmd_list(&fixture.ctx, false, false)),
        format!(
            "feat         feat        dirty  {}\nlonger-name  (detached)  clean  {}",
            fixture.path("feat").display(),
            fixture.path("longer-name").display()
        )
    );
}

#[test]
fn an_empty_forest_lists_nothing() {
    let fixture = Fixture::new();
    let (outcome, shown) = capture(|| cmd_list(&fixture.ctx, false, false));
    assert_eq!(outcome, Ok(Outcome::Nothing));
    assert_eq!(
        shown,
        format!(
            "no worktrees in {} (create one with: wf create BRANCH)\n",
            fixture.ctx.worktrees_dir.display()
        )
    );
    assert_eq!(
        capture(|| cmd_list(&fixture.ctx, true, false)),
        (Ok(Outcome::Nothing), String::new())
    );
    // JSON still describes main
    let data = list_json(&fixture.ctx);
    assert_eq!(data["worktrees"], json!([]));
    assert_eq!(data["main"]["path"], fixture.repo.path.to_str().unwrap());
}

#[test]
fn json_covers_the_whole_forest() {
    let fixture = Fixture::new();
    fixture.create("feature/one");
    fixture.repo.make_dirty(&fixture.repo.path); // the main checkout
    assert_eq!(
        list_json(&fixture.ctx),
        json!({
            "main": {
                "name": "api", "branch": "main", "path": fixture.repo.path, "dirty": true,
                "locked": null, "prunable": null, "running": {},
            },
            "worktrees_dir": fixture.ctx.worktrees_dir,
            "worktrees": [{
                "name": "one", "branch": "feature/one", "path": fixture.path("one"), "dirty": false,
                "locked": null, "prunable": null, "running": {},
            }],
        })
    );
    assert!(
        text(cmd_list(&fixture.ctx, false, true))
            .starts_with("{\n  \"main\": {\n    \"name\": \"api\",")
    );
}

#[test]
fn json_reports_running_scripts() {
    use crate::jobs;
    use std::os::unix::process::CommandExt;
    let fixture = Fixture::new();
    fixture.create("feat");
    let common = git::git_common_dir(&fixture.repo.path).unwrap();
    // A live process group of its own, exactly what `wf run` records.
    let mut process =
        std::process::Command::new("sleep").arg("30").process_group(0).spawn().unwrap();
    let owner = std::process::id() as i32;
    let record = jobs::JobRecord {
        script: "dev".into(),
        worktree: fixture.path("feat").to_string_lossy().into_owned(),
        branch: "feat".into(),
        pgid: process.id() as i32,
        owner_pid: owner,
        boot_id: jobs::boot_id(),
        started_at: jobs::now(),
        stopped_by: None,
    };
    jobs::write_record(&jobs::record_path(&common, "dev", &fixture.path("feat"), owner), &record)
        .unwrap();
    let data = list_json(&fixture.ctx);
    jobs::signal_group(process.id() as i32, nix::sys::signal::Signal::SIGKILL);
    process.wait().unwrap();
    assert_eq!(data["main"]["running"], json!({}));
    assert_eq!(data["worktrees"][0]["running"], json!({"dev": 1}));
}

#[test]
fn every_state_is_listed_and_none_fails() {
    let fixture = forest();
    let human = text(cmd_list(&fixture.ctx, false, false));
    let states: Vec<(String, String)> = human
        .lines()
        .map(|line| {
            let words: Vec<&str> = line.split_whitespace().collect();
            (words[0].to_string(), words[2..words.len() - 1].join(" "))
        })
        .collect();
    let expected = [
        ("gone", "stale"),
        ("held", "clean locked"),
        ("live", "clean"),
        ("nogit", "stale"),
        ("zombie", "stale locked"),
    ];
    assert_eq!(states, expected.map(|(name, state)| (name.to_string(), state.to_string())));

    let porcelain = text(cmd_list(&fixture.ctx, true, false));
    let rows: Vec<Vec<&str>> =
        porcelain.split('\n').map(|line| line.split('\t').collect()).collect();
    assert!(rows.iter().all(|row| row.len() == 6), "{porcelain}");
    let row = |name: &str| rows.iter().find(|row| row[0] == name).unwrap();
    assert_eq!(row("live")[3..], ["0", "", ""]);
    assert_eq!(row("held")[3..], ["0", "locked", ""]);
    // stale: dirty was never asked; the reason's wording is git's own
    assert_eq!(row("gone")[3..5], ["", ""]);
    assert!(row("gone")[5].starts_with("prunable ") && row("nogit")[5].starts_with("prunable "));
    // a reason with a newline and a tab is flattened, not split
    assert_eq!(row("zombie")[3..5], ["", "locked on the usb drive"]);
    assert!(row("zombie")[5].starts_with("prunable "));

    let data = list_json(&fixture.ctx);
    assert_eq!((&data["main"]["locked"], &data["main"]["prunable"]), (&Json::Null, &Json::Null));
    let json_row = |name: &str| {
        data["worktrees"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["name"] == name)
            .unwrap()
            .clone()
    };
    assert_eq!(json_row("live")["dirty"], false);
    assert_eq!(
        (&json_row("held")["dirty"], &json_row("held")["locked"]),
        (&json!(false), &json!(""))
    );
    for name in ["gone", "nogit", "zombie"] {
        assert!(json_row(name)["dirty"].is_null());
        assert!(json_row(name)["prunable"].is_string());
    }
    // the raw reason: JSON has no line protocol to protect
    assert_eq!(json_row("zombie")["locked"], "on the\nusb\tdrive");

    fixture.repo.make_dirty(&fixture.path("held"));
    assert!(text(cmd_list(&fixture.ctx, false, false)).contains("dirty locked"));
}

#[test]
fn a_nested_stale_directory_never_reports_the_main_checkout() {
    // With the worktrees inside the main checkout, `git status` in a
    // directory that lost its `.git` file answers for the main checkout —
    // silently wrong, not a crash.
    let mut fixture = Fixture::new();
    fixture.repo.write_project_config("worktrees_dir: wt\n");
    fixture.reload();
    fixture.create("feat");
    make_stale(&fixture.path("feat"), true);
    fixture.repo.make_dirty(&fixture.repo.path); // the main checkout
    let row = list_json(&fixture.ctx)["worktrees"][0].clone();
    assert!(row["dirty"].is_null() && row["prunable"].is_string());
    let human = text(cmd_list(&fixture.ctx, false, false));
    assert!(human.contains("stale") && !human.contains("dirty"), "{human}");
}

// --- delete -----------------------------------------------------------------

#[test]
fn a_clean_delete_keeps_the_branch_off_a_terminal() {
    let fixture = Fixture::new();
    fixture.create("feat");
    let (outcome, shown) = capture(|| cmd_delete(&fixture.ctx, &["feat".into()], false, None));
    assert_eq!(outcome, Ok(Outcome::Nothing)); // deleted from elsewhere: nowhere to move the shell
    assert_eq!(shown, "deleted worktree 'feat'\n");
    assert!(!fixture.path("feat").exists());
    assert!(git::branch_exists("feat", &fixture.repo.path)); // kept by default
}

#[test]
fn the_branch_goes_when_asked() {
    let fixture = Fixture::new();
    fixture.create("feat");
    let (_, shown) = capture(|| cmd_delete(&fixture.ctx, &["feat".into()], false, Some(true)));
    assert_eq!(shown, "deleted worktree 'feat'\ndeleted branch 'feat'\n");
    assert!(!git::branch_exists("feat", &fixture.repo.path));
}

#[test]
fn a_dirty_worktree_needs_force_off_a_terminal() {
    let fixture = Fixture::new();
    fixture.create("feat");
    for index in 0..12 {
        fs::write(fixture.path("feat").join(format!("f{index:02}")), "x").unwrap();
    }
    let (outcome, shown) = capture(|| cmd_delete(&fixture.ctx, &["feat".into()], false, None));
    let error = outcome.unwrap_err();
    assert_eq!(error.kind, ErrorKind::Cancelled);
    assert_eq!(error.message, "cannot prompt ('Delete anyway?'): not a terminal; use --force");
    // the first ten changes, then an ellipsis
    assert_eq!(shown.lines().count(), 12);
    assert!(shown.starts_with("worktree 'feat' has uncommitted changes:\n  ?? f00\n"), "{shown}");
    assert!(shown.ends_with("  ?? f09\n  ...\n"), "{shown}");
    assert!(fixture.path("feat").exists());

    quiet(|| cmd_delete(&fixture.ctx, &["feat".into()], true, None)).unwrap();
    assert!(!fixture.path("feat").exists());
}

#[test]
fn on_a_terminal_a_dirty_delete_is_confirmed_or_cancelled() {
    let fixture = Fixture::new();
    fixture.create("feat");
    fixture.repo.make_dirty(&fixture.path("feat"));
    let delete = || cmd_delete(&fixture.ctx, &["feat".into()], false, None);

    let declined = quiet(|| with_terminal(&["n"], delete));
    assert_eq!(declined.unwrap_err().kind, ErrorKind::Cancelled);
    assert!(fixture.path("feat").exists());

    // yes delete the dirty worktree, no keep the branch
    let (outcome, shown) = capture(|| with_terminal(&["y", "n"], delete));
    assert_eq!(outcome, Ok(Outcome::Nothing));
    assert!(
        shown.contains("Delete anyway? [y/N] ")
            && shown.contains("Also delete branch 'feat'? [y/N] ")
    );
    assert!(!fixture.path("feat").exists());
    assert!(git::branch_exists("feat", &fixture.repo.path));

    fixture.create("feat");
    quiet(|| with_terminal(&["y"], delete)).unwrap(); // clean: only the branch is asked about
    assert!(!git::branch_exists("feat", &fixture.repo.path));
}

#[test]
fn a_batch_resolves_every_name_before_deleting_any() {
    let fixture = Fixture::new();
    fixture.create("one");
    fixture.create("two");
    assert_eq!(
        message(cmd_delete(&fixture.ctx, &["one".into(), "ghost".into()], false, None)),
        format!("worktree 'ghost' not found in {}", fixture.ctx.worktrees_dir.display())
    );
    assert!(fixture.path("one").exists());
    quiet(|| cmd_delete(&fixture.ctx, &["one".into(), "two".into()], false, None)).unwrap();
    assert!(fixture.names().is_empty());
}

#[test]
fn deleting_the_worktree_we_stand_in_moves_the_shell_to_main() {
    let fixture = Fixture::new();
    fixture.create("feat");
    let inside = fixture.inside("feat");
    let outcome = quiet(|| cmd_delete(&inside, &["feat".into()], false, None));
    assert_eq!(script(outcome), format!("cd {}", fixture.repo.path.display()));
}

#[test]
fn a_locked_worktree_is_never_deleted_not_even_by_force() {
    let fixture = forest();
    assert_eq!(
        message(cmd_delete(&fixture.ctx, &["held".into()], true, None)),
        "worktree 'held' is locked — run: wf unlock held"
    );
    assert!(fixture.path("held").is_dir());
    // the reason, on one line
    assert_eq!(
        message(cmd_delete(&fixture.ctx, &["zombie".into()], false, None)),
        "worktree 'zombie' is locked (on the usb drive) — run: wf unlock zombie"
    );
    // a lock anywhere in the batch deletes nothing
    assert!(
        message(cmd_delete(&fixture.ctx, &["live".into(), "held".into()], true, None))
            .contains("'held' is locked")
    );
    assert!(fixture.path("live").is_dir() && fixture.names().contains(&"live".to_string()));
}

#[test]
fn deleting_a_stale_worktree_prunes_only_that_record() {
    let fixture = forest();
    let (outcome, shown) =
        capture(|| cmd_delete(&fixture.ctx, &["gone".into()], false, Some(false)));
    assert_eq!(outcome, Ok(Outcome::Nothing));
    assert_eq!(shown, "pruned stale worktree 'gone'\n");
    // the other stale record is not swept along
    assert_eq!(fixture.names(), ["held", "live", "nogit", "zombie"]);
    assert!(git::branch_exists("gone", &fixture.repo.path));
}

#[test]
fn a_stale_delete_can_take_the_branch_along() {
    let fixture = forest();
    quiet(|| cmd_delete(&fixture.ctx, &["gone".into()], false, Some(true))).unwrap();
    assert!(!git::branch_exists("gone", &fixture.repo.path));
}

#[test]
fn a_stale_worktree_whose_directory_is_left_keeps_its_files_and_says_so() {
    let fixture = forest();
    fs::write(fixture.path("nogit").join("work.txt"), "mine\n").unwrap();
    let (_, shown) = capture(|| cmd_delete(&fixture.ctx, &["nogit".into()], false, Some(false)));
    assert_eq!(fs::read_to_string(fixture.path("nogit").join("work.txt")).unwrap(), "mine\n");
    assert!(
        shown.contains(&format!("left {} in place", fixture.path("nogit").display())),
        "{shown}"
    );
    // git can only clear this one repository-wide: what else went is named
    assert!(shown.contains("also pruned the other stale worktree records: gone"), "{shown}");
    assert_eq!(fixture.names(), ["held", "live", "zombie"]);
}

#[test]
fn a_batch_survives_a_record_pruned_along_the_way() {
    let fixture = forest();
    let names = ["nogit".to_string(), "gone".into(), "live".into()];
    quiet(|| cmd_delete(&fixture.ctx, &names, false, Some(false))).unwrap();
    assert_eq!(fixture.names(), ["held", "zombie"]);
}

#[test]
fn locked_behind_our_back_is_still_our_message() {
    // Locked between the check and the removal: git refuses, and its text
    // must not be what the user reads.
    let fixture = forest();
    let found = find_managed(&fixture.ctx, "live").unwrap();
    git::worktree_lock(&fixture.repo.path, &found.path, Some("late")).unwrap();
    let error = remove(&fixture.ctx, &found).unwrap_err();
    assert_eq!(error.message, "worktree 'live' is locked (late) — run: wf unlock live");

    // a stale one locked behind our back is not reported pruned
    let stale = find_managed(&fixture.ctx, "nogit").unwrap();
    git::worktree_lock(&fixture.repo.path, &stale.path, Some("late")).unwrap();
    let error = quiet(|| forget(&fixture.ctx, &stale)).unwrap_err();
    assert_eq!(error.message, "worktree 'nogit' is locked (late) — run: wf unlock nogit");
    assert!(fixture.names().contains(&"nogit".to_string()));
}

#[test]
fn other_git_failures_are_not_swallowed() {
    let fixture = forest();
    let mut elsewhere = find_managed(&fixture.ctx, "live").unwrap();
    elsewhere.path = fixture.sandbox.path().join("never-a-worktree");
    let error = remove(&fixture.ctx, &elsewhere).unwrap_err();
    assert_eq!(error.kind, ErrorKind::Git);
    assert!(error.message.starts_with("`git worktree remove --force "), "{error}");
    // a record that is already gone is nothing to forget
    assert_eq!(forget(&fixture.ctx, &elsewhere), Ok(()));
}

// --- checkout ---------------------------------------------------------------

#[test]
fn checkout_collapses_a_worktree_into_main() {
    let fixture = Fixture::new();
    fixture.create("feat");
    let (outcome, shown) = capture(|| cmd_checkout(&fixture.ctx, "feat", false));
    assert_eq!(script(outcome), format!("cd {}", fixture.repo.path.display()));
    assert!(!fixture.path("feat").exists());
    assert_eq!(git::current_branch(&fixture.repo.path).unwrap(), "feat");
    assert_eq!(
        shown,
        format!("deleted worktree 'feat'\nchecked out 'feat' in {}\n", fixture.repo.path.display())
    );
}

#[test]
fn checkout_of_a_dirty_worktree_needs_force_off_a_terminal() {
    let fixture = Fixture::new();
    fixture.create("feat");
    fixture.repo.make_dirty(&fixture.path("feat"));
    let refused = quiet(|| cmd_checkout(&fixture.ctx, "feat", false)).unwrap_err();
    assert_eq!(refused.kind, ErrorKind::Cancelled);
    assert!(
        refused
            .message
            .contains("Delete worktree and checkout its branch in the main repo anyway?")
    );
    quiet(|| cmd_checkout(&fixture.ctx, "feat", true)).unwrap();
    assert_eq!(git::current_branch(&fixture.repo.path).unwrap(), "feat");
}

#[test]
fn checkout_refuses_stale_locked_and_detached_worktrees() {
    let fixture = forest();
    assert_eq!(
        message(cmd_checkout(&fixture.ctx, "gone", false)),
        "worktree 'gone' is stale — run: wf prune"
    );
    assert_eq!(
        message(cmd_checkout(&fixture.ctx, "held", true)),
        "worktree 'held' is locked — run: wf unlock held"
    );
    assert!(fixture.path("held").is_dir());
    assert!(
        message(cmd_checkout(&fixture.ctx, "zombie", false))
            .contains("stale and locked — run: wf unlock zombie")
    );
    fixture.repo.git_in(&fixture.path("live"), &["checkout", "-q", "--detach"]);
    assert_eq!(
        message(cmd_checkout(&fixture.ctx, "live", false)),
        "cannot determine branch for worktree 'live' (detached HEAD)"
    );
}

// --- create over stale records ----------------------------------------------

#[test]
fn create_prunes_a_stale_record_and_recreates_the_worktree() {
    let fixture = forest();
    let (outcome, shown) =
        capture(|| cmd_create(&fixture.ctx, Some("gone"), OpenWith::default(), false, true));
    assert_eq!(outcome, Ok(Outcome::Nothing));
    assert!(shown.contains(&format!(
        "pruned the stale worktree record at {}",
        fixture.path("gone").display()
    )));
    assert!(!shown.contains("fatal:"));
    assert!(fixture.path("gone").join(".git").exists());
}

#[test]
fn create_keeps_a_stale_record_that_is_locked() {
    let fixture = forest();
    let error = message(cmd_create(&fixture.ctx, Some("zombie"), OpenWith::default(), false, true));
    assert!(error.contains("stale and locked — run: wf unlock zombie, then wf prune"), "{error}");
    assert!(fixture.names().contains(&"zombie".to_string()));
}

#[test]
fn create_never_overwrites_a_directory_left_behind() {
    let fixture = forest();
    let result =
        quiet(|| cmd_create(&fixture.ctx, Some("nogit"), OpenWith::default(), false, true));
    assert!(message(result).contains("directory exists but is not a worktree"));
    assert!(fixture.path("nogit").is_dir());
}

#[test]
fn create_clears_a_stale_record_of_another_branch_at_the_same_path() {
    // fix/gone would land in the directory the stale `gone` record names
    let fixture = forest();
    fixture.create("fix/gone");
    assert_eq!(find_managed(&fixture.ctx, "gone").unwrap().branch.as_deref(), Some("fix/gone"));
}

#[test]
fn a_locked_stale_record_outside_the_worktrees_dir_is_named_by_path() {
    let fixture = Fixture::new();
    let elsewhere = fixture.sandbox.path().join("elsewhere").join("feat");
    git::worktree_add(&fixture.repo.path, &elsewhere, "feat", None).unwrap();
    git::worktree_lock(&fixture.repo.path, &elsewhere, None).unwrap();
    fs::remove_dir_all(&elsewhere).unwrap();
    let error =
        cmd_create(&fixture.ctx, Some("feat"), OpenWith::default(), false, true).unwrap_err();
    assert_eq!(
        error.message,
        format!(
            "a stale, locked worktree record at {0} is in the way — run: git worktree unlock {0}, then wf prune",
            elsewhere.display()
        )
    );
}

// --- lock / unlock ----------------------------------------------------------

#[test]
fn lock_and_unlock_round_trip() {
    let fixture = Fixture::new();
    fixture.create("feat");
    assert_eq!(
        capture(|| cmd_lock(&fixture.ctx, "feat", Some("keep"))),
        (Ok(Outcome::Nothing), "locked worktree 'feat'\n".to_string())
    );
    assert_eq!(find_managed(&fixture.ctx, "feat").unwrap().locked.as_deref(), Some("keep"));
    assert_eq!(
        capture(|| cmd_unlock(&fixture.ctx, "feat")),
        (Ok(Outcome::Nothing), "unlocked worktree 'feat'\n".to_string())
    );
    assert_eq!(find_managed(&fixture.ctx, "feat").unwrap().locked, None);
}

#[test]
fn locking_twice_and_unlocking_the_unlocked_are_errors() {
    let fixture = forest();
    assert_eq!(
        message(cmd_lock(&fixture.ctx, "zombie", Some("another"))),
        "worktree 'zombie' is already locked (on the usb drive) — run: wf unlock zombie to lock it anew"
    );
    assert_eq!(
        find_managed(&fixture.ctx, "zombie").unwrap().locked.as_deref(),
        Some("on the\nusb\tdrive")
    );
    assert_eq!(
        message(cmd_lock(&fixture.ctx, "held", None)),
        "worktree 'held' is already locked — run: wf unlock held to lock it anew"
    );
    assert_eq!(message(cmd_unlock(&fixture.ctx, "live")), "worktree 'live' is not locked");
    // the main checkout is not ours to lock
    assert!(message(cmd_lock(&fixture.ctx, "api", None)).contains("not found"));
}

#[test]
fn a_stale_record_can_be_locked_and_unlocked() {
    let fixture = forest();
    quiet(|| cmd_lock(&fixture.ctx, "gone", Some("drive is away"))).unwrap();
    assert_eq!(
        find_managed(&fixture.ctx, "gone").unwrap().locked.as_deref(),
        Some("drive is away")
    );
    quiet(|| cmd_unlock(&fixture.ctx, "zombie")).unwrap();
    assert_eq!(find_managed(&fixture.ctx, "zombie").unwrap().locked, None);
}

#[test]
fn a_lock_or_unlock_raced_by_someone_else_reports_what_is_true_now() {
    let fixture = forest();
    let live = find_managed(&fixture.ctx, "live").unwrap();
    git::worktree_lock(&fixture.repo.path, &live.path, Some("theirs")).unwrap();
    assert_eq!(
        message(lock_found(&fixture.ctx, &live, Some("ours"))),
        "worktree 'live' is already locked (theirs) — run: wf unlock live to lock it anew"
    );
    let held = find_managed(&fixture.ctx, "held").unwrap();
    git::worktree_unlock(&fixture.repo.path, &held.path).unwrap();
    assert_eq!(message(unlock_found(&fixture.ctx, &held)), "worktree 'held' is not locked");
}

#[test]
fn unrelated_git_failures_pass_through_lock_and_unlock() {
    let fixture = forest();
    let mut nowhere = find_managed(&fixture.ctx, "live").unwrap();
    nowhere.path = fixture.sandbox.path().join("never-a-worktree");
    assert_eq!(lock_found(&fixture.ctx, &nowhere, None).unwrap_err().kind, ErrorKind::Git);
    nowhere.locked = Some(String::new());
    assert_eq!(unlock_found(&fixture.ctx, &nowhere).unwrap_err().kind, ErrorKind::Git);
}

// --- prune ------------------------------------------------------------------

#[test]
fn a_dry_run_changes_nothing() {
    let fixture = forest();
    let before = fixture.names();
    let (outcome, shown) = capture(|| cmd_prune(&fixture.ctx, true));
    assert_eq!(outcome, Ok(Outcome::Nothing));
    assert_eq!(
        shown,
        "would prune 2 stale worktree records: gone, nogit\n\
         1 stale worktree record is locked: zombie — unlock to prune\n"
    );
    assert_eq!(fixture.names(), before);
}

#[test]
fn prune_names_everything_it_removed_or_kept() {
    let fixture = forest();
    let (_, shown) = capture(|| cmd_prune(&fixture.ctx, false));
    let lines: Vec<&str> = shown.lines().collect();
    assert_eq!(lines[0], "pruned 2 stale worktree records: gone, nogit");
    assert!(lines[1].starts_with(&format!("left {} in place", fixture.path("nogit").display())));
    assert_eq!(lines[2], "1 stale worktree record is locked: zombie — unlock to prune");
    assert_eq!(fixture.names(), ["held", "live", "zombie"]);
    assert!(fixture.path("nogit").is_dir());

    // only locked ones left is not "nothing to prune"
    assert_eq!(
        capture(|| cmd_prune(&fixture.ctx, false)).1,
        "1 stale worktree record is locked: zombie — unlock to prune\n"
    );
    // a locked stale record goes once unlocked
    quiet(|| cmd_unlock(&fixture.ctx, "zombie")).unwrap();
    assert_eq!(
        capture(|| cmd_prune(&fixture.ctx, false)).1,
        "pruned 1 stale worktree record: zombie\n"
    );
    assert_eq!(fixture.names(), ["held", "live"]);
}

#[test]
fn nothing_to_prune_is_not_an_error() {
    let fixture = Fixture::new();
    for dry_run in [false, true] {
        assert_eq!(
            capture(|| cmd_prune(&fixture.ctx, dry_run)),
            (Ok(Outcome::Nothing), "no stale worktree records\n".to_string())
        );
    }
}

#[test]
fn records_outside_the_worktrees_dir_are_named_by_path() {
    let fixture = Fixture::new();
    let elsewhere = fixture.sandbox.path().join("elsewhere").join("feat");
    git::worktree_add(&fixture.repo.path, &elsewhere, "feat", None).unwrap();
    fs::remove_dir_all(&elsewhere).unwrap();
    assert_eq!(
        capture(|| cmd_prune(&fixture.ctx, false)).1,
        format!("pruned 1 stale worktree record: {}\n", elsewhere.display())
    );
}

// --- init -------------------------------------------------------------------

#[test]
fn init_scaffolds_an_inert_project_config() {
    let mut fixture = Fixture::new();
    let (outcome, shown) = capture(|| cmd_init(&fixture.ctx, false));
    let scaffold = fixture.repo.path.join(".workforest.yaml");
    assert_eq!(outcome, Ok(Outcome::Nothing));
    assert_eq!(
        shown,
        format!(
            "scaffolded {} (all keys commented out; see man 5 workforest)\n",
            scaffold.display()
        )
    );
    // inert: every line is a comment, and loading it changes nothing
    let text = fs::read_to_string(&scaffold).unwrap();
    assert!(text.lines().all(|line| line.is_empty() || line.starts_with('#')));
    fixture.reload();
    assert_eq!(fixture.ctx.config.as_value(), Config::default().as_value());
    assert_eq!(
        fixture.ctx.config.sources.iter().map(|source| source.layer).collect::<Vec<_>>(),
        ["project"]
    );
    // and it is never overwritten
    assert_eq!(
        message(cmd_init(&fixture.ctx, false)),
        format!("{} already exists", scaffold.display())
    );
}

#[test]
fn init_local_goes_into_an_ide_settings_folder() {
    let fixture = Fixture::new();
    assert_eq!(
        message(cmd_init(&fixture.ctx, true)),
        format!(
            "--local needs an IDE settings folder (.vscode/ or .idea/) in {}",
            fixture.repo.path.display()
        )
    );
    fs::create_dir(fixture.repo.path.join(".idea")).unwrap();
    quiet(|| cmd_init(&fixture.ctx, true)).unwrap();
    assert_eq!(
        fs::read_to_string(fixture.repo.path.join(".idea/.workforest.yaml")).unwrap(),
        PROJECT_TEMPLATE
    );
    fs::create_dir(fixture.repo.path.join(".vscode")).unwrap();
    quiet(|| cmd_init(&fixture.ctx, true)).unwrap(); // .vscode/ is looked at first
    assert!(fixture.repo.path.join(".vscode/.workforest.yaml").is_file());
}

// --- pure helpers -------------------------------------------------------------

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
    let row = worktree_json(&listed(worktree("a", Some(""), None), Some(true), false), &running);
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
    assert!(defaults.starts_with("worktrees_dir: $WF_MAIN/../worktrees/$WF_NAME\nopener: ''\n"));
    assert!(defaults.ends_with(
        "  exclusive_scripts: []\n\n# sources (low -> high):\n#   (built-in defaults only)"
    ));

    let config = Config {
        sources: vec![
            ConfigSource { layer: "user", path: "/home/u/.config/workforest/config.yaml".into() },
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
    assert!(show_config(&config, true).starts_with("{\n  \"config\": {\n    \"worktrees_dir\": "));
}
