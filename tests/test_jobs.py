"""jobs: running-script records, liveness classification, preemption."""

import os
import signal
import subprocess
import threading
import time
from collections.abc import Callable, Iterator
from pathlib import Path

import pytest

from workforest import jobs
from workforest.jobs import Job, JobRecord, JobState

HAS_PROC = Path("/proc/stat").is_file()


def wait_for(condition: Callable[[], bool], timeout: float = 5.0) -> None:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if condition():
            return
        time.sleep(0.02)
    raise AssertionError("condition not met in time")


def _reaped_pid() -> int:
    process = subprocess.Popen(["true"])
    process.wait()
    return process.pid


def reap_like_init(process: subprocess.Popen[bytes]) -> None:
    """An orphan's parent is gone, so init reaps it the moment it dies; the
    test process is the parent here, so stand in for init."""
    threading.Thread(target=process.wait, daemon=True).start()


@pytest.fixture
def sleeper() -> Iterator[Callable[[str], subprocess.Popen[bytes]]]:
    """Spawn a command in a process group of its own, like `wf run` does;
    whatever is still alive at teardown is killed."""
    spawned: list[subprocess.Popen[bytes]] = []

    def _spawn(command: str = "sleep 30") -> subprocess.Popen[bytes]:
        process = subprocess.Popen(["/bin/sh", "-c", command], process_group=0)
        spawned.append(process)
        return process

    yield _spawn
    for process in spawned:
        if process.poll() is None:
            os.killpg(process.pid, signal.SIGKILL)
            process.wait()


def make_record(pgid: int, *, owner_pid: int | None = None, **overrides: object) -> JobRecord:
    fields: dict[str, object] = {
        "script": "dev",
        "worktree": "/tmp/wt/feat",
        "branch": "feat",
        "pgid": pgid,
        "owner_pid": os.getpid() if owner_pid is None else owner_pid,
        "boot_id": jobs.boot_id(),
        "started_at": time.time(),
    }
    fields.update(overrides)
    return JobRecord(**fields)  # type: ignore[arg-type]


class TestRecords:
    def test_round_trip_and_layout(self, tmp_path: Path) -> None:
        record = make_record(1234, stopped_by="other")
        path = jobs.record_path(tmp_path, "dev", Path("/x/worktrees/api/feat"))
        assert path == tmp_path / "workforest" / "running" / "dev" / "feat"
        jobs.write_record(path, record)
        assert jobs.read_record(path) == record
        assert not list(path.parent.glob(".*"))  # no temp file left behind
        assert jobs.log_path(tmp_path, "dev", Path("/x/worktrees/api/feat")) == (
            tmp_path / "workforest" / "logs" / "dev" / "feat.log"
        )

    def test_missing_and_corrupt(self, tmp_path: Path) -> None:
        path = tmp_path / "dev" / "feat"
        assert jobs.read_record(path) is None
        path.parent.mkdir()
        path.write_text("{not json")
        assert jobs.read_record(path) is None
        assert not path.exists()
        path.write_text('{"script": "dev"}')  # missing fields
        assert jobs.read_record(path) is None
        assert not path.exists()

    def test_jobs_for_lists_readable_records(self, tmp_path: Path) -> None:
        assert jobs.jobs_for(tmp_path, "dev") == []
        a = make_record(1)
        b = make_record(2)
        jobs.write_record(jobs.record_path(tmp_path, "dev", Path("/w/feat-a")), a)
        jobs.write_record(jobs.record_path(tmp_path, "dev", Path("/w/feat-b")), b)
        (jobs.records_dir(tmp_path, "dev") / ".feat-c.tmp").write_text("partial")
        (jobs.records_dir(tmp_path, "dev") / "feat-d").write_text("garbage")
        found = jobs.jobs_for(tmp_path, "dev")
        assert [job.record for job in found] == [a, b]
        assert [job.path.name for job in found] == ["feat-a", "feat-b"]
        assert jobs.jobs_for(tmp_path, "other") == []

    def test_boot_id_is_stable(self) -> None:
        assert jobs.boot_id() == jobs.boot_id()

    def test_running_scripts_groups_live_records_by_worktree(
        self, tmp_path: Path, sleeper: Callable[[str], subprocess.Popen[bytes]]
    ) -> None:
        assert jobs.running_scripts(tmp_path) == {}
        live = sleeper("sleep 30").pid
        for script, worktree in (("dev", "/w/feat"), ("build", "/w/feat"), ("dev", "/w/other")):
            jobs.write_record(
                jobs.record_path(tmp_path, script, Path(worktree)),
                make_record(live, script=script, worktree=worktree),
            )
        # A dead group: a record nobody cleaned up, which counts for nothing.
        jobs.write_record(
            jobs.record_path(tmp_path, "stale", Path("/w/feat")),
            make_record(_reaped_pid(), script="stale", worktree="/w/feat"),
        )
        (tmp_path / jobs.RUNNING_SUBDIR / "not-a-dir").write_text("")
        assert jobs.running_scripts(tmp_path) == {
            Path("/w/feat"): ["build", "dev"],
            Path("/w/other"): ["dev"],
        }


