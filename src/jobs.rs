//! Running-script records, and stopping a running script.
//!
//! One JSON record per running `wf run`, under the repository's common git
//! dir so every worktree of a project sees the same set:
//! `<common>/workforest/running/<script>/<worktree-name>.<pid>`, the pid
//! being the `wf run` that owns the instance — one worktree may hold
//! several of them at once. A record is a hint, never the truth: it
//! outlives a `kill -9` or a reboot, so whoever reads one verifies that
//! its processes are alive, from this boot, and still the group we
//! started, and drops it otherwise.
//!
//! Cleanup belongs to the owning `wf run`: it runs the script's `cleanup`
//! once its command ends — however it ended — and only then removes its
//! record. Whoever stops it (`wf stop`, or an `exclusive` script starting
//! elsewhere) therefore signals the victim's process group and waits for
//! the record to disappear, which is the moment the victim's cleanup has
//! finished. Only for an orphan (owner dead, group alive) does the stopper
//! run the cleanup itself.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use indexmap::IndexMap;
use nix::errno::Errno;
use nix::sys::signal::{Signal, kill, killpg};
use nix::unistd::{Pid, getpgid};
use serde::{Deserialize, Serialize};

use crate::output;
use crate::util::{self, repr};

pub const RUNNING_SUBDIR: &str = "workforest/running";
pub const LOGS_SUBDIR: &str = "workforest/logs";
const POLL_INTERVAL: Duration = Duration::from_millis(50);

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JobRecord {
    pub script: String,
    /// Absolute path.
    pub worktree: String,
    pub branch: String,
    /// The command's process group (its own leader).
    pub pgid: i32,
    /// The `wf run` waiting on it.
    pub owner_pid: i32,
    pub boot_id: String,
    /// Epoch seconds.
    pub started_at: f64,
    /// Who signalled it, e.g. "`wf stop` in 'feat'".
    #[serde(default)]
    pub stopped_by: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Job {
    pub path: PathBuf,
    pub record: JobRecord,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobState {
    /// Owner and group alive: a running `wf run`.
    Live,
    /// Group alive, owner gone (killed -9): nobody will clean up.
    Orphan,
    /// Nothing left of it, or from another boot.
    Stale,
}

/// Seconds since the epoch, now.
pub fn now() -> f64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0.0, |elapsed| elapsed.as_secs_f64())
}

/// An identifier that changes on reboot, or "" where none is readable.
pub fn boot_id() -> String {
    if let Ok(text) = fs::read_to_string("/proc/sys/kernel/random/boot_id") {
        return text.trim().to_string();
    }
    if cfg!(target_os = "macos") {
        return Command::new("sysctl")
            .args(["-n", "kern.boottime"])
            .output()
            .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
            .unwrap_or_default();
    }
    String::new()
}

pub fn records_dir(common_dir: &Path, script: &str) -> PathBuf {
    common_dir.join(RUNNING_SUBDIR).join(script)
}

/// What one instance is called on disk: the worktree it runs in and the
/// pid of the `wf run` that owns it, since several instances of a script
/// may share a worktree.
fn instance_name(worktree: &Path, pid: i32) -> String {
    format!("{}.{pid}", util::file_name(worktree))
}

pub fn record_path(common_dir: &Path, script: &str, worktree: &Path, pid: i32) -> PathBuf {
    records_dir(common_dir, script).join(instance_name(worktree, pid))
}

/// Where a `background` instance's output goes; kept after it ends, so a
/// crash can be read up on, until `prune` clears it away.
pub fn log_path(common_dir: &Path, script: &str, worktree: &Path, pid: i32) -> PathBuf {
    common_dir.join(LOGS_SUBDIR).join(script).join(format!("{}.log", instance_name(worktree, pid)))
}

/// Atomic: a reader never sees a partial file.
pub fn write_record(path: &Path, record: &JobRecord) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let temporary = path.with_file_name(format!(".{}.tmp", util::file_name(path)));
    fs::write(&temporary, serde_json::to_string(record).expect("a record is plain data"))?;
    fs::rename(&temporary, path)
}

/// None when the file is gone or unreadable (a corrupt one is removed).
pub fn read_record(path: &Path) -> Option<JobRecord> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return None,
        Err(_) => {
            let _ = fs::remove_file(path);
            return None;
        }
    };
    let record = serde_json::from_str(&text).ok();
    if record.is_none() {
        let _ = fs::remove_file(path);
    }
    record
}

