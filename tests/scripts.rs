//! Running scripts through the binary: `wf run`, `wf make`, `wf stop`,
//! groups, background supervisors and `exclusive` — signal handling and
//! the supervisors live in the main thread of their own process, so that
//! is where they are tested.

mod common;

use std::fs;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use common::{Repo, Sandbox, wait_for};
use nix::sys::signal::Signal;
use workforest::jobs;

const SIGTERM: i32 = Signal::SIGTERM as i32;
const SIGINT: i32 = Signal::SIGINT as i32;

fn read(path: &Path) -> String {
    fs::read_to_string(path).unwrap_or_default()
}

fn records(repo: &Repo, script: &str) -> Vec<jobs::Job> {
    jobs::jobs_for(&repo.common_dir(), script)
}

/// The log of the only instance of `script` in the repository root: its
/// name carries the pid of a supervisor the test never sees.
fn sole_log(repo: &Repo, script: &str) -> PathBuf {
    let directory = repo.common_dir().join(jobs::LOGS_SUBDIR).join(script);
    let mut logs: Vec<PathBuf> =
        fs::read_dir(directory).unwrap().flatten().map(|entry| entry.path()).collect();
    assert_eq!(logs.len(), 1, "{logs:?}");
    logs.remove(0)
}

fn reaped_pid() -> i32 {
    let mut child = Command::new("true").spawn().unwrap();
    child.wait().unwrap();
    child.id() as i32
}

/// A process group of its own that nobody waits for but a thread standing
/// in for init: what an orphaned command looks like.
fn orphan_group() -> (i32, std::thread::JoinHandle<i32>) {
    let mut child = Command::new("sleep").arg("30").process_group(0).spawn().unwrap();
    let pid = child.id() as i32;
    (pid, std::thread::spawn(move || common::exit_code(child.wait().unwrap())))
}

fn write_record(repo: &Repo, script: &str, worktree: &Path, pgid: i32, owner_pid: i32) -> PathBuf {
    let path = jobs::record_path(&repo.common_dir(), script, worktree, owner_pid);
    let record = jobs::JobRecord {
        script: script.into(),
        worktree: worktree.to_string_lossy().into_owned(),
        branch: worktree.file_name().unwrap().to_string_lossy().into_owned(),
        pgid,
        owner_pid,
        boot_id: jobs::boot_id(),
        started_at: jobs::now(),
        stopped_by: None,
    };
    jobs::write_record(&path, &record).unwrap();
    path
}

fn has_proc() -> bool {
    Path::new("/proc/stat").is_file()
}

// --- plain commands -----------------------------------------------------------

#[test]
fn a_script_runs_at_the_worktree_root_with_the_wf_family() {
    let sandbox = Sandbox::new();
    let repo = sandbox.repo("api");
    let out = sandbox.path().join("run-out.txt");
    repo.write_project_config(&format!(
        "scripts:\n  record: echo \"$WF_MAIN|$WF_NAME|$WF_BRANCH|$WF_WORKTREE|$WF_WORKTREES_DIR|$PWD\" > {}\n",
        out.display()
    ));
    repo.commit("config");
    let (main, forest) = (repo.path.display(), repo.worktrees_dir());
    let result = repo.wf(&["run", "record"]).ok();
    assert_eq!(result.out, "");
    assert!(
        result.err.starts_with(&format!("running 'record' in {main}: echo ")),
        "{}",
        result.err
    );
    assert_eq!(read(&out), format!("{main}|api|main|{main}|{}|{main}\n", forest.display()));

    // from a subdirectory of a worktree: still its root, and its branch
    let worktree = repo.create("feat");
    fs::create_dir(worktree.join("sub")).unwrap();
    sandbox.wf(&worktree.join("sub"), &["run", "record"]).ok();
    let feat = worktree.display();
    assert_eq!(read(&out), format!("{main}|api|feat|{feat}|{}|{feat}\n", forest.display()));

    repo.git(&["checkout", "-q", "--detach"]);
    repo.wf(&["run", "record"]).ok();
    assert!(read(&out).starts_with(&format!("{main}|api||")), "a detached HEAD has no branch");
}

