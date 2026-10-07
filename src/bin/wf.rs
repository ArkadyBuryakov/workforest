//! `wf`: the short name, for installs that cannot make it a symlink (a
//! PyPI wheel). Runs the `workforest` beside it with the same arguments.

use std::os::unix::process::CommandExt;
use std::process::Command;

fn main() {
    let sibling = std::env::current_exe()
        .and_then(|path| path.canonicalize())
        .ok()
        .and_then(|path| Some(path.parent()?.join("workforest")));
    let Some(sibling) = sibling else {
        eprintln!("wf: cannot locate the workforest executable");
        std::process::exit(1);
    };
    // exec replaces this process: only a failure returns.
    let error = Command::new(&sibling).args(std::env::args_os().skip(1)).exec();
    eprintln!("wf: cannot run {}: {error}", sibling.display());
    std::process::exit(1);
}
