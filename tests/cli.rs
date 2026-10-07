//! cli: the stdout/stderr contract, exit codes, argument handling, and the
//! machine output the editor plugins read — through the built binary.

mod common;

use std::fs;

use common::{DIRECTIVE, Recorder, Sandbox, Terminal};
use serde_json::{Value, json};

const KNOWN_KEYS: &str = "make, opener, openers, scripts, setup_scripts, stop_timeout, symlinks, worktrees_dir, wrappers";

// --- basics -----------------------------------------------------------------

#[test]
fn version_help_and_usage_errors() {
    let sandbox = Sandbox::new();
    let repo = sandbox.repo("api");
    let version = repo.wf(&["--version"]).ok();
    assert_eq!(version.out, format!("workforest {}\n", env!("CARGO_PKG_VERSION")));
    assert_eq!(version.err, "");

    let help = repo.wf(&["--help"]).ok();
    assert!(help.out.contains("Git worktree forest management") && help.err.is_empty());
    assert!(repo.wf(&["create", "--help"]).ok().out.contains("--no-hooks"));

    // missing NAME; an unknown word; exclusive formats: nothing on stdout
    for args in [&["delete"][..], &["mytool", "feat"], &["list", "--json", "--porcelain"]] {
        let result = repo.wf(args);
        assert_eq!((result.code, result.out.as_str()), (2, ""), "{args:?}: {}", result.err);
        assert!(!result.err.is_empty());
    }
}

#[test]
fn wf_is_the_same_program() {
    let sandbox = Sandbox::new();
    let run = sandbox.command(common::WF_BINARY).arg("--version").output().unwrap();
    assert!(run.status.success());
    assert_eq!(
        String::from_utf8_lossy(&run.stdout),
        format!("workforest {}\n", env!("CARGO_PKG_VERSION"))
    );
}

#[test]
fn outside_a_repository_is_an_operational_error() {
    let sandbox = Sandbox::new();
    let result = sandbox.wf(sandbox.path(), &["list"]);
    assert_eq!((result.code, result.out.as_str()), (1, ""));
    assert_eq!(result.err, "Error: Not inside a git repository\n");
}

#[test]
fn a_config_error_is_exit_4() {
    let sandbox = Sandbox::new();
    let repo = sandbox.repo("api");
    let path = repo.write_project_config("opener: 1\n");
    let result = repo.wf(&["list"]);
    assert_eq!(result.code, 4);
    assert_eq!(
        result.err,
        format!("Error: {}: 'opener' must be a string, got int\n", path.display())
    );
    // a malformed section, too
    for (text, message) in [
        ("make: []\n", "'make' must be a mapping"),
        ("make:\n  hidden: yes please\n", "make.hidden must be true or false"),
        ("make:\n  hide_scripts: check\n", "make.hide_scripts must be a list of strings"),
        ("make:\n  show_scripts: [1]\n", "make.show_scripts must be a list of strings"),
    ] {
        repo.write_project_config(text);
        let result = repo.wf(&["config"]);
        assert_eq!(result.code, 4, "{text}");
        assert!(result.err.contains(message), "{}", result.err);
    }
}

#[test]
fn debug_mode_adds_the_error_kind() {
    let sandbox = Sandbox::new();
    let result = sandbox.wf_env(sandbox.path(), &["list"], &[("WORKFOREST_DEBUG", "1")]);
    assert_eq!(result.code, 1);
    assert!(result.err.contains("NotARepo"), "{}", result.err);
    assert_eq!(result.last_error(), "Error: Not inside a git repository");
}

#[test]
fn colors_only_when_asked_for() {
    let sandbox = Sandbox::new();
    let forced =
        sandbox.wf_env(sandbox.path(), &["list"], &[("NO_COLOR", ""), ("CLICOLOR_FORCE", "1")]);
    assert_eq!(forced.err, "\x1b[0;31mError:\x1b[0m Not inside a git repository\n");
    let repo = sandbox.repo("api");
    let colored = sandbox.wf_env(
        &repo.path,
        &["create", "feat", "--no-open"],
        &[("NO_COLOR", ""), ("CLICOLOR_FORCE", "1")],
    );
    assert!(colored.err.starts_with("\x1b[0;32mcreated worktree for 'feat'"), "{:?}", colored.err);
    repo.write_project_config("bogus: 1\n");
    let warned = sandbox.wf_env(
        &repo.path,
        &["list", "--porcelain"],
        &[("NO_COLOR", ""), ("CLICOLOR_FORCE", "1")],
    );
    assert!(
        warned.err.starts_with("\x1b[0;33m") && warned.err.ends_with("\x1b[0m\n"),
        "{:?}",
        warned.err
    );
}

