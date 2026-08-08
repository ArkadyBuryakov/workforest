"""Interactive mode: an fzf-driven port of the MVP's worktree-tui.

Mode tabs (←/→, alt-h/alt-l), an opener carousel for CREATE/OPEN
(ctrl-←/ctrl-→), Enter accepts (in CREATE a non-matching query becomes a new
branch), Esc quits, DELETE stays in the loop for bulk cleanup.

Everything except the fzf subprocess itself is pure and unit-tested; fzf is
the one sanctioned external tool here (DESIGN §6.2).
"""

import os
import shutil
import subprocess
from dataclasses import dataclass

from workforest import commands, completions
from workforest.commands import CommandResult, Context
from workforest.errors import WorkforestError

BASE_MODES = ("create", "open", "checkout", "delete")

_PROMPTS = {
    "create": "Branch/New: ",
    "open": "Open: ",
    "checkout": "Checkout: ",
    "delete": "Delete: ",
    "claude": "Copy session: ",
}

_EXPECT_KEYS = "left,right,alt-h,alt-l,ctrl-left,ctrl-right,esc"


def _claude_active(ctx: Context) -> bool:
    try:
        from workforest.integrations import claude
    except ImportError:  # pragma: no cover
        return False
    return claude.available() and ctx.cwd_root != ctx.main


def available_modes(ctx: Context) -> tuple[str, ...]:
    if _claude_active(ctx):
        return (*BASE_MODES, "claude")
    return BASE_MODES


def build_header(modes: tuple[str, ...], current: str) -> str:
    tabs = [f"[{m.upper()}]" if m == current else f" {m.upper()} " for m in modes]
    return " │ ".join(tabs)


def build_opener_line(names: list[str], current_idx: int) -> str:
    cells = [f"[{n}]" if i == current_idx else f" {n} " for i, n in enumerate(names)]
    return "  " + " | ".join(cells)


def cycle(length: int, idx: int, direction: int) -> int:
    return (idx + direction) % length


def mode_has_opener(mode: str) -> bool:
    return mode in ("create", "open")


def opener_carousel(ctx: Context) -> list[tuple[str, str | None]]:
    """(label, opener_arg) pairs: config `openers` keys, or the derived
    fallback pair — default opener and $SHELL (DESIGN §3.4)."""
    if ctx.config.openers:
        return [(name, name) for name in ctx.config.openers]
    entries: list[tuple[str, str | None]] = [("edit", None)]
    if shell := os.environ.get("SHELL"):
        entries.append(("shell", shell))
    return entries


def candidates(ctx: Context, mode: str) -> list[str]:
    match mode:
        case "create":
            return completions.complete("branches")
        case "claude":
            from workforest.integrations import claude

            return [
                f"{sid}\t{desc}" for sid, desc in claude.list_new_sessions(ctx.main, ctx.cwd_root)
            ]
        case _:
            return [w.name for w in commands.managed_worktrees(ctx)]


@dataclass(slots=True)
class FzfResult:
    query: str
    key: str  # one of _EXPECT_KEYS, or "" for plain enter
    selection: str


def parse_fzf_output(raw: str) -> FzfResult:
    lines = raw.split("\n")
    return FzfResult(
        query=lines[0] if len(lines) > 0 else "",
        key=lines[1] if len(lines) > 1 else "",
        selection=lines[2] if len(lines) > 2 else "",
    )


def _fzf_args(mode: str, opener_line: str | None) -> list[str]:
    accept = "accept-or-print-query" if mode == "create" else "accept"
    args = [
        "fzf",
        f"--prompt={_PROMPTS[mode]}",
        "--header-first",
        f"--expect={_EXPECT_KEYS}",
        f"--bind=enter:{accept}",
        "--bind=alt-j:down,alt-k:up",
        "--no-info",
        "--layout=reverse",
        "--height=~50%",
        "--border=rounded",
        "--margin=1,2",
        "--padding=1",
        "--print-query",
    ]
    if opener_line is not None:
        args += [f"--preview=echo {opener_line!r}", "--preview-window=bottom,1,border-top"]
    if mode == "claude":
        args += ["--delimiter=\t", "--tabstop=4"]
    return args


def _run_fzf(  # pragma: no cover - real fzf needs a terminal
    mode: str, header: str, data: list[str], opener_line: str | None
) -> FzfResult | None:
    args = [*_fzf_args(mode, opener_line), f"--header={header}"]
    result = subprocess.run(
        args, input="\n".join(data), capture_output=True, text=True, check=False
    )
    if result.returncode in (1, 130) and not result.stdout:
        return None  # ctrl-c / aborted
    return parse_fzf_output(result.stdout)


def _execute(ctx: Context, mode: str, selection: str, opener_arg: str | None) -> CommandResult:
    match mode:
        case "create":
            return commands.cmd_create(ctx, selection, opener=opener_arg)
        case "open":
            return commands.cmd_open(ctx, selection, opener=opener_arg)
        case "checkout":
            return commands.cmd_checkout(ctx, selection)
        case "delete":
            return commands.cmd_delete(ctx, [selection])
        case "claude":
            from workforest.integrations import claude

            claude.cmd_copy_session(selection.split("\t", 1)[0])
    return None  # pragma: no cover


def run(initial_mode: str | None = None) -> CommandResult:  # pragma: no cover - loop
    """The interactive loop. Excluded from coverage: every helper it calls
    is unit-tested; this function only wires them to the fzf subprocess."""
    if shutil.which("fzf") is None:
        raise WorkforestError(
            "the TUI requires fzf (e.g. pacman -S fzf); "
            "all actions are also available as plain subcommands"
        )
    ctx = commands.build_context()
    modes = available_modes(ctx)

    mode = initial_mode if initial_mode in modes else None
    if mode is None:
        mode = "open" if commands.managed_worktrees(ctx) else "create"
    opener_idx = 0

    while True:
        carousel = opener_carousel(ctx)
        opener_line = (
            build_opener_line([label for label, _ in carousel], opener_idx)
            if mode_has_opener(mode)
            else None
        )
        result = _run_fzf(mode, build_header(modes, mode), candidates(ctx, mode), opener_line)
        if result is None:
            return None
        match result.key:
            case "left" | "alt-h":
                mode = modes[cycle(len(modes), modes.index(mode), -1)]
            case "right" | "alt-l":
                mode = modes[cycle(len(modes), modes.index(mode), +1)]
            case "ctrl-left":
                if mode_has_opener(mode):
                    opener_idx = cycle(len(carousel), opener_idx, -1)
            case "ctrl-right":
                if mode_has_opener(mode):
                    opener_idx = cycle(len(carousel), opener_idx, +1)
            case "esc":
                return None
            case _:  # enter
                selection = result.selection or (result.query if mode == "create" else "")
                if not selection:
                    continue
                opener_arg = carousel[opener_idx][1] if mode_has_opener(mode) else None
                outcome = _execute(ctx, mode, selection, opener_arg)
                if mode == "delete":
                    continue  # bulk cleanup: stay in the loop
                return outcome
