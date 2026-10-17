use crate::policy::{self, Grant};
use std::{
    cell::RefCell,
    ffi::{CString, OsString},
    fs::File,
    io::{self, Read, Write},
    os::{
        fd::{AsRawFd, FromRawFd, RawFd},
        unix::{ffi::OsStrExt, fs::OpenOptionsExt, process::ExitStatusExt},
    },
    path::Path,
    process::ExitStatus,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

thread_local! {
    static CANCELLATION: RefCell<Option<Arc<AtomicBool>>> = const { RefCell::new(None) };
}

/// Captures the owning run's cancellation flag, including after scope exit.
pub struct WorkerCancellationScope(
    Option<Arc<AtomicBool>>,
    std::marker::PhantomData<std::rc::Rc<()>>,
);
impl WorkerCancellationScope {
    #[must_use]
    pub fn enter(flag: Arc<AtomicBool>) -> Self {
        Self(
            CANCELLATION.with(|current| current.replace(Some(flag))),
            std::marker::PhantomData,
        )
    }
}
impl Drop for WorkerCancellationScope {
    fn drop(&mut self) {
        CANCELLATION.with(|current| current.replace(self.0.take()));
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct JobLimits {
    pub process_memory_bytes: usize,
    pub job_memory_bytes: usize,
    /// Kernel `RLIMIT_CPU` is rounded up to whole seconds, with a one-second minimum.
    pub cpu_time: Duration,
    /// Includes setup; enforced independently of calls to `try_wait`.
    pub wall_time: Duration,
}
impl JobLimits {
    fn validate(self) -> io::Result<()> {
        if self.process_memory_bytes == 0
            || self.job_memory_bytes < self.process_memory_bytes
            || self.cpu_time.is_zero()
            || self.wall_time.is_zero()
            || Instant::now().checked_add(self.wall_time).is_none()
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid worker memory, CPU or wall limit",
            ));
        }
        Ok(())
    }
}

/// Immutable exact ELF bytes. Callers can construct this from the same bytes
/// they hashed, eliminating a path reopen between verification and launch.
pub struct PinnedExecutable {
    file: File,
}
impl PinnedExecutable {
    /// Seal the exact caller-verified bytes for a one-shot kernel launch.
    ///
    /// # Errors
    /// Rejects another executable format/architecture or unavailable memfd/seal support.
    pub fn from_bytes(bytes: &[u8]) -> io::Result<Self> {
        if bytes.len() < 64 || &bytes[..7] != b"\x7fELF\x02\x01\x01" || bytes[18..20] != [62, 0] {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "worker must be an x86_64 little-endian ELF executable",
            ));
        }
        let name = c"private-resource-worker";
        let fd = unsafe {
            libc::memfd_create(name.as_ptr(), libc::MFD_CLOEXEC | libc::MFD_ALLOW_SEALING)
        };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        let mut file = unsafe { File::from_raw_fd(fd) };
        file.write_all(bytes)?;
        if unsafe { libc::fchmod(fd, 0o500) } != 0
            || unsafe {
                libc::fcntl(
                    fd,
                    libc::F_ADD_SEALS,
                    libc::F_SEAL_WRITE
                        | libc::F_SEAL_GROW
                        | libc::F_SEAL_SHRINK
                        | libc::F_SEAL_SEAL,
                )
            } < 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(Self { file })
    }

    /// Open a regular executable without following its final path component.
    /// Use `from_bytes` when its digest was already verified by the caller.
    ///
    /// # Errors
    /// Returns file-read errors and the errors from `from_bytes`.
    pub fn from_path(path: &Path) -> io::Result<Self> {
        let mut source = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(path)?;
        if !source.metadata()?.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "worker is not a regular file",
            ));
        }
        let mut bytes = Vec::new();
        source.read_to_end(&mut bytes)?;
        Self::from_bytes(&bytes)
    }
}

