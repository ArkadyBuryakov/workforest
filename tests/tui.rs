//! The interactive mode on a real (pseudo-)terminal: what is drawn, and
//! that a picked row becomes the command's directive on stdout. The state
//! machine and the drawing are unit-tested in src/tui/.

mod common;

use common::{DIRECTIVE, Sandbox, Terminal};

const ROWS: u16 = 12;
const COLS: u16 = 72;

fn tabs(current: &str) -> String {
    ["create", "open", "checkout", "delete"]
        .map(|mode| {
            let name = mode.to_uppercase();
            if mode == current { format!("[{name}]") } else { format!(" {name} ") }
        })
        .join(" │ ")
        .trim_end()
        .to_string()
}

/// Wait for the interface to be up in `mode`.
fn expect_mode(terminal: &Terminal, mode: &str) {
    terminal.expect_screen(&format!("the {mode} tab"), |lines| lines[0] == tabs(mode));
}

#[test]
fn picking_a_worktree_opens_it() {
    let sandbox = Sandbox::new();
    let repo = sandbox.repo("api");
    let feat = repo.create("feat");
    let fix = repo.create("fix-login");
    repo.make_dirty(&fix);
    repo.git(&["worktree", "lock", &feat.to_string_lossy()]);

    // no arguments: the TUI, in OPEN since there is something to open
    let mut terminal = Terminal::spawn(&sandbox, &repo.path, &[], ROWS, COLS);
    expect_mode(&terminal, "open");
    let listed = terminal.screen();
    assert_eq!(listed[1], "Open:");
    assert_eq!(listed[3], "> feat       feat       clean locked");
    assert_eq!(listed[4], "  fix-login  fix-login  dirty");
    assert_eq!(listed[10], "opener  [edit] |  shell");

    terminal.send("fix");
    terminal.expect_screen("the filtered list", |lines| lines[1] == "Open: fix");
    assert_eq!(terminal.screen()[3], "> fix-login  fix-login  dirty");
    terminal.send("\r");
    let (code, out, output) = terminal.finish();
    assert_eq!(code, 0, "{output}");
    assert!(out.starts_with(&format!("{DIRECTIVE}cd {} && WF_MAIN=", fix.display())), "{out}");
    assert!(out.trim_end().ends_with(" /bin/sh -c stub-editor"));
    assert!(output.contains("\x1b[?1049h") && output.contains("\x1b[?1049l"), "alternate screen");
}

#[test]
fn a_new_branch_is_created_from_what_was_typed_with_the_chosen_opener() {
    let sandbox = Sandbox::new();
    let repo = sandbox.repo("api");
    let mut terminal = Terminal::spawn(&sandbox, &repo.path, &["tui"], ROWS, COLS);
    expect_mode(&terminal, "create"); // nothing to open yet
    terminal.send("brand/new");
    terminal.send("\x1b[1;5C"); // ctrl-→: the next opener, the shell
    terminal.expect_screen("the shell opener", |lines| {
        lines[1] == "Branch/New: brand/new" && lines[10] == "opener   edit  | [shell]"
    });
    assert_eq!(terminal.screen()[3], "  no match — enter creates this branch");
    terminal.send("\r");
    let (code, out, output) = terminal.finish();
    assert_eq!(code, 0, "{output}");
    let worktree = repo.worktrees_dir().join("new");
    assert!(out.starts_with(&format!("{DIRECTIVE}cd {} && ", worktree.display())), "{out}");
    assert!(out.trim_end().ends_with(" /bin/sh -c /bin/sh"), "{out}");
    assert!(out.contains("WF_BRANCH=brand/new"));
    assert_eq!(repo.git_in(&worktree, &["rev-parse", "--abbrev-ref", "HEAD"]), "brand/new");
    assert!(output.contains("created worktree for 'brand/new'"));
}

#[test]
fn delete_stays_in_the_loop_and_esc_leaves_with_nothing() {
    let sandbox = Sandbox::new();
    let repo = sandbox.repo("api");
    let (one, two) = (repo.create("one"), repo.create("two"));
    let mut terminal = Terminal::spawn(&sandbox, &repo.path, &["tui", "delete"], ROWS, COLS);
    expect_mode(&terminal, "delete");
    terminal.send("one");
    terminal.expect_screen("the filtered list", |lines| lines[1] == "Delete: one");
    terminal.send("\r");
    terminal.expect("Also delete branch 'one'? [y/N] ");
    terminal.send("n\n"); // keep it
    // back in the list, which no longer has it
    terminal.expect_nth("\x1b[?1049h", 2);
    terminal.expect_screen("the list without it", |lines| {
        lines[0] == tabs("delete") && lines[3] == "> two  two  clean"
    });
    terminal.send("\x1b");
    let (code, out, output) = terminal.finish();
    assert_eq!((code, out.as_str()), (0, ""), "{output}");
    assert!(output.contains("deleted worktree 'one'"));
    assert!(!one.exists() && two.exists());
    assert_eq!(repo.git(&["branch", "--list", "one"]).trim(), "one");
}

#[test]
fn switching_modes_reloads_the_list() {
    let sandbox = Sandbox::new();
    let repo = sandbox.repo("api");
    repo.add_branch("free-branch");
    let gone = repo.create("gone");
    std::fs::remove_dir_all(&gone).unwrap();
    // only a stale worktree: nothing to open, but DELETE offers it
    let mut terminal = Terminal::spawn(&sandbox, &repo.path, &["tui", "open"], ROWS, COLS);
    expect_mode(&terminal, "open");
    assert_eq!(terminal.screen()[3], "  nothing here");
    terminal.send("\x1b[D"); // ←: CREATE
    expect_mode(&terminal, "create");
    assert_eq!(terminal.screen()[3], "> free-branch  local");
    terminal.send("\x1b[D"); // ←: wraps to DELETE
    expect_mode(&terminal, "delete");
    terminal.expect_screen("the stale worktree", |lines| lines[3] == "> gone  gone  stale");
    terminal.send("\x03"); // ctrl-c leaves, too
    let (code, out, _) = terminal.finish();
    assert_eq!((code, out.as_str()), (0, ""));
}

#[test]
fn without_a_terminal_the_tui_says_so() {
    let sandbox = Sandbox::new();
    let repo = sandbox.repo("api");
    for args in [&[][..], &["tui"]] {
        let result = repo.wf(args);
        assert_eq!((result.code, result.out.as_str()), (1, ""));
        assert_eq!(
            result.err,
            "Error: the TUI needs a terminal; all actions are also available as plain subcommands\n"
        );
    }
    assert_eq!(sandbox.wf(sandbox.path(), &["tui"]).err, "Error: Not inside a git repository\n");
}
