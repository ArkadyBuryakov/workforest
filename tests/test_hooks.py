"""hooks: symlinks + git-status invisibility, setup scripts, named scripts."""

import io
import os
import signal
import subprocess
import sys
import threading
import time
from collections.abc import Callable
from pathlib import Path

import pytest

from workforest import gitutil, hooks, jobs
from workforest.config import Config, ScriptSpec, load_config
from workforest.errors import ScriptKilledError, WorkforestError

from .conftest import Repo


def sole_log(common: Path, script: str, worktree: Path) -> Path:
    """The log of the only instance of `script` in `worktree`: its name
    carries the pid of a supervisor the test never sees."""
    [path] = sorted((common / jobs.LOGS_SUBDIR / script).glob(f"{worktree.name}.*.log"))
    return path


def make_worktree(repo: Repo, name: str = "feat") -> Path:
    target = repo.path.parent / "worktrees" / repo.path.name / name
    gitutil.worktree_add(repo.path, target, name)
    return target


class TestScriptEnv:
    def test_wf_family_only(self, tmp_path: Path) -> None:
        env = hooks.script_env(
            main=tmp_path / "api",
            worktree=tmp_path / "wt" / "feat",
            worktrees_dir=tmp_path / "wt",
            branch="feature/x",
        )
        assert env["WF_MAIN"] == str(tmp_path / "api")
        assert env["WF_NAME"] == "api"
        assert env["WF_WORKTREE"] == str(tmp_path / "wt" / "feat")
        assert env["WF_WORKTREES_DIR"] == str(tmp_path / "wt")
        assert env["WF_BRANCH"] == "feature/x"
        # no MVP compat aliases
        assert "ROOT_TREE_PATH" not in env
        assert "WORK_TREE_PATH" not in env
        assert "WORKTREES_DIR" not in env

    def test_detached_branch_is_empty(self, tmp_path: Path) -> None:
        env = hooks.script_env(
            main=tmp_path, worktree=tmp_path, worktrees_dir=tmp_path, branch=None
        )
        assert env["WF_BRANCH"] == ""


class TestSymlinks:
    def test_creates_links_and_hides_them_from_git(self, repo: Repo) -> None:
        (repo.path / "node_modules").mkdir()
        (repo.path / ".env").write_text("SECRET=1\n")
        worktree = make_worktree(repo)
        cfg = Config(symlinks=["node_modules", ".env"])

        created = hooks.create_symlinks(cfg, main=repo.path, worktree=worktree)

        assert created == ["node_modules", ".env"]
        assert (worktree / "node_modules").is_symlink()
        assert (worktree / "node_modules").resolve() == repo.path / "node_modules"
        assert (worktree / ".env").read_text() == "SECRET=1\n"
        # invisible to git status in the worktree...
        assert gitutil.status_porcelain(worktree) == ""
        # ...but a plain untracked file still shows up
        repo.make_dirty(worktree=worktree)
        assert "dirty.txt" in gitutil.status_porcelain(worktree)

    def test_main_repo_status_unaffected(self, repo: Repo) -> None:
        (repo.path / ".env").write_text("x\n")
        worktree = make_worktree(repo)
        hooks.create_symlinks(Config(symlinks=[".env"]), main=repo.path, worktree=worktree)
        # .env is untracked in main and must stay visible there
        assert ".env" in gitutil.status_porcelain(repo.path)

    def test_missing_source_skipped(self, repo: Repo) -> None:
        worktree = make_worktree(repo)
        cfg = Config(symlinks=["does-not-exist"])
        assert hooks.create_symlinks(cfg, main=repo.path, worktree=worktree) == []

    def test_existing_file_not_clobbered(self, repo: Repo) -> None:
        (repo.path / ".env").write_text("main\n")
        worktree = make_worktree(repo)
        (worktree / ".env").write_text("precious\n")
        cfg = Config(symlinks=[".env"])
        assert hooks.create_symlinks(cfg, main=repo.path, worktree=worktree) == []
        assert (worktree / ".env").read_text() == "precious\n"

    def test_existing_symlink_replaced(self, repo: Repo) -> None:
        (repo.path / ".env").write_text("x\n")
        worktree = make_worktree(repo)
        (worktree / ".env").symlink_to(repo.path / "README.md")
        cfg = Config(symlinks=[".env"])
        assert hooks.create_symlinks(cfg, main=repo.path, worktree=worktree) == [".env"]
        assert (worktree / ".env").resolve() == repo.path / ".env"

    def test_nested_path_creates_parents(self, repo: Repo) -> None:
        (repo.path / ".vscode").mkdir()
        (repo.path / ".vscode" / "settings.json").write_text("{}\n")
        worktree = make_worktree(repo)
        cfg = Config(symlinks=[".vscode/settings.json"])
        created = hooks.create_symlinks(cfg, main=repo.path, worktree=worktree)
        assert created == [".vscode/settings.json"]
        assert (worktree / ".vscode" / "settings.json").is_symlink()
        assert gitutil.status_porcelain(worktree) == ""

    def test_global_excludes_seeded(self, repo: Repo, tmp_path: Path) -> None:
        global_ignore = tmp_path / "global-ignore"
        global_ignore.write_text("*.log\n")
        subprocess.run(
            ["git", "config", "--global", "core.excludesFile", str(global_ignore)],
            check=True,
        )
        (repo.path / ".env").write_text("x\n")
        worktree = make_worktree(repo)
        hooks.create_symlinks(Config(symlinks=[".env"]), main=repo.path, worktree=worktree)
        # the user's global ignores still apply inside the worktree
        (worktree / "noise.log").write_text("x\n")
        assert gitutil.status_porcelain(worktree) == ""


class TestSetupScripts:
    def test_scripts_run_in_worktree_with_env(self, repo: Repo, tmp_path: Path) -> None:
        worktree = make_worktree(repo)
        out = tmp_path / "hook-out.txt"
        cfg = Config(setup_scripts=[f'echo "$WF_BRANCH in $PWD" > {out}'])
        env = hooks.script_env(
            main=repo.path, worktree=worktree, worktrees_dir=worktree.parent, branch="feat"
        )
        failures = hooks.run_setup_scripts(cfg, worktree=worktree, env=env)
        assert failures == 0
        assert out.read_text() == f"feat in {worktree}\n"

    def test_failure_warns_but_continues(self, repo: Repo, tmp_path: Path) -> None:
        worktree = make_worktree(repo)
        out = tmp_path / "second.txt"
        cfg = Config(setup_scripts=["exit 7", f"touch {out}"])
        env = hooks.script_env(
            main=repo.path, worktree=worktree, worktrees_dir=worktree.parent, branch="feat"
        )
        failures = hooks.run_setup_scripts(cfg, worktree=worktree, env=env)
        assert failures == 1
        assert out.exists()  # the second script still ran