// --- unknown config keys ----------------------------------------------------

// An unknown key warns on stderr and the command carries on; stdout — the
// cd protocol and the machine output the editors parse — stays as it is
// without the key.
const UNKNOWN: &str =
    "bogus_key: 1\nscripts:\n  test: {command: 'true', colour: red}\nmake:\n  nope: true\n";

#[test]
fn an_unknown_key_warns_and_the_command_succeeds() {
    let sandbox = Sandbox::new();
    let repo = sandbox.repo("api");
    let path = repo.write_project_config("bogus_key: 1\n");
    let result = repo.wf(&["list"]).ok();
    assert_eq!(result.out, "");
    assert_eq!(
        result.err.lines().next().unwrap(),
        format!("{}: unknown key 'bogus_key', ignored (known keys: {KNOWN_KEYS})", path.display())
    );
    assert!(!result.err.contains("Error"));
}

#[test]
fn machine_output_stays_clean_with_unknown_keys() {
    let sandbox = Sandbox::new();
    let repo = sandbox.repo("api");
    repo.write_project_config("scripts:\n  test: 'true'\n");
    let clean = repo.wf(&["list", "--json"]).ok();
    assert_eq!(clean.err, "");
    repo.write_project_config(UNKNOWN);

    let listed = repo.wf(&["list", "--json"]).ok();
    assert_eq!(listed.out, clean.out);
    assert_eq!(listed.err.matches("unknown key").count(), 3);

    let shown = repo.wf(&["config", "--json"]).ok();
    let config: Value = serde_json::from_str(&shown.out).unwrap();
    assert!(config["config"].get("bogus_key").is_none());
    assert_eq!(config["config"]["scripts"], json!({"test": "true"}));
    assert!(config["config"]["make"].get("nope").is_none());
    assert_eq!(shown.err.matches("unknown key").count(), 3);

    let created = repo.wf(&["create", "feat"]).ok();
    assert_eq!(created.out.lines().count(), 1);
    assert!(
        created.out.starts_with(&format!("{DIRECTIVE}cd ")) && !created.out.contains("unknown key")
    );

    // completion says nothing: its stderr lands in the line being typed
    let completed = repo.wf(&["--complete", "scripts"]).ok();
    assert_eq!((completed.out.as_str(), completed.err.as_str()), ("test\n", ""));
}

#[test]
fn an_error_stays_the_last_line() {
    // The editors show the last stderr line of a failed run.
    let sandbox = Sandbox::new();
    let repo = sandbox.repo("api");
    repo.write_project_config("bogus_key: 1\nopener: 1\n");
    let result = repo.wf(&["list", "--json"]);
    assert_eq!((result.code, result.out.as_str()), (4, ""));
    assert!(result.err.lines().next().unwrap().contains("unknown key 'bogus_key'"));
    assert!(
        result.last_error().starts_with("Error: ")
            && result.last_error().contains("'opener' must be a string")
    );
}

// --- the stdout contract ----------------------------------------------------

#[test]
fn create_emits_only_the_directive() {
    let sandbox = Sandbox::new();
    let repo = sandbox.repo("api");
    let result = repo.wf(&["create", "feat"]).ok();
    let lines: Vec<&str> = result.out.lines().collect();
    assert_eq!(lines.len(), 1);
    let worktree = repo.worktrees_dir().join("feat");
    assert!(lines[0].starts_with(&format!("{DIRECTIVE}cd {} && WF_MAIN=", worktree.display())));
    assert!(lines[0].ends_with(" /bin/sh -c stub-editor"));
    assert_eq!(result.err, format!("created worktree for 'feat' at {}\n", worktree.display()));

    assert_eq!(repo.wf(&["create", "other", "--no-open"]).ok().out, "");
}

