//! hooks: symlinks + git-status invisibility, setup scripts, and the pure
//! side of named scripts. Running them — process groups, supervisors,
//! `wf stop` — is tested through the binary (tests/scripts.rs): signal
//! handling and the terminal hand-over belong to a process of their own.

use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::time::Instant;

use super::run::{pump, test_runner};
use super::*;
use crate::config::Number;
use crate::output::capture;
use crate::testing::{Repo, Sandbox};

const SIGTERM: i32 = Signal::SIGTERM as i32;

fn make_worktree(sandbox: &Sandbox, repo: &Repo) -> PathBuf {
    let target = sandbox.path().join("dev").join("worktrees").join("api").join("feat");
    git::worktree_add(&repo.path, &target, "feat", None).unwrap();
    target
}

fn base_env() -> Env {
    [("SHELL", "/bin/sh"), ("PATH", "/usr/local/bin:/usr/bin:/bin")]
        .into_iter()
        .map(|(name, value)| (name.into(), value.into()))
        .collect()
}

fn text(env: &Env, name: &str) -> String {
    env[std::ffi::OsStr::new(name)].to_string_lossy().into_owned()
}

fn symlinks(paths: &[&str]) -> Config {
    Config { symlinks: paths.iter().map(|path| path.to_string()).collect(), ..Config::default() }
}

fn scripts(pairs: &[(&str, ScriptSpec)]) -> Config {
    Config {
        scripts: pairs.iter().map(|(name, spec)| (name.to_string(), spec.clone())).collect(),
        ..Config::default()
    }
}

fn names(items: &[&str]) -> Vec<String> {
    items.iter().map(|item| item.to_string()).collect()
}

// --- the script environment ---------------------------------------------------

#[test]
fn script_env_is_ours_plus_the_wf_family() {
    let env = script_env(
        &base_env(),
        Path::new("/t/api"),
        Path::new("/t/wt/feat"),
        Path::new("/t/wt"),
        Some("feature/x"),
    );
    assert_eq!(text(&env, "WF_MAIN"), "/t/api");
    assert_eq!(text(&env, "WF_NAME"), "api");
    assert_eq!(text(&env, "WF_WORKTREE"), "/t/wt/feat");
    assert_eq!(text(&env, "WF_WORKTREES_DIR"), "/t/wt");
    assert_eq!(text(&env, "WF_BRANCH"), "feature/x");
    assert_eq!(text(&env, "SHELL"), "/bin/sh");
    assert_eq!(env.len(), base_env().len() + 5);
    let detached = script_env(&Env::new(), Path::new("/t"), Path::new("/t"), Path::new("/t"), None);
    assert_eq!(text(&detached, "WF_BRANCH"), "");
}

// --- symlinks -----------------------------------------------------------------

#[test]
fn creates_links_and_hides_them_from_git() {
    let sandbox = Sandbox::new();
    let repo = sandbox.repo("api");
    fs::create_dir(repo.path.join("node_modules")).unwrap();
    fs::write(repo.path.join(".env"), "SECRET=1\n").unwrap();
    let worktree = make_worktree(&sandbox, &repo);

    let (created, shown) = capture(|| {
        create_symlinks(&symlinks(&["node_modules", "/.env/", "/"]), &repo.path, &worktree)
    });

    assert_eq!(created.unwrap(), ["node_modules", ".env"]);
    assert!(worktree.join("node_modules").is_symlink());
    assert_eq!(
        fs::read_link(worktree.join("node_modules")).unwrap(),
        repo.path.join("node_modules")
    );
    assert_eq!(fs::read_to_string(worktree.join(".env")).unwrap(), "SECRET=1\n");
    assert!(shown.contains(&format!("symlinked .env -> {}", repo.path.join(".env").display())));
    assert!(shown.contains("excluded 2 symlink(s) from git in this worktree"), "{shown}");
    // invisible to git status in the worktree...
    assert_eq!(git::status_porcelain(&worktree).unwrap(), "");
    // ...but a plain untracked file still shows up
    repo.make_dirty(&worktree);
    assert!(git::status_porcelain(&worktree).unwrap().contains("dirty.txt"));
    // .env is untracked in main and must stay visible there
    assert!(git::status_porcelain(&repo.path).unwrap().contains(".env"));
}