class TestNamedScripts:
    def test_runs_from_cwd_with_env(self, repo: Repo, tmp_path: Path) -> None:
        out = tmp_path / "run-out.txt"
        cfg = Config(scripts={"record": ScriptSpec(f'echo "$WF_MAIN|$WF_NAME|$PWD" > {out}')})
        env = hooks.script_env(
            main=repo.path, worktree=repo.path, worktrees_dir=tmp_path, branch="main"
        )
        hooks.run_named_script(cfg, "record", cwd=repo.path, env=env)
        assert out.read_text() == f"{repo.path}|{repo.path.name}|{repo.path}\n"

    def test_extra_args_appended_and_quoted(self, repo: Repo, tmp_path: Path) -> None:
        out = tmp_path / "args.txt"
        # redirection first, so the appended args become echo's arguments:
        # `echo > out check -j2 'a b'` writes "check -j2 a b"
        cfg = Config(scripts={"echoer": ScriptSpec(f"echo > {out}")})
        env = hooks.script_env(
            main=repo.path, worktree=repo.path, worktrees_dir=repo.path, branch=None
        )
        hooks.run_named_script(
            cfg, "echoer", cwd=repo.path, env=env, extra_args=["check", "-j2", "a b"]
        )
        assert out.read_text() == "check -j2 a b\n"

    def test_no_extra_args_runs_snippet_verbatim(self, repo: Repo, tmp_path: Path) -> None:
        out = tmp_path / "plain.txt"
        cfg = Config(scripts={"plain": ScriptSpec(f"echo ran > {out}")})
        env = hooks.script_env(
            main=repo.path, worktree=repo.path, worktrees_dir=repo.path, branch=None
        )
        hooks.run_named_script(cfg, "plain", cwd=repo.path, env=env)
        assert out.read_text() == "ran\n"

    def test_unknown_name_lists_available(self, repo: Repo) -> None:
        cfg = Config(scripts={"a": ScriptSpec("true"), "b": ScriptSpec("true")})
        with pytest.raises(WorkforestError, match=r"no script named 'nope' \(available: a, b\)"):
            hooks.run_named_script(cfg, "nope", cwd=repo.path, env={})

    def test_failing_script_raises_with_code(self, repo: Repo) -> None:
        cfg = Config(scripts={"boom": ScriptSpec("exit 3")})
        env = hooks.script_env(
            main=repo.path, worktree=repo.path, worktrees_dir=repo.path, branch=None
        )
        with pytest.raises(WorkforestError, match="exit code 3"):
            hooks.run_named_script(cfg, "boom", cwd=repo.path, env=env)


def wait_for(condition: Callable[[], object], timeout: float = 10.0) -> None:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if condition():
            return
        time.sleep(0.02)
    raise AssertionError("condition not met in time")


# A script whose command blocks for as long as its argument says (`wf run
# dev 30` vs `extra_args=["0"]`), records where it ran, and cleans up.
DEV_SCRIPT = (
    "scripts:\n"
    "  dev:\n"
    '    command: echo started > "$WF_WORKTREE/started"; sleep\n'
    "    exclusive: true\n"
    '    cleanup: echo "$WF_BRANCH" > "$WF_WORKTREE/cleaned"\n'
)


def env_for(repo: Repo, worktree: Path, branch: str) -> dict[str, str]:
    return hooks.script_env(
        main=repo.path, worktree=worktree, worktrees_dir=worktree.parent, branch=branch
    )


class TestCleanup:
    def test_runs_after_success_and_after_failure(self, repo: Repo, tmp_path: Path) -> None:
        out = tmp_path / "cleaned"
        env = env_for(repo, repo.path, "main")
        cfg = Config(scripts={"ok": ScriptSpec("true", cleanup=f"echo ok > {out}")})
        hooks.run_named_script(cfg, "ok", cwd=repo.path, env=env)
        assert out.read_text() == "ok\n"

        out.unlink()
        cfg = Config(scripts={"bad": ScriptSpec("exit 3", cleanup=f"echo bad > {out}")})
        with pytest.raises(WorkforestError, match="exit code 3"):
            hooks.run_named_script(cfg, "bad", cwd=repo.path, env=env)
        assert out.read_text() == "bad\n"

    def test_failing_cleanup_only_warns(
        self, repo: Repo, capsys: pytest.CaptureFixture[str]
    ) -> None:
        cfg = Config(scripts={"x": ScriptSpec("true", cleanup="exit 7")})
        hooks.run_named_script(cfg, "x", cwd=repo.path, env=env_for(repo, repo.path, "main"))
        assert "cleanup for 'x' failed with exit code 7" in capsys.readouterr().err

    def test_record_lives_only_while_running(self, repo: Repo) -> None:
        common = gitutil.git_common_dir(repo.path)
        probe = f"test -f {jobs.record_path(common, 'x', repo.path, os.getpid())} && echo yes"
        cfg = Config(scripts={"x": ScriptSpec(probe)})
        hooks.run_named_script(cfg, "x", cwd=repo.path, env=env_for(repo, repo.path, "main"))
        assert jobs.jobs_for(common, "x") == []

    def test_unrunnable_shell(self, repo: Repo, monkeypatch: pytest.MonkeyPatch) -> None:
        monkeypatch.setenv("SHELL", "/nonexistent/sh")
        cfg = Config(scripts={"x": ScriptSpec("true")})
        with pytest.raises(WorkforestError, match=r"cannot run 'x' via \$SHELL"):
            hooks.run_named_script(cfg, "x", cwd=repo.path, env={})


