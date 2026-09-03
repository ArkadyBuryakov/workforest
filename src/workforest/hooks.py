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

A group (`bulk`, `pipeline`) is run by a supervisor — a fork of us that
leads the process group and takes the terminal exactly as a command
would, so everything above applies to it unchanged. Inside, a pipeline
runs its members one after another like consecutive `wf run`s; a bulk
forks a runner per member, gives each an output channel of its own, and
relays their lines to its stderr prefixed with the member's name. Members
keep their own records, cleanup, and `exclusive` semantics: `wf stop
MEMBER` works while a group runs it.
"""

import contextlib
import errno
import io
import os
import selectors
import shlex
import signal
import subprocess
import sys
import tempfile
import termios
import time
from collections.abc import Iterator
from dataclasses import dataclass, field
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


def _wait(pgid: int, *, tty_fd: int | None) -> int:
    """Wait for the group leader `pgid` (our child), forwarding the signals
    we get to the whole group. On a terminal, a stopped command (Ctrl-Z)
    stops us too — the shell then owns the job — and is resumed with us."""

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
            return os.waitstatus_to_exitcode(status)
    finally:
        for signum, handler in previous.items():
            signal.signal(signum, handler)
        if tty_fd is not None:  # pragma: no cover - needs a terminal
            _give_terminal(tty_fd, os.getpgrp())


@dataclass(slots=True, frozen=True)
class _JobResult:
    code: int  # exit status, or -N when killed by signal N
    stopped_by: str | None


@dataclass(slots=True, frozen=True)
class _Job:
    """One `wf run` invocation, resolved: the script, its final command
    text (empty for a group), and where it runs and is recorded. `tty`
    is whether it may take the terminal's foreground: a bulk's members
    may not, since only one group can have it."""

    config: Config
    spec: ScriptSpec
    name: str
    snippet: str
    cwd: Path
    env: dict[str, str]
    common_dir: Path  # where the record and the log live
    tty: bool = True

    def record_path(self, pid: int) -> Path:
        return jobs.record_path(self.common_dir, self.name, self.cwd, pid)

    def log_path(self, pid: int) -> Path:
        """The log of the instance owned by `pid`: the detached supervisor,
        which names its own log from the inside and is named from the
        outside by the `wf run` that forked it."""
        return jobs.log_path(self.common_dir, self.name, self.cwd, pid)


def _fileno(stream: int | IO[bytes]) -> int:
    return stream if isinstance(stream, int) else stream.fileno()


def _redirect_output(stdout: int | IO[bytes], stderr: int | IO[bytes] | None) -> None:
    """In a forked child: point fds 1/2 (and the Python streams, which under
    a test harness may wrap something else entirely) at the given sinks;
    a None stderr keeps the inherited one. The streams never own their
    descriptor: a redirect within a redirect (a bulk runner under its
    supervisor) drops the previous objects, which must not close 1 and 2."""
    os.dup2(_fileno(stdout), 1)
    if stderr is not None:
        os.dup2(_fileno(stderr), 2)
    sys.stdout = os.fdopen(1, "w", buffering=1, closefd=False)
    sys.stderr = os.fdopen(2, "w", buffering=1, closefd=False)


def _spawn(job: _Job, sink: _Sink, tty_fd: int | None) -> int:
    """Start the job as the leader of a new process group and return its
    pid: `$SHELL -c` for a command, a forked supervisor for a group."""
    if job.spec.command is not None:
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
        return process.pid
    sys.stdout.flush()
    sys.stderr.flush()
    pid = os.fork()
    if pid == 0:  # pragma: no cover - the forked child
        _supervise_group(job, sink, tty_fd)
    # Both sides set the group so that it exists whichever runs first.
    with contextlib.suppress(OSError):
        os.setpgid(pid, pid)
    return pid


def _run_command(job: _Job, record_path: Path) -> _JobResult:
    """Run the job in its own process group with a job record on disk for
    as long as it runs."""
    tty_fd = _controlling_tty() if job.tty else None
    with _diverted_output() as sink:
        pgid = _spawn(job, sink, tty_fd)
        jobs.write_record(
            record_path,
            jobs.JobRecord(
                script=job.name,
                worktree=str(job.cwd),
                branch=job.env.get("WF_BRANCH", ""),
                pgid=pgid,
                owner_pid=os.getpid(),
                boot_id=jobs.boot_id(),
                started_at=time.time(),
            ),
        )
        code = _wait(pgid, tty_fd=tty_fd)
    record = jobs.read_record(record_path)
    return _JobResult(code, record.stopped_by if record else None)


def _run_cleanup(spec: ScriptSpec, name: str, *, cwd: Path, env: dict[str, str]) -> None:
    if spec.cleanup is None:
        return
    output.success(f"running cleanup for {name!r}: {spec.cleanup}")
    code = run_snippet(spec.cleanup, cwd=cwd, env=env)
    if code != 0:
        output.warn(f"cleanup for {name!r} failed with exit code {code}")


def _run_to_completion(job: _Job) -> _JobResult:
    """Command, then cleanup, then — and only then — the record goes. We
    own the run, so our pid names the instance."""
    record_path = job.record_path(os.getpid())
    try:
        result = _run_command(job, record_path)
        _run_cleanup(job.spec, job.name, cwd=job.cwd, env=job.env)
    finally:
        record_path.unlink(missing_ok=True)
    return result


def _interrupted(code: int) -> bool:
    """Killed by SIGINT — or, as a shell ends after its child was, exit
    128+SIGINT: Ctrl-C reached the command either way."""
    return code in (-signal.SIGINT, 128 + signal.SIGINT)


def _failure(result: _JobResult, name: str) -> str | None:
    """What went wrong, or None for success or an interruption."""
    if result.code == 0 or _interrupted(result.code):
        return None
    if result.code < 0:
        message = f"script {name!r} was killed by {signal.Signals(-result.code).name}"
        if result.stopped_by:
            message += f" (stopped by {result.stopped_by})"
        return message
    return f"script {name!r} failed with exit code {result.code}"


def _raise_for(result: _JobResult, name: str) -> None:
    """An interrupted command ends us the way Ctrl-C would, so a shell
    loop around `wf run` aborts."""
    if _interrupted(result.code):
        output.warn(f"script {name!r} was interrupted")
        raise KeyboardInterrupt
    message = _failure(result, name)
    if message is None:
        return
    if result.code < 0:
        raise ScriptKilledError(message, -result.code)
    raise WorkforestError(message)


def _exit_with(code: int) -> None:  # pragma: no cover - ends a forked child
    """End a forked child the way its command ended: by the same signal
    for a signal death (so the parent reports it as such, and SIGINT
    keeps aborting shell loops), else with the status. Never returns."""
    with contextlib.suppress(Exception):
        sys.stderr.flush()
    if code < 0:
        with contextlib.suppress(OSError, ValueError):
            signal.signal(-code, signal.SIG_DFL)
            os.kill(os.getpid(), -code)
        code = 128 - code  # the signal is blocked or ignored: fall back to the convention
    os._exit(code)


def _supervise_detached(job: _Job) -> None:  # pragma: no cover - the forked child
    """The background supervisor: our fork, in a session of its own, with
    the log as its stdout/stderr. Runs the command exactly like the
    foreground path does and exits as `wf run` would; never returns."""
    code = 1
    try:
        os.setsid()
        devnull = os.open(os.devnull, os.O_RDONLY)
        os.dup2(devnull, 0)
        os.close(devnull)
        log_fd = os.open(job.log_path(os.getpid()), os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o644)
        _redirect_output(log_fd, log_fd)
        os.close(log_fd)
        result = _run_to_completion(job)
        code = result.code if result.code >= 0 else 128 - result.code
        if result.stopped_by:
            output.info(f"stopped by {result.stopped_by}")
    except BaseException as exc:  # nothing may escape into the parent's code path
        output.error(str(exc) or type(exc).__name__)
    finally:
        sys.stderr.flush()
        os._exit(code)


def _log_tail(log_path: Path, lines: int = 10) -> str:
    """The last lines of a log, or "" when there is nothing to read (the
    supervisor died before it could open one)."""
    try:
        text = log_path.read_text(errors="replace")
    except OSError:
        return ""
    return "\n".join(text.strip().splitlines()[-lines:])


def _start_background(job: _Job) -> None:
    """Fork a detached supervisor for the command and return once it is
    clearly running — a command that dies within the grace period is
    reported with the tail of its log instead of failing invisibly. The
    supervisor writes the log; its pid is what names it."""
    job.log_path(os.getpid()).parent.mkdir(parents=True, exist_ok=True)  # one dir per script
    sys.stdout.flush()
    sys.stderr.flush()
    pid = os.fork()
    if pid == 0:  # pragma: no cover - the forked child
        _supervise_detached(job)
    log_path = job.log_path(pid)
    deadline = time.monotonic() + _GRACE_SECONDS
    while time.monotonic() < deadline:
        reaped, status = os.waitpid(pid, os.WNOHANG)
        if reaped:
            code = os.waitstatus_to_exitcode(status)
            if code == 0:
                output.success(f"{job.name!r} already finished (log: {log_path})")
                return
            tail = _log_tail(log_path)
            message = f"script {job.name!r} exited with status {code} right after launch"
            if tail:
                message += ":\n" + tail
            raise WorkforestError(message)
        time.sleep(0.02)
    output.success(f"started {job.name!r} in the background (pid {pid}, log: {log_path})")


# --- groups -----------------------------------------------------------------


def _supervise_group(job: _Job, sink: _Sink, tty_fd: int | None) -> None:  # pragma: no cover
    """The group supervisor: our fork, leading a group of its own and — on
    a terminal — holding its foreground, as a command would. Runs the
    members and ends the way the group's outcome says; never returns."""
    code = 1
    try:
        os.setpgid(0, 0)
        if tty_fd is not None:
            _give_terminal(tty_fd, os.getpgrp())
        _redirect_output(sink.stdout, sink.stderr)
        code = _run_pipeline(job) if job.spec.pipeline is not None else _run_bulk(job)
    except KeyboardInterrupt:
        code = -signal.SIGINT
    except BaseException as exc:  # nothing may escape into the parent's code path
        output.error(str(exc) or type(exc).__name__)
    finally:
        _exit_with(code)


