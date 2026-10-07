//! Fixtures for the integration tests: the built `workforest` binary run
//! against throwaway git repositories.
//!
//! Isolation contract: no test may read or write the real environment.
//! Every process started here — the binary, git, the shells — gets the
//! sandbox's HOME, XDG_CONFIG_HOME and git config, SHELL=/bin/sh, a stub
//! EDITOR, and NO_COLOR; rely on that instead of setting them per test.
#![allow(dead_code)]

use std::ffi::OsString;
use std::fs;
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

pub const BINARY: &str = env!("CARGO_BIN_EXE_workforest");
pub const WF_BINARY: &str = env!("CARGO_BIN_EXE_wf");
/// Marks a stdout line as a directive for the wf shell wrapper.
pub const DIRECTIVE: &str = "\x1f";

/// What a finished process left behind.
#[derive(Debug)]
pub struct Run {
    /// The exit status, or -N when signal N ended it.
    pub code: i32,
    pub out: String,
    pub err: String,
}

impl Run {
    #[track_caller]
    pub fn ok(self) -> Self {
        assert_eq!(self.code, 0, "stdout: {}\nstderr: {}", self.out, self.err);
        self
    }

    /// The last line of stderr: what the editor plugins show for a failure.
    pub fn last_error(&self) -> &str {
        self.err.lines().last().unwrap_or_default()
    }
}

pub fn exit_code(status: ExitStatus) -> i32 {
    status.code().unwrap_or_else(|| -status.signal().unwrap_or(0))
}

/// A temporary directory holding a home, a git config, and repositories.
pub struct Sandbox {
    _dir: tempfile::TempDir,
    root: PathBuf,
}

impl Sandbox {
    pub fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        // git reports real paths; /tmp may be a symlink (macOS).
        let root = dir.path().canonicalize().unwrap();
        fs::create_dir_all(root.join("home/.config")).unwrap();
        fs::write(
            root.join("gitconfig"),
            "[user]\n\tname = Test\n\temail = test@example.invalid\n[init]\n\tdefaultBranch = main\n",
        )
        .unwrap();
        Self { _dir: dir, root }
    }

    pub fn path(&self) -> &Path {
        &self.root
    }

    pub fn home(&self) -> PathBuf {
        self.root.join("home")
    }

    /// A command with the isolated environment; coverage instrumentation
    /// (when the suite runs under it) is the one thing let through.
    pub fn command(&self, program: impl AsRef<std::ffi::OsStr>) -> Command {
        let mut command = Command::new(program);
        for (name, _) in std::env::vars_os() {
            let text = name.to_string_lossy();
            let ours = ["WF_", "WORKFOREST_", "GIT_"].iter().any(|prefix| text.starts_with(prefix));
            if ours || ["VISUAL", "CLICOLOR_FORCE", "VIRTUAL_ENV", "MANPATH"].contains(&&*text) {
                command.env_remove(&name);
            }
        }
        command
            .env("HOME", self.home())
            .env("XDG_CONFIG_HOME", self.home().join(".config"))
            .env("GIT_CONFIG_GLOBAL", self.root.join("gitconfig"))
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("SHELL", "/bin/sh")
            .env("EDITOR", "stub-editor")
            .env("NO_COLOR", "1");
        command
    }

    fn finish(mut command: Command) -> Run {
        let output = command.stdin(Stdio::null()).output().unwrap();
        Run {
            code: exit_code(output.status),
            out: String::from_utf8_lossy(&output.stdout).into_owned(),
            err: String::from_utf8_lossy(&output.stderr).into_owned(),
        }
    }

    /// Run `workforest ARGS` in `cwd`.
    pub fn wf(&self, cwd: &Path, args: &[&str]) -> Run {
        self.wf_env(cwd, args, &[])
    }

    pub fn wf_env(&self, cwd: &Path, args: &[&str], env: &[(&str, &str)]) -> Run {
        let mut command = self.command(BINARY);
        command.current_dir(cwd).args(args).envs(env.iter().copied());
        Self::finish(command)
    }

    /// Start `workforest ARGS` and leave it running.
    pub fn spawn_wf(&self, cwd: &Path, args: &[&str]) -> Running {
        let child = self
            .command(BINARY)
            .current_dir(cwd)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        Running(Some(child))
    }

    /// Run a script in a real shell with the binary on PATH as `workforest`
    /// (and `wf`).
    pub fn shell(&self, shell: &str, script: &str, cwd: &Path) -> Run {
        let bin = self.root.join("bin");
        if !bin.exists() {
            fs::create_dir(&bin).unwrap();
            std::os::unix::fs::symlink(BINARY, bin.join("workforest")).unwrap();
            std::os::unix::fs::symlink(BINARY, bin.join("wf")).unwrap();
        }
        let mut path = OsString::from(&bin);
        path.push(":");
        path.push(std::env::var_os("PATH").unwrap_or_default());
        let mut command = self.command(shell);
        command.args(["-c", script]).current_dir(cwd).env("PATH", path);
        Self::finish(command)
    }

    fn git_in(&self, cwd: &Path, args: &[&str]) -> String {
        let output = self.command("git").current_dir(cwd).args(args).output().unwrap();
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    fn init_bare(&self, path: &Path) {
        fs::create_dir_all(path).unwrap();
        self.git_in(path, &["init", "-q", "--bare"]);
    }

    /// A real git repository at `dev/<name>` with one commit on `main`.
    pub fn repo(&self, name: &str) -> Repo<'_> {
        let path = self.root.join("dev").join(name);
        fs::create_dir_all(&path).unwrap();
        let repo = Repo { sandbox: self, path };
        repo.git(&["init", "-q", "-b", "main"]);
        fs::write(repo.path.join("README.md"), format!("# {name}\n")).unwrap();
        repo.commit("init");
        repo
    }

    pub fn repo_with_origin(&self, name: &str) -> Repo<'_> {
        let repo = self.repo(name);
        let bare = self.root.join("remotes").join(format!("{name}.git"));
        self.init_bare(&bare);
        repo.git(&["remote", "add", "origin", &bare.to_string_lossy()]);
        repo.git(&["push", "-q", "-u", "origin", "main"]);
        repo
    }

    pub fn write_user_config(&self, content: &str) -> PathBuf {
        let directory = self.home().join(".config/workforest");
        fs::create_dir_all(&directory).unwrap();
        let path = directory.join("config.yaml");
        fs::write(&path, content).unwrap();
        path
    }

    /// An executable shell script.
    pub fn script(&self, name: &str, body: &str) -> PathBuf {
        let path = self.root.join(name);
        write_executable(&path, &format!("#!/bin/sh\n{body}\n"));
        path
    }
}