#[test]
fn missing_source_skipped() {
    let sandbox = Sandbox::new();
    let repo = sandbox.repo("api");
    let worktree = make_worktree(&sandbox, &repo);
    let (created, shown) =
        capture(|| create_symlinks(&symlinks(&["does-not-exist"]), &repo.path, &worktree));
    assert!(created.unwrap().is_empty());
    assert!(shown.contains("symlink source does not exist, skipping: "), "{shown}");
    assert!(!shown.contains("excluded"));
}

#[test]
fn existing_file_not_clobbered_and_existing_symlink_replaced() {
    let sandbox = Sandbox::new();
    let repo = sandbox.repo("api");
    fs::write(repo.path.join(".env"), "main\n").unwrap();
    fs::write(repo.path.join("link"), "x\n").unwrap();
    let worktree = make_worktree(&sandbox, &repo);
    fs::write(worktree.join(".env"), "precious\n").unwrap();
    symlink(repo.path.join("README.md"), worktree.join("link")).unwrap();

    let (created, shown) =
        capture(|| create_symlinks(&symlinks(&[".env", "link"]), &repo.path, &worktree));

    assert_eq!(created.unwrap(), ["link"]);
    assert_eq!(fs::read_to_string(worktree.join(".env")).unwrap(), "precious\n");
    assert!(shown.contains("destination exists and is not a symlink, skipping: "), "{shown}");
    assert_eq!(fs::read_link(worktree.join("link")).unwrap(), repo.path.join("link"));
}

#[test]
fn nested_path_creates_parents() {
    let sandbox = Sandbox::new();
    let repo = sandbox.repo("api");
    fs::create_dir(repo.path.join(".vscode")).unwrap();
    fs::write(repo.path.join(".vscode/settings.json"), "{}\n").unwrap();
    let worktree = make_worktree(&sandbox, &repo);
    let (created, _) =
        capture(|| create_symlinks(&symlinks(&[".vscode/settings.json"]), &repo.path, &worktree));
    assert_eq!(created.unwrap(), [".vscode/settings.json"]);
    assert!(worktree.join(".vscode/settings.json").is_symlink());
    assert_eq!(git::status_porcelain(&worktree).unwrap(), "");
}

#[test]
fn the_exclude_file_lists_the_links_under_a_header() {
    let sandbox = Sandbox::new();
    let repo = sandbox.repo("api");
    let worktree = make_worktree(&sandbox, &repo);
    capture(|| exclude_from_git(&worktree, &names(&["a", "b/c"]))).0.unwrap();
    let file = git::git_dir(&worktree).unwrap().join(EXCLUDE_FILE_NAME);
    let text = fs::read_to_string(file).unwrap();
    assert!(text.starts_with("# Managed by workforest: symlinks from the `symlinks` config key\n"));
    assert!(text.ends_with("/a\n/b/c\n"), "{text}");
}

// --- snippets and setup scripts -----------------------------------------------

fn env_for(repo: &Repo, worktree: &Path, branch: &str) -> Env {
    script_env(&base_env(), &repo.path, worktree, worktree.parent().unwrap(), Some(branch))
}

#[test]
fn setup_scripts_run_in_the_worktree_with_the_env() {
    let sandbox = Sandbox::new();
    let repo = sandbox.repo("api");
    let worktree = make_worktree(&sandbox, &repo);
    let out = sandbox.path().join("hook-out.txt");
    let config = Config {
        setup_scripts: vec![format!("echo \"$WF_BRANCH in $PWD\" > {}", out.display())],
        ..Config::default()
    };
    let (failures, shown) =
        capture(|| run_setup_scripts(&config, &worktree, &env_for(&repo, &worktree, "feat")));
    assert_eq!(failures, Ok(0));
    assert_eq!(fs::read_to_string(&out).unwrap(), format!("feat in {}\n", worktree.display()));
    assert!(shown.starts_with("running setup script: echo "), "{shown}");
}

#[test]
fn a_failing_setup_script_warns_but_the_rest_run() {
    let sandbox = Sandbox::new();
    let repo = sandbox.repo("api");
    let out = sandbox.path().join("second.txt");
    let config = Config {
        setup_scripts: vec!["exit 7".into(), format!("touch {}", out.display())],
        ..Config::default()
    };
    let (failures, shown) =
        capture(|| run_setup_scripts(&config, &repo.path, &env_for(&repo, &repo.path, "main")));
    assert_eq!(failures, Ok(1));
    assert!(out.exists()); // the second script still ran
    assert!(shown.contains("setup script failed: exit 7"), "{shown}");
}