def _run_step(job: _Job) -> int:
    """A member's run inside a supervisor: `wf run MEMBER`, reporting the
    way cli.py does, but returning the outcome — the exit status, or -N
    for a death by signal N — instead of raising."""
    try:
        if job.spec.background:
            _start_background(job)
            return 0
        output.success(_start_message(job))
        result = _run_to_completion(job)
    except WorkforestError as exc:
        output.error(str(exc))
        return exc.exit_code
    if _interrupted(result.code):
        output.warn(f"script {job.name!r} was interrupted")
        return -signal.SIGINT
    message = _failure(result, job.name)
    if message is not None:
        output.error(message)
    return result.code


def _run_pipeline(job: _Job) -> int:  # pragma: no cover - runs in the supervisor
    """Members one after another, each a `wf run` of its own with the
    terminal; the first failure ends the pipeline with its outcome."""
    assert job.spec.pipeline is not None
    for index, member in enumerate(job.spec.pipeline, 1):
        output.info(f"{job.name!r} step {index}/{len(job.spec.pipeline)}: {member}")
        try:
            step = _prepare(job.config, member, cwd=job.cwd, env=job.env, tty=job.tty)
        except WorkforestError as exc:
            output.error(str(exc))
            return exc.exit_code
        code = _run_step(step)
        if code != 0:
            return code
    return 0


