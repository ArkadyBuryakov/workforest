"""Creation hooks (symlinks, setup scripts) and named-script execution.

Scripts get exactly one environment variable family, WF_*, and
run via $SHELL -c (sh -c fallback). Their stdout is routed to our stderr so
the cd protocol on stdout stays clean.

A `wf run` command gets a process group of its own so that it can be
stopped as a whole — by `wf stop`, by an `exclusive` script starting
elsewhere, or by a signal forwarded from us. On a terminal the group is
made the foreground one, as a shell would, so Ctrl-C and Ctrl-Z reach the
command directly; we reclaim the terminal when it ends. A `background`
script runs the same way under a detached supervisor (a fork of us) with
its output in a log file. The `cleanup` command then runs however the
command ended, and only afterwards is the job record removed (that
removal is what a stopper waits for).
"""

import contextlib
import io
import os
import shlex
import signal
import subprocess
import sys
import tempfile
import time
from collections.abc import Iterator
from dataclasses import dataclass
from pathlib import Path
from typing import IO

from workforest import gitutil, jobs, output
from workforest.config import Config, ScriptSpec
from workforest.errors import ScriptKilledError, WorkforestError

EXCLUDE_FILE_NAME = "workforest.exclude"


def script_env(
    *,
    main: Path,
    worktree: Path,
    worktrees_dir: Path,
    branch: str | None,
) -> dict[str, str]:
    env = os.environ.copy()
    env.update(
        {
            "WF_MAIN": str(main),
            "WF_NAME": main.name,
            "WF_WORKTREE": str(worktree),
            "WF_WORKTREES_DIR": str(worktrees_dir),
            "WF_BRANCH": branch or "",
        }
    )
    return env


def _shell() -> str:
    return os.environ.get("SHELL") or "sh"


@dataclass(slots=True, frozen=True)
class _Sink:
    """Where a snippet's output goes: `None` inherits ours."""

    stdout: int | IO[bytes]
    stderr: int | IO[bytes] | None


@contextlib.contextmanager
def _diverted_output() -> Iterator[_Sink]:
    """A snippet's stdout goes to our stderr. When that is not a real file
    descriptor (pytest's capsys), both streams go to a temp file that is
    copied to sys.stderr afterwards — no pipe, so a chatty snippet cannot
    deadlock."""
    try:
        stderr_fd: int | None = sys.stderr.fileno()
    except io.UnsupportedOperation, AttributeError:
        stderr_fd = None
    if stderr_fd is not None:
        yield _Sink(stdout=stderr_fd, stderr=None)
        return
    with tempfile.TemporaryFile() as captured:
        yield _Sink(stdout=captured, stderr=captured)
        captured.seek(0)
        sys.stderr.write(captured.read().decode(errors="replace"))


def run_snippet(snippet: str, *, cwd: Path, env: dict[str, str]) -> int:
    """Run a config-defined shell snippet with stdout diverted to stderr."""
    argv = [_shell(), "-c", snippet]
    with _diverted_output() as sink:
        result = subprocess.run(
            argv, cwd=cwd, env=env, stdout=sink.stdout, stderr=sink.stderr, check=False
        )
    return result.returncode


def create_symlinks(config: Config, *, main: Path, worktree: Path) -> list[str]:
    """Symlink configured repo-root-relative paths from main into the
    worktree; returns the created relative paths."""
    created: list[str] = []
    for rel in config.symlinks:
        rel = rel.strip("/")
        if not rel:
            continue
        src = main / rel
        dst = worktree / rel
        if not src.exists():
            output.warn(f"symlink source does not exist, skipping: {src}")
            continue
        if dst.exists() and not dst.is_symlink():
            output.warn(f"destination exists and is not a symlink, skipping: {dst}")
            continue
        dst.parent.mkdir(parents=True, exist_ok=True)
        if dst.is_symlink():
            dst.unlink()
        dst.symlink_to(src)
        output.success(f"symlinked {rel} -> {src}")
        created.append(rel)
    if created:
        exclude_from_git(worktree, created)
    return created


