"""Human-facing output and interaction.

Everything here writes to stderr: stdout is reserved for machine output
(shell directives, porcelain listings, completions) and is owned by cli.py.
"""

import os
import sys

from workforest.errors import CancelledError

_RED = "\033[0;31m"
_GREEN = "\033[0;32m"
_YELLOW = "\033[0;33m"
_RESET = "\033[0m"


def _colors_enabled() -> bool:
    if os.environ.get("NO_COLOR"):
        return False
    if os.environ.get("CLICOLOR_FORCE"):
        return True
    return sys.stderr.isatty()


def _emit(text: str, color: str) -> None:
    if _colors_enabled():
        print(f"{color}{text}{_RESET}", file=sys.stderr)
    else:
        print(text, file=sys.stderr)


def info(text: str) -> None:
    print(text, file=sys.stderr)


def success(text: str) -> None:
    _emit(text, _GREEN)


def warn(text: str) -> None:
    _emit(text, _YELLOW)


def error(text: str) -> None:
    if _colors_enabled():
        print(f"{_RED}Error:{_RESET} {text}", file=sys.stderr)
    else:
        print(f"Error: {text}", file=sys.stderr)


def interactive() -> bool:
    """True when we may prompt the user."""
    return sys.stdin.isatty() and sys.stderr.isatty()


def confirm(question: str, *, default: bool = False) -> bool:
    """Ask a y/N question on the terminal.

    Raises CancelledError when there is no terminal to ask on — callers that
    support an explicit flag (--force) must check interactive() first and
    take that path instead.
    """
    if not interactive():
        raise CancelledError(f"cannot prompt ({question!r}): not a terminal; use --force")
    suffix = "[Y/n]" if default else "[y/N]"
    print(f"{question} {suffix} ", file=sys.stderr, end="", flush=True)
    try:
        answer = input().strip().lower()
    except EOFError, KeyboardInterrupt:
        print(file=sys.stderr)
        return False
    if not answer:
        return default
    return answer in ("y", "yes")