#[test]
fn extra_arguments_reach_the_script_quoted_and_flags_are_not_ours() {
    let sandbox = Sandbox::new();
    let repo = sandbox.repo("api");
    let out = sandbox.path().join("args.txt");
    // redirection first, so the appended args become echo's arguments
    repo.write_project_config(&format!("scripts:\n  echoer: echo > {}\n", out.display()));
    let cases: [(&[&str], &str); 5] = [
        (&["run", "echoer", "check", "-j2", "a b"], "check -j2 a b\n"),
        // --force is a workforest flag elsewhere; here it must pass through
        (&["run", "echoer", "--force", "--delete-branch"], "--force --delete-branch\n"),
        (&["run", "echoer", "-b"], "-b\n"),
        (&["run", "echoer", "--", "-x"], "-x\n"),
        (&["run", "echoer"], "\n"),
    ];
    for (args, expected) in cases {
        let result = repo.wf(args).ok();
        assert_eq!(read(&out), expected, "{args:?}: {}", result.err);
    }
}

#[test]
fn script_output_goes_to_stderr() {
    let sandbox = Sandbox::new();
    let repo = sandbox.repo("api");
    repo.write_project_config("scripts:\n  out: echo to-stdout; echo to-stderr >&2\n");
    let result = repo.wf(&["run", "out"]).ok();
    assert_eq!(result.out, "");
    assert!(result.err.ends_with("to-stdout\nto-stderr\n"), "{}", result.err);
}

#[test]
fn failures_signals_and_interruptions() {
    let sandbox = Sandbox::new();
    let repo = sandbox.repo("api");
    let cleaned = sandbox.path().join("cleaned");
    repo.write_project_config(&format!(
        "scripts:\n  boom: exit 3\n  killed: {{command: 'kill -TERM 0', cleanup: 'echo killed >> {0}'}}\n\
         \x20 int130: {{command: 'exit 130', cleanup: 'echo int >> {0}'}}\n  intsig: kill -INT 0\n\
         \x20 messy: {{command: 'true', cleanup: 'exit 7'}}\n  ok: {{command: 'exit 3', cleanup: 'echo failed >> {0}'}}\n",
        cleaned.display()
    ));
    let boom = repo.wf(&["run", "boom"]);
    assert_eq!(boom.code, 1);
    assert_eq!(boom.last_error(), "Error: script 'boom' failed with exit code 3");

    // `kill 0` signals the command's own process group — not us
    let killed = repo.wf(&["run", "killed"]);
    assert_eq!(killed.code, 128 + SIGTERM);
    assert_eq!(killed.last_error(), "Error: script 'killed' was killed by SIGTERM");

    // Ctrl-C is a warning, not an error, and ends us the way it would: a
    // shell whose child died by SIGINT exits 130; `kill -INT 0` is the
    // real thing
    for name in ["int130", "intsig"] {
        let interrupted = repo.wf(&["run", name]);
        assert_eq!(interrupted.code, -SIGINT, "died by the signal, so shell loops abort");
        assert!(
            interrupted.err.contains(&format!("script '{name}' was interrupted\n")),
            "{}",
            interrupted.err
        );
        assert!(!interrupted.err.contains("Error:"));
    }

    let messy = repo.wf(&["run", "messy"]).ok();
    assert!(messy.err.ends_with(
        "running cleanup for 'messy': exit 7\ncleanup for 'messy' failed with exit code 7\n"
    ));
    assert_eq!(repo.wf(&["run", "ok"]).code, 1);
    // cleanup ran however the command ended
    assert_eq!(read(&cleaned), "killed\nint\nfailed\n");
}

#[test]
fn unknown_scripts_and_an_unrunnable_shell() {
    let sandbox = Sandbox::new();
    let repo = sandbox.repo("api");
    repo.write_project_config("scripts:\n  b: 'true'\n  a: 'true'\n");
    assert_eq!(repo.wf(&["run", "nope"]).err, "Error: no script named 'nope' (available: a, b)\n");
    assert_eq!(repo.wf(&["stop", "nope"]).err, "Error: no script named 'nope' (available: a, b)\n");
    assert_eq!(repo.wf(&["stop", "a"]).err, "Error: 'a' is not running in 'api'\n");
    assert_eq!(
        repo.wf(&["stop", "a", "--all"]).err,
        "Error: 'a' is not running anywhere in this project\n"
    );
    let broken = sandbox.wf_env(&repo.path, &["run", "a"], &[("SHELL", "/nonexistent/sh")]);
    assert_eq!(broken.code, 1);
    assert_eq!(broken.last_error(), "Error: cannot run 'a' via $SHELL: No such file or directory");
}

