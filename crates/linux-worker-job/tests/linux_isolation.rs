#![cfg(all(target_os = "linux", target_arch = "x86_64"))]

use publisher_linux_worker_job::{
    JobLimits, PinnedExecutable, SandboxProfile, SandboxedProcess, WorkerCancellationScope,
};
use std::{
    ffi::OsString,
    fs::{self, File},
    io::{BufRead, Read},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::{fs::symlink, process::ExitStatusExt},
    },
    path::PathBuf,
    process::{Command, ExitStatus, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

struct Fixture {
    root: PathBuf,
    cwd: PathBuf,
    executable: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "pdf-linux-isolation-{}-{nonce}",
            std::process::id()
        ));
        let cwd = root.join("private work with spaces");
        fs::create_dir_all(&cwd).unwrap();
        let executable = root.join("probe");
        fs::copy(
            env!("CARGO_BIN_EXE_publisher-linux-isolation-probe"),
            &executable,
        )
        .unwrap();
        Self {
            root,
            cwd,
            executable,
        }
    }
    fn profile(&self) -> SandboxProfile {
        let profile = SandboxProfile::new().expect("required native isolation backend");
        profile.grant_private_working_directory(&self.cwd).unwrap();
        profile
    }
    fn spawn(&self, mode: &str, limits: JobLimits) -> SandboxedProcess {
        let mut child = self
            .profile()
            .spawn(&self.executable, &[mode.into()], &self.cwd, limits)
            .expect("launch restricted ELF");
        drop(child.take_stdin());
        child
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.root).unwrap();
    }
}
fn limits() -> JobLimits {
    JobLimits {
        process_memory_bytes: 64 * 1024 * 1024,
        job_memory_bytes: 64 * 1024 * 1024,
        cpu_time: Duration::from_secs(2),
        wall_time: Duration::from_secs(10),
    }
}
fn wait(child: &mut SandboxedProcess) -> ExitStatus {
    let deadline = Instant::now() + Duration::from_secs(12);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            return status;
        }
        assert!(Instant::now() < deadline, "worker did not terminate");
        thread::sleep(Duration::from_millis(5));
    }
}
fn finish(mut child: SandboxedProcess) -> (ExitStatus, String, String) {
    let mut stdout = child.take_stdout().unwrap();
    let mut stderr = child.take_stderr().unwrap();
    let status = wait(&mut child);
    let mut out = String::new();
    let mut err = String::new();
    stdout.read_to_string(&mut out).unwrap();
    stderr.read_to_string(&mut err).unwrap();
    (status, out, err)
}
fn readiness(child: &mut SandboxedProcess) -> std::io::BufReader<File> {
    let mut output = std::io::BufReader::new(child.take_stdout().unwrap());
    let mut ready = String::new();
    output.read_line(&mut ready).unwrap();
    assert_eq!(ready.trim(), "ready");
    output
}

