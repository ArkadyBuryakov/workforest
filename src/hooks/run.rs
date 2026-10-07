//! The process side of running scripts: spawning into process groups,
//! handing the terminal over and back, forwarding signals, and the forked
//! supervisors — the detached one behind `background`, the group one
//! behind `bulk` and `pipeline`, and a bulk's per-member runners.
//!
//! A forked child never returns into its parent's code path: each
//! `supervise_*`/`run_member` body ends in `exit_with`.

use std::fs::OpenOptions;
use std::io::Write;
use std::os::fd::{AsRawFd, BorrowedFd, IntoRawFd, OwnedFd};
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use nix::errno::Errno;
use nix::libc;
use nix::poll::{PollFd, PollFlags, PollTimeout, poll};
use nix::sys::signal::{SaFlags, SigAction, SigHandler, SigSet, Signal, sigaction};
use nix::sys::wait::{WaitPidFlag, WaitStatus, waitpid};
use nix::unistd::{ForkResult, Pid, fork, getpgrp, getpid, setpgid, setsid};

use super::{
    Job, JobResult, Prefixer, Runner, bulk_outcome, failure, interrupted, log_tail, prepare,
    run_cleanup, start_message, stderr_sink,
};
use crate::errors::{Error, ErrorKind, Result};
use crate::launch::{GRACE, exit_code};
use crate::util::{self, Env, repr};
use crate::{jobs, output};

const FORWARDED_SIGNALS: [Signal; 3] = [Signal::SIGINT, Signal::SIGTERM, Signal::SIGHUP];
const SIGINT: i32 = Signal::SIGINT as i32;

/// A duplicate of our stderr, for a child's stdout.
fn stderr_for_child() -> std::io::Result<Stdio> {
    Ok(Stdio::from(std::io::stderr().as_fd_owned()?))
}

trait AsFdOwned {
    fn as_fd_owned(&self) -> std::io::Result<OwnedFd>;
}

impl AsFdOwned for std::io::Stderr {
    fn as_fd_owned(&self) -> std::io::Result<OwnedFd> {
        // SAFETY: fd 2 stays open for the life of the process.
        unsafe { BorrowedFd::borrow_raw(self.as_raw_fd()) }.try_clone_to_owned()
    }
}

/// Run a config-defined shell snippet with stdout diverted to stderr;
/// returns its exit status, or -N when signal N killed it.
pub fn run_snippet(snippet: &str, cwd: &Path, env: &Env) -> Result<i32> {
    let shell = util::user_shell(env);
    let cannot_run = |error: std::io::Error| {
        Error::new(format!(
            "cannot run {} via $SHELL ({}): {}",
            repr(snippet),
            repr(&shell),
            util::os_error_text(&error)
        ))
    };
    let status = Command::new(&shell)
        .arg("-c")
        .arg(snippet)
        .current_dir(cwd)
        .env_clear()
        .envs(env)
        .stdout(stderr_for_child().map_err(cannot_run)?)
        .status()
        .map_err(cannot_run)?;
    Ok(exit_code(status))
}

/// A descriptor of our controlling terminal, or None when there is none
/// (piped, or under test).
fn controlling_tty() -> Option<i32> {
    // SAFETY: isatty only inspects the descriptor.
    [0, 2, 1].into_iter().find(|fd| unsafe { libc::isatty(*fd) } == 1)
}

/// Make `pgid` the terminal's foreground group. SIGTTOU is what a
/// background group gets for trying, so it is ignored around the call.
/// Async-signal-safe: also runs between fork and exec.
fn give_terminal(fd: i32, pgid: i32) {
    // SAFETY: plain syscalls on a descriptor we hold; the previous SIGTTOU
    // disposition is put back.
    unsafe {
        let previous = libc::signal(libc::SIGTTOU, libc::SIG_IGN);
        libc::tcsetpgrp(fd, pgid);
        libc::signal(libc::SIGTTOU, previous);
    }
}