class TestClassify:
    def test_live(self, sleeper: Callable[[str], subprocess.Popen[bytes]]) -> None:
        process = sleeper("sleep 30")
        assert jobs.classify(make_record(process.pid)) is JobState.LIVE

    def test_orphan_when_owner_is_gone(
        self, sleeper: Callable[[str], subprocess.Popen[bytes]]
    ) -> None:
        process = sleeper("sleep 30")
        assert jobs.classify(make_record(process.pid, owner_pid=_reaped_pid())) is JobState.ORPHAN

    def test_stale_when_group_is_gone(self) -> None:
        assert jobs.classify(make_record(_reaped_pid())) is JobState.STALE

    def test_stale_when_pid_is_not_a_group_leader(self) -> None:
        # Our own pid leads no group of its own under pytest... unless it does;
        # a child in our group is never a leader.
        process = subprocess.Popen(["sleep", "30"])
        try:
            assert jobs.classify(make_record(process.pid)) is JobState.STALE
        finally:
            process.kill()
            process.wait()

    def test_stale_from_another_boot(
        self, sleeper: Callable[[str], subprocess.Popen[bytes]]
    ) -> None:
        process = sleeper("sleep 30")
        assert jobs.classify(make_record(process.pid, boot_id="previous-boot")) is JobState.STALE

    def test_alive_treats_permission_error_as_alive(self, monkeypatch: pytest.MonkeyPatch) -> None:
        def denied(pid: int, sig: int) -> None:
            raise PermissionError

        monkeypatch.setattr(os, "kill", denied)
        assert jobs._alive(1) is True


@pytest.mark.skipif(not HAS_PROC, reason="needs /proc")
class TestProcessStartedBefore:
    def test_against_own_process(self) -> None:
        assert jobs.process_started_before(os.getpid(), time.time() + 1) is True
        assert jobs.process_started_before(os.getpid(), 0.0) is False

    def test_unknown_pid(self) -> None:
        assert jobs.process_started_before(_reaped_pid(), time.time()) is None


