"""hooks: symlinks + git-status invisibility, setup scripts, named scripts."""

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
        probe = f"test -f {jobs.record_path(common, 'x', repo.path)} && echo yes"
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
        jobs.write_record(jobs.record_path(common, "dev", feat), record)
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
        record = jobs.JobRecord(
            script="dev",
            worktree=str(gone),
            branch="gone",
            pgid=sleeper.pid,
            owner_pid=_reaped_pid(),
            boot_id=jobs.boot_id(),
            started_at=time.time(),
        )
        jobs.write_record(jobs.record_path(common, "dev", gone), record)
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

        log = jobs.log_path(common, "bg", repo.path)
        err = capsys.readouterr().err
        assert "started 'bg' in the background (pid " in err
        assert str(log) in err
        [job] = jobs.jobs_for(common, "bg")
        assert job.record.owner_pid != os.getpid()  # the supervisor owns it
        assert jobs.classify(job.record) is jobs.JobState.LIVE

        wait_for(lambda: not job.path.exists())
        assert cleaned.read_text() == "done\n"
        text = log.read_text()
        assert "out main\n" in text
        assert "err\n" in text
        assert "running cleanup for 'bg'" in text

    def test_second_instance_in_the_same_worktree_is_refused(
        self, repo: Repo, tmp_path: Path
    ) -> None:
        cfg = Config(scripts={"srv": ScriptSpec("sleep 30", background=True)})
        env = env_for(repo, repo.path, "main")
        hooks.run_named_script(cfg, "srv", cwd=repo.path, env=env)
        with pytest.raises(WorkforestError, match=r"'srv' is already running in 'api' \(pid \d+\)"):
            hooks.run_named_script(cfg, "srv", cwd=repo.path, env=env)
        with pytest.raises(WorkforestError, match="`wf stop srv` first"):
            hooks.run_named_script(cfg, "srv", cwd=repo.path, env=env, background=False)
        hooks.stop_script(cfg, "srv", cwd=repo.path, env=env)

        # a stale record from a dead instance is no obstacle
        common = gitutil.git_common_dir(repo.path)
        jobs.write_record(
            jobs.record_path(common, "srv", repo.path),
            jobs.JobRecord("srv", str(repo.path), "main", _reaped_pid(), _reaped_pid(), "", 0.0),
        )
        hooks.run_named_script(
            Config(scripts={"srv": ScriptSpec("true")}), "srv", cwd=repo.path, env=env
        )

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
        assert "stopped by `wf stop` in 'api'" in jobs.log_path(common, "bg", repo.path).read_text()


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
        path = jobs.record_path(common, "x", repo.path)
        jobs.write_record(
            path,
            jobs.JobRecord("x", str(repo.path), "main", _reaped_pid(), _reaped_pid(), "", 0.0),
        )
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
            jobs.write_record(
                jobs.record_path(common, "x", worktree),
                jobs.JobRecord(
                    "x",
                    str(worktree),
                    worktree.name,
                    process.pid,
                    _reaped_pid(),
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

        jobs.write_record(jobs.record_path(common, "x", repo.path), record)
        hooks.stop_script(
            Config(scripts={"x": ScriptSpec("true")}, stop_timeout=7), "x", cwd=repo.path, env=env
        )
        jobs.write_record(jobs.record_path(common, "x", repo.path), record)
        hooks.stop_script(
            Config(scripts={"x": ScriptSpec("true", stop_timeout=2)}, stop_timeout=7),
            "x",
            cwd=repo.path,
            env=env,
        )
        assert seen == [7, 2]