/// The group the forwarding handler signals; 0 when nothing is waited for.
static FORWARD_TO: AtomicI32 = AtomicI32::new(0);

extern "C" fn forward_signal(signum: i32) {
    let pgid = FORWARD_TO.load(Ordering::SeqCst);
    if pgid > 0 {
        let saved = Errno::last_raw();
        // SAFETY: killpg is async-signal-safe.
        unsafe { libc::killpg(pgid, signum) };
        Errno::set_raw(saved);
    }
}

extern "C" fn ignore_signal(_signum: i32) {}

static INTERRUPTED: AtomicBool = AtomicBool::new(false);
static SUSPEND: AtomicBool = AtomicBool::new(false);
static RESUME: AtomicBool = AtomicBool::new(false);

extern "C" fn note_signal(signum: i32) {
    let flag = match signum {
        libc::SIGTSTP => &SUSPEND,
        libc::SIGCONT => &RESUME,
        _ => &INTERRUPTED,
    };
    flag.store(true, Ordering::SeqCst);
}

/// Install a handler; returns what was there, to put back.
fn handle(signal: Signal, handler: extern "C" fn(i32)) -> Option<SigAction> {
    let action = SigAction::new(SigHandler::Handler(handler), SaFlags::SA_RESTART, SigSet::empty());
    // SAFETY: every handler here only touches atomics or makes an
    // async-signal-safe syscall.
    unsafe { sigaction(signal, &action) }.ok()
}

fn restore(signal: Signal, previous: Option<SigAction>) {
    if let Some(previous) = previous {
        // SAFETY: puts back a disposition that was installed before.
        let _ = unsafe { sigaction(signal, &previous) };
    }
}

fn status_code(status: WaitStatus) -> Option<i32> {
    match status {
        WaitStatus::Exited(_, code) => Some(code),
        WaitStatus::Signaled(_, signal, _) => Some(-(signal as i32)),
        _ => None,
    }
}

/// Wait for the group leader `pgid` (our child), forwarding the signals we
/// get to the whole group. On a terminal, a stopped command (Ctrl-Z) stops
/// us too — the shell then owns the job — and is resumed with us.
fn wait(pgid: i32, tty_fd: Option<i32>) -> i32 {
    FORWARD_TO.store(pgid, Ordering::SeqCst);
    let previous = FORWARDED_SIGNALS.map(|signal| (signal, handle(signal, forward_signal)));
    let flags = tty_fd.map(|_| WaitPidFlag::WUNTRACED);
    let code = loop {
        match waitpid(Pid::from_raw(pgid), flags) {
            Ok(WaitStatus::Stopped(..)) => {
                if let Some(fd) = tty_fd {
                    give_terminal(fd, getpgrp().as_raw());
                    let _ = nix::sys::signal::kill(getpid(), Signal::SIGSTOP);
                    give_terminal(fd, pgid);
                }
                jobs::signal_group(pgid, Signal::SIGCONT);
            }
            Ok(status) => {
                if let Some(code) = status_code(status) {
                    break code;
                }
            }
            Err(Errno::EINTR) => {}
            Err(_) => break 1, // not our child after all: nothing to wait for
        }
    };
    for (signal, action) in previous {
        restore(signal, action);
    }
    FORWARD_TO.store(0, Ordering::SeqCst);
    if let Some(fd) = tty_fd {
        give_terminal(fd, getpgrp().as_raw());
    }
    code
}

/// Point fds 1 and 2 at `fd`.
fn redirect_output(fd: i32) {
    // SAFETY: dup2 onto the standard descriptors of a process we own.
    unsafe {
        libc::dup2(fd, 1);
        libc::dup2(fd, 2);
    }
}

fn stdin_from_devnull() {
    if let Ok(devnull) = OpenOptions::new().read(true).open("/dev/null") {
        // SAFETY: dup2 onto our own stdin.
        unsafe { libc::dup2(devnull.as_raw_fd(), 0) };
    }
}

