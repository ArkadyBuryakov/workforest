# Code conventions

- Python 3.14, uv-managed. Verify changes with `make check` (ruff lint +
  format check, mypy, pytest with a 90% coverage floor). Run
  `uv run ruff format .` rather than hand-formatting.
- When a return value or constant bundles fields whose positions carry
  meaning, use a small named dataclass — `@dataclass(slots=True,
  frozen=True)` — not an anonymous tuple or nested dict. Plain dicts are for
  genuine key→value lookups only (env maps, branch→remotes).
- Pre-1.0 with zero users: on breaking changes keep the clean design and
  state what to re-run (e.g. re-eval shell-init). Never add
  backward-compatibility shims.