#[test]
fn the_record_lives_only_while_the_script_runs() {
    let sandbox = Sandbox::new();
    let repo = sandbox.repo("api");
    let seen = sandbox.path().join("seen");
    let directory = jobs::records_dir(&repo.common_dir(), "x");
    repo.write_project_config(&format!(
        "scripts:\n  x: ls {} > {}\n",
        directory.display(),
        seen.display()
    ));
    repo.wf(&["run", "x"]).ok();
    assert!(read(&seen).starts_with("api."), "{}", read(&seen));
    assert!(records(&repo, "x").is_empty());
}

#[test]
fn stale_records_do_not_count_as_running() {
    let sandbox = Sandbox::new();
    let repo = sandbox.repo("api");
    repo.write_project_config("scripts:\n  x: 'true'\n");
    let dead = reaped_pid();
    let path = write_record(&repo, "x", &repo.path, dead, dead);
    assert_eq!(repo.wf(&["stop", "x"]).err, "Error: 'x' is not running in 'api'\n");
    assert!(!path.exists());
}

// --- setup hooks ----------------------------------------------------------------

#[test]
fn created_symlinks_stay_hidden_and_global_excludes_still_apply() {
    let sandbox = Sandbox::new();
    let repo = sandbox.repo("api");
    let ignore = sandbox.path().join("global-ignore");
    fs::write(&ignore, "*.log\n").unwrap();
    repo.git(&["config", "--global", "core.excludesFile", &ignore.to_string_lossy()]);
    repo.write_project_config("symlinks: ['.env']\nsetup_scripts: ['echo from-setup']\n");
    repo.commit("config");
    fs::write(repo.path.join(".env"), "x\n").unwrap(); // untracked, like a real .env

    let created = repo.wf(&["create", "feat", "--no-open"]).ok();
    assert!(created.err.contains("from-setup\n"), "a setup script's stdout is our stderr");
    let worktree = repo.worktrees_dir().join("feat");
    assert!(worktree.join(".env").is_symlink());
    // the user's global ignores still apply inside the worktree
    fs::write(worktree.join("noise.log"), "x\n").unwrap();
    assert_eq!(repo.git_in(&worktree, &["status", "--porcelain"]), "");
    let exclude = read(&repo.common_dir().join("worktrees/feat/workforest.exclude"));
    assert!(
        exclude.contains("# --- snapshot of global core.excludesFile")
            && exclude.ends_with("*.log\n# --- workforest symlinks ---\n/.env\n"),
        "{exclude}"
    );
}

// --- exclusive ----------------------------------------------------------------

// A script whose command blocks for as long as its argument says (`wf run
// dev 30`), records where it ran, and cleans up.
const DEV_SCRIPT: &str = "scripts:\n  dev:\n    command: echo started > \"$WF_WORKTREE/started\"; sleep\n    \
                          exclusive: true\n    cleanup: echo \"$WF_BRANCH\" > \"$WF_WORKTREE/cleaned\"\n";

/// A real second `wf run dev 30`, up and recorded.
fn run_victim(sandbox: &Sandbox, repo: &Repo, worktree: &Path) -> common::Running {
    repo.write_project_config(DEV_SCRIPT);
    let victim = sandbox.spawn_wf(worktree, &["run", "dev", "30"]);
    wait_for("the victim to start", || {
        !records(repo, "dev").is_empty() && worktree.join("started").exists()
    });
    victim
}

#[test]
fn an_exclusive_script_preempts_the_instance_in_another_worktree() {
    let sandbox = Sandbox::new();
    let repo = sandbox.repo("api");
    let feat = repo.create("feat");
    let victim = run_victim(&sandbox, &repo, &feat);

    let ours = repo.wf(&["run", "dev", "0"]).ok();
    assert!(ours.err.contains("stopping 'dev' in 'feat' (pgid "), "{}", ours.err);

    let victim = victim.finish();
    assert_eq!(victim.code, 128 + SIGTERM);
    assert_eq!(
        victim.last_error(),
        "Error: script 'dev' was killed by SIGTERM (stopped by `wf run dev` in 'api')"
    );
    assert_eq!(read(&feat.join("cleaned")), "feat\n"); // the victim's own cleanup
    assert_eq!(read(&repo.path.join("cleaned")), "main\n"); // then ours
    assert!(records(&repo, "dev").is_empty());
}

#[test]
fn an_exclusive_script_preempts_the_instance_in_the_same_worktree() {
    let sandbox = Sandbox::new();
    let repo = sandbox.repo("api");
    let victim = run_victim(&sandbox, &repo, &repo.path);
    repo.wf(&["run", "dev", "0"]).ok();
    assert!(victim.finish().err.contains("stopped by `wf run dev` in 'api'"));
    assert!(records(&repo, "dev").is_empty());
}