/// Start the job as the leader of a new process group and return its pid:
/// `$SHELL -c` for a command, a forked supervisor for a group.
fn spawn(job: &Job, tty_fd: Option<i32>) -> Result<i32> {
    if job.spec.command.is_none() {
        // SAFETY: we are single-threaded here (worker threads only exist
        // inside `list`), so the child may run ordinary code.
        return match unsafe { fork() } {
            Ok(ForkResult::Child) => supervise_group(job, tty_fd),
            Ok(ForkResult::Parent { child }) => {
                // Both sides set the group so that it exists whichever
                // runs first.
                let _ = setpgid(child, child);
                Ok(child.as_raw())
            }
            Err(errno) => Err(Error::new(format!("cannot fork: {}", errno.desc()))),
        };
    }
    let cannot_run = |error: std::io::Error| {
        Error::new(format!(
            "cannot run {} via $SHELL: {}",
            repr(&job.name),
            util::os_error_text(&error)
        ))
    };
    let mut command = Command::new(util::user_shell(&job.env));
    command
        .arg("-c")
        .arg(&job.snippet)
        .current_dir(&job.cwd)
        .env_clear()
        .envs(&job.env)
        .stdout(stderr_for_child().map_err(cannot_run)?)
        .process_group(0);
    if let Some(fd) = tty_fd {
        // Before exec, the command claims the terminal for its own (new)
        // group itself: doing it only from the parent would race the
        // command's first read.
        // SAFETY: give_terminal is async-signal-safe.
        unsafe {
            command.pre_exec(move || {
                give_terminal(fd, libc::getpgrp());
                Ok(())
            });
        }
    }
    // The child is reaped by `wait`, by pid: the handle is not needed.
    Ok(command.spawn().map_err(cannot_run)?.id() as i32)
}

/// Run the job in its own process group with a job record on disk for as
/// long as it runs.
fn run_command(job: &Job, record_path: &Path) -> Result<JobResult> {
    let tty_fd = if job.tty { controlling_tty() } else { None };
    let pgid = spawn(job, tty_fd)?;
    let record = jobs::JobRecord {
        script: job.name.clone(),
        worktree: job.cwd.to_string_lossy().into_owned(),
        branch: util::env_get(&job.env, "WF_BRANCH").unwrap_or_default(),
        pgid,
        owner_pid: getpid().as_raw(),
        boot_id: jobs::boot_id(),
        started_at: jobs::now(),
        stopped_by: None,
    };
    if let Err(error) = jobs::write_record(record_path, &record) {
        output::warn(&format!(
            "cannot record {} as running ({}): `wf stop` will not find it",
            repr(&job.name),
            util::os_error_text(&error)
        ));
    }
    let code = wait(pgid, tty_fd);
    let stopped_by = jobs::read_record(record_path).and_then(|record| record.stopped_by);
    Ok(JobResult { code, stopped_by })
}

/// Command, then cleanup, then — and only then — the record goes. We own
/// the run, so our pid names the instance.
pub(crate) fn run_to_completion(job: &Job) -> Result<JobResult> {
    let record_path = job.record_path(getpid().as_raw());
    let outcome = run_command(job, &record_path).and_then(|result| {
        // Ctrl-C during the cleanup reaches the cleanup itself (it shares
        // our group); we stay to drop the record, then end as interrupted.
        INTERRUPTED.store(false, Ordering::SeqCst);
        let previous = handle(Signal::SIGINT, note_signal);
        let cleaned = run_cleanup(&job.spec, &job.name, &job.cwd, &job.env);
        restore(Signal::SIGINT, previous);
        if INTERRUPTED.swap(false, Ordering::SeqCst) {
            return Err(Error::interrupted());
        }
        cleaned.map(|()| result)
    });
    let _ = std::fs::remove_file(&record_path);
    outcome
}

/// Write what coverage instrumentation has counted so far: a process that
/// ends by `_exit` or by a signal never reaches the usual exit hook.
pub fn flush_coverage() {
    #[cfg(coverage)]
    {
        unsafe extern "C" {
            fn __llvm_profile_write_file() -> i32;
        }
        // SAFETY: provided by the profiling runtime the build links in.
        unsafe { __llvm_profile_write_file() };
    }
}