class TestStop:
    def test_stale_record_is_dropped_silently(self, tmp_path: Path) -> None:
        path = tmp_path / "feat"
        record = make_record(_reaped_pid())
        jobs.write_record(path, record)
        jobs.stop(Job(path, record), by="main", timeout=5.0)
        assert not path.exists()

    def test_live_job_is_signalled_and_waited_for(
        self,
        tmp_path: Path,
        sleeper: Callable[[str], subprocess.Popen[bytes]],
        capsys: pytest.CaptureFixture[str],
    ) -> None:
        process = sleeper("sleep 30")
        path = tmp_path / "feat"
        jobs.write_record(path, make_record(process.pid))
        seen: dict[str, object] = {}

        def owner() -> None:
            # What the victim's `wf run` does: notice its command died, read
            # who did it, run cleanup, then remove the record.
            process.wait()
            record = jobs.read_record(path)
            seen["stopped_by"] = record.stopped_by if record else None
            time.sleep(0.2)  # cleanup takes a moment
            seen["cleanup_done"] = time.monotonic()
            path.unlink()

        thread = threading.Thread(target=owner)
        thread.start()
        jobs.stop(Job(path, jobs.read_record(path)), by="main", timeout=5.0)  # type: ignore[arg-type]
        returned = time.monotonic()
        thread.join()

        assert process.returncode == -signal.SIGTERM
        assert seen["stopped_by"] == "main"
        assert returned >= seen["cleanup_done"]  # type: ignore[operator]
        assert not path.exists()
        assert "stopping 'dev' in 'feat'" in capsys.readouterr().err

    def test_live_job_escalates_to_sigkill(
        self,
        tmp_path: Path,
        sleeper: Callable[[str], subprocess.Popen[bytes]],
        capsys: pytest.CaptureFixture[str],
    ) -> None:
        ready = tmp_path / "ready"
        process = sleeper(f'trap "" TERM; touch {ready}; sleep 30')
        wait_for(ready.exists)
        path = tmp_path / "feat"
        record = make_record(process.pid)
        jobs.write_record(path, record)
        threading.Thread(target=lambda: (process.wait(), path.unlink())).start()

        jobs.stop(Job(path, record), by="main", timeout=0.3)

        wait_for(lambda: process.poll() is not None)
        assert process.returncode == -signal.SIGKILL
        assert "ignored SIGTERM for 0.3s, killing it" in capsys.readouterr().err

    def test_owner_that_never_finishes_has_its_record_dropped(
        self,
        tmp_path: Path,
        sleeper: Callable[[str], subprocess.Popen[bytes]],
        capsys: pytest.CaptureFixture[str],
    ) -> None:
        process = sleeper("sleep 30")
        path = tmp_path / "feat"
        record = make_record(process.pid)
        jobs.write_record(path, record)

        jobs.stop(Job(path, record), by="main", timeout=0.2)

        assert process.wait() == -signal.SIGTERM
        assert not path.exists()
        assert "did not finish; dropping the record" in capsys.readouterr().err

    def test_orphan_is_killed_and_cleaned_up_here(
        self,
        tmp_path: Path,
        sleeper: Callable[[str], subprocess.Popen[bytes]],
        monkeypatch: pytest.MonkeyPatch,
        capsys: pytest.CaptureFixture[str],
    ) -> None:
        process = sleeper("sleep 30")
        path = tmp_path / "feat"
        record = make_record(process.pid, owner_pid=_reaped_pid())
        jobs.write_record(path, record)
        monkeypatch.setattr(jobs, "process_started_before", lambda pid, when: True)
        cleaned: list[JobRecord] = []
        reap_like_init(process)

        jobs.stop(Job(path, record), by="main", timeout=5.0, orphan_cleanup=cleaned.append)

        assert process.returncode == -signal.SIGTERM
        assert cleaned == [record]
        assert not path.exists()
        assert "stopping orphaned 'dev' in 'feat'" in capsys.readouterr().err

    @pytest.mark.skipif(not HAS_PROC, reason="needs /proc")
    def test_orphan_check_is_real_on_linux(
        self, tmp_path: Path, sleeper: Callable[[str], subprocess.Popen[bytes]]
    ) -> None:
        process = sleeper("sleep 30")
        time.sleep(0.05)
        path = tmp_path / "feat"
        record = make_record(process.pid, owner_pid=_reaped_pid(), started_at=time.time())
        jobs.write_record(path, record)
        reap_like_init(process)
        jobs.stop(Job(path, record), by="main", timeout=5.0, orphan_cleanup=None)
        assert process.returncode == -signal.SIGTERM

    def test_orphan_with_recycled_pid_is_left_alone(
        self,
        tmp_path: Path,
        sleeper: Callable[[str], subprocess.Popen[bytes]],
        monkeypatch: pytest.MonkeyPatch,
    ) -> None:
        process = sleeper("sleep 30")
        path = tmp_path / "feat"
        record = make_record(process.pid, owner_pid=_reaped_pid())
        jobs.write_record(path, record)
        monkeypatch.setattr(jobs, "process_started_before", lambda pid, when: False)

        jobs.stop(Job(path, record), by="main", timeout=5.0)

        assert process.poll() is None  # not ours: untouched
        assert not path.exists()

    def test_unverifiable_orphan_is_reported_not_killed(
        self,
        tmp_path: Path,
        sleeper: Callable[[str], subprocess.Popen[bytes]],
        monkeypatch: pytest.MonkeyPatch,
        capsys: pytest.CaptureFixture[str],
    ) -> None:
        process = sleeper("sleep 30")
        path = tmp_path / "feat"
        record = make_record(process.pid, owner_pid=_reaped_pid())
        jobs.write_record(path, record)
        monkeypatch.setattr(jobs, "process_started_before", lambda pid, when: None)

        jobs.stop(Job(path, record), by="main", timeout=5.0)

        assert process.poll() is None
        assert not path.exists()
        assert "cannot verify, so leaving it alone" in capsys.readouterr().err

    def test_signal_group_ignores_a_vanished_group(self) -> None:
        jobs.signal_group(_reaped_pid(), signal.SIGTERM)  # no exception