def exclude_from_git(worktree: Path, rel_paths: list[str]) -> None:
    """Hide the given root-relative paths from git status in this worktree
    only, via a per-worktree core.excludesFile seeded with the user's global
    excludes (so overriding the file loses nothing)."""
    git_dir = gitutil.git_dir(worktree)
    exclude_file = git_dir / EXCLUDE_FILE_NAME

    gitutil.set_config(worktree, "extensions.worktreeConfig", "true")
    gitutil.set_config(worktree, "core.excludesFile", str(exclude_file), per_worktree=True)

    lines = ["# Managed by workforest: symlinks from the `symlinks` config key"]
    global_excludes = gitutil.global_excludes_file()
    if global_excludes is not None and global_excludes.is_file():
        lines.append(
            f"# --- snapshot of global core.excludesFile ({global_excludes}), "
            "taken at worktree creation; later edits there do not apply here ---"
        )
        lines.append(global_excludes.read_text().rstrip("\n"))
        lines.append("# --- workforest symlinks ---")
    lines.extend(f"/{rel}" for rel in rel_paths)
    exclude_file.write_text("\n".join(lines) + "\n")
    output.success(f"excluded {len(rel_paths)} symlink(s) from git in this worktree")


def run_setup_scripts(config: Config, *, worktree: Path, env: dict[str, str]) -> int:
    """Run setup_scripts in order; failures warn but do not abort. Returns
    the number of failed scripts."""
    failures = 0
    for snippet in config.setup_scripts:
        output.success(f"running setup script: {snippet}")
        if run_snippet(snippet, cwd=worktree, env=env) != 0:
            output.warn(f"setup script failed: {snippet}")
            failures += 1
    return failures


_FORWARDED_SIGNALS = (signal.SIGINT, signal.SIGTERM, signal.SIGHUP)

# How long a background command gets to fail before we report it as started:
# long enough to catch a bad command, short enough to be imperceptible.
_GRACE_SECONDS = 0.3


def _controlling_tty() -> int | None:
    """A descriptor of our controlling terminal, or None when there is none
    (piped, or under test)."""
    for stream in (sys.stdin, sys.stderr, sys.stdout):
        try:
            fd = stream.fileno()
        except io.UnsupportedOperation, AttributeError, ValueError:
            continue
        if os.isatty(fd):
            return fd
    return None


def _give_terminal(fd: int, pgid: int) -> None:  # pragma: no cover - needs a terminal
    """Make `pgid` the terminal's foreground group. SIGTTOU is what a
    background group gets for trying, so it is ignored around the call."""
    previous = signal.signal(signal.SIGTTOU, signal.SIG_IGN)
    try:
        os.tcsetpgrp(fd, pgid)
    except OSError:
        pass
    finally:
        signal.signal(signal.SIGTTOU, previous)


def _take_terminal_in_child(fd: int) -> None:  # pragma: no cover - runs in the forked child
    """Before exec, the command claims the terminal for its own (new)
    group itself: doing it only from the parent would race the command's
    first read."""
    _give_terminal(fd, os.getpgrp())


def _wait(process: subprocess.Popen[bytes], *, tty_fd: int | None) -> int:
    """Wait for the command's group leader, forwarding the signals we get to
    the whole group. On a terminal, a stopped command (Ctrl-Z) stops us
    too — the shell then owns the job — and is resumed with us."""
    pgid = process.pid

    def forward(signum: int, _frame: object) -> None:
        jobs.signal_group(pgid, signal.Signals(signum))

    previous = {signum: signal.signal(signum, forward) for signum in _FORWARDED_SIGNALS}
    try:
        while True:
            _, status = os.waitpid(pgid, os.WUNTRACED if tty_fd is not None else 0)
            if os.WIFSTOPPED(status):  # pragma: no cover - needs a terminal
                assert tty_fd is not None
                _give_terminal(tty_fd, os.getpgrp())
                os.kill(os.getpid(), signal.SIGSTOP)
                _give_terminal(tty_fd, pgid)
                jobs.signal_group(pgid, signal.SIGCONT)
                continue
            code = os.waitstatus_to_exitcode(status)
            break
    finally:
        for signum, handler in previous.items():
            signal.signal(signum, handler)
        if tty_fd is not None:  # pragma: no cover - needs a terminal
            _give_terminal(tty_fd, os.getpgrp())
    process.returncode = code  # reaped here, not by Popen
    return code


@dataclass(slots=True, frozen=True)
class _JobResult:
    code: int  # exit status, or -N when killed by signal N
    stopped_by: str | None


@dataclass(slots=True, frozen=True)
class _Job:
    """One `wf run` invocation, resolved: the script, its final command
    text, and where it runs and is recorded."""

    spec: ScriptSpec
    name: str
    snippet: str
    cwd: Path
    env: dict[str, str]
    record_path: Path