fn sorted_entries(directory: &Path) -> Vec<PathBuf> {
    let mut paths: Vec<PathBuf> = fs::read_dir(directory)
        .map(|entries| entries.flatten().map(|entry| entry.path()).collect())
        .unwrap_or_default();
    paths.sort();
    paths
}

pub fn jobs_for(common_dir: &Path, script: &str) -> Vec<Job> {
    sorted_entries(&records_dir(common_dir, script))
        .into_iter()
        .filter(|path| !util::file_name(path).starts_with('.'))
        .filter_map(|path| read_record(&path).map(|record| Job { path, record }))
        .collect()
}

/// Forget what the script's dead instances left behind in this worktree:
/// their stale records, and the logs of every instance no longer recorded.
/// The live ones keep both.
pub fn prune(common_dir: &Path, script: &str, worktree: &Path) {
    let mut live = Vec::new();
    for job in jobs_for(common_dir, script) {
        if Path::new(&job.record.worktree) != worktree {
            continue;
        }
        if classify(&job.record) == JobState::Stale {
            let _ = fs::remove_file(&job.path);
        } else {
            live.push(util::file_name(&job.path));
        }
    }
    let prefix = format!("{}.", util::file_name(worktree));
    for path in sorted_entries(&common_dir.join(LOGS_SUBDIR).join(script)) {
        // "<worktree>.<pid>.log" for one of this worktree's instances
        let name = util::file_name(&path);
        let Some(stem) = name.strip_suffix(".log") else {
            continue;
        };
        let is_instance = stem
            .strip_prefix(&prefix)
            .is_some_and(|pid| !pid.is_empty() && pid.chars().all(|c| c.is_ascii_digit()));
        if is_instance && !live.iter().any(|kept| kept == stem) {
            let _ = fs::remove_file(&path);
        }
    }
}

/// Whether a signal could be sent: a process we may not signal is alive.
fn alive_from(result: nix::Result<()>) -> bool {
    !matches!(result, Err(Errno::ESRCH))
}

fn alive(pid: i32) -> bool {
    alive_from(kill(Pid::from_raw(pid), None))
}

/// Our command is the leader of its own group; a recycled pid is unlikely
/// to be.
fn leads_group(pgid: i32) -> bool {
    pgid > 0 && getpgid(Some(Pid::from_raw(pgid))).is_ok_and(|group| group.as_raw() == pgid)
}

pub fn classify(record: &JobRecord) -> JobState {
    if record.boot_id != boot_id() || !leads_group(record.pgid) {
        return JobState::Stale;
    }
    if alive(record.owner_pid) { JobState::Live } else { JobState::Orphan }
}

/// Which scripts are running where: worktree path → script name → how
/// many instances of it run there. A record whose processes are gone
/// (stale) does not count; it is left on disk for its owner or `wf stop`
/// to clean up.
pub fn running_scripts(common_dir: &Path) -> IndexMap<PathBuf, BTreeMap<String, usize>> {
    let mut found: IndexMap<PathBuf, BTreeMap<String, usize>> = IndexMap::new();
    for script_dir in sorted_entries(&common_dir.join(RUNNING_SUBDIR)) {
        if !script_dir.is_dir() {
            continue;
        }
        let script = util::file_name(&script_dir);
        for job in jobs_for(common_dir, &script) {
            if classify(&job.record) != JobState::Stale {
                let counts = found.entry(PathBuf::from(&job.record.worktree)).or_default();
                *counts.entry(script.clone()).or_default() += 1;
            }
        }
    }
    found
}

/// When a process started, in epoch seconds, from `/proc`.
fn started_at(stat: &str, proc_stat: &str, ticks_per_second: f64) -> Option<f64> {
    // Field 22 (1-based) is the start time in clock ticks since boot; the
    // command name in field 2 may contain spaces, so split after its ')'.
    let after_name = &stat[stat.rfind(')')? + 1..];
    let ticks: f64 = after_name.split_whitespace().nth(19)?.parse().ok()?;
    let boot: f64 = proc_stat
        .lines()
        .find(|line| line.starts_with("btime"))?
        .split_whitespace()
        .nth(1)?
        .parse()
        .ok()?;
    Some(boot + ticks / ticks_per_second)
}

