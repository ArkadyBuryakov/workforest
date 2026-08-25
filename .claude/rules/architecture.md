# Architecture invariants

- `gitutil.py` is the only module that spawns git. Consumers get typed
  results; worktree data comes from `--porcelain -z` output, never from
  parsing the human-readable form.
- `cli.py` is the sole stdout writer. Commands return
  `ShellAction | str | None`; stdout carries the shell-wrapper cd protocol,
  so nothing else may print there (hook/script stdout is diverted to stderr).
- `tui.py`: everything except the fzf subprocess is pure and unit-tested;
  fzf is the only sanctioned external tool there.
- `completions.py` must never break the shell: any error yields an empty
  candidate list, and output stays plain `NAME<TAB>ANNOTATION` lines.
- `integrations/claude.py` is experimental: it reads Claude Code's private
  on-disk state. Session lines are rewritten by JSON parsing, never by
  string substitution.
- README.md is the project reference; there is no separate design document.
  The man pages under `man/` (`workforest.1`, `workforest.5`) are its
  installed counterpart: any change to commands, options, config keys,
  environment variables, or exit codes updates both README.md and the
  affected page in the same change. `tests/test_man.py` catches drift from
  `cli.py`, not from prose — keep the wording in sync by hand.
- Script groups (`hooks.py`): a `bulk`/`pipeline` runs under a forked
  supervisor that leads the process group and takes the tty exactly like a
  command, so records, `wf stop`, `exclusive`, `cleanup`, and `-b` need no
  group-specific code. Bulk members must never take the tty (only one
  group can own it); they run in the supervisor's process group so a
  signal to the group reaches them all, write to a pty when stderr is a
  terminal (so their programs keep colors) and to a pipe otherwise, and
  the supervisor relays their lines prefixed. A bulk always waits for
  every member. An outcome of `-SIGINT` or exit 130 is an interruption
  (warning, `wf` dies by SIGINT), never a failure. Forked-child bodies are
  `# pragma: no cover`; the logic lives in tested pure helpers
  (`_Prefixer`, `_pump`, `_bulk_outcome`, `_run_step`).
