---
paths:
  - "tests/**/*.py"
---

# Test conventions

- Isolation contract (see `tests/conftest.py`): no test may read or write
  the real environment. The autouse `isolated_env` fixture redirects HOME,
  XDG_CONFIG_HOME, and git config, and pins SHELL=/bin/sh, EDITOR, and
  NO_COLOR — rely on it instead of patching these per test.
- Build repos through the `repo` / `make_repo` fixtures and the `Repo`
  helper methods (`add_branch`, `add_remote`, `write_project_config`,
  `make_dirty`) rather than raw git calls.
- Coverage floor is 90% (`--cov-fail-under=90` in pyproject.toml); pure
  helpers are expected to be unit-tested, subprocess/terminal glue may be
  `# pragma: no cover`.