/// Whether `pid` was started before epoch `when` — the definitive
/// recycled-pid check. None where /proc is unavailable.
pub fn process_started_before(pid: i32, when: f64) -> Option<bool> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let proc_stat = fs::read_to_string("/proc/stat").ok()?;
    let ticks = nix::unistd::sysconf(nix::unistd::SysconfVar::CLK_TCK).ok().flatten()? as f64;
    Some(started_at(&stat, &proc_stat, ticks)? < when)
}

pub fn signal_group(pgid: i32, signal: Signal) {
    if pgid > 0 {
        let _ = killpg(Pid::from_raw(pgid), signal);
    }
}

fn wait_until(condition: &dyn Fn() -> bool, timeout: f64) -> bool {
    let deadline = Instant::now() + Duration::from_secs_f64(timeout.max(0.0));
    while Instant::now() < deadline {
        if condition() {
            return true;
        }
        thread::sleep(POLL_INTERVAL);
    }
    condition()
}

/// SIGTERM the group and wait for `done`; escalate to SIGKILL.
fn terminate_group(pgid: i32, done: &dyn Fn() -> bool, timeout: f64) {
    signal_group(pgid, Signal::SIGTERM);
    if wait_until(done, timeout) {
        return;
    }
    output::warn(&format!(
        "process group {pgid} ignored SIGTERM for {}s, killing it",
        util::format_seconds(timeout)
    ));
    signal_group(pgid, Signal::SIGKILL);
    wait_until(done, timeout);
}

/// Stop a running instance: SIGTERM its group, SIGKILL after `timeout`
/// seconds. `by` is recorded for the victim's own report.
///
/// A live job's owner runs its own cleanup and removes the record; we wait
/// for that. An orphan is killed here and its cleanup run via
/// `orphan_cleanup` — but only when /proc confirms the pid is not a
/// recycled one; elsewhere it is reported and left alone.
pub fn stop(job: &Job, by: &str, timeout: f64, orphan_cleanup: Option<&dyn Fn(&JobRecord)>) {
    stop_with(job, by, timeout, orphan_cleanup, &process_started_before);
}