/// End the process by `signum`, as the default action would; returns only
/// if the signal is blocked or ignored.
pub fn die_by(signum: i32) {
    flush_coverage();
    // SAFETY: resets a disposition and signals ourselves.
    unsafe {
        libc::signal(signum, libc::SIG_DFL);
        libc::kill(libc::getpid(), signum);
    }
}

/// End a forked child the way its command ended: by the same signal for a
/// signal death (so the parent reports it as such, and SIGINT keeps
/// aborting shell loops), else with the status. Never returns.
fn exit_with(code: i32) -> ! {
    let _ = std::io::stderr().flush();
    let mut code = code;
    if code < 0 {
        die_by(-code);
        code = 128 - code; // the signal is blocked or ignored: fall back to the convention
    }
    flush_coverage();
    // SAFETY: ends this process without running the parent's exit hooks.
    unsafe { libc::_exit(code) }
}

/// What a supervisor's work came to: the outcome, with an error reported
/// and turned into one.
fn outcome_of(result: Result<i32>) -> i32 {
    match result {
        Ok(code) => code,
        Err(error) if error.kind == ErrorKind::Interrupted => -SIGINT,
        Err(error) => {
            output::error(&error.message);
            1
        }
    }
}

/// The background supervisor: our fork, in a session of its own, with the
/// log as its stdout/stderr. Runs the command exactly like the foreground
/// path does and exits as `wf run` would; never returns.
fn supervise_detached(job: &Job) -> ! {
    let _ = setsid();
    stdin_from_devnull();
    let log = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o644)
        .open(job.log_path(getpid().as_raw()));
    if let Ok(log) = log {
        redirect_output(log.as_raw_fd());
    }
    let code = match run_to_completion(job) {
        Ok(result) => {
            if let Some(by) = result.stopped_by.as_deref().filter(|by| !by.is_empty()) {
                output::info(&format!("stopped by {by}"));
            }
            if result.code >= 0 { result.code } else { 128 - result.code }
        }
        Err(error) => {
            output::error(&error.message);
            1
        }
    };
    flush_coverage();
    // SAFETY: ends this process without running the parent's exit hooks.
    unsafe { libc::_exit(code) }
}

/// Fork a detached supervisor for the command and return once it is
/// clearly running — a command that dies within the grace period is
/// reported with the tail of its log instead of failing invisibly. The
/// supervisor writes the log; its pid is what names it.
pub(crate) fn start_background(job: &Job) -> Result<()> {
    let own_log = job.log_path(getpid().as_raw());
    if let Some(directory) = own_log.parent() {
        // one dir per script
        std::fs::create_dir_all(directory).map_err(|error| {
            Error::new(format!(
                "cannot create {}: {}",
                directory.display(),
                util::os_error_text(&error)
            ))
        })?;
    }
    // SAFETY: single-threaded here, see `spawn`.
    let pid = match unsafe { fork() } {
        Ok(ForkResult::Child) => supervise_detached(job),
        Ok(ForkResult::Parent { child }) => child,
        Err(errno) => return Err(Error::new(format!("cannot fork: {}", errno.desc()))),
    };
    let log_path = job.log_path(pid.as_raw());
    let deadline = Instant::now() + GRACE;
    while Instant::now() < deadline {
        let reaped = waitpid(pid, Some(WaitPidFlag::WNOHANG)).ok().and_then(status_code);
        if let Some(code) = reaped {
            if code == 0 {
                output::success(&format!(
                    "{} already finished (log: {})",
                    repr(&job.name),
                    log_path.display()
                ));
                return Ok(());
            }
            let tail = log_tail(&log_path, 10);
            let mut message =
                format!("script {} exited with status {code} right after launch", repr(&job.name));
            if !tail.is_empty() {
                message.push_str(":\n");
                message.push_str(&tail);
            }
            return Err(Error::new(message));
        }
        thread::sleep(Duration::from_millis(20));
    }
    output::success(&format!(
        "started {} in the background (pid {pid}, log: {})",
        repr(&job.name),
        log_path.display()
    ));
    Ok(())
}

