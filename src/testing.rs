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

    pub fn make_dirty(&self, worktree: &Path) {
        fs::write(worktree.join("dirty.txt"), "uncommitted\n").unwrap();
    }

    pub fn write_project_config(&self, content: &str) -> PathBuf {
        let config = self.path.join(".workforest.yaml");
        fs::write(&config, content).unwrap();
        config
    }
}
