---
paths:
  - "tests/**/*.rs"
  - "src/**/tests.rs"
  - "src/testing.rs"
---

# Test conventions

- Two layers. Pure logic is unit-tested beside the code (`mod tests` in the
  file, or a `tests.rs` next to it). What the command line does — exit
  codes, the stdout protocol, scripts and their supervisors, prompts, the
  shell function, the TUI — is tested in `tests/*.rs` by running the built
  binary against temporary git repositories.
- Isolation contract: no test may read or write the real environment.
  Unit tests share one process, so they never touch the process
  environment or working directory: pass paths and a `util::Env`, build a
  `Context` with `Sandbox::context`, capture messages with
  `output::capture`, and script a terminal with `output::with_terminal`
  (`src/testing.rs`). Integration tests start every process through
  `Sandbox::command` (`tests/common/mod.rs`), which redirects HOME,
  XDG_CONFIG_HOME and git config and pins SHELL=/bin/sh, EDITOR, and
  NO_COLOR — rely on it instead of setting these per test.
- Build repos through `Sandbox::repo` / `repo_with_origin` and the `Repo`
  helper methods (`add_branch`, `add_remote`, `write_project_config`,
  `make_dirty`) rather than raw git calls.
- Anything that forks, installs signal handlers, or needs a terminal is an
  integration test: it belongs to a process of its own. `Terminal`
  (`tests/common/mod.rs`) gives the binary a pseudo-terminal; wait for what
  it shows (`expect`, `expect_screen`) before typing the next thing —
  never sleep, and never type ahead.
- A test that writes an executable and then runs it uses
  `write_executable` / `Sandbox::script`: written by a child process, so a
  concurrent test's fork cannot hold it open ("text file busy").
- Coverage floor is 90% of lines (`COVERAGE_FLOOR` in the Makefile), with
  no file excluded: the integration tests run the instrumented binary, and
  forked children flush their counts before they exit.