// --- groups -----------------------------------------------------------------

/// The group supervisor: our fork, leading a group of its own and — on a
/// terminal — holding its foreground, as a command would. Runs the members
/// and ends the way the group's outcome says; never returns.
fn supervise_group(job: &Job, tty_fd: Option<i32>) -> ! {
    let _ = setpgid(Pid::from_raw(0), Pid::from_raw(0));
    if let Some(fd) = tty_fd {
        give_terminal(fd, getpgrp().as_raw());
    }
    // SAFETY: our stdout becomes our stderr, as a command's does.
    unsafe { libc::dup2(2, 1) };
    let result = if job.spec.pipeline.is_some() { run_pipeline(job) } else { run_bulk(job) };
    exit_with(outcome_of(result))
}

/// A member's run inside a supervisor: `wf run MEMBER`, reporting the way
/// `cli.rs` does, but returning the outcome — the exit status, or -N for a
/// death by signal N — instead of failing. Only an interruption of our own
/// (Ctrl-C during a cleanup) is an error.
pub(crate) fn run_step(job: &Job) -> Result<i32> {
    let finished = if job.spec.background {
        start_background(job).map(|()| None)
    } else {
        output::success(&start_message(job));
        run_to_completion(job).map(Some)
    };
    let result = match finished {
        Ok(None) => return Ok(0),
        Ok(Some(result)) => result,
        Err(error) if error.kind == ErrorKind::Interrupted => return Err(error),
        Err(error) => {
            output::error(&error.message);
            return Ok(error.exit_code());
        }
    };
    if interrupted(result.code) {
        output::warn(&format!("script {} was interrupted", repr(&job.name)));
        return Ok(-SIGINT);
    }
    if let Some(message) = failure(&result, &job.name) {
        output::error(&message);
    }
    Ok(result.code)
}

/// Members one after another, each a `wf run` of its own with the
/// terminal; the first failure ends the pipeline with its outcome.
fn run_pipeline(job: &Job) -> Result<i32> {
    let members = job.spec.pipeline.as_deref().unwrap_or_default();
    for (index, member) in members.iter().enumerate() {
        output::info(&format!(
            "{} step {}/{}: {member}",
            repr(&job.name),
            index + 1,
            members.len()
        ));
        let step = match prepare(job.config, member, &job.cwd, &job.env, &[], job.tty) {
            Ok(step) => step,
            Err(error) => {
                output::error(&error.message);
                return Ok(error.exit_code());
            }
        };
        let code = run_step(&step)?;
        if code != 0 {
            return Ok(code);
        }
    }
    Ok(0)
}

fn set_cloexec(fd: i32) {
    // SAFETY: fcntl on a descriptor we just opened.
    unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) };
}

/// (read end, write end) of a member's output channel: a pseudo-terminal
/// when our stderr is one, so the member's programs keep coloring their
/// output as they do for `wf run`; a pipe otherwise (a log, CI). Neither
/// end survives an exec on its own: the member gets the write end as its
/// stdout and stderr, and nothing else holds it.
pub(crate) fn open_channel(tty: bool) -> Result<(i32, i32)> {
    let failed =
        |errno: Errno| Error::new(format!("cannot open an output channel: {}", errno.desc()));
    let (read_end, write_end) = if tty {
        let mut size: libc::winsize = unsafe { std::mem::zeroed() };
        // SAFETY: TIOCGWINSZ fills the struct it is given.
        let sized = unsafe { libc::ioctl(2, libc::TIOCGWINSZ, &mut size) } == 0;
        let pty = nix::pty::openpty(sized.then_some(&size), None).map_err(failed)?;
        (pty.master.into_raw_fd(), pty.slave.into_raw_fd())
    } else {
        let (read_end, write_end) = nix::unistd::pipe().map_err(failed)?;
        (read_end.into_raw_fd(), write_end.into_raw_fd())
    };
    set_cloexec(read_end);
    set_cloexec(write_end);
    Ok((read_end, write_end))
}