/// Explicit object grants plus the fixed minimal Ubuntu ELF runtime. The
/// runtime paths are canonicalized once and pinned; no directory-wide runtime
/// grants or inherited environment variables enter the worker.
pub struct SandboxProfile {
    grants: Mutex<Vec<Grant>>,
    abi: u32,
}
impl SandboxProfile {
    /// Establish the required ABI and pin the minimal qualified Ubuntu runtime.
    ///
    /// # Errors
    /// Fails when the required ABI or any fixed runtime file is unavailable.
    pub fn new() -> io::Result<Self> {
        let abi = policy::abi()?;
        let profile = Self {
            grants: Mutex::new(Vec::new()),
            abi,
        };
        profile.grant_read_execute(&std::fs::canonicalize("/lib64/ld-linux-x86-64.so.2")?)?;
        for runtime in [
            "/lib/x86_64-linux-gnu/libc.so.6",
            "/lib/x86_64-linux-gnu/libm.so.6",
            "/lib/x86_64-linux-gnu/libgcc_s.so.1",
            "/etc/ld.so.cache",
        ] {
            profile.grant_read(&std::fs::canonicalize(runtime)?)?;
        }
        Ok(profile)
    }
    #[must_use]
    pub fn landlock_abi(&self) -> u32 {
        self.abi
    }
    #[must_use]
    pub fn policy_id(&self) -> &'static str {
        policy::POLICY_ID
    }

    fn grant(&self, path: &Path, access: u64) -> io::Result<()> {
        let anchor = policy::anchor(path)?;
        self.grants
            .lock()
            .map_err(|_| io::Error::other("poisoned grants"))?
            .push(Grant { anchor, access });
        Ok(())
    }
    /// Grant read access to this object (recursively for a directory).
    /// # Errors
    /// Fails for a missing path, symlink, nonregular object or poisoned profile.
    pub fn grant_read(&self, path: &Path) -> io::Result<()> {
        self.grant(path, policy::READ)
    }
    /// Grant read and execute access to this object.
    /// # Errors
    /// Returns the same object-validation errors as `grant_read`.
    pub fn grant_read_execute(&self, path: &Path) -> io::Result<()> {
        self.grant(path, policy::READ_EXECUTE)
    }
    /// Grant read, write and truncate access to a pre-created output object.
    /// # Errors
    /// Returns the same object-validation errors as `grant_read`.
    pub fn grant_read_write(&self, path: &Path) -> io::Result<()> {
        self.grant(path, policy::READ_WRITE)
    }
    /// Grant regular-file/directory creation and removal inside private work.
    /// # Errors
    /// Fails when the path is not a real directory or cannot be pinned.
    pub fn grant_private_working_directory(&self, path: &Path) -> io::Result<()> {
        let anchor = policy::anchor(path)?;
        if !anchor.metadata()?.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "private working area must be a directory",
            ));
        }
        self.grants
            .lock()
            .map_err(|_| io::Error::other("poisoned grants"))?
            .push(Grant {
                anchor,
                access: policy::PRIVATE,
            });
        Ok(())
    }

    /// Pin and launch an ELF worker under these grants.
    /// # Errors
    /// Returns the errors from `PinnedExecutable::from_path` and `spawn_pinned`.
    pub fn spawn(
        &self,
        executable: &Path,
        args: &[OsString],
        working_directory: &Path,
        limits: JobLimits,
    ) -> io::Result<SandboxedProcess> {
        self.spawn_pinned(
            &PinnedExecutable::from_path(executable)?,
            args,
            working_directory,
            limits,
        )
    }

    /// Launch the sealed bytes after all kernel restrictions are active.
    ///
    /// # Errors
    /// Rejects invalid limits/arguments, cancellation, missing kernel features,
    /// unavailable runtime/grants or any setup/exec failure. No fallback runs.
    #[allow(clippy::too_many_lines)] // One launch/ownership state machine; no post-fork Rust cleanup.
    pub fn spawn_pinned(
        &self,
        executable: &PinnedExecutable,
        args: &[OsString],
        working_directory: &Path,
        limits: JobLimits,
    ) -> io::Result<SandboxedProcess> {
        limits.validate()?;
        let cancellation = CANCELLATION.with(|current| current.borrow().clone());
        if cancellation
            .as_ref()
            .is_some_and(|flag| flag.load(Ordering::Acquire))
        {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "owning run cancelled before launch",
            ));
        }
        let ruleset = policy::ruleset(
            &self
                .grants
                .lock()
                .map_err(|_| io::Error::other("poisoned grants"))?,
        )?;
        let cwd = policy::anchor(working_directory)?;
        if !cwd.metadata()?.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "worker cwd must be a directory",
            ));
        }
        let executable = executable.file.try_clone()?;
        let mut arguments = vec![c"private-resource-worker".to_owned()];
        for arg in args {
            arguments.push(CString::new(arg.as_bytes()).map_err(|_| {
                io::Error::new(io::ErrorKind::InvalidInput, "NUL in worker argument")
            })?);
        }
        let mut temporary_variable = b"TMPDIR=".to_vec();
        temporary_variable.extend_from_slice(working_directory.as_os_str().as_bytes());
        let environment = [
            CString::new(temporary_variable)
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "NUL in worker cwd"))?,
            c"LANG=C".to_owned(),
        ];
        let (stdin_read, stdin_write) = pipe()?;
        let (stdout_read, stdout_write) = pipe()?;
        let (stderr_read, stderr_write) = pipe()?;
        let (startup_read, startup_write) = pipe()?;
        let (barrier_read, barrier_write) = pipe()?;
        let mut inherited_files = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &raw mut inherited_files) } != 0 {
            return Err(io::Error::last_os_error());
        }
        if inherited_files.rlim_cur < policy::BOOTSTRAP_FD as u64 + 7 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "inherited descriptor limit cannot hold the worker bootstrap",
            ));
        }
        let stopped = Arc::new(AtomicBool::new(false));
        let deadline_terminated = Arc::new(AtomicBool::new(false));
        let owner_deadline_terminated = Arc::clone(&deadline_terminated);
        let status = Arc::new(Mutex::new(None));
        let (sender, receiver) = mpsc::sync_channel(1);
        let owner_stopped = Arc::clone(&stopped);
        let owner_status = Arc::clone(&status);
        let deadline = Instant::now() + limits.wall_time;
        // This thread both forks and remains alive until reaping. Linux
        // PDEATHSIG follows the *forking thread*, not merely the host PID.
        let owner = thread::Builder::new()
            .name("private-linux-worker-owner".into())
            .spawn(move || {
                let mut filter = policy::filter();
                let argv_ptrs = arguments
                    .iter()
                    .map(|arg| arg.as_ptr())
                    .chain(std::iter::once(std::ptr::null()))
                    .collect::<Vec<_>>();
                let env_ptrs = environment
                    .iter()
                    .map(|arg| arg.as_ptr())
                    .chain(std::iter::once(std::ptr::null()))
                    .collect::<Vec<_>>();
                let original_parent = unsafe { libc::getpid() };
                let mut raw_pidfd = -1_i32;
                let clone = CloneArgs {
                    flags: libc::CLONE_PIDFD as u64,
                    pidfd: (&raw mut raw_pidfd) as u64,
                    exit_signal: libc::SIGCHLD as u64,
                    ..CloneArgs::default()
                };
                // CLONE_PIDFD creates ownership atomically, including when the
                // host uses SIGCHLD auto-reaping. There is no numeric-PID
                // open/kill race and no weaker fork fallback.
                let cloned = unsafe {
                    libc::syscall(libc::SYS_clone3, &raw const clone, size_of::<CloneArgs>())
                };
                if cloned < 0 {
                    let _ = sender.send(Err(io::Error::last_os_error()));
                    return;
                }
                let child = i32::try_from(cloned).unwrap_or(-1);
                if child == 0 {
                    // All objects/pointers exist before fork. This branch uses only
                    // raw async-signal-safe calls and never unwinds or allocates.
                    unsafe {
                        child_exec(
                            &ChildSetup {
                                executable: executable.as_raw_fd(),
                                ruleset: ruleset.as_raw_fd(),
                                cwd: cwd.as_raw_fd(),
                                stdin: stdin_read.as_raw_fd(),
                                stdout: stdout_write.as_raw_fd(),
                                stderr: stderr_write.as_raw_fd(),
                                startup: startup_write.as_raw_fd(),
                                barrier: barrier_read.as_raw_fd(),
                                barrier_write: barrier_write.as_raw_fd(),
                                parent: original_parent,
                                limits,
                                argv: argv_ptrs.as_ptr(),
                                env: env_ptrs.as_ptr(),
                            },
                            &mut filter,
                        )
                    };
                }
                drop(stdin_read);
                drop(stdout_write);
                drop(stderr_write);
                drop(startup_write);
                drop(barrier_read);
                drop(executable);
                drop(ruleset);
                drop(cwd);
                let pidfd = unsafe { File::from_raw_fd(raw_pidfd) };
                if unsafe {
                    libc::syscall(
                        libc::SYS_pidfd_send_signal,
                        pidfd.as_raw_fd(),
                        0,
                        std::ptr::null::<u8>(),
                        0,
                    )
                } != 0
                {
                    let error = io::Error::last_os_error();
                    // The unreleased bootstrap exits on barrier EOF. It has
                    // not executed any worker bytes, so no kill fallback is
                    // required if stable signaling itself is unavailable.
                    drop(barrier_write);
                    reap(child);
                    let _ = sender.send(Err(error));
                    return;
                }
                let mut barrier_write = barrier_write;
                if let Err(error) = barrier_write.write_all(&[1]) {
                    kill_and_reap(&pidfd, child);
                    let _ = sender.send(Err(error));
                    return;
                }
                drop(barrier_write);
                let startup_result = read_startup(startup_read, deadline);
                if let Err(error) = startup_result {
                    kill_and_reap(&pidfd, child);
                    let _ = sender.send(Err(error));
                    return;
                }
                if sender.send(Ok(child)).is_err() {
                    kill_and_reap(&pidfd, child);
                    return;
                }
                let mut killed = false;
                loop {
                    let cancelled = owner_stopped.load(Ordering::Acquire)
                        || cancellation
                            .as_ref()
                            .is_some_and(|flag| flag.load(Ordering::Acquire));
                    if !killed && (cancelled || Instant::now() >= deadline) {
                        owner_deadline_terminated.store(!cancelled, Ordering::Release);
                        signal_kill(&pidfd);
                        killed = true;
                    }
                    let mut raw = 0;
                    let waited = unsafe { libc::waitpid(child, &raw mut raw, libc::WNOHANG) };
                    if waited == child {
                        *owner_status
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner) =
                            Some(Ok(ExitStatus::from_raw(raw)));
                        break;
                    }
                    if waited < 0 && io::Error::last_os_error().raw_os_error() != Some(libc::EINTR)
                    {
                        *owner_status
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner) =
                            Some(Err(io::Error::last_os_error()
                                .raw_os_error()
                                .unwrap_or(libc::ECHILD)));
                        break;
                    }
                    thread::sleep(Duration::from_millis(5));
                }
            })?;
        match receiver.recv() {
            Ok(Ok(pid)) => Ok(SandboxedProcess {
                stdin: Some(stdin_write),
                stdout: Some(stdout_read),
                stderr: Some(stderr_read),
                stopped,
                deadline_terminated,
                status,
                owner: Some(owner),
                pid,
            }),
            Ok(Err(error)) => {
                let _ = owner.join();
                Err(error)
            }
            Err(_) => {
                let _ = owner.join();
                Err(io::Error::other("worker owner failed during startup"))
            }
        }
    }
}