#[test]
fn instances_of_a_script_that_is_not_exclusive_coexist() {
    let sandbox = Sandbox::new();
    let repo = sandbox.repo("api");
    let feat = repo.create("feat");
    let mut victim = run_victim(&sandbox, &repo, &feat);
    repo.write_project_config(&DEV_SCRIPT.replace("exclusive: true", "exclusive: false"));
    repo.wf(&["run", "dev", "0"]).ok();
    assert!(victim.is_running());
    assert_eq!(records(&repo, "dev").len(), 1);
}

#[test]
fn a_signal_to_wf_is_forwarded_to_the_command() {
    let sandbox = Sandbox::new();
    let repo = sandbox.repo("api");
    let feat = repo.create("feat");
    let victim = run_victim(&sandbox, &repo, &feat);
    victim.signal(Signal::SIGTERM);
    let ended = victim.finish();
    assert_eq!(ended.code, 128 + SIGTERM);
    assert!(
        ended.err.contains("script 'dev' was killed by SIGTERM\n")
            && !ended.err.contains("stopped by")
    );
    assert_eq!(read(&feat.join("cleaned")), "feat\n");
}

#[test]
fn sigint_ends_wf_the_way_ctrl_c_does() {
    let sandbox = Sandbox::new();
    let repo = sandbox.repo("api");
    let feat = repo.create("feat");
    let victim = run_victim(&sandbox, &repo, &feat);
    victim.signal(Signal::SIGINT);
    assert_eq!(victim.finish().code, -SIGINT); // died by the signal, so shell loops abort
    assert_eq!(read(&feat.join("cleaned")), "feat\n");
    assert!(records(&repo, "dev").is_empty());
}

#[test]
fn an_orphan_is_stopped_and_cleaned_up_by_whoever_preempts_it() {
    if !has_proc() {
        return; // verifying an orphan's pid needs /proc
    }
    let sandbox = Sandbox::new();
    let repo = sandbox.repo("api");
    let feat = repo.create("feat");
    repo.write_project_config(DEV_SCRIPT);
    let (pgid, reaper) = orphan_group();
    std::thread::sleep(std::time::Duration::from_millis(50));
    write_record(&repo, "dev", &feat, pgid, reaped_pid());

    let ours = repo.wf(&["run", "dev", "0"]).ok();
    assert!(ours.err.contains("stopping orphaned 'dev' in 'feat'"), "{}", ours.err);
    assert_eq!(reaper.join().unwrap(), -SIGTERM);
    assert_eq!(read(&feat.join("cleaned")), "feat\n");
    assert!(records(&repo, "dev").is_empty());
}

#[test]
fn an_orphan_of_a_deleted_worktree_is_stopped_without_cleanup() {
    if !has_proc() {
        return;
    }
    let sandbox = Sandbox::new();
    let repo = sandbox.repo("api");
    repo.write_project_config(DEV_SCRIPT);
    let (pgid, reaper) = orphan_group();
    std::thread::sleep(std::time::Duration::from_millis(50));
    let gone = sandbox.path().join("dev/gone");
    write_record(&repo, "dev", &gone, pgid, reaped_pid());

    let ours = repo.wf(&["run", "dev", "0"]).ok();
    assert_eq!(reaper.join().unwrap(), -SIGTERM);
    assert!(
        ours.err
            .contains(&format!("skipping cleanup for 'dev': {} no longer exists", gone.display())),
        "{}",
        ours.err
    );
}

// --- background ---------------------------------------------------------------

#[test]
fn a_background_script_detaches_logs_and_cleans_up() {
    let sandbox = Sandbox::new();
    let repo = sandbox.repo("api");
    let cleaned = sandbox.path().join("cleaned");
    repo.write_project_config(&format!(
        "scripts:\n  bg:\n    command: echo \"out $WF_BRANCH\"; echo err >&2; sleep 0.6; exit 3\n    \
         background: true\n    cleanup: echo done > {}\n",
        cleaned.display()
    ));
    let started = Instant::now();
    let result = repo.wf(&["run", "bg"]).ok();
    assert!(started.elapsed().as_millis() < 600, "returned while the command still ran");

    let found = records(&repo, "bg");
    assert_eq!(found.len(), 1);
    let record = &found[0].record;
    let log = jobs::log_path(&repo.common_dir(), "bg", &repo.path, record.owner_pid);
    assert_eq!(
        result.err,
        format!(
            "started 'bg' in the background (pid {}, log: {})\n",
            record.owner_pid,
            log.display()
        )
    );
    assert_eq!(jobs::classify(record), jobs::JobState::Live);
    let listed: serde_json::Value =
        serde_json::from_str(&repo.wf(&["list", "--json"]).ok().out).unwrap();
    assert_eq!(listed["main"]["running"], serde_json::json!({"bg": 1}));

    wait_for("the script to end", || !found[0].path.exists());
    assert_eq!(read(&cleaned), "done\n");
    let text = read(&log);
    assert!(
        text.contains("out main\n")
            && text.contains("err\n")
            && text.contains("running cleanup for 'bg'"),
        "{text}"
    );
}