#[test]
fn a_snippet_reports_its_status_or_its_signal() {
    let sandbox = Sandbox::new();
    let env = base_env();
    assert_eq!(run_snippet("exit 0", sandbox.path(), &env), Ok(0));
    assert_eq!(run_snippet("exit 9", sandbox.path(), &env), Ok(9));
    assert_eq!(run_snippet("kill -TERM $$", sandbox.path(), &env), Ok(-SIGTERM));
    let mut broken = env.clone();
    broken.insert("SHELL".into(), "/nonexistent/sh".into());
    assert_eq!(
        run_snippet("true", sandbox.path(), &broken).unwrap_err().message,
        "cannot run 'true' via $SHELL ('/nonexistent/sh'): No such file or directory"
    );
}

#[test]
fn cleanup_runs_only_when_there_is_one_and_only_warns_on_failure() {
    let sandbox = Sandbox::new();
    let env = base_env();
    let none = ScriptSpec::command("true");
    assert_eq!(capture(|| run_cleanup(&none, "x", sandbox.path(), &env)), (Ok(()), String::new()));
    let failing = ScriptSpec { cleanup: Some("exit 7".into()), ..ScriptSpec::command("true") };
    let (result, shown) = capture(|| run_cleanup(&failing, "x", sandbox.path(), &env));
    assert_eq!(result, Ok(()));
    assert_eq!(shown, "running cleanup for 'x': exit 7\ncleanup for 'x' failed with exit code 7\n");
}

// --- outcomes -----------------------------------------------------------------

fn result(code: i32, stopped_by: Option<&str>) -> JobResult {
    JobResult { code, stopped_by: stopped_by.map(str::to_string) }
}

#[test]
fn failures_say_how_the_script_ended() {
    assert_eq!(failure(&result(0, None), "x"), None);
    assert_eq!(failure(&result(130, None), "x"), None);
    assert_eq!(failure(&result(-SIGINT, None), "x"), None);
    assert_eq!(failure(&result(3, None), "x").unwrap(), "script 'x' failed with exit code 3");
    assert_eq!(failure(&result(-SIGTERM, None), "x").unwrap(), "script 'x' was killed by SIGTERM");
    assert_eq!(
        failure(&result(-SIGTERM, Some("`wf stop` in 'api'")), "x").unwrap(),
        "script 'x' was killed by SIGTERM (stopped by `wf stop` in 'api')"
    );
    // who stopped it matters only when a signal did
    assert_eq!(
        failure(&result(1, Some("someone")), "x").unwrap(),
        "script 'x' failed with exit code 1"
    );
}

#[test]
fn raise_for_maps_outcomes_to_errors() {
    assert_eq!(raise_for(&result(0, None), "x"), Ok(()));
    let failed = raise_for(&result(3, None), "x").unwrap_err();
    assert_eq!((failed.kind, failed.exit_code()), (ErrorKind::Error, 1));
    let killed = raise_for(&result(-SIGTERM, None), "x").unwrap_err();
    assert_eq!(killed.kind, ErrorKind::ScriptKilled(SIGTERM));
    assert_eq!(killed.exit_code(), 128 + SIGTERM);
    // Ctrl-C is a warning, not an error
    for code in [130, -SIGINT] {
        let (outcome, shown) = capture(|| raise_for(&result(code, None), "x"));
        assert_eq!(outcome.unwrap_err().kind, ErrorKind::Interrupted);
        assert_eq!(shown, "script 'x' was interrupted\n");
    }
}

#[test]
fn log_tail_is_the_last_lines_or_nothing() {
    let sandbox = Sandbox::new();
    let log = sandbox.path().join("x.log");
    assert_eq!(log_tail(&log, 10), "");
    fs::write(&log, "\n1\n2\n3\n\n").unwrap();
    assert_eq!(log_tail(&log, 2), "2\n3");
    assert_eq!(log_tail(&log, 10), "1\n2\n3");
}

// --- resolving scripts --------------------------------------------------------

#[test]
fn unknown_script_lists_the_available_ones() {
    let config = scripts(&[("b", ScriptSpec::command("true")), ("a", ScriptSpec::command("true"))]);
    assert_eq!(
        resolve_script(&config, "nope").unwrap_err().message,
        "no script named 'nope' (available: a, b)"
    );
    assert_eq!(
        resolve_script(&Config::default(), "nope").unwrap_err().message,
        "no script named 'nope' (available: none defined)"
    );
}

