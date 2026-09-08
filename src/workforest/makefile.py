"""Makefile targets as scripts.

Where `make` is installed and the worktree root holds a makefile, every
target it defines is runnable as `wf make TARGET` — the same machinery
`wf run` uses, so a target is recorded, stopped, and counted like any
other script. Its script name is the target prefixed with `make:`, which
is what job records, `wf stop --make`, and `wf list --json` show; nothing
in `scripts` is shadowed, and the two namespaces never collide.

Targets are discovered by reading the makefile (and what it includes)
rather than by asking make: listing what a project can run must not
evaluate `$(shell ...)`. The parse is therefore approximate by design —
it finds the plain rules a human would call and leaves generated ones
alone — and it never gates `wf make`, which passes the target to make
untouched.
"""

import re
import shlex
import shutil
from pathlib import Path

from workforest.config import Config, ScriptSpec
from workforest.errors import WorkforestError

PREFIX = "make:"
# The names GNU make looks for, in its own order.
MAKEFILE_NAMES = ("GNUmakefile", "makefile", "Makefile")
_INCLUDE = re.compile(r"^[-s]?include\s+(.*)$")
_MAX_INCLUDE_DEPTH = 8


def script_name(target: str) -> str:
    return f"{PREFIX}{target}"


def target_of(name: str) -> str | None:
    """The target a `make:` script name addresses, else None."""
    return name[len(PREFIX) :] if name.startswith(PREFIX) and len(name) > len(PREFIX) else None


def makefile_path(root: Path) -> Path | None:
    for name in MAKEFILE_NAMES:
        candidate = root / name
        if candidate.is_file():
            return candidate
    return None


def available(root: Path) -> bool:
    """Whether `wf make` has anything to offer here."""
    return shutil.which("make") is not None and makefile_path(root) is not None


def require(root: Path) -> None:
    if shutil.which("make") is None:
        raise WorkforestError("make is not installed")
    if makefile_path(root) is None:
        names = ", ".join(MAKEFILE_NAMES)
        raise WorkforestError(f"no makefile in {root} (looked for {names})")


def rule_targets(line: str) -> list[str]:
    """The targets one makefile line defines, ignoring everything that is
    not a plain rule: recipes and comments, variable assignments (`:=`
    and friends), pattern rules, `$(...)`-built names, and make's own
    dot-targets (`.PHONY`)."""
    if not line or line[0] in " \t#":
        return []
    head, separator, rest = line.partition(":")
    if not separator or "=" in head or "#" in head:
        return []
    if rest.lstrip(":").startswith("="):  # `X := v`, `X ::= v`, `X :::= v`
        return []
    return [
        target
        for target in head.split()
        if not target.startswith(".") and "%" not in target and "$" not in target
    ]


def _included(line: str, directory: Path) -> list[Path]:
    """The files an `include`/`-include`/`sinclude` line pulls in; a path
    built from variables is not one we can resolve, so it is skipped."""
    match = _INCLUDE.match(line)
    if match is None:
        return []
    return [directory / word for word in match.group(1).split() if "$" not in word]


def _collect(path: Path, found: list[str], seen: set[Path], depth: int) -> None:
    resolved = path.resolve()
    if depth > _MAX_INCLUDE_DEPTH or resolved in seen:
        return
    seen.add(resolved)
    try:
        text = path.read_text(errors="replace")
    except OSError:  # an `-include` of something not generated yet, or unreadable
        return
    for line in text.splitlines():
        found.extend(target for target in rule_targets(line) if target not in found)
        for included in _included(line, path.parent):
            _collect(included, found, seen, depth + 1)


def targets(root: Path) -> list[str]:
    """Every target the worktree's makefile defines, in file order (make's
    default goal first); empty where there is no makefile."""
    path = makefile_path(root)
    if path is None:
        return []
    found: list[str] = []
    _collect(path, found, set(), 0)
    return found


def visible_targets(config: Config, root: Path) -> list[str]:
    """The targets offered by name — completions, editor lists. `wf make`
    itself is not restricted by these: hiding a target keeps it out of the
    way, it does not take it away."""
    spec = config.make
    if spec.hidden or not available(root):
        return []
    if spec.show_scripts:
        allowed = frozenset(spec.show_scripts)
        return [target for target in targets(root) if target in allowed]
    left_out = frozenset(spec.hide_scripts)
    return [target for target in targets(root) if target not in left_out]


def spec_for(config: Config, target: str) -> ScriptSpec:
    """The synthetic `scripts` entry a target runs as."""
    return ScriptSpec(
        command=f"make {shlex.quote(target)}",
        exclusive=target in config.make.exclusive_scripts,
    )
