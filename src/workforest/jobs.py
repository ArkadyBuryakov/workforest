"""Running-script records, and stopping a running script.

One JSON record per running `wf run`, under the repository's common git
dir so every worktree of a project sees the same set:
`<common>/workforest/running/<script>/<worktree-name>`. A record is a
hint, never the truth: it outlives a `kill -9` or a reboot, so whoever
reads one verifies that its processes are alive, from this boot, and
still the group we started, and drops it otherwise.

Cleanup belongs to the owning `wf run`: it runs the script's `cleanup`
once its command ends — however it ended — and only then removes its
record. Whoever stops it (`wf stop`, or an `exclusive` script starting
elsewhere) therefore signals the victim's process group and waits for
the record to disappear, which is the moment the victim's cleanup has
finished. Only for an orphan (owner dead, group alive) does the stopper
run the cleanup itself.
"""

import contextlib
import enum
import json
import os
import signal
import subprocess
import sys
import time
from collections.abc import Callable
from dataclasses import asdict, dataclass
from pathlib import Path

from workforest import output

RUNNING_SUBDIR = Path("workforest") / "running"
LOGS_SUBDIR = Path("workforest") / "logs"
_POLL_INTERVAL = 0.05


@dataclass(slots=True, frozen=True)
class JobRecord:
    script: str
    worktree: str  # absolute path
    branch: str
    pgid: int  # the command's process group (its own leader)
    owner_pid: int  # the `wf run` waiting on it
    boot_id: str
    started_at: float  # epoch seconds
    stopped_by: str | None = None  # who signalled it, e.g. "`wf stop` in 'feat'"


@dataclass(slots=True, frozen=True)
class Job:
    path: Path
    record: JobRecord


class JobState(enum.Enum):
    LIVE = "live"  # owner and group alive: a running `wf run`
    ORPHAN = "orphan"  # group alive, owner gone (killed -9): nobody will clean up
    STALE = "stale"  # nothing left of it, or from another boot


def boot_id() -> str:
    """An identifier that changes on reboot, or "" where none is readable."""
    try:
        return Path("/proc/sys/kernel/random/boot_id").read_text().strip()
    except OSError:
        pass
    if sys.platform == "darwin":  # pragma: no cover - macOS only
        result = subprocess.run(
            ["sysctl", "-n", "kern.boottime"], capture_output=True, text=True, check=False
        )
        return result.stdout.strip()
    return ""


def records_dir(common_dir: Path, script: str) -> Path:
    return common_dir / RUNNING_SUBDIR / script


def record_path(common_dir: Path, script: str, worktree: Path) -> Path:
    return records_dir(common_dir, script) / worktree.name


def log_path(common_dir: Path, script: str, worktree: Path) -> Path:
    """Where a `background` run's output goes; overwritten by the next run
    and kept afterwards, so a crash can be read up on."""
    return common_dir / LOGS_SUBDIR / script / f"{worktree.name}.log"


def write_record(path: Path, record: JobRecord) -> None:
    """Atomic: a reader never sees a partial file."""
    path.parent.mkdir(parents=True, exist_ok=True)
    tmp = path.with_name(f".{path.name}.tmp")
    tmp.write_text(json.dumps(asdict(record)))
    tmp.replace(path)


def read_record(path: Path) -> JobRecord | None:
    """None when the file is gone or unreadable (a corrupt one is removed)."""
    try:
        return JobRecord(**json.loads(path.read_text()))
    except FileNotFoundError:
        return None
    except OSError, ValueError, TypeError:
        path.unlink(missing_ok=True)
        return None


def jobs_for(common_dir: Path, script: str) -> list[Job]:
    directory = records_dir(common_dir, script)
    if not directory.is_dir():
        return []
    jobs = []
    for path in sorted(directory.iterdir()):
        if path.name.startswith("."):
            continue
        record = read_record(path)
        if record is not None:
            jobs.append(Job(path, record))
    return jobs