class TestExclusive:
    """The running instance is a real second `wf run`: signal handling and
    the terminal hand-over live in the main thread of their own process."""

    @pytest.fixture
    def start_wf_run(self) -> Callable[..., subprocess.Popen[str]]:
        started: list[subprocess.Popen[str]] = []

        def _start(cwd: Path, *args: str) -> subprocess.Popen[str]:
            process = subprocess.Popen(
                [sys.executable, "-m", "workforest", "run", *args],
                cwd=cwd,
                stdin=subprocess.DEVNULL,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                text=True,
            )
            started.append(process)
            return process

        yield _start
        for process in started:
            if process.poll() is None:
                process.kill()
                process.wait()

    def run_victim(
        self, repo: Repo, start: Callable[..., subprocess.Popen[str]], worktree: Path
    ) -> subprocess.Popen[str]:
        repo.write_project_config(DEV_SCRIPT)
        victim = start(worktree, "dev", "30")
        common = gitutil.git_common_dir(repo.path)
        wait_for(lambda: jobs.jobs_for(common, "dev") and (worktree / "started").exists())
        return victim

    def test_preempts_instance_in_another_worktree(
        self, repo: Repo, start_wf_run: Callable[..., subprocess.Popen[str]]
    ) -> None:
        feat = make_worktree(repo, "feat")
        victim = self.run_victim(repo, start_wf_run, feat)

        cfg = load_config(repo.path)
        hooks.run_named_script(
            cfg, "dev", cwd=repo.path, env=env_for(repo, repo.path, "main"), extra_args=["0"]
        )

        _, err = victim.communicate(timeout=10)
        assert victim.returncode == 128 + signal.SIGTERM
        assert "script 'dev' was killed by SIGTERM (stopped by `wf run dev` in 'api')" in err
        assert (feat / "cleaned").read_text() == "feat\n"  # the victim's own cleanup
        assert (repo.path / "cleaned").read_text() == "main\n"  # then ours
        assert jobs.jobs_for(gitutil.git_common_dir(repo.path), "dev") == []

    def test_preempts_instance_in_the_same_worktree(
        self, repo: Repo, start_wf_run: Callable[..., subprocess.Popen[str]]
    ) -> None:
        victim = self.run_victim(repo, start_wf_run, repo.path)
        hooks.run_named_script(
            load_config(repo.path),
            "dev",
            cwd=repo.path,
            env=env_for(repo, repo.path, "main"),
            extra_args=["0"],
        )
        _, err = victim.communicate(timeout=10)
        assert "stopped by `wf run dev` in 'api'" in err
        assert jobs.jobs_for(gitutil.git_common_dir(repo.path), "dev") == []

    def test_non_exclusive_instances_coexist(
        self, repo: Repo, start_wf_run: Callable[..., subprocess.Popen[str]]
    ) -> None:
        feat = make_worktree(repo, "feat")
        victim = self.run_victim(repo, start_wf_run, feat)
        cfg = load_config(repo.path)
        cfg.scripts["dev"] = ScriptSpec(cfg.scripts["dev"].command)  # not exclusive
        hooks.run_named_script(
            cfg, "dev", cwd=repo.path, env=env_for(repo, repo.path, "main"), extra_args=["0"]
        )
        assert victim.poll() is None
        assert len(jobs.jobs_for(gitutil.git_common_dir(repo.path), "dev")) == 1

    def test_signal_to_wf_is_forwarded_to_the_command(
        self, repo: Repo, start_wf_run: Callable[..., subprocess.Popen[str]]
    ) -> None:
        feat = make_worktree(repo, "feat")
        victim = self.run_victim(repo, start_wf_run, feat)
        os.kill(victim.pid, signal.SIGTERM)
        _, err = victim.communicate(timeout=10)
        assert victim.returncode == 128 + signal.SIGTERM
        assert "script 'dev' was killed by SIGTERM\n" in err
        assert "stopped by" not in err
        assert (feat / "cleaned").read_text() == "feat\n"

    def test_sigint_ends_wf_the_way_ctrl_c_does(
        self, repo: Repo, start_wf_run: Callable[..., subprocess.Popen[str]]
    ) -> None:
        feat = make_worktree(repo, "feat")
        victim = self.run_victim(repo, start_wf_run, feat)
        os.kill(victim.pid, signal.SIGINT)
        victim.communicate(timeout=10)
        assert victim.returncode == -signal.SIGINT  # died by the signal, so shell loops abort
        assert (feat / "cleaned").read_text() == "feat\n"

    def test_orphan_is_cleaned_up_by_the_preemptor(
        self, repo: Repo, monkeypatch: pytest.MonkeyPatch
    ) -> None:
        feat = make_worktree(repo, "feat")
        repo.write_project_config(DEV_SCRIPT)
        common = gitutil.git_common_dir(repo.path)
        sleeper = subprocess.Popen(["sleep", "30"], process_group=0)
        threading.Thread(target=sleeper.wait, daemon=True).start()
        dead = subprocess.Popen(["true"])
        dead.wait()
        record = jobs.JobRecord(
            script="dev",
            worktree=str(feat),
            branch="feat",
            pgid=sleeper.pid,
            owner_pid=dead.pid,
            boot_id=jobs.boot_id(),
            started_at=time.time(),
        )
        jobs.write_record(jobs.record_path(common, "dev", feat, dead.pid), record)
        monkeypatch.setattr(jobs, "process_started_before", lambda pid, when: True)

        hooks.run_named_script(
            load_config(repo.path),
            "dev",
            cwd=repo.path,
            env=env_for(repo, repo.path, "main"),
            extra_args=["0"],
        )

        assert sleeper.returncode == -signal.SIGTERM
        assert (feat / "cleaned").read_text() == "feat\n"
        assert jobs.jobs_for(common, "dev") == []

    def test_orphan_of_a_deleted_worktree_skips_cleanup(
        self, repo: Repo, monkeypatch: pytest.MonkeyPatch, capsys: pytest.CaptureFixture[str]
    ) -> None:
        repo.write_project_config(DEV_SCRIPT)
        common = gitutil.git_common_dir(repo.path)
        sleeper = subprocess.Popen(["sleep", "30"], process_group=0)
        threading.Thread(target=sleeper.wait, daemon=True).start()
        gone = repo.path.parent / "gone"
        owner = _reaped_pid()
        record = jobs.JobRecord(
            script="dev",
            worktree=str(gone),
            branch="gone",
            pgid=sleeper.pid,
            owner_pid=owner,
            boot_id=jobs.boot_id(),
            started_at=time.time(),
        )
        jobs.write_record(jobs.record_path(common, "dev", gone, owner), record)
        monkeypatch.setattr(jobs, "process_started_before", lambda pid, when: True)

        hooks.run_named_script(
            load_config(repo.path),
            "dev",
            cwd=repo.path,
            env=env_for(repo, repo.path, "main"),
            extra_args=["0"],
        )
        assert sleeper.returncode == -signal.SIGTERM
        assert f"skipping cleanup for 'dev': {gone} no longer exists" in capsys.readouterr().err


def _reaped_pid() -> int:
    process = subprocess.Popen(["true"])
    process.wait()
    return process.pid