@dataclass(slots=True)
class _Runner:
    """A bulk member's runner process and the read end of its output
    channel; `fd` is -1 once the channel is closed."""

    name: str
    pid: int
    fd: int
    code: int | None = None  # its outcome once reaped: exit status, or -N for signal N


_PALETTE = ("\033[36m", "\033[35m", "\033[34m", "\033[32m", "\033[33m", "\033[91m")
_RESET = "\033[0m"


@dataclass(slots=True)
class _Prefixer:
    """Turns a bulk member's raw output into lines prefixed with its name,
    padded so the prefixes line up and colored when stderr is a terminal.
    Bytes are kept until their line completes; a pty's `\r\n` becomes
    `\n`."""

    names: tuple[str, ...]
    color: bool
    _pending: dict[str, bytes] = field(default_factory=dict)

    def prefix(self, name: str) -> str:
        width = max(len(n) for n in self.names)
        label = f"{name:<{width}} | "
        if self.color:
            paint = _PALETTE[self.names.index(name) % len(_PALETTE)]
            return f"{paint}{label}{_RESET}"
        return label

    def feed(self, name: str, data: bytes) -> str:
        """Complete lines out of `data` (and what was pending), prefixed."""
        buffered = self._pending.pop(name, b"") + data
        *lines, rest = buffered.split(b"\n")
        if rest:
            self._pending[name] = rest
        prefix = self.prefix(name)
        return "".join(
            f"{prefix}{line.removesuffix(b'\r').decode(errors='replace')}\n" for line in lines
        )

    def flush(self, name: str) -> str:
        """A final partial line, terminated."""
        return self.feed(name, b"\n") if self._pending.get(name) else ""