type SharedStatus = Arc<Mutex<Option<Result<ExitStatus, i32>>>>;

#[derive(Default)]
#[repr(C)]
struct CloneArgs {
    flags: u64,
    pidfd: u64,
    child_tid: u64,
    parent_tid: u64,
    exit_signal: u64,
    stack: u64,
    stack_size: u64,
    tls: u64,
    set_tid: u64,
    set_tid_size: u64,
    cgroup: u64,
}

/// The sole process owner; Drop, terminate and captured cancellation kill via
/// pidfd and reap before releasing this handle. Pipes never inherit host FDs.
pub struct SandboxedProcess {
    stdin: Option<File>,
    stdout: Option<File>,
    stderr: Option<File>,
    stopped: Arc<AtomicBool>,
    deadline_terminated: Arc<AtomicBool>,
    status: SharedStatus,
    owner: Option<JoinHandle<()>>,
    pid: i32,
}
impl SandboxedProcess {
    pub fn take_stdin(&mut self) -> Option<File> {
        self.stdin.take()
    }
    pub fn take_stdout(&mut self) -> Option<File> {
        self.stdout.take()
    }
    pub fn take_stderr(&mut self) -> Option<File> {
        self.stderr.take()
    }
    #[must_use]
    pub fn id(&self) -> u32 {
        self.pid.cast_unsigned()
    }
    /// Whether the independent owner terminated this worker at its wall deadline.
    /// The reason survives reaping so callers can preserve their timeout error.
    #[must_use]
    pub fn deadline_terminated(&self) -> bool {
        self.deadline_terminated.load(Ordering::Acquire)
    }
    /// Return a cached exit status; the owning thread enforces deadlines independently.
    /// # Errors
    /// Returns OS wait errors or an unavailable status lock.
    pub fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        match *self
            .status
            .lock()
            .map_err(|_| io::Error::other("poisoned worker status"))?
        {
            Some(Ok(status)) => Ok(Some(status)),
            Some(Err(errno)) => Err(io::Error::from_raw_os_error(errno)),
            None => Ok(None),
        }
    }
    /// Kill this worker through its pidfd and join its reaping owner.
    /// # Errors
    /// Returns OS wait errors or a failed owner thread.
    pub fn terminate(&mut self) -> io::Result<()> {
        self.stopped.store(true, Ordering::Release);
        if let Some(owner) = self.owner.take() {
            owner
                .join()
                .map_err(|_| io::Error::other("worker owner panicked"))?;
        }
        self.try_wait().map(|_| ())
    }
}
impl Drop for SandboxedProcess {
    fn drop(&mut self) {
        let _ = self.terminate();
    }
}

