"""Claude Code session integration: copy a session from the main worktree's
project dir into the current worktree's, rewriting cwd fields (DESIGN §3.7).

EXPERIMENTAL: this reads and writes Claude Code's private on-disk state
(~/.claude/projects layout, session .jsonl format, history.jsonl), none of
which is a stable interface — any Claude Code update may break it.

Pure file operations — no external binary. Lines are rewritten by JSON
parsing, never by string substitution.
"""

import json
import shutil
from dataclasses import dataclass
from pathlib import Path
from typing import Any

from workforest import gitutil, output
from workforest.errors import WorkforestError

_DESCRIPTION_LIMIT = 80


@dataclass(slots=True, frozen=True)
class Session:
    id: str
    description: str


def claude_dir() -> Path:
    return Path.home() / ".claude"


def available() -> bool:
    return claude_dir().is_dir()


def project_dir(path: Path) -> Path:
    """Claude Code encodes a project path by replacing '/' and '.' with '-'."""
    encoded = str(path).replace("/", "-").replace(".", "-")
    return claude_dir() / "projects" / encoded


def _history_entries() -> list[dict[str, Any]]:
    history = claude_dir() / "history.jsonl"
    if not history.is_file():
        return []
    entries: list[dict[str, Any]] = []
    for line in history.read_text().splitlines():
        try:
            entry = json.loads(line)
        except json.JSONDecodeError:
            continue
        if isinstance(entry, dict):
            entries.append(entry)
    return entries


def list_sessions(main: Path) -> list[Session]:
    """Sessions of the main worktree's project, with history descriptions."""
    directory = project_dir(main)
    if not directory.is_dir():
        return []
    session_ids = sorted(f.stem for f in directory.glob("*.jsonl"))
    if not session_ids:
        return []
    descriptions: dict[str, str] = {}
    for entry in _history_entries():
        if entry.get("project") != str(main):
            continue
        session_id = entry.get("sessionId")
        if not isinstance(session_id, str) or session_id in descriptions:
            continue
        display = entry.get("display")
        text = display if isinstance(display, str) and display else "(no description)"
        if len(text) > _DESCRIPTION_LIMIT:
            text = text[: _DESCRIPTION_LIMIT - 3] + "..."
        descriptions[session_id] = text
    return [Session(sid, descriptions.get(sid, "(no description)")) for sid in session_ids]


def list_new_sessions(main: Path, current: Path) -> list[Session]:
    """Sessions not yet copied into the current worktree's project dir."""
    current_dir = project_dir(current)
    return [
        session
        for session in list_sessions(main)
        if not (current_dir / f"{session.id}.jsonl").is_file()
    ]


def _rewrite_line(line: str, main: Path, current: Path) -> str:
    try:
        entry = json.loads(line)
    except json.JSONDecodeError:
        return line
    if isinstance(entry, dict) and entry.get("cwd") == str(main):
        entry["cwd"] = str(current)
        return json.dumps(entry, separators=(",", ":"))
    return line


def copy_session(session_id: str, *, main: Path, current: Path) -> None:
    src_dir = project_dir(main)
    dst_dir = project_dir(current)
    src = src_dir / f"{session_id}.jsonl"
    if not src.is_file():
        raise WorkforestError(f"session {session_id!r} not found in {src_dir}")
    dst = dst_dir / f"{session_id}.jsonl"
    if dst.exists():
        output.warn(f"session {session_id!r} already exists in {dst_dir}")
        return
    dst_dir.mkdir(parents=True, exist_ok=True)

    rewritten = [_rewrite_line(line, main, current) for line in src.read_text().splitlines()]
    dst.write_text("\n".join(rewritten) + "\n")

    session_assets = src_dir / session_id
    if session_assets.is_dir():
        shutil.copytree(session_assets, dst_dir / session_id, dirs_exist_ok=True)

    # Append a rewritten history entry so Claude Code discovers the copy.
    for entry in _history_entries():
        if entry.get("sessionId") == session_id:
            entry["project"] = str(current)
            history = claude_dir() / "history.jsonl"
            with history.open("a") as fh:
                fh.write(json.dumps(entry, separators=(",", ":")) + "\n")
            break

    output.success(f"copied session {session_id!r} to {dst_dir}")


def cmd_copy_session(session_id: str) -> None:
    """CLI entry: must run from a non-main worktree."""
    root = gitutil.repo_root()
    main = gitutil.main_worktree(root)
    if root == main:
        raise WorkforestError("copy-session must run from a non-main worktree")
    copy_session(session_id, main=main, current=root)