def _alive(pid: int) -> bool:
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return False
    except PermissionError:
        return True
    return True


def _leads_group(pgid: int) -> bool:
    """Our command is the leader of its own group; a recycled pid is
    unlikely to be."""
    try:
        return os.getpgid(pgid) == pgid
    except ProcessLookupError:
        return False


def classify(record: JobRecord) -> JobState:
    if record.boot_id != boot_id() or not _leads_group(record.pgid):
        return JobState.STALE
    return JobState.LIVE if _alive(record.owner_pid) else JobState.ORPHAN


def process_started_before(pid: int, when: float) -> bool | None:
    """Whether `pid` was started before epoch `when` — the definitive
    recycled-pid check. None where /proc is unavailable."""
    try:
        stat = Path(f"/proc/{pid}/stat").read_text()
        proc_stat = Path("/proc/stat").read_text()
    except OSError:
        return None
    # Field 22 (1-based) is the start time in clock ticks since boot; the
    # command name in field 2 may contain spaces, so split after its ')'.
    ticks = int(stat.rpartition(")")[2].split()[19])
    btime = next(
        int(line.split()[1]) for line in proc_stat.splitlines() if line.startswith("btime")
    )
    return btime + ticks / os.sysconf("SC_CLK_TCK") < when


def signal_group(pgid: int, signum: signal.Signals) -> None:
    with contextlib.suppress(ProcessLookupError):
        os.killpg(pgid, signum)


def _wait_until(condition: Callable[[], bool], timeout: float) -> bool:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if condition():
            return True
        time.sleep(_POLL_INTERVAL)
    return condition()


def _terminate_group(pgid: int, *, done: Callable[[], bool], timeout: float) -> None:
    """SIGTERM the group and wait for `done`; escalate to SIGKILL."""
    signal_group(pgid, signal.SIGTERM)
    if _wait_until(done, timeout):
        return
    output.warn(f"process group {pgid} ignored SIGTERM for {timeout:g}s, killing it")
    signal_group(pgid, signal.SIGKILL)
    _wait_until(done, timeout)


def stop(
    job: Job,
    *,
    by: str,
    timeout: float,
    orphan_cleanup: Callable[[JobRecord], None] | None = None,
) -> None:
    """Stop a running instance: SIGTERM its group, SIGKILL after `timeout`
    seconds. `by` is recorded for the victim's own report.

    A live job's owner runs its own cleanup and removes the record; we wait
    for that. An orphan is killed here and its cleanup run via
    `orphan_cleanup` — but only when /proc confirms the pid is not a
    recycled one; elsewhere it is reported and left alone.
    """
    record = job.record
    label = f"{record.script!r} in {Path(record.worktree).name!r}"
    match classify(record):
        case JobState.STALE:
            job.path.unlink(missing_ok=True)
        case JobState.LIVE:
            output.info(f"stopping {label} (pgid {record.pgid})")
            write_record(job.path, JobRecord(**{**asdict(record), "stopped_by": by}))
            _terminate_group(record.pgid, done=lambda: not job.path.exists(), timeout=timeout)
            if job.path.exists():
                output.warn(f"{label}: its `wf run` did not finish; dropping the record")
                job.path.unlink(missing_ok=True)
        case JobState.ORPHAN:
            verified = process_started_before(record.pgid, record.started_at)
            if verified is None:
                output.warn(
                    f"{label} (pgid {record.pgid}) may still be running without its "
                    "`wf run`; cannot verify, so leaving it alone"
                )
                job.path.unlink(missing_ok=True)
                return
            if not verified:
                job.path.unlink(missing_ok=True)  # pid recycled: nothing of ours left
                return
            output.info(f"stopping orphaned {label} (pgid {record.pgid})")
            _terminate_group(
                record.pgid, done=lambda: not _leads_group(record.pgid), timeout=timeout
            )
            if orphan_cleanup is not None:
                orphan_cleanup(record)
            job.path.unlink(missing_ok=True)