#[test]
fn the_background_flag_overrides_the_entry() {
    let sandbox = Sandbox::new();
    let repo = sandbox.repo("api");
    repo.write_project_config(
        "scripts:\n  fg: 'true'\n  quick: {command: 'true', background: true}\n",
    );
    for flag in ["-b", "--background"] {
        let result = repo.wf(&["run", flag, "fg"]).ok();
        assert!(
            result.err.contains("'fg' already finished")
                || result.err.contains("started 'fg' in the background"),
            "{}",
            result.err
        );
    }
    // a quick success is reported as finished
    assert!(repo.wf(&["run", "quick"]).ok().err.starts_with("'quick' already finished (log: "));
    wait_for("the records to go", || {
        records(&repo, "fg").is_empty() && records(&repo, "quick").is_empty()
    });
}

#[test]
fn an_immediate_background_failure_is_reported_with_the_log() {
    let sandbox = Sandbox::new();
    let repo = sandbox.repo("api");
    repo.write_project_config(
        "scripts:\n  bad: {command: 'echo boom >&2; exit 4', background: true}\n",
    );
    let result = repo.wf(&["run", "bad"]);
    assert_eq!(result.code, 1);
    assert_eq!(result.err, "Error: script 'bad' exited with status 4 right after launch:\nboom\n");
}

#[test]
fn stop_ends_a_background_instance_and_waits_for_its_cleanup() {
    let sandbox = Sandbox::new();
    let repo = sandbox.repo("api");
    repo.write_project_config(
        "scripts:\n  srv:\n    command: sleep 30\n    cleanup: touch cleaned\n",
    );
    assert!(repo.wf(&["run", "-b", "srv"]).ok().err.contains("started 'srv' in the background"));
    let found = records(&repo, "srv");

    let stopped = repo.wf(&["stop", "srv"]).ok();
    assert!(stopped.err.starts_with("stopping 'srv' in 'api' (pgid "), "{}", stopped.err);
    assert!(!found[0].path.exists());
    assert!(repo.path.join("cleaned").exists());
    assert!(read(&sole_log(&repo, "srv")).contains("stopped by `wf stop` in 'api'"));
    assert_eq!(repo.wf(&["stop", "srv"]).err, "Error: 'srv' is not running in 'api'\n");
}

#[test]
fn instances_get_a_record_and_a_log_each_and_stop_ends_them_all() {
    let sandbox = Sandbox::new();
    let repo = sandbox.repo("api");
    let cleaned = sandbox.path().join("cleaned");
    repo.write_project_config(&format!(
        "scripts:\n  srv: {{command: 'sleep 30', background: true, cleanup: 'echo x >> {}'}}\n",
        cleaned.display()
    ));
    // what a dead instance left behind goes when the next one starts
    let dead = reaped_pid();
    let stale_record = write_record(&repo, "srv", &repo.path, dead, dead);
    let stale_log = jobs::log_path(&repo.common_dir(), "srv", &repo.path, dead);
    fs::create_dir_all(stale_log.parent().unwrap()).unwrap();
    fs::write(&stale_log, "output of a run that is long gone").unwrap();

    repo.wf(&["run", "srv"]).ok();
    repo.wf(&["run", "srv"]).ok();
    assert!(!stale_log.exists() && !stale_record.exists());
    let found = records(&repo, "srv");
    let owners: Vec<i32> = found.iter().map(|job| job.record.owner_pid).collect();
    assert_eq!(owners.len(), 2);
    assert_ne!(owners[0], owners[1]);
    for pid in &owners {
        assert!(jobs::log_path(&repo.common_dir(), "srv", &repo.path, *pid).is_file());
    }

    repo.wf(&["stop", "srv"]).ok(); // every instance here
    assert!(records(&repo, "srv").is_empty());
    assert_eq!(read(&cleaned), "x\nx\n"); // each instance ran its own cleanup
}