fn stop_with(
    job: &Job,
    by: &str,
    timeout: f64,
    orphan_cleanup: Option<&dyn Fn(&JobRecord)>,
    started_before: &dyn Fn(i32, f64) -> Option<bool>,
) {
    let record = &job.record;
    let label = format!(
        "{} in {}",
        repr(&record.script),
        repr(&util::file_name(Path::new(&record.worktree)))
    );
    let drop_record = || {
        let _ = fs::remove_file(&job.path);
    };
    match classify(record) {
        JobState::Stale => drop_record(),
        JobState::Live => {
            output::info(&format!("stopping {label} (pgid {})", record.pgid));
            let marked = JobRecord { stopped_by: Some(by.to_string()), ..record.clone() };
            let _ = write_record(&job.path, &marked);
            terminate_group(record.pgid, &|| !job.path.exists(), timeout);
            if job.path.exists() {
                output::warn(&format!("{label}: its `wf run` did not finish; dropping the record"));
                drop_record();
            }
        }
        JobState::Orphan => match started_before(record.pgid, record.started_at) {
            None => {
                output::warn(&format!(
                    "{label} (pgid {}) may still be running without its `wf run`; \
                     cannot verify, so leaving it alone",
                    record.pgid
                ));
                drop_record();
            }
            Some(false) => drop_record(), // pid recycled: nothing of ours left
            Some(true) => {
                output::info(&format!("stopping orphaned {label} (pgid {})", record.pgid));
                terminate_group(record.pgid, &|| !leads_group(record.pgid), timeout);
                if let Some(cleanup) = orphan_cleanup {
                    cleanup(record);
                }
                drop_record();
            }
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::output::capture;
    use crate::testing::Sandbox;
    use std::cell::RefCell;
    use std::os::unix::process::{CommandExt, ExitStatusExt};
    use std::process::Child;
    use std::sync::{Arc, Mutex};

    fn wait_for(condition: impl Fn() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if condition() {
                return;
            }
            thread::sleep(Duration::from_millis(20));
        }
        panic!("condition not met in time");
    }

    fn reaped_pid() -> i32 {
        let mut child = Command::new("true").spawn().unwrap();
        child.wait().unwrap();
        child.id() as i32
    }

    /// A command in a process group of its own, like `wf run` starts it;
    /// killed when the test is done with it.
    struct Sleeper(Option<Child>);

    impl Sleeper {
        fn spawn(command: &str) -> Self {
            Self(Some(
                Command::new("/bin/sh").args(["-c", command]).process_group(0).spawn().unwrap(),
            ))
        }

        fn pid(&self) -> i32 {
            self.0.as_ref().unwrap().id() as i32
        }

        /// An orphan's parent is gone, so init reaps it the moment it
        /// dies; the test process is the parent here, so stand in for
        /// init. The result is the signal that ended it.
        fn reap_like_init(&mut self) -> thread::JoinHandle<Option<i32>> {
            let mut child = self.0.take().unwrap();
            thread::spawn(move || child.wait().unwrap().signal())
        }
    }

    impl Drop for Sleeper {
        fn drop(&mut self) {
            if let Some(child) = &mut self.0 {
                signal_group(child.id() as i32, Signal::SIGKILL);
                let _ = child.wait();
            }
        }
    }

    fn record(pgid: i32) -> JobRecord {
        JobRecord {
            script: "dev".into(),
            worktree: "/tmp/wt/feat".into(),
            branch: "feat".into(),
            pgid,
            owner_pid: std::process::id() as i32,
            boot_id: boot_id(),
            started_at: now(),
            stopped_by: None,
        }
    }

    fn file_names(directory: &Path) -> Vec<String> {
        sorted_entries(directory).iter().map(|path| util::file_name(path)).collect()
    }

    #[test]
    fn round_trip_and_layout() {
        let sandbox = Sandbox::new();
        let common = sandbox.path();
        let written = JobRecord { stopped_by: Some("other".into()), ..record(1234) };
        let worktree = Path::new("/x/worktrees/api/feat");
        let path = record_path(common, "dev", worktree, 4321);
        assert_eq!(path, common.join("workforest/running/dev/feat.4321"));
        write_record(&path, &written).unwrap();
        assert_eq!(read_record(&path), Some(written));
        assert_eq!(file_names(path.parent().unwrap()), ["feat.4321"]); // no temp file left behind
        assert_eq!(
            log_path(common, "dev", worktree, 4321),
            common.join("workforest/logs/dev/feat.4321.log")
        );
    }

    #[test]
    fn missing_and_corrupt() {
        let sandbox = Sandbox::new();
        let path = sandbox.path().join("dev").join("feat");
        assert_eq!(read_record(&path), None);
        fs::create_dir(path.parent().unwrap()).unwrap();
        for text in ["{not json", "{\"script\": \"dev\"}", "[1]"] {
            fs::write(&path, text).unwrap();
            assert_eq!(read_record(&path), None, "{text}");
            assert!(!path.exists());
        }
        let mut with_extra = serde_json::to_value(record(1)).unwrap();
        with_extra["from_the_future"] = true.into();
        fs::write(&path, with_extra.to_string()).unwrap();
        assert_eq!(read_record(&path), None);
        // a directory where a record should be is unreadable, not missing
        let directory = sandbox.path().join("dev").join("dir");
        fs::create_dir(&directory).unwrap();
        assert_eq!(read_record(&directory), None);
    }

    #[test]
    fn a_record_without_stopped_by_reads_as_none() {
        let sandbox = Sandbox::new();
        let path = sandbox.path().join("feat");
        let mut json = serde_json::to_value(record(7)).unwrap();
        json.as_object_mut().unwrap().remove("stopped_by");
        fs::write(&path, json.to_string()).unwrap();
        assert_eq!(read_record(&path).unwrap().stopped_by, None);
    }

    #[test]
    fn jobs_for_lists_readable_records() {
        let sandbox = Sandbox::new();
        let common = sandbox.path();
        assert!(jobs_for(common, "dev").is_empty());
        let (a, b) = (record(1), record(2));
        write_record(&record_path(common, "dev", Path::new("/w/feat-a"), 1), &a).unwrap();
        write_record(&record_path(common, "dev", Path::new("/w/feat-b"), 2), &b).unwrap();
        fs::write(records_dir(common, "dev").join(".feat-c.1.tmp"), "partial").unwrap();
        fs::write(records_dir(common, "dev").join("feat-d.4"), "garbage").unwrap();
        let found = jobs_for(common, "dev");
        assert_eq!(found.iter().map(|job| job.record.clone()).collect::<Vec<_>>(), [a, b]);
        assert_eq!(
            found.iter().map(|job| util::file_name(&job.path)).collect::<Vec<_>>(),
            ["feat-a.1", "feat-b.2"]
        );
        assert!(jobs_for(common, "other").is_empty());
    }

    #[test]
    fn prune_keeps_live_instances_and_other_worktrees() {
        let sandbox = Sandbox::new();
        let common = sandbox.path();
        let sleeper = Sleeper::spawn("sleep 30");
        let at =
            |pgid: i32, worktree: &str| JobRecord { worktree: worktree.into(), ..record(pgid) };
        let records = [
            (at(sleeper.pid(), "/w/feat"), "feat.1"),
            (at(reaped_pid(), "/w/feat"), "feat.2"),
            (at(reaped_pid(), "/w/other"), "other.3"),
        ];
        for (record, name) in &records {
            write_record(&records_dir(common, "dev").join(name), record).unwrap();
        }
        let log_dir = common.join(LOGS_SUBDIR).join("dev");
        fs::create_dir_all(&log_dir).unwrap();
        for name in ["feat.1.log", "feat.2.log", "feat.notapid.log", "other.3.log", "feat.9.txt"] {
            fs::write(log_dir.join(name), "output").unwrap();
        }

        prune(common, "dev", Path::new("/w/feat"));

        assert_eq!(file_names(&records_dir(common, "dev")), ["feat.1", "other.3"]);
        // The dead instance's log goes with its record; the live one's, a
        // file that is not an instance log, and another worktree's stay.
        assert_eq!(
            file_names(&log_dir),
            ["feat.1.log", "feat.9.txt", "feat.notapid.log", "other.3.log"]
        );
    }

    #[test]
    fn prune_without_records_or_logs() {
        let sandbox = Sandbox::new();
        prune(sandbox.path(), "dev", Path::new("/w/feat")); // nothing on disk yet
        assert!(!sandbox.path().join("workforest").exists());
    }

    #[test]
    fn boot_id_is_stable() {
        assert_eq!(boot_id(), boot_id());
    }

    #[test]
    fn running_scripts_groups_live_records_by_worktree() {
        let sandbox = Sandbox::new();
        let common = sandbox.path();
        assert!(running_scripts(common).is_empty());
        let sleeper = Sleeper::spawn("sleep 30");
        let instances = [
            ("dev", "/w/feat", 1),
            ("dev", "/w/feat", 2), // a second instance counts
            ("build", "/w/feat", 3),
            ("dev", "/w/other", 4),
        ];
        for (script, worktree, owner) in instances {
            let entry = JobRecord {
                script: script.into(),
                worktree: worktree.into(),
                ..record(sleeper.pid())
            };
            write_record(&record_path(common, script, Path::new(worktree), owner), &entry).unwrap();
        }
        // A dead group: a record nobody cleaned up, which counts for nothing.
        let dead = reaped_pid();
        let entry =
            JobRecord { script: "stale".into(), worktree: "/w/feat".into(), ..record(dead) };
        write_record(&record_path(common, "stale", Path::new("/w/feat"), dead), &entry).unwrap();
        fs::write(common.join(RUNNING_SUBDIR).join("not-a-dir"), "").unwrap();

        let running = running_scripts(common);
        assert_eq!(running.len(), 2);
        assert_eq!(
            running[Path::new("/w/feat")],
            BTreeMap::from([("build".to_string(), 1), ("dev".to_string(), 2)])
        );
        assert_eq!(running[Path::new("/w/other")], BTreeMap::from([("dev".to_string(), 1)]));
    }

    #[test]
    fn classify_live_orphan_and_stale() {
        let sleeper = Sleeper::spawn("sleep 30");
        assert_eq!(classify(&record(sleeper.pid())), JobState::Live);
        let orphan = JobRecord { owner_pid: reaped_pid(), ..record(sleeper.pid()) };
        assert_eq!(classify(&orphan), JobState::Orphan);
        assert_eq!(classify(&record(reaped_pid())), JobState::Stale);
        let other_boot = JobRecord { boot_id: "previous-boot".into(), ..record(sleeper.pid()) };
        assert_eq!(classify(&other_boot), JobState::Stale);
        assert_eq!(classify(&record(0)), JobState::Stale);
    }

    #[test]
    fn stale_when_pid_is_not_a_group_leader() {
        // A child in our group is never a leader.
        let mut child = Command::new("sleep").arg("30").spawn().unwrap();
        let state = classify(&record(child.id() as i32));
        child.kill().unwrap();
        child.wait().unwrap();
        assert_eq!(state, JobState::Stale);
    }

    #[test]
    fn alive_treats_a_permission_error_as_alive() {
        assert!(alive_from(Err(Errno::EPERM)));
        assert!(alive_from(Ok(())));
        assert!(!alive_from(Err(Errno::ESRCH)));
        assert!(alive(std::process::id() as i32));
    }

    #[test]
    fn start_time_comes_from_proc() {
        let stat = "42 (a b) c) S 1 42 42 0 -1 4194304 100 0 0 0 1 2 0 0 20 0 1 0 500 1000 10 18446744073709551615";
        assert_eq!(started_at(stat, "cpu 1 2\nbtime 1000\nprocesses 5\n", 100.0), Some(1005.0));
        assert_eq!(started_at("garbage", "btime 1000\n", 100.0), None);
        assert_eq!(started_at(stat, "no boot time\n", 100.0), None);
    }

    #[test]
    fn process_started_before_against_own_process() {
        if !Path::new("/proc/stat").is_file() {
            return; // needs /proc
        }
        let own = std::process::id() as i32;
        assert_eq!(process_started_before(own, now() + 1.0), Some(true));
        assert_eq!(process_started_before(own, 0.0), Some(false));
        assert_eq!(process_started_before(reaped_pid(), now()), None);
    }

    #[test]
    fn stop_drops_a_stale_record_silently() {
        let sandbox = Sandbox::new();
        let path = sandbox.path().join("feat");
        let stale = record(reaped_pid());
        write_record(&path, &stale).unwrap();
        let ((), shown) =
            capture(|| stop(&Job { path: path.clone(), record: stale }, "main", 5.0, None));
        assert!(!path.exists());
        assert_eq!(shown, "");
    }

    #[test]
    fn stop_signals_a_live_job_and_waits_for_its_owner() {
        let sandbox = Sandbox::new();
        let path = sandbox.path().join("feat");
        let mut sleeper = Sleeper::spawn("sleep 30");
        let live = record(sleeper.pid());
        write_record(&path, &live).unwrap();
        let seen = Arc::new(Mutex::new((None, None)));

        // What the victim's `wf run` does: notice its command died, read
        // who did it, run cleanup, then remove the record.
        let mut child = sleeper.0.take().unwrap();
        let (owner_path, owner_seen) = (path.clone(), Arc::clone(&seen));
        let owner = thread::spawn(move || {
            let status = child.wait().unwrap();
            let stopped_by = read_record(&owner_path).and_then(|record| record.stopped_by);
            thread::sleep(Duration::from_millis(200)); // cleanup takes a moment
            *owner_seen.lock().unwrap() = (stopped_by, Some(Instant::now()));
            fs::remove_file(&owner_path).unwrap();
            status.signal()
        });
        let ((), shown) =
            capture(|| stop(&Job { path: path.clone(), record: live }, "main", 5.0, None));
        let returned = Instant::now();

        assert_eq!(owner.join().unwrap(), Some(Signal::SIGTERM as i32));
        let (stopped_by, cleanup_done) = seen.lock().unwrap().clone();
        assert_eq!(stopped_by.as_deref(), Some("main"));
        assert!(returned >= cleanup_done.unwrap());
        assert!(!path.exists());
        assert!(shown.contains("stopping 'dev' in 'feat'"), "{shown}");
    }

    #[test]
    fn stop_escalates_to_sigkill() {
        let sandbox = Sandbox::new();
        let ready = sandbox.path().join("ready");
        let mut sleeper =
            Sleeper::spawn(&format!("trap \"\" TERM; touch {}; sleep 30", ready.display()));
        wait_for(|| ready.exists());
        let path = sandbox.path().join("feat");
        let live = record(sleeper.pid());
        write_record(&path, &live).unwrap();
        let mut child = sleeper.0.take().unwrap();
        let owner_path = path.clone();
        let owner = thread::spawn(move || {
            let status = child.wait().unwrap();
            fs::remove_file(&owner_path).unwrap();
            status.signal()
        });

        let ((), shown) = capture(|| stop(&Job { path, record: live }, "main", 0.3, None));

        assert_eq!(owner.join().unwrap(), Some(Signal::SIGKILL as i32));
        assert!(shown.contains("ignored SIGTERM for 0.3s, killing it"), "{shown}");
    }

    #[test]
    fn an_owner_that_never_finishes_has_its_record_dropped() {
        let sandbox = Sandbox::new();
        let path = sandbox.path().join("feat");
        let mut sleeper = Sleeper::spawn("sleep 30");
        let live = record(sleeper.pid());
        write_record(&path, &live).unwrap();

        let ((), shown) =
            capture(|| stop(&Job { path: path.clone(), record: live }, "main", 0.2, None));

        assert_eq!(sleeper.reap_like_init().join().unwrap(), Some(Signal::SIGTERM as i32));
        assert!(!path.exists());
        assert!(shown.contains("did not finish; dropping the record"), "{shown}");
    }

    fn orphan(sandbox: &Sandbox, sleeper: &Sleeper) -> Job {
        let path = sandbox.path().join("feat");
        let record = JobRecord { owner_pid: reaped_pid(), ..record(sleeper.pid()) };
        write_record(&path, &record).unwrap();
        Job { path, record }
    }

    #[test]
    fn an_orphan_is_killed_and_cleaned_up_here() {
        let sandbox = Sandbox::new();
        let mut sleeper = Sleeper::spawn("sleep 30");
        let job = orphan(&sandbox, &sleeper);
        let cleaned = RefCell::new(Vec::new());
        let reaper = sleeper.reap_like_init();

        let ((), shown) = capture(|| {
            let cleanup = |record: &JobRecord| cleaned.borrow_mut().push(record.clone());
            stop_with(&job, "main", 5.0, Some(&cleanup), &|_, _| Some(true));
        });

        assert_eq!(reaper.join().unwrap(), Some(Signal::SIGTERM as i32));
        assert_eq!(*cleaned.borrow(), std::slice::from_ref(&job.record));
        assert!(!job.path.exists());
        assert!(shown.contains("stopping orphaned 'dev' in 'feat'"), "{shown}");
    }

    #[test]
    fn the_orphan_check_is_real_on_linux() {
        if !Path::new("/proc/stat").is_file() {
            return; // needs /proc
        }
        let sandbox = Sandbox::new();
        let mut sleeper = Sleeper::spawn("sleep 30");
        thread::sleep(Duration::from_millis(50));
        let mut job = orphan(&sandbox, &sleeper);
        job.record.started_at = now();
        let reaper = sleeper.reap_like_init();
        capture(|| stop(&job, "main", 5.0, None));
        assert_eq!(reaper.join().unwrap(), Some(Signal::SIGTERM as i32));
        assert!(!job.path.exists());
    }

    #[test]
    fn an_orphan_with_a_recycled_pid_is_left_alone() {
        let sandbox = Sandbox::new();
        let sleeper = Sleeper::spawn("sleep 30");
        let job = orphan(&sandbox, &sleeper);
        capture(|| stop_with(&job, "main", 5.0, None, &|_, _| Some(false)));
        assert!(leads_group(sleeper.pid())); // not ours: untouched
        assert!(!job.path.exists());
    }

    #[test]
    fn an_unverifiable_orphan_is_reported_not_killed() {
        let sandbox = Sandbox::new();
        let sleeper = Sleeper::spawn("sleep 30");
        let job = orphan(&sandbox, &sleeper);
        let ((), shown) = capture(|| stop_with(&job, "main", 5.0, None, &|_, _| None));
        assert!(leads_group(sleeper.pid()));
        assert!(!job.path.exists());
        assert!(shown.contains("cannot verify, so leaving it alone"), "{shown}");
    }

    #[test]
    fn signal_group_ignores_a_vanished_group() {
        signal_group(reaped_pid(), Signal::SIGTERM); // no panic
        signal_group(0, Signal::SIGTERM); // and never our own group
    }
}