def _run_command(job: _Job) -> _JobResult:
    """Run the command in its own process group with a job record on disk
    for as long as it runs."""
    tty_fd = _controlling_tty()
    with _diverted_output() as sink:
        try:
            process = subprocess.Popen(
                [_shell(), "-c", job.snippet],
                cwd=job.cwd,
                env=job.env,
                stdout=sink.stdout,
                stderr=sink.stderr,
                process_group=0,
                preexec_fn=(lambda: _take_terminal_in_child(tty_fd))
                if tty_fd is not None
                else None,
            )
        except OSError as exc:
            raise WorkforestError(
                f"cannot run {job.name!r} via $SHELL: {exc.strerror or exc}"
            ) from exc
        jobs.write_record(
            job.record_path,
            jobs.JobRecord(
                script=job.name,
                worktree=str(job.cwd),
                branch=job.env.get("WF_BRANCH", ""),
                pgid=process.pid,
                owner_pid=os.getpid(),
                boot_id=jobs.boot_id(),
                started_at=time.time(),
            ),
        )
        code = _wait(process, tty_fd=tty_fd)
    record = jobs.read_record(job.record_path)
    return _JobResult(code, record.stopped_by if record else None)


def _run_cleanup(spec: ScriptSpec, name: str, *, cwd: Path, env: dict[str, str]) -> None:
    if spec.cleanup is None:
        return
    output.success(f"running cleanup for {name!r}: {spec.cleanup}")
    code = run_snippet(spec.cleanup, cwd=cwd, env=env)
    if code != 0:
        output.warn(f"cleanup for {name!r} failed with exit code {code}")


def _run_to_completion(job: _Job) -> _JobResult:
    """Command, then cleanup, then — and only then — the record goes."""
    try:
        result = _run_command(job)
        _run_cleanup(job.spec, job.name, cwd=job.cwd, env=job.env)
    finally:
        job.record_path.unlink(missing_ok=True)
    return result


def _raise_for(result: _JobResult, name: str) -> None:
    """A command killed by SIGINT ends us the way Ctrl-C would, so a shell
    loop around `wf run` aborts."""
    if result.code == -signal.SIGINT:
        raise KeyboardInterrupt
    if result.code < 0:
        message = f"script {name!r} was killed by {signal.Signals(-result.code).name}"
        if result.stopped_by:
            message += f" (stopped by {result.stopped_by})"
        raise ScriptKilledError(message, -result.code)
    if result.code != 0:
        raise WorkforestError(f"script {name!r} failed with exit code {result.code}")


def _supervise_detached(job: _Job, log_fd: int) -> None:  # pragma: no cover - the forked child
    """The background supervisor: our fork, in a session of its own, with
    the log as its stdout/stderr. Runs the command exactly like the
    foreground path does and exits as `wf run` would; never returns."""
    code = 1
    try:
        os.setsid()
        devnull = os.open(os.devnull, os.O_RDONLY)
        os.dup2(devnull, 0)
        os.dup2(log_fd, 1)
        os.dup2(log_fd, 2)
        os.close(devnull)
        os.close(log_fd)
        # Python-level streams may wrap something else entirely (a test
        # harness's capture); rebind them to the descriptors just set up.
        sys.stdout = os.fdopen(1, "w", buffering=1)
        sys.stderr = os.fdopen(2, "w", buffering=1)
        result = _run_to_completion(job)
        code = result.code if result.code >= 0 else 128 - result.code
        if result.stopped_by:
            output.info(f"stopped by {result.stopped_by}")
    except BaseException as exc:  # nothing may escape into the parent's code path
        output.error(str(exc) or type(exc).__name__)
    finally:
        sys.stderr.flush()
        os._exit(code)


def _start_background(job: _Job, log_path: Path) -> None:
    """Fork a detached supervisor for the command and return once it is
    clearly running — a command that dies within the grace period is
    reported with the tail of its log instead of failing invisibly."""
    log_path.parent.mkdir(parents=True, exist_ok=True)
    log_fd = os.open(log_path, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o644)
    sys.stdout.flush()
    sys.stderr.flush()
    pid = os.fork()
    if pid == 0:  # pragma: no cover - the forked child
        _supervise_detached(job, log_fd)
    os.close(log_fd)
    deadline = time.monotonic() + _GRACE_SECONDS
    while time.monotonic() < deadline:
        reaped, status = os.waitpid(pid, os.WNOHANG)
        if reaped:
            code = os.waitstatus_to_exitcode(status)
            if code == 0:
                output.success(f"{job.name!r} already finished (log: {log_path})")
                return
            tail = log_path.read_text(errors="replace").strip().splitlines()[-10:]
            message = f"script {job.name!r} exited with status {code} right after launch"
            if tail:
                message += ":\n" + "\n".join(tail)
            raise WorkforestError(message)
        time.sleep(0.02)
    output.success(f"started {job.name!r} in the background (pid {pid}, log: {log_path})")