#[test]
fn stop_is_for_this_worktree_unless_all() {
    let sandbox = Sandbox::new();
    let repo = sandbox.repo("api");
    let feat = repo.create("feat");
    repo.write_project_config(
        "scripts:\n  x: {command: 'sleep 30', background: true, cleanup: 'touch cleaned'}\n",
    );
    repo.wf(&["run", "x"]).ok();
    sandbox.wf(&feat, &["run", "x"]).ok();
    assert_eq!(records(&repo, "x").len(), 2);

    sandbox.wf(&feat, &["stop", "x"]).ok();
    assert!(feat.join("cleaned").exists() && !repo.path.join("cleaned").exists());
    assert_eq!(records(&repo, "x").len(), 1); // main's instance untouched

    sandbox.wf(&feat, &["stop", "x", "--all"]).ok();
    assert!(repo.path.join("cleaned").exists());
    assert!(records(&repo, "x").is_empty());
}

// --- groups -------------------------------------------------------------------

#[test]
fn a_pipeline_runs_members_in_order_and_stops_at_the_first_failure() {
    let sandbox = Sandbox::new();
    let repo = sandbox.repo("api");
    let log = sandbox.path().join("steps");
    repo.write_project_config(&format!(
        "scripts:\n  a: echo a >> {0}\n  b: echo \"b $WF_BRANCH\" >> {0}; exit 3\n  c: echo c >> {0}\n\
         \x20 chain: {{pipeline: [a, b, c], cleanup: 'echo done >> {0}'}}\n  good: {{pipeline: [a, c]}}\n",
        log.display()
    ));
    let result = repo.wf(&["run", "chain"]);
    assert_eq!(result.code, 1);
    assert_eq!(read(&log), "a\nb main\ndone\n");
    assert!(
        result.err.starts_with(&format!("running 'chain' in {}: a, b, c\n", repo.path.display()))
    );
    assert!(
        result.err.contains("'chain' step 1/3: a\n")
            && result.err.contains("'chain' step 2/3: b\n")
    );
    assert!(!result.err.contains("step 3/3"));
    assert!(result.err.contains("Error: script 'b' failed with exit code 3\n"));
    assert_eq!(result.last_error(), "Error: script 'chain' failed with exit code 3");

    fs::remove_file(&log).unwrap();
    repo.wf(&["run", "good"]).ok();
    assert_eq!(read(&log), "a\nc\n");
    assert_eq!(
        repo.wf(&["run", "good", "extra"]).err,
        "Error: 'good' is a group of scripts and takes no arguments\n"
    );
}

#[test]
fn a_bulk_runs_members_at_once_with_prefixed_output() {
    let sandbox = Sandbox::new();
    let repo = sandbox.repo("api");
    let flag = sandbox.path().join("slow-started");
    repo.write_project_config(&format!(
        "scripts:\n  slow: touch {0}; echo slow-out; echo slow-err >&2; sleep 0.3\n\
         \x20 quick: while ! test -f {0}; do sleep 0.01; done; echo saw-slow\n  both: {{bulk: [slow, quick]}}\n",
        flag.display()
    ));
    // `quick` is done only once `slow` has started: proof that they overlap
    let started = Instant::now();
    let result = repo.wf(&["run", "both"]).ok();
    assert!(started.elapsed().as_secs() < 5);
    assert!(
        result
            .err
            .starts_with(&format!("running 'both' in {}: slow, quick\n", repo.path.display()))
    );
    for line in [
        "slow  | slow-out\n",
        "slow  | slow-err\n",
        "quick | saw-slow\n",
        "slow  | running 'slow' in ",
    ] {
        assert!(result.err.contains(line), "{line:?} in {}", result.err);
    }
}

#[test]
fn a_bulk_waits_for_all_and_reports_each_failure() {
    let sandbox = Sandbox::new();
    let repo = sandbox.repo("api");
    let log = sandbox.path().join("log");
    repo.write_project_config(&format!(
        "scripts:\n  bad: exit 3\n  worse: sleep 0.2; exit 4\n  fine: sleep 0.3; echo fine >> {}\n\
         \x20 all: {{bulk: [bad, worse, fine]}}\n",
        log.display()
    ));
    let result = repo.wf(&["run", "all"]);
    assert_eq!(result.code, 1);
    assert_eq!(read(&log), "fine\n"); // not cut short by the failures
    assert!(
        result.err.contains("bad   | Error: script 'bad' failed with exit code 3\n"),
        "{}",
        result.err
    );
    assert!(result.err.contains("Error: member 'bad' of 'all' failed with exit code 3\n"));
    assert!(result.err.contains("Error: member 'worse' of 'all' failed with exit code 4\n"));
    assert!(!result.err.contains("member 'fine'"));
    assert_eq!(result.last_error(), "Error: script 'all' failed with exit code 3");
}