#[test]
fn actual_kernel_denies_files_symlinks_other_runs_network_processes_exec_and_inherited_fds() {
    let fixture = Fixture::new();
    let allowed = fixture.root.join("allowed.txt");
    let denied = fixture.root.join("outside.txt");
    let other = fixture.root.join("other-run.txt");
    let escape = fixture.cwd.join("escaping-link");
    let report = fixture.root.join("authorized-output.txt");
    fs::write(&allowed, b"allowed").unwrap();
    fs::write(&denied, b"private").unwrap();
    fs::write(&other, b"another job").unwrap();
    fs::write(&report, b"").unwrap();
    symlink(&denied, &escape).unwrap();
    let profile = fixture.profile();
    profile.grant_read(&allowed).unwrap();
    profile.grant_read_write(&report).unwrap();
    assert!(profile.landlock_abi() >= 3);
    eprintln!(
        "abi={};policy={}",
        profile.landlock_abi(),
        profile.policy_id()
    );
    let mut descriptors = [0_i32; 2];
    assert_eq!(unsafe { libc::pipe(descriptors.as_mut_ptr()) }, 0);
    let read = unsafe { File::from_raw_fd(descriptors[0]) };
    let write = unsafe { File::from_raw_fd(descriptors[1]) };
    let inherited = unsafe { libc::fcntl(write.as_raw_fd(), libc::F_DUPFD, 200) };
    assert!(inherited >= 200);
    let inherited = unsafe { File::from_raw_fd(inherited) };
    let args = [
        OsString::from("isolation"),
        allowed.into_os_string(),
        denied.clone().into_os_string(),
        escape.into_os_string(),
        other.clone().into_os_string(),
        report.clone().into_os_string(),
        inherited.as_raw_fd().to_string().into(),
    ];
    let child = profile
        .spawn(&fixture.executable, &args, &fixture.cwd, limits())
        .unwrap();
    let (status, output, error) = finish(child);
    assert!(status.success(), "{status}; {output}; {error}");
    assert!(output.contains("files-denied;socket-denied;process-denied;thread-denied;fds-closed;exec-denied;environment-sanitized"), "{output}");
    assert_eq!(fs::read(denied).unwrap(), b"private");
    assert_eq!(fs::read(other).unwrap(), b"another job");
    assert_eq!(fs::read(report).unwrap(), b"authorized");
    assert_eq!(
        fs::read(fixture.cwd.join("private-new-file")).unwrap(),
        b"private"
    );
    drop(inherited);
    drop(write);
    drop(read);
}

#[test]
fn timestamp_changes_are_denied_for_read_only_and_ungranted_objects() {
    use std::{fs::FileTimes, os::unix::fs::MetadataExt};
    let fixture = Fixture::new();
    let readonly = fixture.root.join("readonly-timestamps");
    let outside = fixture.root.join("outside-timestamps");
    let initial = UNIX_EPOCH + Duration::from_secs(1);
    for path in [&readonly, &outside] {
        let file = File::create(path).unwrap();
        file.set_times(FileTimes::new().set_accessed(initial).set_modified(initial))
            .unwrap();
    }
    let profile = fixture.profile();
    profile.grant_read(&readonly).unwrap();
    let child = profile
        .spawn(
            &fixture.executable,
            &[
                "timestamps".into(),
                readonly.clone().into_os_string(),
                outside.clone().into_os_string(),
            ],
            &fixture.cwd,
            limits(),
        )
        .unwrap();
    let (status, out, err) = finish(child);
    assert!(status.success(), "{status}; {out}; {err}");
    assert_eq!(out.trim(), "timestamps-denied");
    for path in [&readonly, &outside] {
        let metadata = fs::metadata(path).unwrap();
        assert_eq!((metadata.atime(), metadata.atime_nsec()), (1, 0));
        assert_eq!((metadata.mtime(), metadata.mtime_nsec()), (1, 0));
    }
}

#[test]
fn a_1024_descriptor_soft_limit_still_launches_the_restricted_worker() {
    let fixture = Fixture::new();
    for (soft, expected) in [
        (1024, "ready"),
        (73, "ready"),
        (72, "descriptor-budget-denied"),
    ] {
        let output = Command::new(&fixture.executable)
            .arg("low-fd-host")
            .arg(soft.to_string())
            .arg(&fixture.cwd)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "limit={soft}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), expected);
    }
}

#[test]
fn immutable_executable_and_object_grants_survive_path_replacement() {
    let fixture = Fixture::new();
    let input = fixture.root.join("input");
    let output = fixture.root.join("output");
    fs::write(&input, b"allowed").unwrap();
    fs::write(&output, b"original").unwrap();
    let profile = fixture.profile();
    profile.grant_read(&input).unwrap();
    profile.grant_read_write(&output).unwrap();
    let pinned = PinnedExecutable::from_bytes(&fs::read(&fixture.executable).unwrap()).unwrap();
    fs::rename(&input, fixture.root.join("old-input")).unwrap();
    fs::rename(&output, fixture.root.join("old-output")).unwrap();
    fs::write(&input, b"substituted private data").unwrap();
    fs::write(&output, b"substituted output").unwrap();
    fs::remove_file(&fixture.executable).unwrap();
    fs::write(&fixture.executable, b"replaced executable").unwrap();
    let child = profile
        .spawn_pinned(
            &pinned,
            &[
                "read-denied".into(),
                input.into_os_string(),
                output.clone().into_os_string(),
            ],
            &fixture.cwd,
            limits(),
        )
        .unwrap();
    let (status, result, error) = finish(child);
    assert!(status.success(), "{status}; {result}; {error}");
    assert_eq!(result.trim(), "substitution-denied");
    assert_eq!(fs::read(output).unwrap(), b"substituted output");
    assert_eq!(
        fs::read(fixture.root.join("old-output")).unwrap(),
        b"original"
    );
}

