#![cfg(windows)]

use publisher_worker_job::{AppContainerProfile, JobLimits};
use std::{
    ffi::OsString,
    fs,
    io::Read,
    net::TcpListener,
    os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use windows_sys::Win32::{
    Foundation::{GetHandleInformation, HANDLE_FLAG_INHERIT, SetHandleInformation},
    Security::SECURITY_ATTRIBUTES,
    System::Pipes::CreatePipe,
};

struct ResourceFixture {
    root: std::path::PathBuf,
    probe: std::path::PathBuf,
}

impl ResourceFixture {
    fn new() -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "publisher-resource-test-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir(&root).expect("create resource fixture");
        let probe = root.join("publisher-isolation-probe.exe");
        let built_probe = std::env::current_exe()
            .expect("test executable")
            .parent()
            .and_then(|dependencies| dependencies.parent())
            .expect("target release directory")
            .join("publisher-isolation-probe.exe");
        fs::copy(built_probe, &probe).expect("stage resource probe");
        Self { root, probe }
    }

    fn spawn(&self, mode: &str) -> publisher_worker_job::SandboxedProcess {
        let profile = AppContainerProfile::create_unique("PublisherV2.ResourceTest")
            .expect("create resource profile");
        profile
            .grant_read_execute(&self.root)
            .expect("grant probe execution");
        let mut child = profile
            .spawn(
                &self.probe,
                &[OsString::from(mode)],
                &self.root,
                JobLimits {
                    process_memory_bytes: 64 * 1024 * 1024,
                    job_memory_bytes: 64 * 1024 * 1024,
                    cpu_time: Duration::from_millis(250),
                },
            )
            .expect("launch resource probe");
        drop(child.take_stdin());
        child
    }
}

impl Drop for ResourceFixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.root).expect("clean resource fixture");
    }
}

