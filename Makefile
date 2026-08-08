# Dev targets wrap `uv run`; `uv sync` is the only setup step.

.PHONY: check test lint type cov sync install uninstall

sync:
	uv sync

test:
	uv run pytest

lint:
	uv run ruff check .
	uv run ruff format --check .

type:
	uv run mypy

check: lint type test

cov:
	uv run pytest --cov-report=html
	@echo "open htmlcov/index.html"

# Install the current checkout as a uv tool (~/.local/bin/workforest).
# --reinstall so re-running picks up changes even without a version bump.
install:
	uv tool install --reinstall .
	@echo
	@echo 'workforest installed. Make sure your shell rc has:'
	@echo '  eval "$$(workforest shell-init)"'

uninstall:
	uv tool uninstall workforest