#[test]
fn kernel_memory_cpu_and_independent_wall_limits_are_enforced() {
    let fixture = Fixture::new();
    let (status, output, error) = finish(fixture.spawn("memory", limits()));
    assert!(status.success(), "{status}; {output}; {error}");
    assert_eq!(
        output.lines().collect::<Vec<_>>(),
        ["ready", "memory-denied"]
    );
    let cpu_limit = JobLimits {
        cpu_time: Duration::from_millis(200),
        ..limits()
    };
    let start = Instant::now();
    let (status, output, error) = finish(fixture.spawn("cpu", cpu_limit));
    assert_eq!(
        status.signal(),
        Some(libc::SIGKILL),
        "{status}; {output}; {error}"
    );
    assert_eq!(output.trim(), "ready");
    assert!(start.elapsed() < Duration::from_secs(5));
    let mut child = fixture.spawn(
        "sleep",
        JobLimits {
            wall_time: Duration::from_millis(300),
            ..limits()
        },
    );
    let mut output = readiness(&mut child);
    thread::sleep(Duration::from_millis(600)); // No try_wait calls drive this deadline.
    let status = child
        .try_wait()
        .unwrap()
        .expect("wall watchdog must already have reaped worker");
    assert_eq!(status.signal(), Some(libc::SIGKILL));
    assert!(
        child.deadline_terminated(),
        "wall termination reason must survive reaping"
    );
    let mut remainder = String::new();
    output.read_to_string(&mut remainder).unwrap();
    assert!(remainder.is_empty());
}

#[test]
fn drop_and_run_cancellation_close_pipes_and_preserve_an_independent_run() {
    let fixture = Fixture::new();
    let mut dropped = fixture.spawn("sleep", limits());
    let output = readiness(&mut dropped);
    let (sender, receiver) = mpsc::channel();
    let reader = thread::spawn(move || {
        let mut output = output;
        let mut bytes = Vec::new();
        output.read_to_end(&mut bytes).unwrap();
        sender.send(bytes).unwrap();
    });
    let start = Instant::now();
    drop(dropped);
    assert!(
        receiver
            .recv_timeout(Duration::from_secs(2))
            .unwrap()
            .is_empty()
    );
    assert!(start.elapsed() < Duration::from_secs(2));
    reader.join().unwrap();
    let cancellation = Arc::new(AtomicBool::new(false));
    let mut cancelled = {
        let _scope = WorkerCancellationScope::enter(Arc::clone(&cancellation));
        fixture.spawn("sleep", limits())
    };
    let mut independent = fixture.spawn("sleep", limits());
    let mut cancelled_output = readiness(&mut cancelled);
    let _independent_output = readiness(&mut independent);
    cancellation.store(true, Ordering::Release);
    assert_eq!(wait(&mut cancelled).signal(), Some(libc::SIGKILL));
    assert!(
        !cancelled.deadline_terminated(),
        "cancellation is distinct from a deadline"
    );
    let mut remainder = String::new();
    cancelled_output.read_to_string(&mut remainder).unwrap();
    assert!(remainder.is_empty());
    assert!(
        independent.try_wait().unwrap().is_none(),
        "cancellation crossed run ownership"
    );
    independent.terminate().unwrap();
}