fn pipe() -> io::Result<(File, File)> {
    let mut descriptors = [-1; 2];
    if unsafe { libc::pipe2(descriptors.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe {
        (
            File::from_raw_fd(descriptors[0]),
            File::from_raw_fd(descriptors[1]),
        )
    })
}
fn signal_kill(pidfd: &File) {
    unsafe {
        libc::syscall(
            libc::SYS_pidfd_send_signal,
            pidfd.as_raw_fd(),
            libc::SIGKILL,
            std::ptr::null::<u8>(),
            0,
        );
    }
}
fn kill_and_reap(pidfd: &File, child: i32) {
    signal_kill(pidfd);
    reap(child);
}
fn reap(child: i32) {
    loop {
        let result = unsafe { libc::waitpid(child, std::ptr::null_mut(), 0) };
        if result >= 0 || io::Error::last_os_error().raw_os_error() != Some(libc::EINTR) {
            break;
        }
    }
}
fn read_startup(mut pipe: File, deadline: Instant) -> io::Result<()> {
    let mut poll = libc::pollfd {
        fd: pipe.as_raw_fd(),
        events: libc::POLLIN | libc::POLLHUP,
        revents: 0,
    };
    let mut report = Vec::with_capacity(5);
    loop {
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "confinement startup exceeded wall limit",
            ));
        }
        let result = unsafe { libc::poll(&raw mut poll, 1, 10) };
        if result < 0 {
            if io::Error::last_os_error().raw_os_error() == Some(libc::EINTR) {
                continue;
            }
            return Err(io::Error::last_os_error());
        }
        if result > 0 {
            let mut bytes = [0_u8; 5];
            match pipe.read(&mut bytes)? {
                0 => break,
                count => {
                    report.extend_from_slice(&bytes[..count]);
                    if report.len() > 5 {
                        return Err(io::Error::other("invalid confinement startup report"));
                    }
                }
            }
        }
    }
    match report.as_slice() {
        [0xa5] => Ok(()),
        [a, b, c, d] | [0xa5, a, b, c, d] => Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "Linux worker confinement setup failed: {}",
                io::Error::from_raw_os_error(i32::from_ne_bytes([*a, *b, *c, *d]))
            ),
        )),
        _ => Err(io::Error::other(
            "worker died before confinement startup completed",
        )),
    }
}