#[test]
fn listings_and_dumps_are_data_on_stdout() {
    let sandbox = Sandbox::new();
    let repo = sandbox.repo("api");
    let worktree = repo.create("feat");
    assert_eq!(
        repo.wf(&["list", "--porcelain"]).ok().out,
        format!("feat\tfeat\t{}\t0\t\t\n", worktree.display())
    );
    assert_eq!(repo.wf(&["list"]).ok().out, format!("feat  feat  clean  {}\n", worktree.display()));
    let forest: Value = serde_json::from_str(&repo.wf(&["list", "--json"]).ok().out).unwrap();
    assert_eq!(forest["main"]["path"], repo.path.to_str().unwrap());
    assert_eq!(forest["worktrees"][0]["name"], "feat");

    let dump = repo.wf(&["config"]).ok().out;
    assert!(dump.starts_with("worktrees_dir: $WF_MAIN/../worktrees/$WF_NAME\n"));
    assert!(dump.ends_with("# sources (low -> high):\n#   (built-in defaults only)\n"));

    let checkout = repo.wf(&["checkout", "feat"]).ok();
    assert_eq!(checkout.out, format!("{DIRECTIVE}cd {}\n", repo.path.display()));
}

#[test]
fn config_names_its_sources_in_order() {
    let sandbox = Sandbox::new();
    let repo = sandbox.repo("api");
    let user = sandbox.write_user_config("opener: user\n");
    let project = repo.write_project_config("opener: proj\nmake:\n  exclusive_scripts: [dev]\n");
    let shown: Value = serde_json::from_str(&repo.wf(&["config", "--json"]).ok().out).unwrap();
    assert_eq!(shown["config"]["opener"], "proj");
    assert_eq!(
        shown["sources"],
        json!([{"layer": "user", "path": user}, {"layer": "project", "path": project}])
    );
    let dump = repo.wf(&["config"]).ok().out;
    assert!(dump.contains("  exclusive_scripts:\n  - dev\n"), "{dump}");
    assert!(dump.ends_with(&format!(
        "#   user: {}\n#   project: {}\n",
        user.display(),
        project.display()
    )));
    // outside a repository: the global layers only
    let outside: Value =
        serde_json::from_str(&sandbox.wf(sandbox.path(), &["config", "--json"]).ok().out).unwrap();
    assert_eq!(outside["config"]["opener"], "user");
    assert_eq!(outside["sources"].as_array().unwrap().len(), 1);
}

// --- openers ----------------------------------------------------------------

#[test]
fn opener_wrap_and_path_flags() {
    let sandbox = Sandbox::new();
    let repo = sandbox.repo("api");
    repo.write_project_config("wrappers:\n  env: 'direnv exec . $SHELL -c \"$WF_COMMAND\"'\n");
    repo.create("feat");
    let opened = repo.wf(&["open", "feat", "-o", "mytool \"$WF_TARGET\"", "-p", "README.md"]).ok();
    assert!(
        opened.out.trim_end().ends_with(" /bin/sh -c 'mytool \"$WF_TARGET\"'"),
        "{}",
        opened.out
    );
    assert!(opened.out.contains("WF_TARGET=README.md"));

    let wrapped = repo.wf(&["open", "feat", "-o", "mytool", "-w", "env"]).ok();
    assert!(
        wrapped
            .out
            .trim_end()
            .ends_with(" WF_COMMAND=mytool /bin/sh -c 'direnv exec . $SHELL -c \"$WF_COMMAND\"'"),
        "{}",
        wrapped.out
    );
    let unknown = repo.wf(&["open", "feat", "-o", "mytool", "--wrap", "nope"]);
    assert_eq!(unknown.code, 1);
    assert_eq!(unknown.err, "Error: unknown wrapper 'nope' (available: env)\n");
}