fn close(fd: i32) {
    // SAFETY: closes a descriptor this module opened and still owns.
    unsafe { libc::close(fd) };
}

/// Relay what the channel holds; close it at EOF (a pty reports that as
/// EIO once every writer is gone).
fn drain(runner: &mut Runner, prefixer: &mut Prefixer, sink: &mut dyn Write) {
    let mut buffer = vec![0u8; 65536];
    while runner.fd >= 0 {
        // SAFETY: reads into a buffer of the stated length.
        let count = unsafe { libc::read(runner.fd, buffer.as_mut_ptr().cast(), buffer.len()) };
        if count > 0 {
            let _ =
                sink.write_all(prefixer.feed(&runner.name, &buffer[..count as usize]).as_bytes());
            continue;
        }
        if count < 0 && matches!(Errno::last(), Errno::EAGAIN | Errno::EINTR) {
            break;
        }
        // EOF, EIO from a pty, or a channel that broke: nothing more comes.
        let _ = sink.write_all(prefixer.flush(&runner.name).as_bytes());
        close(runner.fd);
        runner.fd = -1;
    }
    let _ = sink.flush();
}

/// Relay every runner's output to `sink` until all have ended and their
/// channels are drained. A channel a reaped runner left open (a daemon it
/// spawned still holds the write end) is drained once more and closed.
/// `tick` runs once per round, for what signals asked of us meanwhile.
pub(crate) fn pump(
    runners: &mut [Runner],
    prefixer: &mut Prefixer,
    sink: &mut dyn Write,
    tick: &mut dyn FnMut(&[Runner]),
) {
    for runner in runners.iter() {
        // SAFETY: fcntl on a descriptor we own.
        unsafe {
            let flags = libc::fcntl(runner.fd, libc::F_GETFL);
            libc::fcntl(runner.fd, libc::F_SETFL, flags | libc::O_NONBLOCK);
        }
    }
    while runners.iter().any(|runner| runner.code.is_none() || runner.fd >= 0) {
        let open: Vec<usize> = (0..runners.len()).filter(|index| runners[*index].fd >= 0).collect();
        // SAFETY: the descriptors stay open until `drain` closes them below.
        let mut fds: Vec<PollFd> = open
            .iter()
            .map(|index| {
                PollFd::new(
                    unsafe { BorrowedFd::borrow_raw(runners[*index].fd) },
                    PollFlags::POLLIN,
                )
            })
            .collect();
        let ready: Vec<usize> = match poll(&mut fds, PollTimeout::from(50u8)) {
            Ok(count) if count > 0 => open
                .iter()
                .zip(&fds)
                .filter(|(_, fd)| fd.revents().is_some_and(|events| !events.is_empty()))
                .map(|(index, _)| *index)
                .collect(),
            Ok(_) => Vec::new(),
            Err(_) => {
                // interrupted by a signal; and never spin on a broken set
                if fds.is_empty() {
                    thread::sleep(Duration::from_millis(50));
                }
                Vec::new()
            }
        };
        drop(fds);
        if open.is_empty() {
            thread::sleep(Duration::from_millis(50));
        }
        for index in ready {
            drain(&mut runners[index], prefixer, sink);
        }
        for runner in runners.iter_mut() {
            if runner.code.is_some() {
                continue;
            }
            let reaped = waitpid(Pid::from_raw(runner.pid), Some(WaitPidFlag::WNOHANG));
            let code = match reaped {
                Ok(status) => status_code(status),
                Err(Errno::EINTR) => None,
                Err(_) => Some(1), // not a child of ours: it cannot be waited for
            };
            let Some(code) = code else {
                continue;
            };
            runner.code = Some(code);
            if runner.fd >= 0 {
                drain(runner, prefixer, sink);
                if runner.fd >= 0 {
                    let _ = sink.write_all(prefixer.flush(&runner.name).as_bytes());
                    close(runner.fd);
                    runner.fd = -1;
                }
            }
        }
        let _ = sink.flush();
        tick(runners);
    }
}