/// Write an executable file through a child process: a file this (many-
/// threaded) process had open for writing could still be held by another
/// test's fork when it is first run — "text file busy".
pub fn write_executable(path: &Path, content: &str) {
    let mut writer = Command::new("sh")
        .args(["-c", "cat > \"$1\" && chmod +x \"$1\"", "sh"])
        .arg(path)
        .stdin(Stdio::piped())
        .spawn()
        .unwrap();
    writer.stdin.take().unwrap().write_all(content.as_bytes()).unwrap();
    assert!(writer.wait().unwrap().success());
}

/// Handle to a throwaway git repository.
pub struct Repo<'a> {
    sandbox: &'a Sandbox,
    pub path: PathBuf,
}

impl Repo<'_> {
    pub fn git(&self, args: &[&str]) -> String {
        self.sandbox.git_in(&self.path, args)
    }

    pub fn git_in(&self, cwd: &Path, args: &[&str]) -> String {
        self.sandbox.git_in(cwd, args)
    }

    pub fn commit(&self, message: &str) {
        self.git(&["add", "-A"]);
        self.git(&["commit", "-q", "--allow-empty", "-m", message]);
    }

    /// A local branch, pushed to origin when there is one.
    pub fn add_branch(&self, name: &str) {
        self.git(&["branch", name]);
        if self.git(&["remote"]).lines().any(|remote| remote == "origin") {
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
        let bare = self.sandbox.root.join("remotes").join(format!("{repo_name}-{name}.git"));
        self.sandbox.init_bare(&bare);
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

    /// Where this repository's worktrees live by default.
    pub fn worktrees_dir(&self) -> PathBuf {
        self.path.parent().unwrap().join("worktrees").join(self.path.file_name().unwrap())
    }

    /// `workforest ARGS` at the repository root.
    pub fn wf(&self, args: &[&str]) -> Run {
        self.sandbox.wf(&self.path, args)
    }

    /// A managed worktree for a new branch; returns its path.
    pub fn create(&self, branch: &str) -> PathBuf {
        self.wf(&["create", branch, "--no-open"]).ok();
        self.worktrees_dir().join(branch.rsplit('/').next().unwrap())
    }

    /// The repository's shared git dir.
    pub fn common_dir(&self) -> PathBuf {
        self.path.join(".git")
    }
}

/// A `workforest` left running by a test; killed when the test is done.
pub struct Running(Option<Child>);

impl Running {
    pub fn pid(&self) -> i32 {
        self.0.as_ref().unwrap().id() as i32
    }

    pub fn is_running(&mut self) -> bool {
        self.0.as_mut().unwrap().try_wait().unwrap().is_none()
    }

    pub fn signal(&self, signal: nix::sys::signal::Signal) {
        nix::sys::signal::kill(nix::unistd::Pid::from_raw(self.pid()), signal).unwrap();
    }

    /// Wait for it to end.
    pub fn finish(mut self) -> Run {
        let output = self.0.take().unwrap().wait_with_output().unwrap();
        Run {
            code: exit_code(output.status),
            out: String::from_utf8_lossy(&output.stdout).into_owned(),
            err: String::from_utf8_lossy(&output.stderr).into_owned(),
        }
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        if let Some(child) = &mut self.0 {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

#[track_caller]
pub fn wait_for(what: &str, condition: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if condition() {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("not in time: {what}");
}

/// Executable stub that logs each invocation instead of doing anything.
pub struct Recorder {
    pub path: PathBuf,
    log: PathBuf,
}

impl Recorder {
    pub fn new(sandbox: &Sandbox) -> Self {
        let log = sandbox.path().join("recorder.log");
        let body = format!(
            "echo \"argv=$* argc=$# cwd=$PWD wf_worktree=$WF_WORKTREE virtual_env=$VIRTUAL_ENV\" >> {}",
            log.display()
        );
        Self { path: sandbox.script("recorder", &body), log }
    }

    pub fn lines(&self) -> Vec<String> {
        fs::read_to_string(&self.log).unwrap_or_default().lines().map(str::to_string).collect()
    }

    /// Poll for detached spawns that write the log asynchronously.
    pub fn wait_for_lines(&self, count: usize) -> Vec<String> {
        wait_for("the recorder log", || self.lines().len() >= count);
        self.lines()
    }
}

/// A process whose stdin and stderr are a terminal: prompts appear, and
/// what is "typed" is ours to script. stdout stays a pipe, as it is under
/// the shell wrapper.
pub struct Terminal {
    child: Child,
    master: fs::File,
}

impl Terminal {
    /// Start `workforest ARGS` on a terminal of `rows` × `cols`.
    pub fn spawn(sandbox: &Sandbox, cwd: &Path, args: &[&str], rows: u16, cols: u16) -> Self {
        let size = nix::libc::winsize { ws_row: rows, ws_col: cols, ws_xpixel: 0, ws_ypixel: 0 };
        let pty = nix::pty::openpty(Some(&size), None).unwrap();
        let slave = |fd: &OwnedFd| Stdio::from(fd.try_clone().unwrap());
        let mut command = sandbox.command(BINARY);
        command
            .current_dir(cwd)
            .args(args)
            .env("TERM", "xterm-256color")
            .stdin(slave(&pty.slave))
            .stderr(slave(&pty.slave))
            .stdout(Stdio::piped());
        let slave_fd = pty.slave.as_raw_fd();
        // The terminal becomes the child's controlling one, as a shell's is.
        unsafe {
            use std::os::unix::process::CommandExt;
            command.pre_exec(move || {
                nix::unistd::setsid().map_err(std::io::Error::from)?;
                if nix::libc::ioctl(slave_fd, nix::libc::TIOCSCTTY as _, 0) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let child = command.spawn().unwrap();
        drop(pty.slave);
        // SAFETY: the master descriptor is ours alone from here on.
        let master =
            unsafe { fs::File::from_raw_fd(std::os::fd::IntoRawFd::into_raw_fd(pty.master)) };
        Self { child, master }
    }

    /// Type something.
    pub fn send(&mut self, text: &str) {
        self.master.write_all(text.as_bytes()).unwrap();
    }

    /// Give the program a moment to act on what was typed.
    pub fn settle(&self) {
        std::thread::sleep(Duration::from_millis(300));
    }

    /// Wait for the end: the exit code, stdout, and everything the terminal
    /// was sent (stderr, echoes and escape sequences included).
    pub fn finish(mut self) -> (i32, String, String) {
        let mut stdout = self.child.stdout.take().unwrap();
        let out = std::thread::spawn(move || {
            let mut text = String::new();
            let _ = stdout.read_to_string(&mut text);
            text
        });
        let mut screen = Vec::new();
        let mut buffer = [0u8; 4096];
        // EIO once the child, the last holder of the other end, is gone.
        while let Ok(count) = self.master.read(&mut buffer) {
            if count == 0 {
                break;
            }
            screen.extend_from_slice(&buffer[..count]);
        }
        let status = self.child.wait().unwrap();
        (exit_code(status), out.join().unwrap(), String::from_utf8_lossy(&screen).replace('\r', ""))
    }
}
