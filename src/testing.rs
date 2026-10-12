//! Fixtures for the unit tests.
//!
//! Isolation contract: no test may read or write the real environment.
//! Unit tests share one process, so nothing here touches the environment
//! or the working directory: every helper takes explicit paths, and git —
//! ours and the fixtures' — runs against a private config file.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

/// A git config with a test identity, in place of the user's.
pub fn gitconfig() -> &'static Path {
    static FILE: OnceLock<PathBuf> = OnceLock::new();
    FILE.get_or_init(|| {
        let path = std::env::temp_dir().join(format!("workforest-test-gitconfig-{}", std::process::id()));
        fs::write(
            &path,
            "[user]\n\tname = Test\n\temail = test@example.invalid\n[init]\n\tdefaultBranch = main\n",
        )
        .unwrap();
        path
    })
}

/// A temporary directory to build repositories and config layers in.
pub struct Sandbox {
    dir: tempfile::TempDir,
    root: PathBuf,
}

impl Sandbox {
    pub fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        // git reports real paths; /tmp may be a symlink (macOS).
        let root = dir.path().canonicalize().unwrap();
        Self { dir, root }
    }

    pub fn path(&self) -> &Path {
        &self.root
    }

    /// A real git repository at `dev/<name>` with one commit on `main`.
    pub fn repo(&self, name: &str) -> Repo {
        let path = self.root.join("dev").join(name);
        fs::create_dir_all(&path).unwrap();
        let repo = Repo { path, sandbox: self.root.clone() };
        repo.git(&["init", "-q", "-b", "main"]);
        fs::write(repo.path.join("README.md"), format!("# {name}\n")).unwrap();
        repo.commit("init");
        repo
    }

    pub fn repo_with_origin(&self, name: &str) -> Repo {
        let repo = self.repo(name);
        let bare = self.root.join("remotes").join(format!("{name}.git"));
        init_bare(&bare);
        repo.git(&["remote", "add", "origin", &bare.to_string_lossy()]);
        repo.git(&["push", "-q", "-u", "origin", "main"]);
        repo
    }

    /// Keep the directory after the test, for a post-mortem.
    #[allow(dead_code)]
    pub fn keep(self) -> PathBuf {
        self.dir.keep()
    }
}

fn git_command() -> Command {
    let mut command = Command::new("git");
    command.env("GIT_CONFIG_GLOBAL", gitconfig()).env("GIT_CONFIG_SYSTEM", "/dev/null");
    command
}

fn init_bare(path: &Path) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let status = git_command().args(["init", "-q", "--bare"]).arg(path).status().unwrap();
    assert!(status.success());
}

impl Sandbox {
    /// Where this sandbox's system and user config layers would be.
    pub fn roots(&self) -> crate::config::Roots {
        crate::config::Roots {
            system_dir: self.root.join("etc-workforest"),
            user_dir: self.root.join("home").join(".config").join("workforest"),
        }
    }

    /// The environment a command sees: SHELL and EDITOR pinned to stubs, a
    /// home of its own, and nothing of the user's but PATH.
    pub fn env(&self) -> crate::util::Env {
        [
            ("SHELL".into(), "/bin/sh".into()),
            ("EDITOR".into(), "stub-editor".into()),
            ("HOME".into(), self.root.join("home").into_os_string()),
            ("PATH".into(), std::env::var_os("PATH").unwrap_or_default()),
        ]
        .into_iter()
        .collect()
    }

    /// What `build_context` gives a command started in `cwd` — from this
    /// sandbox's config layers and environment, never the real ones.
    pub fn context(&self, repo: &Repo, cwd: &Path) -> crate::commands::Context {
        let config = crate::config::load_config_in(&self.roots(), Some(&repo.path)).unwrap();
        let worktrees_dir = crate::config::resolve_worktrees_dir(&config, &repo.path).unwrap();
        crate::commands::Context {
            cwd_root: cwd.to_path_buf(),
            main: repo.path.clone(),
            config,
            worktrees_dir,
            env: self.env(),
        }
    }
}

/// Handle to a throwaway git repository.
pub struct Repo {
    pub path: PathBuf,
    sandbox: PathBuf,
}

impl Repo {
    pub fn git(&self, args: &[&str]) -> String {
        self.git_in(&self.path, args)
    }

    pub fn git_in(&self, cwd: &Path, args: &[&str]) -> String {
        let output = git_command().current_dir(cwd).args(args).output().unwrap();
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    pub fn commit(&self, message: &str) {
        self.git(&["add", "-A"]);
        self.git(&["commit", "-q", "--allow-empty", "-m", message]);
    }

    fn has_origin(&self) -> bool {
        self.git(&["remote"]).lines().any(|remote| remote == "origin")
    }

    /// A local branch, pushed to origin when there is one.
    pub fn add_branch(&self, name: &str) {
        self.git(&["branch", name]);
        if self.has_origin() {
            self.git(&["push", "-q", "origin", name]);
        }
    }

    /// A branch that exists only on `remote`.
    pub fn add_remote_only_branch(&self, name: &str, remote: &str) {
        self.git(&["branch", name]);
        self.git(&["push", "-q", remote, name]);
        self.git(&["branch", "-D", name]);
    }

    /// Another bare remote alongside origin.
    pub fn add_remote(&self, name: &str) {
        let repo_name = self.path.file_name().unwrap().to_string_lossy();
        let bare = self.sandbox.join("remotes").join(format!("{repo_name}-{name}.git"));
        init_bare(&bare);
        self.git(&["remote", "add", name, &bare.to_string_lossy()]);
    }

    pub fn write_project_config(&self, content: &str) {
        fs::write(self.path.join(".workforest.yaml"), content).unwrap();
    }

    pub fn make_dirty(&self, worktree: &Path) {
        fs::write(worktree.join("dirty.txt"), "uncommitted\n").unwrap();
    }
}

/// Write an executable file through a child process: a file this (many-
/// threaded) test process had open for writing could still be held by
/// another test's fork when it is first run — "text file busy".
pub fn write_executable(path: &Path, content: &str) {
    use std::io::Write;
    let mut writer = Command::new("sh")
        .args(["-c", "cat > \"$1\" && chmod +x \"$1\"", "sh"])
        .arg(path)
        .stdin(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    writer.stdin.take().unwrap().write_all(content.as_bytes()).unwrap();
    assert!(writer.wait().unwrap().success());
}

/// Executable stub that logs each invocation instead of doing anything.
pub struct Recorder {
    pub path: PathBuf,
    log: PathBuf,
}

impl Recorder {
    pub fn new(directory: &Path) -> Self {
        let log = directory.join("recorder.log");
        let path = directory.join("recorder");
        write_executable(
            &path,
            &format!(
                "#!/bin/sh\necho \"argv=$* argc=$# cwd=$PWD wf_worktree=$WF_WORKTREE \
                 virtual_env=$VIRTUAL_ENV\" >> {}\n",
                log.display()
            ),
        );
        Self { path, log }
    }

    pub fn lines(&self) -> Vec<String> {
        fs::read_to_string(&self.log).unwrap_or_default().lines().map(str::to_string).collect()
    }

    /// Poll for detached spawns that write the log asynchronously.
    pub fn wait_for_lines(&self, count: usize) -> Vec<String> {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while std::time::Instant::now() < deadline {
            let lines = self.lines();
            if lines.len() >= count {
                return lines;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        panic!("recorder log never reached {count} line(s): {:?}", self.lines());
    }
}