#[test]
fn the_default_opener_follows_the_environment() {
    let sandbox = Sandbox::new();
    let repo = sandbox.repo("api");
    repo.create("feat");
    let visual = sandbox.wf_env(&repo.path, &["open", "feat"], &[("VISUAL", "visual-tool")]).ok();
    assert!(visual.out.trim_end().ends_with(" /bin/sh -c visual-tool"));
    let none = sandbox.wf_env(&repo.path, &["open", "feat"], &[("EDITOR", "")]);
    assert_eq!(none.code, 1);
    assert_eq!(
        none.err,
        "Error: no opener: pass -o, set `opener` in a config file, or export $VISUAL/$EDITOR\n"
    );
}

#[test]
fn a_background_opener_spawns_and_prints_no_directive() {
    let sandbox = Sandbox::new();
    let repo = sandbox.repo("api");
    let recorder = Recorder::new(&sandbox);
    repo.write_project_config(&format!(
        "openers:\n  rec: {{command: '{} --flag', background: true}}\n",
        recorder.path.display()
    ));
    let worktree = repo.create("feat");
    let opened = repo.wf(&["open", "feat", "-o", "rec"]).ok();
    assert_eq!(opened.out, "");
    assert_eq!(opened.err, "opened feat in the background\n");
    let line = &recorder.wait_for_lines(1)[0];
    assert!(
        line.contains("argv=--flag") && line.contains(&format!("cwd={}", worktree.display())),
        "{line}"
    );
}

// --- exit codes -------------------------------------------------------------

#[test]
fn exit_codes_for_cancelled_missing_and_stale() {
    let sandbox = Sandbox::new();
    let repo = sandbox.repo("api");
    let worktree = repo.create("feat");
    repo.make_dirty(&worktree);
    let cancelled = repo.wf(&["delete", "feat"]);
    assert_eq!(cancelled.code, 3);
    assert_eq!(
        cancelled.last_error(),
        "Error: cannot prompt ('Delete anyway?'): not a terminal; use --force"
    );
    assert_eq!(repo.wf(&["open", "ghost"]).code, 1);

    let gone = repo.create("gone");
    fs::remove_dir_all(&gone).unwrap();
    let stale = repo.wf(&["open", "gone"]);
    assert_eq!((stale.code, stale.out.as_str()), (1, ""));
    assert!(stale.err.starts_with("Error: worktree 'gone' is stale"), "{}", stale.err);
    for flags in [&["list"][..], &["list", "--porcelain"], &["list", "--json"]] {
        repo.wf(flags).ok();
    }
}

#[test]
fn lock_unlock_and_prune_through_the_cli() {
    let sandbox = Sandbox::new();
    let repo = sandbox.repo("api");
    repo.create("live");
    let locked = repo.wf(&["lock", "live", "--reason", "mine"]).ok();
    assert_eq!((locked.out.as_str(), locked.err.as_str()), ("", "locked worktree 'live'\n"));
    let again = repo.wf(&["lock", "live"]);
    assert_eq!(again.code, 1);
    assert!(again.err.contains("already locked (mine)"));
    assert_eq!(repo.wf(&["--complete", "unlockable"]).out, "live\tmine\n");
    assert_eq!(repo.wf(&["--complete", "lockable"]).out, "");
    assert_eq!(repo.wf(&["unlock", "live"]).ok().err, "unlocked worktree 'live'\n");
    for flags in [&["prune"][..], &["prune", "-n"], &["prune", "--dry-run"]] {
        let pruned = repo.wf(flags).ok();
        assert_eq!((pruned.out.as_str(), pruned.err.as_str()), ("", "no stale worktree records\n"));
    }
}

// --- prompts ----------------------------------------------------------------

#[test]
fn on_a_terminal_delete_asks_and_ctrl_c_cancels_the_command() {
    let sandbox = Sandbox::new();
    let repo = sandbox.repo("api");
    let worktree = repo.create("feat");
    repo.make_dirty(&worktree);

    let mut terminal = Terminal::spawn(&sandbox, &repo.path, &["delete", "feat"], 24, 80);
    terminal.settle();
    terminal.send("\x03"); // Ctrl-C at the question
    let (code, out, screen) = terminal.finish();
    assert_eq!((code, out.as_str()), (3, ""), "{screen}");
    assert!(
        screen.contains("Delete anyway? [y/N] ") && screen.contains("Error: cancelled"),
        "{screen}"
    );
    assert!(worktree.exists());

    let mut terminal = Terminal::spawn(&sandbox, &repo.path, &["delete", "feat"], 24, 80);
    terminal.send("y\n");
    terminal.settle();
    terminal.send("\x04"); // Ctrl-D declines deleting the branch
    let (code, _, screen) = terminal.finish();
    assert_eq!(code, 0, "{screen}");
    assert!(
        screen.contains("deleted worktree 'feat'")
            && screen.contains("Also delete branch 'feat'? [y/N] ")
    );
    assert!(!screen.contains("deleted branch"));
    assert!(!worktree.exists());
}