class TestInterrupted:
    def test_ctrl_c_is_a_warning_not_an_error(
        self, repo: Repo, tmp_path: Path, capsys: pytest.CaptureFixture[str]
    ) -> None:
        out = tmp_path / "cleaned"
        env = env_for(repo, repo.path, "main")
        # a shell whose child died by SIGINT exits 130; `kill -INT 0` is the real thing
        for name, spec in (
            ("shell", _spec("exit 130", cleanup=f"echo ok > {out}")),
            ("signal", _spec("kill -INT 0")),
        ):
            with pytest.raises(KeyboardInterrupt):
                hooks.run_named_script(Config(scripts={name: spec}), name, cwd=repo.path, env=env)
            err = capsys.readouterr().err
            assert f"script {name!r} was interrupted" in err
            assert "Error:" not in err
        assert out.read_text() == "ok\n"

    def test_interrupted_bulk_member_interrupts_the_group(
        self, repo: Repo, capsys: pytest.CaptureFixture[str]
    ) -> None:
        cfg = Config(
            scripts={
                "infra": _spec("exit 130"),
                "web": _spec("exit 3"),
                "dev": ScriptSpec(bulk=("infra", "web")),
            }
        )
        with pytest.raises(KeyboardInterrupt):
            hooks.run_named_script(cfg, "dev", cwd=repo.path, env=env_for(repo, repo.path, "main"))
        err = capsys.readouterr().err
        assert "infra | script 'infra' was interrupted" in err
        assert "web   | Error: script 'web' failed with exit code 3" in err
        assert "script 'dev' was interrupted" in err
        assert "Error: member" not in err


class TestKilledCommand:
    def test_exit_code_is_128_plus_signal(self, repo: Repo, tmp_path: Path) -> None:
        out = tmp_path / "cleaned"
        # `kill 0` signals the command's own process group — not us
        cfg = Config(scripts={"x": ScriptSpec("kill -TERM 0", cleanup=f"echo ok > {out}")})
        with pytest.raises(ScriptKilledError, match=r"script 'x' was killed by SIGTERM$") as info:
            hooks.run_named_script(cfg, "x", cwd=repo.path, env=env_for(repo, repo.path, "main"))
        assert info.value.exit_code == 128 + signal.SIGTERM
        assert out.read_text() == "ok\n"


class TestBackground:
    def test_detaches_logs_and_cleans_up(
        self, repo: Repo, tmp_path: Path, capsys: pytest.CaptureFixture[str]
    ) -> None:
        cleaned = tmp_path / "cleaned"
        cfg = Config(
            scripts={
                "bg": ScriptSpec(
                    'echo "out $WF_BRANCH"; echo err >&2; sleep 0.5; exit 3',
                    background=True,
                    cleanup=f"echo done > {cleaned}",
                )
            }
        )
        common = gitutil.git_common_dir(repo.path)
        started = time.monotonic()
        hooks.run_named_script(cfg, "bg", cwd=repo.path, env=env_for(repo, repo.path, "main"))
        assert time.monotonic() - started < 0.5  # returned while the command still ran

        [job] = jobs.jobs_for(common, "bg")
        assert job.record.owner_pid != os.getpid()  # the supervisor owns it
        log = jobs.log_path(common, "bg", repo.path, job.record.owner_pid)
        err = capsys.readouterr().err
        assert "started 'bg' in the background (pid " in err
        assert str(log) in err
        assert jobs.classify(job.record) is jobs.JobState.LIVE

        wait_for(lambda: not job.path.exists())
        assert cleaned.read_text() == "done\n"
        text = log.read_text()
        assert "out main\n" in text
        assert "err\n" in text
        assert "running cleanup for 'bg'" in text

    def test_flag_overrides_entry(self, repo: Repo, capsys: pytest.CaptureFixture[str]) -> None:
        cfg = Config(scripts={"bg": ScriptSpec("true", background=True)})
        env = env_for(repo, repo.path, "main")
        hooks.run_named_script(cfg, "bg", cwd=repo.path, env=env, background=False)
        assert "running 'bg' in" in capsys.readouterr().err

        cfg = Config(scripts={"fg": ScriptSpec("true")})
        hooks.run_named_script(cfg, "fg", cwd=repo.path, env=env, background=True)
        err = capsys.readouterr().err
        assert "'fg' already finished" in err or "started 'fg' in the background" in err
        wait_for(lambda: jobs.jobs_for(gitutil.git_common_dir(repo.path), "fg") == [])

    def test_immediate_failure_is_reported_with_the_log(self, repo: Repo) -> None:
        cfg = Config(scripts={"bad": ScriptSpec("echo boom >&2; exit 4", background=True)})
        with pytest.raises(
            WorkforestError, match="exited with status 4 right after launch"
        ) as info:
            hooks.run_named_script(cfg, "bad", cwd=repo.path, env=env_for(repo, repo.path, "main"))
        assert str(info.value).endswith("boom")

    def test_quick_success_is_reported_as_finished(
        self, repo: Repo, capsys: pytest.CaptureFixture[str]
    ) -> None:
        cfg = Config(scripts={"quick": ScriptSpec("true", background=True)})
        hooks.run_named_script(cfg, "quick", cwd=repo.path, env=env_for(repo, repo.path, "main"))
        assert "'quick' already finished" in capsys.readouterr().err

    def test_stop_ends_a_background_instance(
        self, repo: Repo, tmp_path: Path, capsys: pytest.CaptureFixture[str]
    ) -> None:
        cleaned = tmp_path / "cleaned"
        cfg = Config(
            scripts={"bg": ScriptSpec("sleep 30", background=True, cleanup=f"touch {cleaned}")}
        )
        env = env_for(repo, repo.path, "main")
        hooks.run_named_script(cfg, "bg", cwd=repo.path, env=env)
        common = gitutil.git_common_dir(repo.path)
        [job] = jobs.jobs_for(common, "bg")

        hooks.stop_script(cfg, "bg", cwd=repo.path, env=env)

        assert not job.path.exists()
        assert cleaned.exists()
        assert "stopping 'bg' in 'api'" in capsys.readouterr().err
        log = jobs.log_path(common, "bg", repo.path, job.record.owner_pid)
        assert "stopped by `wf stop` in 'api'" in log.read_text()


