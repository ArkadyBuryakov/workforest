# All targets wrap `uv run`; `uv sync` is the only setup step.

.PHONY: check test lint type cov sync

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