def _open_channel(*, tty: bool) -> tuple[int, int]:
    """(read end, write end) of a member's output channel: a pseudo-terminal
    when our stderr is one, so the member's programs keep coloring their
    output as they do for `wf run`; a pipe otherwise (a log, CI)."""
    if not tty:
        return os.pipe()
    master, slave = os.openpty()
    with contextlib.suppress(OSError, termios.error):
        termios.tcsetwinsize(slave, termios.tcgetwinsize(sys.stderr.fileno()))
    return master, slave


def _drain(runner: _Runner, prefixer: _Prefixer, sink: IO[str]) -> None:
    """Relay what the channel holds; close it at EOF (a pty reports that as
    EIO once every writer is gone)."""
    while runner.fd >= 0:
        try:
            data = os.read(runner.fd, 65536)
        except BlockingIOError:
            return
        except OSError as exc:
            if exc.errno != errno.EIO:
                raise
            data = b""
        if data:
            sink.write(prefixer.feed(runner.name, data))
            continue
        sink.write(prefixer.flush(runner.name))
        os.close(runner.fd)
        runner.fd = -1
    sink.flush()


def _pump(runners: list[_Runner], prefixer: _Prefixer, sink: IO[str]) -> None:
    """Relay every runner's output to `sink` until all have ended and their
    channels are drained. A channel a reaped runner left open (a daemon it
    spawned still holds the write end) is drained once more and closed."""
    selector = selectors.DefaultSelector()
    for runner in runners:
        os.set_blocking(runner.fd, False)
        selector.register(runner.fd, selectors.EVENT_READ, runner)
    while any(runner.code is None or runner.fd >= 0 for runner in runners):
        for key, _ in selector.select(timeout=0.05):
            runner = key.data
            _drain(runner, prefixer, sink)
            if runner.fd < 0:
                selector.unregister(key.fd)
        for runner in runners:
            if runner.code is not None:
                continue
            reaped, status = os.waitpid(runner.pid, os.WNOHANG)
            if not reaped:
                continue
            runner.code = os.waitstatus_to_exitcode(status)
            if runner.fd >= 0:
                fd = runner.fd
                _drain(runner, prefixer, sink)
                if runner.fd >= 0:
                    os.close(runner.fd)
                    runner.fd = -1
                selector.unregister(fd)
        sink.flush()
    selector.close()


def _describe_outcome(code: int) -> str:
    if code < 0:
        return f"was killed by {signal.Signals(-code).name}"
    return f"failed with exit code {code}"


def _bulk_outcome(name: str, runners: list[_Runner]) -> int:
    """The group's outcome from its members': success only when all
    succeeded; an interruption wins (Ctrl-C stays Ctrl-C, and each runner
    has already said so), else any other signal death, else the first
    failing member's status."""
    codes = {runner.name: runner.code for runner in runners if runner.code}
    if any(_interrupted(code or 0) for code in codes.values()):
        return -signal.SIGINT
    for member, code in codes.items():
        output.error(f"member {member!r} of {name!r} {_describe_outcome(code or 0)}")
    return (
        next((code for code in codes.values() if code and code < 0), None)
        or next(iter(codes.values()), 0)
        or 0
    )


def _run_member(job: _Job, write_fd: int) -> None:  # pragma: no cover - the forked child
    """A bulk member's runner: reads nothing, writes to its channel, and
    ends the way the member did; never returns."""
    code = 1
    try:
        devnull = os.open(os.devnull, os.O_RDONLY)
        os.dup2(devnull, 0)
        os.close(devnull)
        _redirect_output(write_fd, write_fd)
        os.close(write_fd)
        code = _run_step(job)
    except KeyboardInterrupt:
        code = -signal.SIGINT
    except BaseException as exc:
        output.error(str(exc) or type(exc).__name__)
    finally:
        _exit_with(code)