class TestConcurrent:
    """A script that is not `exclusive` runs as many times as it is asked
    to, in one worktree or across several."""

    def test_foreground_run_joins_a_live_instance(self, repo: Repo, tmp_path: Path) -> None:
        cfg = Config(scripts={"srv": ScriptSpec(f"touch {tmp_path / 'ran'}")})
        common = gitutil.git_common_dir(repo.path)
        sleeper = subprocess.Popen(["sleep", "30"], process_group=0)
        live = jobs.record_path(common, "srv", repo.path, os.getpid() - 1)
        jobs.write_record(
            live,
            jobs.JobRecord(
                script="srv",
                worktree=str(repo.path),
                branch="main",
                pgid=sleeper.pid,
                owner_pid=os.getpid(),
                boot_id=jobs.boot_id(),
                started_at=time.time(),
            ),
        )
        try:
            hooks.run_named_script(cfg, "srv", cwd=repo.path, env=env_for(repo, repo.path, "main"))
        finally:
            os.killpg(sleeper.pid, signal.SIGKILL)
            sleeper.wait()
        assert (tmp_path / "ran").exists()
        assert live.is_file()  # the instance already running is untouched

    def test_instances_get_a_record_and_a_log_each(
        self, repo: Repo, tmp_path: Path, capsys: pytest.CaptureFixture[str]
    ) -> None:
        cleaned = tmp_path / "cleaned"
        cfg = Config(
            scripts={"srv": ScriptSpec("sleep 30", background=True, cleanup=f"echo x >> {cleaned}")}
        )
        env = env_for(repo, repo.path, "main")
        common = gitutil.git_common_dir(repo.path)
        hooks.run_named_script(cfg, "srv", cwd=repo.path, env=env)
        hooks.run_named_script(cfg, "srv", cwd=repo.path, env=env)

        found = jobs.jobs_for(common, "srv")
        owners = [job.record.owner_pid for job in found]
        assert len(set(owners)) == 2
        assert [job.path.name for job in found] == sorted(f"api.{pid}" for pid in owners)
        for pid in owners:
            assert jobs.log_path(common, "srv", repo.path, pid).is_file()
        assert capsys.readouterr().err.count("started 'srv' in the background") == 2

        hooks.stop_script(cfg, "srv", cwd=repo.path, env=env)  # every instance here
        assert jobs.jobs_for(common, "srv") == []
        assert cleaned.read_text() == "x\nx\n"  # each instance ran its own cleanup

    def test_start_clears_out_dead_instances(self, repo: Repo) -> None:
        cfg = Config(scripts={"srv": ScriptSpec("sleep 30", background=True)})
        env = env_for(repo, repo.path, "main")
        common = gitutil.git_common_dir(repo.path)
        dead = _reaped_pid()
        jobs.write_record(
            jobs.record_path(common, "srv", repo.path, dead),
            jobs.JobRecord("srv", str(repo.path), "main", dead, dead, "", 0.0),
        )
        stale_log = jobs.log_path(common, "srv", repo.path, dead)
        stale_log.parent.mkdir(parents=True, exist_ok=True)
        stale_log.write_text("output of a run that is long gone")

        hooks.run_named_script(cfg, "srv", cwd=repo.path, env=env)
        assert not stale_log.exists()
        assert len(jobs.jobs_for(common, "srv")) == 1
        hooks.stop_script(cfg, "srv", cwd=repo.path, env=env)


class TestStop:
    def test_unknown_script(self, repo: Repo) -> None:
        with pytest.raises(WorkforestError, match="no script named 'nope'"):
            hooks.stop_script(Config(), "nope", cwd=repo.path, env={})

    def test_nothing_running(self, repo: Repo) -> None:
        cfg = Config(scripts={"x": ScriptSpec("true")})
        with pytest.raises(WorkforestError, match="'x' is not running in 'api'"):
            hooks.stop_script(cfg, "x", cwd=repo.path, env={})
        with pytest.raises(WorkforestError, match="'x' is not running anywhere in this project"):
            hooks.stop_script(cfg, "x", cwd=repo.path, env={}, everywhere=True)

    def test_stale_records_do_not_count(self, repo: Repo) -> None:
        cfg = Config(scripts={"x": ScriptSpec("true")})
        common = gitutil.git_common_dir(repo.path)
        dead = _reaped_pid()
        path = jobs.record_path(common, "x", repo.path, dead)
        jobs.write_record(path, jobs.JobRecord("x", str(repo.path), "main", dead, dead, "", 0.0))
        with pytest.raises(WorkforestError, match="not running"):
            hooks.stop_script(cfg, "x", cwd=repo.path, env={})
        assert not path.exists()

    def test_this_worktree_only_unless_everywhere(
        self, repo: Repo, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
    ) -> None:
        feat = make_worktree(repo, "feat")
        cfg = Config(scripts={"x": ScriptSpec("sleep 30", cleanup="touch cleaned")})
        common = gitutil.git_common_dir(repo.path)
        sleepers = []
        for worktree in (repo.path, feat):
            process = subprocess.Popen(["sleep", "30"], process_group=0)
            threading.Thread(target=process.wait, daemon=True).start()
            sleepers.append(process)
            owner = _reaped_pid()
            jobs.write_record(
                jobs.record_path(common, "x", worktree, owner),
                jobs.JobRecord(
                    "x",
                    str(worktree),
                    worktree.name,
                    process.pid,
                    owner,
                    jobs.boot_id(),
                    time.time(),
                ),
            )
        monkeypatch.setattr(jobs, "process_started_before", lambda pid, when: True)
        env = env_for(repo, feat, "feat")

        hooks.stop_script(cfg, "x", cwd=feat, env=env)
        wait_for(lambda: sleepers[1].returncode is not None)
        assert sleepers[0].poll() is None  # main's instance untouched
        assert (feat / "cleaned").exists()
        assert not (repo.path / "cleaned").exists()

        hooks.stop_script(cfg, "x", cwd=feat, env=env, everywhere=True)
        wait_for(lambda: sleepers[0].returncode is not None)
        assert (repo.path / "cleaned").exists()
        assert jobs.jobs_for(common, "x") == []

    def test_per_script_timeout_beats_global(
        self, repo: Repo, monkeypatch: pytest.MonkeyPatch
    ) -> None:
        seen: list[float] = []

        def fake_stop(job: jobs.Job, *, by: str, timeout: float, orphan_cleanup: object) -> None:
            seen.append(timeout)
            job.path.unlink()

        monkeypatch.setattr(jobs, "stop", fake_stop)
        common = gitutil.git_common_dir(repo.path)
        record = jobs.JobRecord("x", str(repo.path), "main", os.getpid(), os.getpid(), "", 0.0)
        monkeypatch.setattr(jobs, "classify", lambda record: jobs.JobState.LIVE)
        env = env_for(repo, repo.path, "main")

        jobs.write_record(jobs.record_path(common, "x", repo.path, os.getpid()), record)
        hooks.stop_script(
            Config(scripts={"x": ScriptSpec("true")}, stop_timeout=7), "x", cwd=repo.path, env=env
        )
        jobs.write_record(jobs.record_path(common, "x", repo.path, os.getpid()), record)
        hooks.stop_script(
            Config(scripts={"x": ScriptSpec("true", stop_timeout=2)}, stop_timeout=7),
            "x",
            cwd=repo.path,
            env=env,
        )
        assert seen == [7, 2]