// --- completions ------------------------------------------------------------

#[test]
fn branch_candidates_say_where_a_branch_lives() {
    let sandbox = Sandbox::new();
    let repo = sandbox.repo_with_origin("api");
    repo.add_remote("upstream");
    repo.add_branch("everywhere");
    repo.add_branch("taken");
    repo.add_remote_only_branch("remote-only", "origin");
    repo.add_branch("shared");
    repo.git(&["push", "-q", "upstream", "shared"]);
    repo.git(&["branch", "-D", "shared"]);
    repo.create("taken");

    let listed = repo.wf(&["--complete", "branches"]).ok();
    let rows: Vec<(&str, &str)> =
        listed.out.lines().map(|line| line.split_once('\t').unwrap()).collect();
    let find = |name: &str| {
        rows.iter().find(|(candidate, _)| *candidate == name).map(|(_, location)| *location)
    };
    assert_eq!(find("everywhere"), Some("local, origin"));
    // remote-only branches are offered remote-qualified
    assert_eq!((find("remote-only"), find("origin/remote-only")), (None, Some("origin")));
    assert_eq!(
        (find("shared"), find("origin/shared"), find("upstream/shared")),
        (None, Some("origin"), Some("upstream"))
    );
    // checked out already: in a worktree, and in the main one
    assert_eq!((find("taken"), find("main")), (None, None));
}

#[test]
fn worktree_script_and_opener_candidates() {
    let sandbox = Sandbox::new();
    let repo = sandbox.repo("api");
    repo.write_project_config(
        "scripts:\n  migrate: 'true'\n  test: 'true'\n  step: {command: 'true', hidden: true}\n\
         openers:\n  edit: '$EDITOR \"$WF_TARGET\"'\n  win: {from: edit, wrap: kitty}\n\
         wrappers:\n  kitty: 'kitty $SHELL -c \"$WF_COMMAND\"'\n",
    );
    repo.create("feature/x");
    assert_eq!(repo.wf(&["--complete", "worktrees"]).out, "x\n");
    assert_eq!(repo.wf(&["--complete", "scripts"]).out, "migrate\ntest\n"); // `step` is hidden
    // wrappers are not openers; descriptions carry the resolved command
    assert_eq!(
        repo.wf(&["--complete", "openers"]).out,
        "edit\t$EDITOR \"$WF_TARGET\"\nwin\t$EDITOR \"$WF_TARGET\" via kitty\n"
    );
    let commands = repo.wf(&["--complete", "commands"]).ok().out;
    assert!(commands.lines().all(|line| line.split('\t').count() == 2)); // NAME, DESCRIPTION
    assert!(commands.contains("create\tcreate (or reuse) a worktree for a branch and open it\n"));
    assert!(!commands.contains("edit\t")); // openers are not top-level words
    assert!(!commands.contains("claude\t")); // gated: no ~/.claude
}

#[test]
fn completion_never_errors() {
    let sandbox = Sandbox::new();
    for topic in [
        "branches",
        "worktrees",
        "scripts",
        "openers",
        "make",
        "lockable",
        "claude-sessions",
        "bogus",
        "",
    ] {
        let result = sandbox.wf(sandbox.path(), &["--complete", topic]).ok();
        assert_eq!((result.out.as_str(), result.err.as_str()), ("", ""), "{topic}");
    }
    assert!(sandbox.wf(sandbox.path(), &["--complete", "commands"]).out.contains("create\t"));
    assert_eq!(sandbox.wf(sandbox.path(), &["--complete"]).ok().out, "");
    // and a broken config is no reason to say anything
    let repo = sandbox.repo("api");
    repo.write_project_config("opener: [broken\n");
    for topic in ["worktrees", "scripts", "openers", "make"] {
        let result = repo.wf(&["--complete", topic]).ok();
        assert_eq!((result.out.as_str(), result.err.as_str()), ("", ""), "{topic}");
    }
}