struct ChildSetup {
    executable: RawFd,
    ruleset: RawFd,
    cwd: RawFd,
    stdin: RawFd,
    stdout: RawFd,
    stderr: RawFd,
    startup: RawFd,
    barrier: RawFd,
    barrier_write: RawFd,
    parent: i32,
    limits: JobLimits,
    argv: *const *const libc::c_char,
    env: *const *const libc::c_char,
}

/// Never returns. Safety: all descriptors and pointers are live in the forked
/// address space. No Rust allocation, formatting, locking, unwinding or Drop.
unsafe fn child_exec(setup: &ChildSetup, filter: &mut [libc::sock_filter]) -> ! {
    unsafe {
        if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL, 0, 0, 0) != 0
            || libc::getppid() != setup.parent
        {
            child_error(setup.startup);
        }
        libc::close(setup.barrier_write);
        let mut byte = 0_u8;
        if libc::read(setup.barrier, (&raw mut byte).cast(), 1) != 1 {
            libc::_exit(126);
        }
        if libc::syscall(libc::SYS_close_range, 3_u32, u32::MAX, 4_u32) != 0 {
            child_error(setup.startup);
        }
        // Reserve the one-shot executable/error descriptors before lowering
        // NOFILE. Duplicate first to avoid source-descriptor collisions.
        let descriptors = [
            setup.executable,
            setup.startup,
            setup.stdin,
            setup.stdout,
            setup.stderr,
            setup.cwd,
            setup.ruleset,
        ];
        let mut copied = [-1; 7];
        for (index, descriptor) in descriptors.iter().enumerate() {
            copied[index] = libc::fcntl(*descriptor, libc::F_DUPFD_CLOEXEC, policy::BOOTSTRAP_FD);
            if copied[index] < 0 {
                child_error(setup.startup);
            }
        }
        let [executable, startup, stdin, stdout, stderr, cwd, ruleset] = copied;
        if libc::dup3(executable, policy::EXEC_FD, libc::O_CLOEXEC) < 0
            || libc::dup3(startup, policy::ERROR_FD, libc::O_CLOEXEC) < 0
        {
            child_error(startup);
        }
        if libc::dup2(stdin, 0) < 0 || libc::dup2(stdout, 1) < 0 || libc::dup2(stderr, 2) < 0 {
            child_error(policy::ERROR_FD);
        }
        if libc::fchdir(cwd) != 0 {
            child_error(policy::ERROR_FD);
        }
        let cpu = setup
            .limits
            .cpu_time
            .as_secs()
            .saturating_add(u64::from(setup.limits.cpu_time.subsec_nanos() > 0))
            .max(1);
        for (resource, value) in [
            (libc::RLIMIT_AS, setup.limits.process_memory_bytes as u64),
            (libc::RLIMIT_CPU, cpu),
            (libc::RLIMIT_NOFILE, policy::OPEN_FILES),
            (libc::RLIMIT_CORE, 0),
            (libc::RLIMIT_FSIZE, setup.limits.job_memory_bytes as u64),
            (libc::RLIMIT_STACK, 8 * 1024 * 1024),
        ] {
            let limit = libc::rlimit {
                rlim_cur: value,
                rlim_max: value,
            };
            if libc::setrlimit(resource, &raw const limit) != 0 {
                child_error(policy::ERROR_FD);
            }
        }
        if libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0
            || libc::syscall(libc::SYS_landlock_restrict_self, ruleset, 0) != 0
        {
            child_error(policy::ERROR_FD);
        }
        policy::set_tgid(filter, libc::getpid().cast_unsigned());
        let program = libc::sock_fprog {
            len: u16::try_from(filter.len()).unwrap_or(0),
            filter: filter.as_mut_ptr(),
        };
        if libc::syscall(libc::SYS_seccomp, 1, 0, &raw const program) != 0 {
            child_error(policy::ERROR_FD);
        }
        let ready = 0xa5_u8;
        if libc::write(policy::ERROR_FD, (&raw const ready).cast(), 1) != 1 {
            libc::_exit(126);
        }
        libc::syscall(
            libc::SYS_execveat,
            policy::EXEC_FD,
            c"".as_ptr(),
            setup.argv,
            setup.env,
            libc::AT_EMPTY_PATH,
        );
        child_error(policy::ERROR_FD);
    }
}
unsafe fn child_error(fd: RawFd) -> ! {
    unsafe {
        let errno = *libc::__errno_location();
        libc::write(fd, (&raw const errno).cast(), size_of::<i32>());
        libc::_exit(126);
    }
}
