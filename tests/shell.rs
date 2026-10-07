//! Shell integration: `shell-init` output driven in real bash/zsh
//! sessions — the `wf` function, the cd protocol, completion registration.

mod common;

use std::fs;
use std::process::Command;

use common::Sandbox;

fn installed(shell: &str) -> bool {
    Command::new(shell).arg("-c").arg("true").output().is_ok_and(|output| output.status.success())
}

fn shells() -> Vec<&'static str> {
    ["bash", "zsh"].into_iter().filter(|shell| installed(shell)).collect()
}

#[test]
fn shell_init_output_is_valid_syntax() {
    let sandbox = Sandbox::new();
    for shell in shells() {
        let init = sandbox.wf(sandbox.path(), &["shell-init", shell]).ok();
        assert!(init.out.starts_with("# Workforest shell integration"));
        assert!(init.out.contains("_workforest_complete")); // completion follows the wrapper
        let script = sandbox.path().join(format!("init.{shell}"));
        fs::write(&script, &init.out).unwrap();
        let check = Command::new(shell).arg("-n").arg(&script).output().unwrap();
        assert!(check.status.success(), "{}", String::from_utf8_lossy(&check.stderr));
    }
}

#[test]
fn the_static_completion_files_are_valid_syntax() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let bash = root.join("resources/shell/completion.bash");
    let check = Command::new("bash").arg("-n").arg(&bash).output().unwrap();
    assert!(check.status.success(), "{}", String::from_utf8_lossy(&check.stderr));
    if installed("zsh") {
        let zsh = root.join("completions/_workforest");
        let script = format!("autoload -Uz compinit; compinit -u; source {}", zsh.display());
        let sandbox = Sandbox::new();
        let check = sandbox.shell("zsh", &script, sandbox.path());
        // sourcing outside completion context must at least parse; compadd
        // errors are acceptable, syntax errors are not
        assert!(!check.err.contains("parse error"), "{}", check.err);
    }
}

#[test]
fn the_shell_is_detected_from_the_environment() {
    let sandbox = Sandbox::new();
    let zsh = sandbox.wf_env(sandbox.path(), &["shell-init"], &[("SHELL", "/usr/bin/zsh")]).ok();
    assert!(zsh.out.contains("compdef"));
    let fish = sandbox.wf_env(sandbox.path(), &["shell-init"], &[("SHELL", "/usr/bin/fish")]);
    assert_eq!(fish.code, 1);
    assert!(fish.err.contains("cannot detect shell from $SHELL (fish)"), "{}", fish.err);
    // SHELL is /bin/sh in the sandbox
    assert_eq!(sandbox.wf(sandbox.path(), &["shell-init"]).code, 1);
    assert_eq!(sandbox.wf(sandbox.path(), &["shell-init", "fish"]).code, 2);
}

#[test]
fn zsh_registration_survives_compinit_ordering() {
    // Regression: eval'ing shell-init before compinit must still register
    // completions (deferred via a one-shot precmd hook).
    if !installed("zsh") {
        return;
    }
    let sandbox = Sandbox::new();
    let repo = sandbox.repo("api");
    let init = "eval \"$(workforest shell-init zsh)\"";
    let compinit = "autoload -Uz compinit && compinit -u";
    for order in [[init, compinit], [compinit, init]] {
        let script = [
            order[0],
            order[1],
            "for f in $precmd_functions; do $f; done", // simulate the first prompt
            "print -r -- \"${_comps[wf]:-NOTHING}:${_comps[workforest]:-NOTHING}\"",
            "print -r -- \"hooks:${#precmd_functions}\"",
        ]
        .join("\n");
        let result = sandbox.shell("zsh", &script, &repo.path).ok();
        assert!(result.out.contains("_workforest_complete:_workforest_complete"), "{}", result.out);
        assert!(result.out.contains("hooks:0"), "the one-shot hook removed itself");
    }
}