#[test]
fn a_make_name_is_a_synthetic_script_unless_configured() {
    let config = scripts(&[("make:own", ScriptSpec::command("echo mine"))]);
    assert_eq!(
        resolve_script(&config, "make:check").unwrap().command.as_deref(),
        Some("make check")
    );
    assert_eq!(resolve_script(&config, "make:own").unwrap().command.as_deref(), Some("echo mine"));
    assert!(resolve_script(&config, "make:").is_err());
}

#[test]
fn stop_timeout_of_a_group_is_its_members_longest() {
    let with_timeout = |seconds: i64| ScriptSpec {
        stop_timeout: Some(Number::Int(seconds)),
        ..ScriptSpec::command("x")
    };
    let bulk =
        |members: &[&str]| ScriptSpec { bulk: Some(names(members)), ..ScriptSpec::default() };
    let config = Config {
        stop_timeout: Number::Int(3),
        ..scripts(&[
            ("a", with_timeout(5)),
            ("b", ScriptSpec::command("x")),
            ("inner", bulk(&["a", "b"])),
            ("outer", ScriptSpec { pipeline: Some(names(&["inner"])), ..ScriptSpec::default() }),
            ("own", ScriptSpec { stop_timeout: Some(Number::Int(2)), ..bulk(&["a"]) }),
            ("loose", bulk(&["gone"])), // a hand-built Config may dangle
        ])
    };
    let timeout = |name: &str| stop_timeout(&config, &config.scripts[name]);
    assert_eq!(timeout("b"), 3.0);
    assert_eq!(timeout("inner"), 5.0);
    assert_eq!(timeout("outer"), 5.0);
    assert_eq!(timeout("own"), 2.0);
    assert_eq!(timeout("loose"), 3.0);
}

#[test]
fn preparing_appends_quoted_arguments_and_refuses_them_for_a_group() {
    let sandbox = Sandbox::new();
    let repo = sandbox.repo("api");
    let group = ScriptSpec { bulk: Some(names(&["a"])), ..ScriptSpec::default() };
    let config = scripts(&[("a", ScriptSpec::command("echo")), ("g", group)]);
    let env = base_env();
    let job =
        prepare(&config, "a", &repo.path, &env, &names(&["check", "-j2", "a b"]), true).unwrap();
    assert_eq!(job.snippet, "echo check -j2 'a b'");
    assert_eq!(
        start_message(&job),
        format!("running 'a' in {}: echo check -j2 'a b'", repo.path.display())
    );
    assert_eq!(job.record_path(7), repo.path.join(".git/workforest/running/a/api.7"));
    assert_eq!(job.log_path(7), repo.path.join(".git/workforest/logs/a/api.7.log"));

    let plain = prepare(&config, "g", &repo.path, &env, &[], true).unwrap();
    assert_eq!(start_message(&plain), format!("running 'g' in {}: a", repo.path.display()));
    assert_eq!(
        prepare(&config, "g", &repo.path, &env, &names(&["x"]), true).unwrap_err().message,
        "'g' is a group of scripts and takes no arguments"
    );
    assert!(prepare(&config, "nope", &repo.path, &env, &[], true).is_err());
}

#[test]
fn stop_needs_a_known_script_that_is_running() {
    let sandbox = Sandbox::new();
    let repo = sandbox.repo("api");
    let config = scripts(&[("x", ScriptSpec::command("true"))]);
    let env = base_env();
    let message = |name: &str, everywhere: bool| {
        stop_script(&config, name, &repo.path, &env, everywhere).unwrap_err().message
    };
    assert_eq!(message("nope", false), "no script named 'nope' (available: x)");
    assert_eq!(message("x", false), "'x' is not running in 'api'");
    assert_eq!(message("x", true), "'x' is not running anywhere in this project");
    assert!(running_scripts(&repo.path).unwrap().is_empty());
}

// --- the prefixer -------------------------------------------------------------

#[test]
fn prefixer_pads_to_the_longest_name() {
    let mut prefixer = Prefixer::new(&names(&["api", "frontend"]), false);
    assert_eq!(prefixer.feed("api", b"hello\n"), "api      | hello\n");
    assert_eq!(prefixer.feed("frontend", b"hi\n"), "frontend | hi\n");
}

#[test]
fn prefixer_keeps_partial_lines_until_they_complete() {
    let mut prefixer = Prefixer::new(&names(&["a"]), false);
    assert_eq!(prefixer.feed("a", b"one\ntw"), "a | one\n");
    assert_eq!(prefixer.feed("a", b"o\nthree"), "a | two\n");
    assert_eq!(prefixer.flush("a"), "a | three\n");
    assert_eq!(prefixer.flush("a"), "");
}

