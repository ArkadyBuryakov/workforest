//! The interactive mode on a real (pseudo-)terminal: what is drawn, and
//! that a picked row becomes the command's directive on stdout. The state
//! machine and the drawing are unit-tested in src/tui/.

mod common;

use common::{DIRECTIVE, Sandbox, Terminal};

const ROWS: u16 = 12;
const COLS: u16 = 72;

/// The last frame of what a terminal was sent, as lines of text: a small
/// emulation — cursor addressing and printing, which is all a frame uses.
fn screen(output: &str) -> Vec<String> {
    let mut grid = vec![vec![' '; COLS as usize]; ROWS as usize];
    let (mut row, mut col) = (0usize, 0usize);
    let mut chars = output.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch != '\x1b' {
            if ch >= ' ' && row < grid.len() && col < COLS as usize {
                grid[row][col] = ch;
                col += 1;
            }
            continue;
        }
        if chars.next() != Some('[') {
            continue;
        }
        let mut parameters = String::new();
        let command = loop {
            match chars.next() {
                Some(c) if c.is_ascii_digit() || c == ';' || c == '?' => parameters.push(c),
                Some(c) => break c,
                None => break ' ',
            }
        };
        match command {
            'H' => {
                let mut parts =
                    parameters.split(';').map(|part| part.parse::<usize>().unwrap_or(1));
                row = parts.next().unwrap_or(1).saturating_sub(1);
                col = parts.next().unwrap_or(1).saturating_sub(1);
            }
            'J' if parameters == "2" => grid.iter_mut().for_each(|line| line.fill(' ')),
            // leaving the alternate screen ends the frame we want to read
            'l' if parameters == "?1049" => break,
            _ => {}
        }
    }
    grid.iter().map(|line| line.iter().collect::<String>().trim_end().to_string()).collect()
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
    terminal.settle();
    terminal.send("fix");
    terminal.settle();
    terminal.send("\r");
    let (code, out, output) = terminal.finish();
    assert_eq!(code, 0, "{output}");
    assert!(out.starts_with(&format!("{DIRECTIVE}cd {} && WF_MAIN=", fix.display())), "{out}");
    assert!(out.trim_end().ends_with(" /bin/sh -c stub-editor"));

    let lines = screen(&output);
    assert_eq!(lines[0], " CREATE  │ [OPEN] │  CHECKOUT  │  DELETE");
    assert_eq!(lines[1], "Open: fix");
    assert_eq!(lines[3], "> fix-login  fix-login  dirty");
    assert_eq!(lines[10], "opener  [edit] |  shell");
    assert!(output.contains("clean locked"), "the unfiltered list showed feat's state");
    assert!(output.contains("\x1b[?1049h") && output.contains("\x1b[?1049l"), "alternate screen");
}

#[test]
fn a_new_branch_is_created_from_what_was_typed_with_the_chosen_opener() {
    let sandbox = Sandbox::new();
    let repo = sandbox.repo("api");
    let mut terminal = Terminal::spawn(&sandbox, &repo.path, &["tui"], ROWS, COLS);
    terminal.settle(); // CREATE: nothing to open yet
    terminal.send("brand/new");
    terminal.send("\x1b[1;5C"); // ctrl-→: the next opener, the shell
    terminal.settle();
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
    terminal.settle();
    terminal.send("one\r");
    terminal.settle();
    terminal.send("n\n"); // "Also delete branch 'one'?" — keep it
    terminal.settle();
    terminal.settle(); // back in the list
    terminal.send("\x1b");
    let (code, out, output) = terminal.finish();
    assert_eq!((code, out.as_str()), (0, ""), "{output}");
    assert!(
        output.contains("deleted worktree 'one'")
            && output.contains("Also delete branch 'one'? [y/N]")
    );
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
    terminal.settle();
    terminal.send("\x1b[D"); // ←: CREATE
    terminal.settle();
    terminal.send("\x1b[D"); // ←: wraps to DELETE
    terminal.settle();
    terminal.send("\x03"); // ctrl-c leaves, too
    let (code, out, output) = terminal.finish();
    assert_eq!((code, out.as_str()), (0, ""));
    assert!(output.contains("nothing here"), "OPEN had nothing: {output}");
    assert!(output.contains("free-branch"), "CREATE lists branches");
    let lines = screen(&output);
    assert_eq!(lines[0], " CREATE  │  OPEN  │  CHECKOUT  │ [DELETE]");
    assert_eq!(lines[3], "> gone  gone  stale");
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