def _spec(command: str, **kwargs: object) -> ScriptSpec:
    return ScriptSpec(command, **kwargs)  # type: ignore[arg-type]


class TestGroups:
    """`bulk` and `pipeline` entries run through the same machinery as a
    command: their supervisor is the group leader the parent waits on."""

    def test_pipeline_runs_members_in_order_and_stops_at_failure(
        self, repo: Repo, tmp_path: Path, capsys: pytest.CaptureFixture[str]
    ) -> None:
        log = tmp_path / "steps"
        cfg = Config(
            scripts={
                "a": _spec(f"echo a >> {log}"),
                "b": _spec(f"echo b >> {log}; exit 3"),
                "c": _spec(f"echo c >> {log}"),
                "chain": ScriptSpec(pipeline=("a", "b", "c"), cleanup=f"echo done >> {log}"),
            }
        )
        with pytest.raises(WorkforestError, match="script 'chain' failed with exit code 3"):
            hooks.run_named_script(
                cfg, "chain", cwd=repo.path, env=env_for(repo, repo.path, "main")
            )
        assert log.read_text() == "a\nb\ndone\n"
        err = capsys.readouterr().err
        assert f"running 'chain' in {repo.path}: a, b, c" in err
        assert "'chain' step 1/3: a" in err
        assert "'chain' step 2/3: b" in err
        assert "step 3/3" not in err
        assert "Error: script 'b' failed with exit code 3" in err

    def test_pipeline_succeeds_when_every_step_does(self, repo: Repo, tmp_path: Path) -> None:
        log = tmp_path / "steps"
        cfg = Config(
            scripts={
                "a": _spec(f"echo a >> {log}"),
                "b": _spec(f'echo "b $WF_BRANCH" >> {log}'),
                "chain": ScriptSpec(pipeline=("a", "b")),
            }
        )
        hooks.run_named_script(cfg, "chain", cwd=repo.path, env=env_for(repo, repo.path, "main"))
        assert log.read_text() == "a\nb main\n"

    def test_bulk_runs_members_at_once_with_prefixed_output(
        self, repo: Repo, tmp_path: Path, capsys: pytest.CaptureFixture[str]
    ) -> None:
        flag = tmp_path / "slow-started"
        cfg = Config(
            scripts={
                "slow": _spec(f"touch {flag}; echo slow-out; echo slow-err >&2; sleep 0.3"),
                # done only once `slow` has started: proof that they overlap
                "quick": _spec(f"while ! test -f {flag}; do sleep 0.01; done; echo saw-slow"),
                "both": ScriptSpec(bulk=("slow", "quick")),
            }
        )
        started = time.monotonic()
        hooks.run_named_script(cfg, "both", cwd=repo.path, env=env_for(repo, repo.path, "main"))
        assert time.monotonic() - started < 2
        err = capsys.readouterr().err
        assert f"running 'both' in {repo.path}: slow, quick" in err
        assert "slow  | slow-out\n" in err
        assert "slow  | slow-err\n" in err
        assert "quick | saw-slow\n" in err
        assert "slow  | running 'slow' in" in err

    def test_bulk_waits_for_all_and_reports_each_failure(
        self, repo: Repo, tmp_path: Path, capsys: pytest.CaptureFixture[str]
    ) -> None:
        log = tmp_path / "log"
        cfg = Config(
            scripts={
                "bad": _spec("exit 3"),
                "worse": _spec("sleep 0.2; exit 4"),
                "fine": _spec(f"sleep 0.3; echo fine >> {log}"),
                "all": ScriptSpec(bulk=("bad", "worse", "fine")),
            }
        )
        with pytest.raises(WorkforestError, match="script 'all' failed with exit code 3"):
            hooks.run_named_script(cfg, "all", cwd=repo.path, env=env_for(repo, repo.path, "main"))
        assert log.read_text() == "fine\n"  # not cut short by the failures
        err = capsys.readouterr().err
        assert "bad   | Error: script 'bad' failed with exit code 3" in err
        assert "Error: member 'bad' of 'all' failed with exit code 3" in err
        assert "Error: member 'worse' of 'all' failed with exit code 4" in err
        assert "member 'fine'" not in err

    def test_bulk_member_killed_by_signal(self, repo: Repo) -> None:
        cfg = Config(
            scripts={
                "victim": _spec("kill -TERM 0"),
                "fine": _spec("true"),
                "all": ScriptSpec(bulk=("fine", "victim")),
            }
        )
        with pytest.raises(ScriptKilledError, match="script 'all' was killed by SIGTERM") as info:
            hooks.run_named_script(cfg, "all", cwd=repo.path, env=env_for(repo, repo.path, "main"))
        assert info.value.exit_code == 128 + signal.SIGTERM

    def test_group_takes_no_arguments(self, repo: Repo) -> None:
        cfg = Config(scripts={"a": _spec("true"), "g": ScriptSpec(bulk=("a",))})
        with pytest.raises(
            WorkforestError, match="'g' is a group of scripts and takes no arguments"
        ):
            hooks.run_named_script(cfg, "g", cwd=repo.path, env={}, extra_args=["x"])

    def test_members_keep_their_own_records_while_the_group_runs(
        self, repo: Repo, tmp_path: Path
    ) -> None:
        common = gitutil.git_common_dir(repo.path)
        out = tmp_path / "seen"
        probe = " && ".join(
            f'test -n "$(ls {jobs.records_dir(common, name)})"' for name in ("m", "g")
        )
        cfg = Config(
            scripts={"m": _spec(f"{probe} && echo yes > {out}"), "g": ScriptSpec(bulk=("m",))}
        )
        hooks.run_named_script(cfg, "g", cwd=repo.path, env=env_for(repo, repo.path, "main"))
        assert out.read_text() == "yes\n"
        assert jobs.jobs_for(common, "m") == [] and jobs.jobs_for(common, "g") == []

    def test_member_already_running_is_started_again(self, repo: Repo) -> None:
        cfg = Config(
            scripts={
                "srv": _spec("sleep 30", background=True),
                "chain": ScriptSpec(pipeline=("srv",)),
            }
        )
        env = env_for(repo, repo.path, "main")
        common = gitutil.git_common_dir(repo.path)
        hooks.run_named_script(cfg, "srv", cwd=repo.path, env=env)
        try:
            hooks.run_named_script(cfg, "chain", cwd=repo.path, env=env)
            assert len(jobs.jobs_for(common, "srv")) == 2
        finally:
            hooks.stop_script(cfg, "srv", cwd=repo.path, env=env)  # both of them
        assert jobs.jobs_for(common, "srv") == []

    def test_stop_group_stops_members_and_runs_every_cleanup(
        self, repo: Repo, tmp_path: Path, capsys: pytest.CaptureFixture[str]
    ) -> None:
        cleaned = tmp_path / "cleaned"
        cfg = Config(
            scripts={
                "s1": _spec("sleep 30", cleanup=f"echo s1 >> {cleaned}"),
                "s2": _spec("sleep 30", cleanup=f"echo s2 >> {cleaned}"),
                "servers": ScriptSpec(
                    bulk=("s1", "s2"), background=True, cleanup=f"echo servers >> {cleaned}"
                ),
            }
        )
        env = env_for(repo, repo.path, "main")
        common = gitutil.git_common_dir(repo.path)
        hooks.run_named_script(cfg, "servers", cwd=repo.path, env=env)
        wait_for(lambda: len(jobs.jobs_for(common, "s1") + jobs.jobs_for(common, "s2")) == 2)

        hooks.stop_script(cfg, "servers", cwd=repo.path, env=env)

        assert sorted(cleaned.read_text().split()) == ["s1", "s2", "servers"]
        assert cleaned.read_text().endswith("servers\n")  # the group's own cleanup comes last
        for name in ("s1", "s2", "servers"):
            assert jobs.jobs_for(common, name) == []
        log = sole_log(common, "servers", repo.path).read_text()
        assert "s1 | Error: script 's1' was killed by SIGTERM" in log
        assert "Error: member 's2' of 'servers' was killed by SIGTERM" in log
        assert "stopped by `wf stop` in 'api'" in log

    def test_stop_one_member_out_of_a_running_bulk(self, repo: Repo, tmp_path: Path) -> None:
        cleaned = tmp_path / "cleaned"
        cfg = Config(
            scripts={
                "s1": _spec("sleep 30", cleanup=f"echo s1 >> {cleaned}"),
                "s2": _spec("sleep 0.5"),
                "servers": ScriptSpec(bulk=("s1", "s2"), background=True),
            }
        )
        env = env_for(repo, repo.path, "main")
        common = gitutil.git_common_dir(repo.path)
        hooks.run_named_script(cfg, "servers", cwd=repo.path, env=env)
        wait_for(lambda: jobs.jobs_for(common, "s1"))

        hooks.stop_script(cfg, "s1", cwd=repo.path, env=env)

        assert cleaned.read_text() == "s1\n"
        wait_for(lambda: jobs.jobs_for(common, "servers") == [])  # s2 ends on its own
        log = sole_log(common, "servers", repo.path).read_text()
        assert (
            "s1 | Error: script 's1' was killed by SIGTERM (stopped by `wf stop` in 'api')" in log
        )
        assert "member 's2'" not in log

    def test_nested_groups(
        self, repo: Repo, tmp_path: Path, capsys: pytest.CaptureFixture[str]
    ) -> None:
        log = tmp_path / "log"
        cfg = Config(
            scripts={
                "a": _spec(f"echo a >> {log}"),
                "b": _spec("echo b-out"),
                "c": _spec("echo c-out"),
                "pair": ScriptSpec(bulk=("b", "c")),
                "chain": ScriptSpec(pipeline=("a", "pair")),
            }
        )
        hooks.run_named_script(cfg, "chain", cwd=repo.path, env=env_for(repo, repo.path, "main"))
        assert log.read_text() == "a\n"
        err = capsys.readouterr().err
        assert "'chain' step 2/2: pair" in err
        assert "b | b-out\n" in err
        assert "c | c-out\n" in err

    def test_background_member_of_a_pipeline_is_started_and_left_running(
        self, repo: Repo, tmp_path: Path
    ) -> None:
        cfg = Config(
            scripts={
                "srv": _spec("sleep 30", background=True),
                "after": _spec("true"),
                "chain": ScriptSpec(pipeline=("srv", "after")),
            }
        )
        env = env_for(repo, repo.path, "main")
        common = gitutil.git_common_dir(repo.path)
        hooks.run_named_script(cfg, "chain", cwd=repo.path, env=env)
        try:
            [job] = jobs.jobs_for(common, "srv")
            assert jobs.classify(job.record) is jobs.JobState.LIVE
            assert jobs.jobs_for(common, "chain") == []
        finally:
            hooks.stop_script(cfg, "srv", cwd=repo.path, env=env)

    def test_stop_timeout_of_a_group_is_its_members_longest(self) -> None:
        cfg = Config(
            scripts={
                "a": _spec("x", stop_timeout=5),
                "b": _spec("x"),
                "inner": ScriptSpec(bulk=("a", "b")),
                "outer": ScriptSpec(pipeline=("inner",)),
                "own": ScriptSpec(bulk=("a",), stop_timeout=2),
                "loose": ScriptSpec(bulk=("gone",)),  # a hand-built Config may dangle
            },
            stop_timeout=3,
        )
        assert hooks._stop_timeout(cfg, cfg.scripts["b"]) == 3
        assert hooks._stop_timeout(cfg, cfg.scripts["inner"]) == 5
        assert hooks._stop_timeout(cfg, cfg.scripts["outer"]) == 5
        assert hooks._stop_timeout(cfg, cfg.scripts["own"]) == 2
        assert hooks._stop_timeout(cfg, cfg.scripts["loose"]) == 3