/// A bulk member's runner: reads nothing, writes to its channel, and ends
/// the way the member did; never returns.
fn run_member(job: &Job, write_fd: i32) -> ! {
    stdin_from_devnull();
    redirect_output(write_fd);
    close(write_fd);
    exit_with(outcome_of(run_step(job)))
}

fn signal_members(runners: &[Runner], job: &Job, signal: Signal) {
    for runner in runners.iter().filter(|runner| runner.code.is_none()) {
        for member in jobs::jobs_for(&job.common_dir, &runner.name) {
            // this runner's instance, not another
            if member.record.owner_pid == runner.pid {
                jobs::signal_group(member.record.pgid, signal);
            }
        }
    }
}

/// All members at once, each under a runner of its own that shares our
/// process group (so a signal to the group reaches every runner, which
/// forwards it to its member) but not the terminal; their output is
/// relayed here, line by line, prefixed. Done when all have ended.
fn run_bulk(job: &Job) -> Result<i32> {
    let members = job.spec.bulk.clone().unwrap_or_default();
    // SAFETY: isatty only inspects the descriptor.
    let on_tty = unsafe { libc::isatty(2) } == 1;
    let mut runners = Vec::new();
    for member in &members {
        let step = prepare(job.config, member, &job.cwd, &job.env, &[], false)?;
        let (read_fd, write_fd) = open_channel(on_tty)?;
        // SAFETY: single-threaded here, see `spawn`.
        match unsafe { fork() } {
            Ok(ForkResult::Child) => {
                close(read_fd);
                run_member(&step, write_fd)
            }
            Ok(ForkResult::Parent { child }) => {
                close(write_fd);
                runners.push(Runner {
                    name: member.clone(),
                    pid: child.as_raw(),
                    fd: read_fd,
                    code: None,
                });
            }
            Err(errno) => return Err(Error::new(format!("cannot fork: {}", errno.desc()))),
        }
    }
    // Signals for us reach the runners too (same group); we only outlive
    // them to relay the rest of their output. Ctrl-Z, which the terminal
    // sends the group, is passed on to the members' groups and back.
    for signal in FORWARDED_SIGNALS {
        handle(signal, ignore_signal);
    }
    handle(Signal::SIGTSTP, note_signal);
    handle(Signal::SIGCONT, note_signal);
    let mut prefixer = Prefixer::new(&members, output::colors_enabled());
    let mut tick = |runners: &[Runner]| {
        if SUSPEND.swap(false, Ordering::SeqCst) {
            signal_members(runners, job, Signal::SIGTSTP);
            let _ = nix::sys::signal::kill(getpid(), Signal::SIGSTOP);
        }
        if RESUME.swap(false, Ordering::SeqCst) {
            signal_members(runners, job, Signal::SIGCONT);
        }
    };
    pump(&mut runners, &mut prefixer, &mut stderr_sink(), &mut tick);
    Ok(bulk_outcome(&job.name, &runners))
}

/// A child process handed to a test as a [`Runner`], its output on a
/// channel of the given kind.
#[cfg(test)]
#[allow(clippy::zombie_processes)] // the pump reaps it, by pid
pub(crate) fn test_runner(name: &str, command: &str, tty: bool) -> Runner {
    use std::os::fd::FromRawFd;
    let (read_fd, write_fd) = open_channel(tty).unwrap();
    // SAFETY: the write end is duplicated for the child and closed below.
    let (out, err) = unsafe {
        (OwnedFd::from_raw_fd(libc::dup(write_fd)), OwnedFd::from_raw_fd(libc::dup(write_fd)))
    };
    let child = Command::new("sh").args(["-c", command]).stdout(out).stderr(err).spawn().unwrap();
    close(write_fd);
    Runner { name: name.to_string(), pid: child.id() as i32, fd: read_fd, code: None }
}