#[test]
fn prefixer_pty_line_endings_and_undecodable_bytes() {
    let mut prefixer = Prefixer::new(&names(&["a"]), false);
    let replacement = char::REPLACEMENT_CHARACTER;
    assert_eq!(
        prefixer.feed("a", b"crlf\r\nraw \xff\n"),
        format!("a | crlf\na | raw {replacement}\n")
    );
}

#[test]
fn prefixer_colors_only_the_prefix() {
    let mut prefixer = Prefixer::new(&names(&["a", "b"]), true);
    assert_eq!(prefixer.feed("a", b"x\n"), "\x1b[36ma | \x1b[0mx\n");
    assert_eq!(prefixer.feed("b", b"x\n"), "\x1b[35mb | \x1b[0mx\n");
}

// --- the pump -----------------------------------------------------------------

fn pumped(runners: &mut [Runner]) -> String {
    let labels: Vec<String> = runners.iter().map(|runner| runner.name.clone()).collect();
    let mut sink = Vec::new();
    let mut rounds = 0;
    pump(runners, &mut Prefixer::new(&labels, false), &mut sink, &mut |_| rounds += 1);
    assert!(rounds > 0);
    String::from_utf8(sink).unwrap()
}

#[test]
fn pump_relays_every_runner_until_all_have_ended() {
    let mut runners = [
        test_runner("a", "echo a1; sleep 0.1; echo a2; exit 3", false),
        test_runner("b", "printf 'b-no-newline'", false),
    ];
    let relayed = pumped(&mut runners);
    let mut lines: Vec<&str> = relayed.lines().collect();
    lines.sort_unstable();
    assert_eq!(lines, ["a | a1", "a | a2", "b | b-no-newline"]);
    assert_eq!(runners.iter().map(|runner| runner.code).collect::<Vec<_>>(), [Some(3), Some(0)]);
    assert!(runners.iter().all(|runner| runner.fd < 0));
}

#[test]
fn a_daemon_holding_the_channel_does_not_hold_the_pump() {
    let mut runners = [test_runner("a", "sleep 5 & echo hi", false)];
    let started = Instant::now();
    assert_eq!(pumped(&mut runners), "a | hi\n");
    assert!(started.elapsed().as_secs() < 2);
}

#[test]
fn a_pty_channel_gives_the_member_a_terminal_and_a_pipe_does_not() {
    let probe = "test -t 1 && echo tty || echo no-tty";
    assert_eq!(pumped(&mut [test_runner("a", probe, true)]), "a | tty\n");
    assert_eq!(pumped(&mut [test_runner("a", probe, false)]), "a | no-tty\n");
}

#[test]
fn a_runner_killed_by_a_signal_has_a_negative_outcome() {
    let mut runners = [test_runner("a", "kill -TERM $$", false)];
    pumped(&mut runners);
    assert_eq!(runners[0].code, Some(-SIGTERM));
}

// --- the bulk outcome ---------------------------------------------------------

fn outcome(codes: &[i32]) -> (i32, String) {
    let runners: Vec<Runner> = codes
        .iter()
        .enumerate()
        .map(|(index, code)| Runner {
            name: format!("m{index}"),
            pid: 0,
            fd: -1,
            code: Some(*code),
        })
        .collect();
    capture(|| bulk_outcome("g", &runners))
}

#[test]
fn bulk_all_good() {
    assert_eq!(outcome(&[0, 0]), (0, String::new()));
}

#[test]
fn bulk_first_failure_wins_by_default() {
    let (code, shown) = outcome(&[0, 4, 3]);
    assert_eq!(code, 4);
    assert_eq!(
        shown,
        "Error: member 'm1' of 'g' failed with exit code 4\nError: member 'm2' of 'g' failed with exit code 3\n"
    );
}

#[test]
fn bulk_signal_deaths_win_over_statuses() {
    let (code, shown) = outcome(&[4, -SIGTERM]);
    assert_eq!(code, -SIGTERM);
    assert!(shown.contains("Error: member 'm1' of 'g' was killed by SIGTERM"), "{shown}");
}

#[test]
fn bulk_an_interruption_wins_and_is_not_an_error() {
    for codes in [&[4, -SIGTERM, -SIGINT][..], &[4, 130][..]] {
        assert_eq!(outcome(codes), (-SIGINT, String::new()));
    }
}