class TestRunStep:
    """The member runner's view of a run: an outcome, not an exception."""

    def step(self, repo: Repo, cfg: Config, name: str) -> int:
        job = hooks._prepare(cfg, name, cwd=repo.path, env=env_for(repo, repo.path, "main"))
        return hooks._run_step(job)

    def test_outcomes(self, repo: Repo, capsys: pytest.CaptureFixture[str]) -> None:
        cfg = Config(
            scripts={
                "ok": _spec("true"),
                "bad": _spec("exit 5"),
                "killed": _spec("kill -TERM 0"),
                "interrupted": _spec("kill -INT 0"),
            }
        )
        assert self.step(repo, cfg, "ok") == 0
        assert self.step(repo, cfg, "bad") == 5
        assert self.step(repo, cfg, "killed") == -signal.SIGTERM
        assert self.step(repo, cfg, "interrupted") == -signal.SIGINT
        err = capsys.readouterr().err
        assert "Error: script 'bad' failed with exit code 5" in err
        assert "Error: script 'killed' was killed by SIGTERM" in err
        assert "script 'interrupted' was interrupted" in err
        assert "Error: script 'interrupted'" not in err

    def test_a_shell_exiting_130_counts_as_interrupted(
        self, repo: Repo, capsys: pytest.CaptureFixture[str]
    ) -> None:
        cfg = Config(scripts={"x": _spec("exit 130")})
        assert self.step(repo, cfg, "x") == -signal.SIGINT
        assert "script 'x' was interrupted" in capsys.readouterr().err

    def test_background_member_counts_as_done(self, repo: Repo) -> None:
        cfg = Config(scripts={"srv": _spec("sleep 30", background=True)})
        assert self.step(repo, cfg, "srv") == 0
        hooks.stop_script(cfg, "srv", cwd=repo.path, env=env_for(repo, repo.path, "main"))

    def test_unrunnable_shell_is_an_error_outcome(
        self, repo: Repo, monkeypatch: pytest.MonkeyPatch, capsys: pytest.CaptureFixture[str]
    ) -> None:
        monkeypatch.setenv("SHELL", "/nonexistent/sh")
        assert self.step(repo, Config(scripts={"x": _spec("true")}), "x") == 1
        assert "cannot run 'x' via $SHELL" in capsys.readouterr().err