def _orphan_cleanup(
    spec: ScriptSpec, name: str, env: dict[str, str], record: jobs.JobRecord
) -> None:
    """The cleanup of an instance whose own `wf run` is gone, in that
    instance's worktree and with its WF_* values."""
    worktree = Path(record.worktree)
    if not worktree.is_dir():
        output.warn(f"skipping cleanup for {name!r}: {worktree} no longer exists")
        return
    _run_cleanup(
        spec,
        name,
        cwd=worktree,
        env={**env, "WF_WORKTREE": record.worktree, "WF_BRANCH": record.branch},
    )


def _resolve_script(config: Config, name: str) -> ScriptSpec:
    spec = config.scripts.get(name)
    if spec is None:
        available = ", ".join(sorted(config.scripts)) or "none defined"
        raise WorkforestError(f"no script named {name!r} (available: {available})")
    return spec


def _stop_timeout(config: Config, spec: ScriptSpec) -> float:
    return config.stop_timeout if spec.stop_timeout is None else spec.stop_timeout


def _stop_jobs(
    config: Config, name: str, found: list[jobs.Job], *, by: str, env: dict[str, str]
) -> None:
    spec = _resolve_script(config, name)
    for job in found:
        jobs.stop(
            job,
            by=by,
            timeout=_stop_timeout(config, spec),
            orphan_cleanup=lambda record: _orphan_cleanup(spec, name, env, record),
        )


def stop_script(
    config: Config,
    name: str,
    *,
    cwd: Path,
    env: dict[str, str],
    everywhere: bool = False,
) -> None:
    """`wf stop`: stop the script's instance in this worktree, or in every
    worktree of the project. Stale records are dropped, not counted."""
    _resolve_script(config, name)
    common_dir = gitutil.git_common_dir(cwd)
    running = []
    for job in jobs.jobs_for(common_dir, name):
        if jobs.classify(job.record) is jobs.JobState.STALE:
            job.path.unlink(missing_ok=True)
        elif everywhere or Path(job.record.worktree) == cwd:
            running.append(job)
    if not running:
        where = "anywhere in this project" if everywhere else f"in {cwd.name!r}"
        raise WorkforestError(f"{name!r} is not running {where}")
    _stop_jobs(config, name, running, by=f"`wf stop` in {cwd.name!r}", env=env)


def run_named_script(
    config: Config,
    name: str,
    *,
    cwd: Path,
    env: dict[str, str],
    extra_args: list[str] | None = None,
    background: bool | None = None,
) -> None:
    """Run a `scripts` entry from the merged config; raise on failure.

    extra_args are shell-quoted and appended to the command, so
    `wf run make check` runs `make check` for a script defined as `make`.
    An `exclusive` script first stops its running instance anywhere in the
    project (that instance's cleanup included); any other script refuses to
    start while it is already running in this worktree. `background`
    overrides the entry's own setting.
    """
    spec = _resolve_script(config, name)
    snippet = spec.command
    if extra_args:
        snippet = f"{snippet} {' '.join(shlex.quote(arg) for arg in extra_args)}"
    common_dir = gitutil.git_common_dir(cwd)
    if spec.exclusive:
        _stop_jobs(
            config,
            name,
            jobs.jobs_for(common_dir, name),
            by=f"`wf run {name}` in {cwd.name!r}",
            env=env,
        )
    record_path = jobs.record_path(common_dir, name, cwd)
    # One instance per script per worktree: records and logs are keyed that
    # way, and `wf stop NAME` means "the" instance here.
    if (existing := jobs.read_record(record_path)) is not None:
        if jobs.classify(existing) is jobs.JobState.STALE:
            record_path.unlink(missing_ok=True)
        else:
            raise WorkforestError(
                f"{name!r} is already running in {cwd.name!r} (pid {existing.pgid}); "
                f"`wf stop {name}` first"
            )
    job = _Job(spec, name, snippet, cwd, env, record_path)
    if spec.background if background is None else background:
        _start_background(job, jobs.log_path(common_dir, name, cwd))
        return
    output.success(f"running {name!r} in {cwd}: {snippet}")
    _raise_for(_run_to_completion(job), name)