fn bounded_wait(child: &mut publisher_worker_job::SandboxedProcess) -> std::process::ExitStatus {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(status) = child.try_wait().expect("query resource probe") {
            return status;
        }
        assert!(
            Instant::now() < deadline,
            "resource probe exceeded wall deadline"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn appcontainer_memory_limit_denies_bounded_allocation() {
    let fixture = ResourceFixture::new();
    let mut child = fixture.spawn("memory");
    let mut stdout = child.take_stdout().expect("memory stdout");
    let status = bounded_wait(&mut child);
    let mut output = String::new();
    stdout
        .read_to_string(&mut output)
        .expect("read memory report");
    assert!(
        status.success(),
        "allocation probe failed unexpectedly: {status}; {output}"
    );
    assert_eq!(
        output.lines().collect::<Vec<_>>(),
        ["ready", "memory-denied"]
    );
}

#[test]
fn appcontainer_cpu_limit_terminates_busy_worker() {
    let fixture = ResourceFixture::new();
    let mut child = fixture.spawn("cpu");
    let mut stdout = child.take_stdout().expect("cpu stdout");
    let status = bounded_wait(&mut child);
    let mut output = String::new();
    stdout.read_to_string(&mut output).expect("read cpu report");
    assert_eq!(
        output.trim(),
        "ready",
        "worker must start before its CPU budget expires"
    );
    assert!(
        !status.success(),
        "busy worker unexpectedly completed normally"
    );
}

#[test]
fn dropping_running_appcontainer_closes_worker_pipes_promptly() {
    use std::io::BufRead;
    let fixture = ResourceFixture::new();
    let mut child = fixture.spawn("sleep");
    let mut stdout = std::io::BufReader::new(child.take_stdout().expect("sleep stdout"));
    let (sender, receiver) = std::sync::mpsc::channel();
    let reader = std::thread::spawn(move || {
        let mut line = String::new();
        stdout.read_line(&mut line).expect("read worker readiness");
        sender.send(line).expect("send worker readiness");
        let mut remainder = String::new();
        stdout
            .read_to_string(&mut remainder)
            .expect("wait for pipe EOF");
        sender.send(remainder).expect("send pipe EOF");
    });
    assert_eq!(
        receiver
            .recv_timeout(Duration::from_secs(5))
            .expect("worker readiness")
            .trim(),
        "ready"
    );
    assert!(child.try_wait().expect("query sleeping worker").is_none());
    let started = Instant::now();
    drop(child);
    assert_eq!(
        receiver
            .recv_timeout(Duration::from_secs(2))
            .expect("worker pipe must close after drop"),
        ""
    );
    assert!(
        started.elapsed() < Duration::from_secs(7),
        "drop exceeded termination deadline"
    );
    reader.join().expect("join pipe reader");
}

#[test]
fn explicit_job_cancellation_stops_only_its_worker_after_scope_returns() {
    use publisher_worker_job::WorkerCancellationScope;
    use std::{io::BufRead, sync::{Arc, atomic::{AtomicBool, Ordering}}};
    let fixture = ResourceFixture::new();
    let cancellation = Arc::new(AtomicBool::new(false));
    let mut cancelled = {
        let _scope = WorkerCancellationScope::enter(Arc::clone(&cancellation));
        fixture.spawn("sleep")
    };
    let mut independent = fixture.spawn("sleep");
    let mut output = std::io::BufReader::new(cancelled.take_stdout().unwrap());
    let mut line = String::new();
    output.read_line(&mut line).unwrap();
    assert_eq!(line.trim(), "ready");
    cancellation.store(true, Ordering::Release);
    let start = Instant::now();
    let status = cancelled.try_wait().unwrap().expect("cancelled worker must exit");
    assert!(!status.success());
    assert!(start.elapsed() < Duration::from_secs(2), "cancellation exceeded local acceptance deadline");
    let mut remaining = String::new();
    output.read_to_string(&mut remaining).unwrap();
    assert!(remaining.is_empty());
    assert!(independent.try_wait().unwrap().is_none(), "another job must stay alive");
    independent.terminate().unwrap();
}

#[test]
fn unrelated_inheritable_pipe_is_not_passed_to_worker() {
    use std::io::BufRead;

    let mut security = SECURITY_ATTRIBUTES {
        nLength: u32::try_from(std::mem::size_of::<SECURITY_ATTRIBUTES>())
            .expect("security attributes size"),
        lpSecurityDescriptor: std::ptr::null_mut(),
        bInheritHandle: 1,
    };
    let mut raw_read = std::ptr::null_mut();
    let mut raw_write = std::ptr::null_mut();
    assert_ne!(
        unsafe { CreatePipe(&raw mut raw_read, &raw mut raw_write, &raw mut security, 0) },
        0,
        "create unrelated inheritable pipe"
    );
    let read = unsafe { OwnedHandle::from_raw_handle(raw_read.cast()) };
    let write = unsafe { OwnedHandle::from_raw_handle(raw_write.cast()) };
    assert_ne!(
        unsafe { SetHandleInformation(read.as_raw_handle().cast(), HANDLE_FLAG_INHERIT, 0) },
        0,
        "keep the sentinel read end in the parent"
    );
    let mut write_flags = 0_u32;
    assert_ne!(
        unsafe { GetHandleInformation(write.as_raw_handle().cast(), &raw mut write_flags) },
        0,
        "query sentinel write end"
    );
    assert_ne!(write_flags & HANDLE_FLAG_INHERIT, 0);

    let (sender, receiver) = std::sync::mpsc::channel();
    let reader = std::thread::spawn(move || {
        let mut file = fs::File::from(read);
        let mut bytes = Vec::new();
        let result = file.read_to_end(&mut bytes).map(|_| bytes);
        sender.send(result).expect("send sentinel pipe result");
    });
    let fixture = ResourceFixture::new();
    let mut child = fixture.spawn("sleep");
    let mut stdout = std::io::BufReader::new(child.take_stdout().expect("sleep stdout"));
    let (ready_sender, ready_receiver) = std::sync::mpsc::channel();
    let ready_reader = std::thread::spawn(move || {
        let mut line = String::new();
        let result = stdout.read_line(&mut line).map(|_| line);
        ready_sender.send(result).expect("send worker readiness");
    });
    match ready_receiver.recv_timeout(Duration::from_secs(5)) {
        Ok(Ok(line)) if line.trim() == "ready" => {}
        other => {
            drop(child);
            drop(write);
            reader
                .join()
                .expect("join sentinel reader after failed readiness");
            ready_reader
                .join()
                .expect("join readiness reader after worker cleanup");
            panic!("worker did not report readiness: {other:?}");
        }
    }
    ready_reader.join().expect("join readiness reader");
    assert!(child.try_wait().expect("query sleeping worker").is_none());

    drop(write);
    let eof = receiver.recv_timeout(Duration::from_secs(2));
    if eof.is_err() {
        drop(child);
        reader
            .join()
            .expect("join sentinel reader after worker cleanup");
        panic!("unrelated inheritable write handle kept the sentinel pipe open");
    }
    assert!(
        eof.expect("sentinel EOF deadline")
            .expect("read sentinel pipe")
            .is_empty()
    );
    assert!(
        child
            .try_wait()
            .expect("query worker after sentinel EOF")
            .is_none()
    );
    drop(child);
    reader.join().expect("join sentinel reader");
}

#[test]
#[allow(clippy::too_many_lines)]
fn appcontainer_denies_host_files_network_and_children() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time")
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "publisher-isolation-test-{}-{nonce}",
        std::process::id()
    ));
    let sandbox = root.join("sandbox with spaces");
    let outside = root.join("outside");
    fs::create_dir_all(&sandbox).expect("create sandbox directory");
    fs::create_dir_all(&outside).expect("create outside directory");
    let denied_path = outside.join("private.txt");
    fs::write(&denied_path, b"private").expect("write outside sentinel");

    let profile = AppContainerProfile::create_unique("PublisherV2.IsolationTest")
        .expect("create unique AppContainer profile");
    let profile_name = profile.profile_name();
    profile
        .grant_read_execute(&sandbox)
        .expect("grant sandbox read and execute");
    let allowed_path = sandbox.join("allowed.txt");
    fs::write(&allowed_path, b"allowed").expect("write allowed sentinel");
    let probe = sandbox.join("publisher-isolation-probe.exe");
    let report = sandbox.join("report.txt");
    let built_probe = std::env::current_exe()
        .expect("test executable path")
        .parent()
        .and_then(|dependencies| dependencies.parent())
        .expect("target debug directory")
        .join("publisher-isolation-probe.exe");
    fs::copy(&built_probe, &probe).expect("stage isolation probe");
    fs::write(&report, []).expect("pre-create report file");
    profile
        .grant_read_write(&report)
        .expect("grant report read and write");

    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind loopback listener");
    listener
        .set_nonblocking(true)
        .expect("make listener nonblocking");
    let port = listener.local_addr().expect("listener address").port();
    let arguments = [
        allowed_path.into_os_string(),
        denied_path.clone().into_os_string(),
        OsString::from(port.to_string()),
        report.clone().into_os_string(),
        profile_name,
    ];
    let mut child = profile
        .spawn(
            &probe,
            &arguments,
            &sandbox,
            JobLimits {
                process_memory_bytes: 128 * 1024 * 1024,
                job_memory_bytes: 128 * 1024 * 1024,
                cpu_time: Duration::from_secs(5),
            },
        )
        .expect("launch probe in AppContainer");
    drop(child.take_stdin());
    let mut stdout = child.take_stdout().expect("probe stdout");
    let mut stderr = child.take_stderr().expect("probe stderr");
    let deadline = Instant::now() + Duration::from_secs(10);
    let status = loop {
        if let Some(status) = child.try_wait().expect("query probe") {
            break status;
        }
        if Instant::now() >= deadline {
            child.terminate().expect("terminate timed-out probe");
            panic!("AppContainer isolation probe timed out");
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let mut pipe_output = String::new();
    stdout
        .read_to_string(&mut pipe_output)
        .expect("read probe stdout");
    let mut pipe_error = String::new();
    stderr
        .read_to_string(&mut pipe_error)
        .expect("read probe stderr");
    let output = fs::read_to_string(&report).expect("read probe report");
    let expected_prefix = "appcontainer=true;allowed_read=true;denied_read=true;denied_write=true;network_denied=true;";
    assert!(
        status.success(),
        "probe failed with {status:?}: {output}; {pipe_error}"
    );
    assert_eq!(pipe_output.trim(), "pipe-ok", "{pipe_error}");
    assert!(output.starts_with(expected_prefix), "{output}");
    assert!(
        output.contains("child_denied=true;environment_sanitized=true"),
        "{output}"
    );
    assert!(
        listener.accept().is_err(),
        "the capability-free AppContainer unexpectedly reached loopback"
    );

    drop(child);
    fs::remove_dir_all(&root).expect("clean isolation fixture");
}