class TestPrefixer:
    def test_pads_to_the_longest_name(self) -> None:
        prefixer = hooks._Prefixer(("api", "frontend"), color=False)
        assert prefixer.feed("api", b"hello\n") == "api      | hello\n"
        assert prefixer.feed("frontend", b"hi\n") == "frontend | hi\n"

    def test_keeps_partial_lines_until_they_complete(self) -> None:
        prefixer = hooks._Prefixer(("a",), color=False)
        assert prefixer.feed("a", b"one\ntw") == "a | one\n"
        assert prefixer.feed("a", b"o\nthree") == "a | two\n"
        assert prefixer.flush("a") == "a | three\n"
        assert prefixer.flush("a") == ""

    def test_pty_line_endings_and_undecodable_bytes(self) -> None:
        prefixer = hooks._Prefixer(("a",), color=False)
        assert prefixer.feed("a", b"crlf\r\nraw \xff\n") == "a | crlf\na | raw �\n"

    def test_colors_only_the_prefix(self) -> None:
        prefixer = hooks._Prefixer(("a", "b"), color=True)
        assert prefixer.feed("a", b"x\n") == "\033[36ma | \033[0mx\n"
        assert prefixer.feed("b", b"x\n") == "\033[35mb | \033[0mx\n"


class TestPump:
    def runner(self, name: str, command: str, *, tty: bool = False) -> hooks._Runner:
        read_fd, write_fd = hooks._open_channel(tty=tty)
        process = subprocess.Popen(["sh", "-c", command], stdout=write_fd, stderr=write_fd)
        os.close(write_fd)
        return hooks._Runner(name, process.pid, read_fd)

    def test_relays_every_runner_until_all_have_ended(self) -> None:
        runners = [
            self.runner("a", "echo a1; sleep 0.1; echo a2; exit 3"),
            self.runner("b", "printf 'b-no-newline'"),
        ]
        sink = io.StringIO()
        hooks._pump(runners, hooks._Prefixer(("a", "b"), color=False), sink)
        assert sorted(sink.getvalue().splitlines()) == ["a | a1", "a | a2", "b | b-no-newline"]
        assert [runner.code for runner in runners] == [3, 0]
        assert all(runner.fd < 0 for runner in runners)

    def test_a_daemon_holding_the_channel_does_not_hold_the_pump(self) -> None:
        runners = [self.runner("a", "sleep 5 & echo hi")]
        sink = io.StringIO()
        started = time.monotonic()
        hooks._pump(runners, hooks._Prefixer(("a",), color=False), sink)
        assert time.monotonic() - started < 2
        assert sink.getvalue() == "a | hi\n"

    def test_pty_channel_gives_the_member_a_terminal(self) -> None:
        runners = [self.runner("a", "test -t 1 && echo tty || echo no-tty", tty=True)]
        sink = io.StringIO()
        hooks._pump(runners, hooks._Prefixer(("a",), color=False), sink)
        assert sink.getvalue() == "a | tty\n"

    def test_pipe_channel_does_not(self) -> None:
        runners = [self.runner("a", "test -t 1 && echo tty || echo no-tty")]
        sink = io.StringIO()
        hooks._pump(runners, hooks._Prefixer(("a",), color=False), sink)
        assert sink.getvalue() == "a | no-tty\n"


class TestBulkOutcome:
    def outcome(self, *codes: int, capsys: pytest.CaptureFixture[str]) -> tuple[int, str]:
        runners = [hooks._Runner(f"m{i}", 0, -1, code) for i, code in enumerate(codes)]
        result = hooks._bulk_outcome("g", runners)
        return result, capsys.readouterr().err

    def test_all_good(self, capsys: pytest.CaptureFixture[str]) -> None:
        assert self.outcome(0, 0, capsys=capsys) == (0, "")

    def test_first_failure_wins_by_default(self, capsys: pytest.CaptureFixture[str]) -> None:
        code, err = self.outcome(0, 4, 3, capsys=capsys)
        assert code == 4
        assert "Error: member 'm1' of 'g' failed with exit code 4" in err
        assert "Error: member 'm2' of 'g' failed with exit code 3" in err
        assert "m0" not in err

    def test_signal_deaths_win_over_statuses(self, capsys: pytest.CaptureFixture[str]) -> None:
        assert self.outcome(4, -signal.SIGTERM, capsys=capsys)[0] == -signal.SIGTERM

    def test_an_interruption_wins_and_is_not_an_error(
        self, capsys: pytest.CaptureFixture[str]
    ) -> None:
        for codes in ((4, -signal.SIGTERM, -signal.SIGINT), (4, 130)):
            code, err = self.outcome(*codes, capsys=capsys)
            assert code == -signal.SIGINT
            assert err == ""