#[test]
fn a_bulk_member_killed_or_interrupted_decides_how_the_group_ends() {
    let sandbox = Sandbox::new();
    let repo = sandbox.repo("api");
    repo.write_project_config(
        "scripts:\n  victim: kill -TERM 0\n  fine: 'true'\n  all: {bulk: [fine, victim]}\n\
         \x20 infra: exit 130\n  web: exit 3\n  dev: {bulk: [infra, web]}\n",
    );
    let killed = repo.wf(&["run", "all"]);
    assert_eq!(killed.code, 128 + SIGTERM);
    assert_eq!(killed.last_error(), "Error: script 'all' was killed by SIGTERM");

    // an interrupted member interrupts the group, and that is not an error
    let interrupted = repo.wf(&["run", "dev"]);
    assert_eq!(interrupted.code, -SIGINT);
    assert!(
        interrupted.err.contains("infra | script 'infra' was interrupted\n"),
        "{}",
        interrupted.err
    );
    assert!(interrupted.err.contains("web   | Error: script 'web' failed with exit code 3\n"));
    assert!(interrupted.err.contains("script 'dev' was interrupted\n"));
    assert!(!interrupted.err.contains("Error: member"));
}

#[test]
fn members_keep_their_own_records_while_the_group_runs() {
    let sandbox = Sandbox::new();
    let repo = sandbox.repo("api");
    let out = sandbox.path().join("seen");
    let running = repo.common_dir().join(jobs::RUNNING_SUBDIR);
    repo.write_project_config(&format!(
        "scripts:\n  m: test -n \"$(ls {0}/m)\" && test -n \"$(ls {0}/g)\" && echo yes > {1}\n  g: {{bulk: [m]}}\n",
        running.display(),
        out.display()
    ));
    repo.wf(&["run", "g"]).ok();
    assert_eq!(read(&out), "yes\n");
    assert!(records(&repo, "m").is_empty() && records(&repo, "g").is_empty());
}

#[test]
fn nested_groups() {
    let sandbox = Sandbox::new();
    let repo = sandbox.repo("api");
    let log = sandbox.path().join("log");
    repo.write_project_config(&format!(
        "scripts:\n  a: echo a >> {}\n  b: echo b-out\n  c: echo c-out\n  pair: {{bulk: [b, c]}}\n\
         \x20 chain: {{pipeline: [a, pair]}}\n",
        log.display()
    ));
    let result = repo.wf(&["run", "chain"]).ok();
    assert_eq!(read(&log), "a\n");
    assert!(result.err.contains("'chain' step 2/2: pair\n"));
    assert!(
        result.err.contains("b | b-out\n") && result.err.contains("c | c-out\n"),
        "{}",
        result.err
    );
}

#[test]
fn a_background_member_of_a_pipeline_is_started_and_left_running() {
    let sandbox = Sandbox::new();
    let repo = sandbox.repo("api");
    repo.write_project_config(
        "scripts:\n  srv: {command: 'sleep 30', background: true}\n  after: 'true'\n  chain: {pipeline: [srv, after]}\n",
    );
    repo.wf(&["run", "srv"]).ok();
    repo.wf(&["run", "chain"]).ok();
    // a member already running is started again
    let found = records(&repo, "srv");
    assert_eq!(found.len(), 2);
    assert!(found.iter().all(|job| jobs::classify(&job.record) == jobs::JobState::Live));
    assert!(records(&repo, "chain").is_empty());
    repo.wf(&["stop", "srv"]).ok(); // both of them
    assert!(records(&repo, "srv").is_empty());
}