#[test]
fn a_full_stdout_pipe_cannot_hold_a_worker_beyond_its_wall_limit() {
    let fixture = Fixture::new();
    let mut child = fixture.spawn(
        "flood",
        JobLimits {
            wall_time: Duration::from_millis(250),
            ..limits()
        },
    );
    let mut output = child.take_stdout().unwrap();
    // Deliberately leave the pipe undrained while the worker blocks in write.
    assert_eq!(wait(&mut child).signal(), Some(libc::SIGKILL));
    let mut bytes = Vec::new();
    output.read_to_end(&mut bytes).unwrap();
    assert!(!bytes.is_empty());
    assert!(bytes.len() < 1024 * 1024, "pipe unexpectedly unbounded");
}

#[test]
fn killing_the_host_triggers_os_backed_worker_death() {
    let fixture = Fixture::new();
    let mut host = Command::new(&fixture.executable)
        .arg("host")
        .arg(&fixture.cwd)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut output = std::io::BufReader::new(host.stdout.take().unwrap());
    let mut line = String::new();
    output.read_line(&mut line).unwrap();
    assert!(line.starts_with("owner-ready "), "{line}");
    let child_pid = line
        .trim()
        .split(' ')
        .nth(1)
        .unwrap()
        .parse::<i32>()
        .unwrap();
    let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, child_pid, 0) };
    assert!(fd >= 0);
    let pidfd = unsafe { File::from_raw_fd(i32::try_from(fd).unwrap()) };
    host.kill().unwrap();
    host.wait().unwrap();
    let mut poll = libc::pollfd {
        fd: pidfd.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    assert_eq!(
        unsafe { libc::poll(&raw mut poll, 1, 2000) },
        1,
        "worker survived forced owner death"
    );
    let mut remainder = String::new();
    output.read_to_string(&mut remainder).unwrap();
    assert!(remainder.is_empty());
}

#[test]
fn missing_required_kernel_primitives_fail_before_the_parser_marker() {
    let fixture = Fixture::new();
    // Deny the actual syscall in a separate supervisor. The production child
    // inherits this policy, so these are genuine OS failures, not a mock flag.
    for syscall in [
        libc::SYS_landlock_create_ruleset,
        libc::SYS_seccomp,
        libc::SYS_close_range,
        libc::SYS_clone3,
        libc::SYS_memfd_create,
        libc::SYS_landlock_restrict_self,
        libc::SYS_prctl,
        libc::SYS_pidfd_send_signal,
    ] {
        let marker = fixture.root.join(format!("marker-{syscall}"));
        fs::write(&marker, b"").unwrap();
        let output = Command::new(&fixture.executable)
            .arg("setup-failure")
            .arg(syscall.to_string())
            .arg(&marker)
            .arg(&fixture.cwd)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "syscall={syscall}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&output.stdout).trim(),
            "setup-denied"
        );
        assert!(
            fs::read(marker).unwrap().is_empty(),
            "worker processed input after setup failure {syscall}"
        );
    }
}

#[test]
fn incompatible_syscall_architectures_cannot_bypass_the_filter() {
    let fixture = Fixture::new();
    let (status, output, error) = finish(fixture.spawn("architecture", limits()));
    assert_eq!(
        status.signal(),
        Some(libc::SIGSYS),
        "{status}; {output}; {error}"
    );
}

#[test]
fn unrelated_inheritable_pipe_writer_is_closed_while_worker_remains_alive() {
    let fixture = Fixture::new();
    let mut descriptors = [0_i32; 2];
    assert_eq!(unsafe { libc::pipe(descriptors.as_mut_ptr()) }, 0);
    let mut read = unsafe { File::from_raw_fd(descriptors[0]) };
    let write = unsafe { File::from_raw_fd(descriptors[1]) };
    let mut child = fixture.spawn("sleep", limits());
    let _output = readiness(&mut child);
    let (sender, receiver) = mpsc::channel();
    let reader = thread::spawn(move || {
        let mut bytes = Vec::new();
        read.read_to_end(&mut bytes).unwrap();
        sender.send(bytes).unwrap();
    });
    drop(write);
    let eof = receiver.recv_timeout(Duration::from_secs(2));
    if eof.is_err() {
        child.terminate().unwrap();
    }
    assert!(
        eof.expect("unrelated inherited pipe stayed open")
            .is_empty()
    );
    assert!(child.try_wait().unwrap().is_none());
    child.terminate().unwrap();
    reader.join().unwrap();
}