// --- the Claude integration -------------------------------------------------

#[test]
fn the_claude_subcommand_exists_only_with_claude_around() {
    let sandbox = Sandbox::new();
    let repo = sandbox.repo("api");
    // "claude" is not a subcommand without ~/.claude: usage error
    assert_eq!(repo.wf(&["claude", "copy-session", "x"]).code, 2);
    assert!(!repo.wf(&["--help"]).out.contains("claude"));
    fs::create_dir(sandbox.home().join(".claude")).unwrap();
    assert!(repo.wf(&["--help"]).ok().out.contains("claude"));
    assert!(repo.wf(&["--complete", "commands"]).out.contains("claude\tClaude Code integration"));
    assert_eq!(repo.wf(&["claude"]).code, 2);
}

#[test]
fn copying_a_claude_session_into_a_worktree() {
    let sandbox = Sandbox::new();
    let repo = sandbox.repo("api");
    let worktree = repo.create("feat");
    let encode = |path: &std::path::Path| path.to_string_lossy().replace(['/', '.'], "-");
    let projects = sandbox.home().join(".claude/projects");
    let main_project = projects.join(encode(&repo.path));
    fs::create_dir_all(&main_project).unwrap();
    fs::write(
        main_project.join("abc-123.jsonl"),
        format!("{}\n{{\"type\":\"meta\"}}\n", json!({"cwd": repo.path, "type": "user"})),
    )
    .unwrap();
    fs::write(
        sandbox.home().join(".claude/history.jsonl"),
        format!(
            "{}\n",
            json!({"sessionId": "abc-123", "project": repo.path, "display": "fix the login bug"})
        ),
    )
    .unwrap();

    let from_main = repo.wf(&["claude", "copy-session", "abc-123"]);
    assert_eq!(from_main.code, 1);
    assert_eq!(from_main.err, "Error: copy-session must run from a non-main worktree\n");
    assert_eq!(repo.wf(&["--complete", "claude-sessions"]).out, ""); // empty from the main worktree
    assert_eq!(sandbox.wf(&worktree, &["--complete", "claude-sessions"]).out, "abc-123\n");

    sandbox.wf(&worktree, &["claude", "copy-session", "abc-123"]).ok();
    let copied =
        fs::read_to_string(projects.join(encode(&worktree)).join("abc-123.jsonl")).unwrap();
    let first: Value = serde_json::from_str(copied.lines().next().unwrap()).unwrap();
    assert_eq!(first["cwd"], worktree.to_str().unwrap());
    assert_eq!(sandbox.wf(&worktree, &["--complete", "claude-sessions"]).out, "");
    let ghost = sandbox.wf(&worktree, &["claude", "copy-session", "ghost"]);
    assert_eq!(ghost.code, 1);
    assert!(ghost.err.contains("session 'ghost' not found"));
}

// --- init -------------------------------------------------------------------

#[test]
fn init_writes_the_starter_once() {
    let sandbox = Sandbox::new();
    let repo = sandbox.repo("api");
    let target = repo.path.join(".workforest.yaml");
    let first = repo.wf(&["init"]).ok();
    assert_eq!(
        first.err,
        format!("scaffolded {} (all keys commented out; see man 5 workforest)\n", target.display())
    );
    assert!(fs::metadata(&target).unwrap().len() > 0);
    let second = repo.wf(&["init"]);
    assert_eq!(second.code, 1);
    assert_eq!(second.err, format!("Error: {} already exists\n", target.display()));
    assert_eq!(repo.wf(&["init", "--local"]).code, 1);
    // the starter changes nothing
    let shown: Value = serde_json::from_str(&repo.wf(&["config", "--json"]).ok().out).unwrap();
    assert_eq!(shown["config"]["scripts"], json!({}));
    assert_eq!(shown["sources"][0]["layer"], "project");
}