#[test]
fn an_install_with_pages_off_the_man_path_gets_a_manpath_entry() {
    // What a tool environment (uv tool, pipx) looks like: the executable
    // in PREFIX/bin, the pages in PREFIX/share/man, a symlink on $PATH.
    let sandbox = Sandbox::new();
    let prefix = sandbox.path().join("it's a tool env"); // quoting survives
    fs::create_dir_all(prefix.join("bin")).unwrap();
    fs::create_dir_all(prefix.join("share/man/man1")).unwrap();
    // the same file under another name: nothing is written, so nothing is busy
    let copied =
        Command::new("cp").arg(common::BINARY).arg(prefix.join("bin/workforest")).status().unwrap();
    assert!(copied.success());
    fs::write(prefix.join("share/man/man1/workforest.1"), ".TH X 1\n").unwrap();
    let man = prefix.join("share/man").display().to_string();

    let init = sandbox
        .command(prefix.join("bin/workforest"))
        .args(["shell-init", "bash"])
        .output()
        .unwrap();
    let init = String::from_utf8_lossy(&init.stdout).into_owned();
    assert!(init.contains("export MANPATH='"), "{init}");
    assert!(
        init.starts_with("# Workforest shell integration") && init.contains("_workforest_complete")
    );

    // unset: a trailing colon means "then the system default"; set: ours
    // goes first; already there: untouched
    let cases = [
        (None, format!("{man}:")),
        (Some("/x/man".to_string()), format!("{man}:/x/man")),
        (Some(format!("{man}:/x/man")), format!("{man}:/x/man")),
    ];
    for shell in shells() {
        for (before, after) in &cases {
            let script = sandbox.path().join("manpath.sh");
            fs::write(&script, format!("{init}\nprintf '%s' \"$MANPATH\"\n")).unwrap();
            let mut command = sandbox.command(shell);
            command.arg(&script);
            if let Some(before) = before {
                command.env("MANPATH", before);
            }
            let output = command.output().unwrap();
            assert_eq!(
                &String::from_utf8_lossy(&output.stdout),
                after,
                "{shell}, MANPATH={before:?}"
            );
        }
    }
    // the binary in the build tree has no pages beside it: nothing to add
    assert!(!sandbox.wf(sandbox.path(), &["shell-init", "bash"]).out.contains("MANPATH"));
}

#[test]
fn wf_changes_directory_under_either_name() {
    let sandbox = Sandbox::new();
    let repo = sandbox.repo("api");
    let worktree = repo.create("feat");
    for shell in shells() {
        // Both spellings are the wrapper, so an alias for either (`alias
        // wfo='workforest open'`) changes directory too.
        for name in ["wf", "workforest"] {
            let script = format!(
                "eval \"$(workforest shell-init {shell})\"\n{name} open feat -o true\npwd\n"
            );
            let result = sandbox.shell(shell, &script, &repo.path).ok();
            assert_eq!(result.out.trim(), worktree.to_str().unwrap(), "{shell} {name}");
        }
    }
}

#[test]
fn wf_passes_data_through_and_never_evals_it() {
    let sandbox = Sandbox::new();
    let repo = sandbox.repo("api");
    // Regression: a worktree named `cd` makes `wf list` lines start with
    // "cd " — the directive sentinel, not the text, decides what is eval'd.
    repo.create("cd");
    for shell in shells() {
        let script = format!(
            "eval \"$(workforest shell-init {shell})\"\nwf list --porcelain\nwf list\npwd\n"
        );
        let result = sandbox.shell(shell, &script, &repo.path).ok();
        let lines: Vec<&str> = result.out.lines().collect();
        assert!(lines[0].starts_with("cd\tcd\t"), "{}", result.out);
        assert!(lines[1].starts_with("cd  cd  clean  ")); // the listing passed through as data
        assert_eq!(lines[2], repo.path.to_str().unwrap()); // and the shell did not move
    }
}

#[test]
fn wf_propagates_failure() {
    let sandbox = Sandbox::new();
    let repo = sandbox.repo("api");
    for shell in shells() {
        let script = format!("eval \"$(workforest shell-init {shell})\"\nwf open ghost\n");
        let result = sandbox.shell(shell, &script, &repo.path);
        assert_eq!(result.code, 1, "{shell}");
        assert!(result.err.contains("not found"));
    }
}

#[test]
fn delete_from_inside_moves_the_shell_back_to_main() {
    let sandbox = Sandbox::new();
    let repo = sandbox.repo("api");
    for shell in shells() {
        let worktree = repo.create("inside");
        let script = format!(
            "eval \"$(workforest shell-init {shell})\"\nwf delete inside --delete-branch\npwd\n"
        );
        let result = sandbox.shell(shell, &script, &worktree).ok();
        assert_eq!(result.out.trim(), repo.path.to_str().unwrap(), "{shell}");
    }
}