def _run_bulk(job: _Job) -> int:  # pragma: no cover - runs in the supervisor
    """All members at once, each under a runner of its own that shares our
    process group (so a signal to the group reaches every runner, which
    forwards it to its member) but not the terminal; their output is
    relayed here, line by line, prefixed. Done when all have ended."""
    assert job.spec.bulk is not None
    on_tty = sys.stderr.isatty()
    runners: list[_Runner] = []
    for member in job.spec.bulk:
        step = _prepare(job.config, member, cwd=job.cwd, env=job.env, tty=False)
        read_fd, write_fd = _open_channel(tty=on_tty)
        sys.stdout.flush()
        sys.stderr.flush()
        pid = os.fork()
        if pid == 0:
            os.close(read_fd)
            _run_member(step, write_fd)
        os.close(write_fd)
        runners.append(_Runner(member, pid, read_fd))
    # Signals for us reach the runners too (same group); we only outlive
    # them to relay the rest of their output. Ctrl-Z, which the terminal
    # sends the group, is passed on to the members' groups and back.
    for signum in _FORWARDED_SIGNALS:
        signal.signal(signum, lambda *_: None)
    signal.signal(signal.SIGTSTP, lambda *_: _suspend_members(runners, job))
    signal.signal(signal.SIGCONT, lambda *_: _signal_members(runners, job, signal.SIGCONT))
    prefixer = _Prefixer(job.spec.bulk, color=output.colors_enabled())
    _pump(runners, prefixer, sys.stderr)
    return _bulk_outcome(job.name, runners)


def _signal_members(
    runners: list[_Runner], job: _Job, signum: signal.Signals
) -> None:  # pragma: no cover - runs in the supervisor's signal handlers
    common_dir = gitutil.git_common_dir(job.cwd)
    for runner in runners:
        if runner.code is not None:
            continue
        for member in jobs.jobs_for(common_dir, runner.name):
            if member.record.owner_pid == runner.pid:  # this runner's instance, not another
                jobs.signal_group(member.record.pgid, signum)


def _suspend_members(runners: list[_Runner], job: _Job) -> None:  # pragma: no cover
    _signal_members(runners, job, signal.SIGTSTP)
    os.kill(os.getpid(), signal.SIGSTOP)


# --- entry points -------------------------------------------------------------


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
    """The entry's own, else — for a group — the longest any member may
    need (its members are stopped through it), else the global one."""
    if spec.stop_timeout is not None:
        return spec.stop_timeout
    members = [config.scripts[m] for m in spec.members if m in config.scripts]
    if members:
        return max(_stop_timeout(config, member) for member in members)
    return config.stop_timeout


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


def running_scripts(cwd: Path) -> dict[Path, dict[str, int]]:
    """Which scripts of this project are running, by worktree path — the
    records of every worktree, since they share the common git dir."""
    return jobs.running_scripts(gitutil.git_common_dir(cwd))


def _start_message(job: _Job) -> str:
    what = job.snippet if job.spec.command is not None else ", ".join(job.spec.members)
    return f"running {job.name!r} in {job.cwd}: {what}"


def _prepare(
    config: Config,
    name: str,
    *,
    cwd: Path,
    env: dict[str, str],
    extra_args: list[str] | None = None,
    tty: bool = True,
) -> _Job:
    """Resolve a script and clear the way for it: an `exclusive` one first
    stops every running instance in the project (their cleanup included);
    any other simply joins the instances already running here."""
    spec = _resolve_script(config, name)
    snippet = spec.command or ""
    if extra_args:
        if spec.command is None:
            raise WorkforestError(f"{name!r} is a group of scripts and takes no arguments")
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
    # Any number of instances may share a worktree, each recorded and
    # logged under the pid of the run that owns it; what dead ones left
    # behind goes now.
    jobs.prune(common_dir, name, cwd)
    return _Job(config, spec, name, snippet, cwd, env, common_dir, tty)


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
    `wf run make check -j2` runs `make check -j2` for a script defined as
    `make`; a group takes none. `background` overrides the entry's own
    setting.
    """
    job = _prepare(config, name, cwd=cwd, env=env, extra_args=extra_args)
    if job.spec.background if background is None else background:
        _start_background(job)
        return
    output.success(_start_message(job))
    _raise_for(_run_to_completion(job), name)