#[test]
fn stopping_a_group_stops_its_members_and_runs_every_cleanup() {
    let sandbox = Sandbox::new();
    let repo = sandbox.repo("api");
    let cleaned = sandbox.path().join("cleaned");
    repo.write_project_config(&format!(
        "scripts:\n  s1: {{command: 'sleep 30', cleanup: 'echo s1 >> {0}'}}\n  s2: {{command: 'sleep 30', cleanup: 'echo s2 >> {0}'}}\n\
         \x20 servers: {{bulk: [s1, s2], background: true, cleanup: 'echo servers >> {0}'}}\n",
        cleaned.display()
    ));
    repo.wf(&["run", "servers"]).ok();
    wait_for("both members to start", || {
        records(&repo, "s1").len() + records(&repo, "s2").len() == 2
    });

    repo.wf(&["stop", "servers"]).ok();

    let mut lines: Vec<String> = read(&cleaned).lines().map(str::to_string).collect();
    assert_eq!(lines.last().map(String::as_str), Some("servers")); // the group's own cleanup comes last
    lines.sort();
    assert_eq!(lines, ["s1", "s2", "servers"]);
    for name in ["s1", "s2", "servers"] {
        assert!(records(&repo, name).is_empty(), "{name}");
    }
    let log = read(&sole_log(&repo, "servers"));
    assert!(log.contains("s1 | Error: script 's1' was killed by SIGTERM"), "{log}");
    assert!(log.contains("Error: member 's2' of 'servers' was killed by SIGTERM"));
    assert!(log.contains("stopped by `wf stop` in 'api'"));
}

#[test]
fn stopping_one_member_leaves_the_rest_of_a_running_bulk() {
    let sandbox = Sandbox::new();
    let repo = sandbox.repo("api");
    let cleaned = sandbox.path().join("cleaned");
    repo.write_project_config(&format!(
        "scripts:\n  s1: {{command: 'sleep 30', cleanup: 'echo s1 >> {}'}}\n  s2: sleep 0.5\n\
         \x20 servers: {{bulk: [s1, s2], background: true}}\n",
        cleaned.display()
    ));
    repo.wf(&["run", "servers"]).ok();
    wait_for("s1 to start", || !records(&repo, "s1").is_empty());

    repo.wf(&["stop", "s1"]).ok();

    assert_eq!(read(&cleaned), "s1\n");
    wait_for("the group to end", || records(&repo, "servers").is_empty()); // s2 ends on its own
    let log = read(&sole_log(&repo, "servers"));
    assert!(
        log.contains(
            "s1 | Error: script 's1' was killed by SIGTERM (stopped by `wf stop` in 'api')"
        ),
        "{log}"
    );
    assert!(!log.contains("member 's2'"));
}

// --- make ---------------------------------------------------------------------

fn have_make() -> bool {
    Command::new("make").arg("--version").output().is_ok()
}

#[test]
fn make_targets_run_like_scripts() {
    if !have_make() {
        return;
    }
    let sandbox = Sandbox::new();
    let repo = sandbox.repo("api");
    let none = repo.wf(&["make", "hello"]);
    assert_eq!(none.code, 1);
    assert!(none.err.starts_with("Error: no makefile in "), "{}", none.err);

    fs::write(
        repo.path.join("Makefile"),
        "hello:\n\t@echo hello-from-make\nshow:\n\t@echo goal=$(GOAL)\nboom:\n\t@exit 3\n",
    )
    .unwrap();
    // at the worktree root, whatever directory we stand in
    fs::create_dir(repo.path.join("sub")).unwrap();
    let hello = sandbox.wf(&repo.path.join("sub"), &["make", "hello"]).ok();
    assert!(
        hello.err.contains("running 'make:hello' in ") && hello.err.contains("hello-from-make\n")
    );
    assert!(repo.wf(&["make", "show", "GOAL=yes"]).ok().err.contains("goal=yes\n"));
    // reported exactly as a failing `wf run` command is
    let boom = repo.wf(&["make", "boom"]);
    assert_eq!(boom.code, 1);
    assert_eq!(boom.last_error(), "Error: script 'make:boom' failed with exit code 2");
    assert_ne!(repo.wf(&["make", "nope"]).code, 0); // an unknown target is make's to reject

    assert_eq!(
        repo.wf(&["stop", "--make", "hello"]).err,
        "Error: 'make:hello' is not running in 'api'\n"
    );
    assert_eq!(repo.wf(&["--complete", "make"]).out, "hello\nshow\nboom\n");
}

#[test]
fn hiding_make_targets_keeps_them_runnable() {
    if !have_make() {
        return;
    }
    let sandbox = Sandbox::new();
    let repo = sandbox.repo("api");
    fs::write(repo.path.join("Makefile"), "build:\n\t@echo b\ntest:\n\t@echo t\n").unwrap();
    repo.write_project_config("make:\n  hide_scripts: [test]\n");
    assert_eq!(repo.wf(&["--complete", "make"]).out, "build\n");
    repo.write_project_config("make:\n  hidden: true\n");
    assert_eq!(repo.wf(&["--complete", "make"]).out, "");
    repo.wf(&["make", "test"]).ok();
}
