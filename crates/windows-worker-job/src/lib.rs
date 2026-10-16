//! Narrow safe wrapper around the Windows isolation calls used by private workers.

#![cfg(windows)]

use std::{
    ffi::{OsStr, OsString},
    fs::File,
    io,
    os::windows::{
        ffi::{OsStrExt, OsStringExt},
        io::{AsRawHandle, FromRawHandle, OwnedHandle},
        process::ExitStatusExt,
    },
    path::Path,
    process::{Child, ExitStatus},
    time::Duration,
    cell::RefCell,
    sync::{Arc, atomic::{AtomicBool, Ordering}},
};

thread_local! {
    static CANCELLATION: RefCell<Option<Arc<AtomicBool>>> = const { RefCell::new(None) };
}

/// Propagates one owning job's cancellation to workers spawned on this thread.
/// Reader threads do not need the scope: each process retains the same flag.
pub struct WorkerCancellationScope(Option<Arc<AtomicBool>>, std::marker::PhantomData<std::rc::Rc<()>>);

impl WorkerCancellationScope {
    #[must_use]
    pub fn enter(flag: Arc<AtomicBool>) -> Self {
        Self(CANCELLATION.with(|current| current.replace(Some(flag))), std::marker::PhantomData)
    }
}

impl Drop for WorkerCancellationScope {
    fn drop(&mut self) {
        CANCELLATION.with(|current| current.replace(self.0.take()));
    }
}
use windows_sys::Win32::{
    Foundation::{
        CloseHandle, GENERIC_EXECUTE, GENERIC_READ, GENERIC_WRITE, HANDLE, HANDLE_FLAG_INHERIT,
        LocalFree, SetHandleInformation, WAIT_OBJECT_0, WAIT_TIMEOUT,
    },
    Security::{
        Authorization::{
            EXPLICIT_ACCESS_W, GRANT_ACCESS, GetNamedSecurityInfoW, NO_MULTIPLE_TRUSTEE,
            SE_FILE_OBJECT, SetEntriesInAclW, SetNamedSecurityInfoW, TRUSTEE_IS_GROUP,
            TRUSTEE_IS_SID, TRUSTEE_W,
        },
        DACL_SECURITY_INFORMATION, FreeSid,
        Isolation::{
            CreateAppContainerProfile, DeleteAppContainerProfile,
            DeriveAppContainerSidFromAppContainerName,
        },
        PSID, SECURITY_ATTRIBUTES, SECURITY_CAPABILITIES, SUB_CONTAINERS_AND_OBJECTS_INHERIT,
    },
    System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_ACTIVE_PROCESS,
        JOB_OBJECT_LIMIT_JOB_MEMORY, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        JOB_OBJECT_LIMIT_PROCESS_MEMORY, JOB_OBJECT_LIMIT_PROCESS_TIME,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
        SetInformationJobObject, TerminateJobObject,
    },
    System::Pipes::CreatePipe,
    System::Threading::{
        CREATE_NO_WINDOW, CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT, CreateProcessW,
        DeleteProcThreadAttributeList, EXTENDED_STARTUPINFO_PRESENT, GetExitCodeProcess,
        InitializeProcThreadAttributeList, PROC_THREAD_ATTRIBUTE_HANDLE_LIST,
        PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES, PROCESS_INFORMATION, ResumeThread,
        STARTF_USESTDHANDLES, STARTUPINFOEXW, TerminateProcess, UpdateProcThreadAttribute,
        WaitForSingleObject,
    },
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct JobLimits {
    pub process_memory_bytes: usize,
    pub job_memory_bytes: usize,
    pub cpu_time: Duration,
}

impl JobLimits {
    fn cpu_ticks(self) -> io::Result<i64> {
        let ticks = self.cpu_time.as_nanos() / 100;
        i64::try_from(ticks)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "CPU limit is too large"))
    }
}

/// Owns the kill-on-close Job Object assigned to one worker process.
pub struct WorkerJob {
    handle: HANDLE,
}

impl WorkerJob {
    fn create(limits: JobLimits) -> io::Result<Self> {
        if limits.process_memory_bytes == 0 || limits.job_memory_bytes < limits.process_memory_bytes
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "job memory must be at least the nonzero process memory limit",
            ));
        }
        let handle = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if handle.is_null() {
            return Err(io::Error::last_os_error());
        }
        let job = Self { handle };
        job.configure(limits)?;
        Ok(job)
    }

    fn configure(&self, limits: JobLimits) -> io::Result<()> {
        let mut information = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        information.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
            | JOB_OBJECT_LIMIT_ACTIVE_PROCESS
            | JOB_OBJECT_LIMIT_PROCESS_MEMORY
            | JOB_OBJECT_LIMIT_JOB_MEMORY
            | JOB_OBJECT_LIMIT_PROCESS_TIME;
        information.BasicLimitInformation.ActiveProcessLimit = 1;
        information.BasicLimitInformation.PerProcessUserTimeLimit = limits.cpu_ticks()?;
        information.ProcessMemoryLimit = limits.process_memory_bytes;
        information.JobMemoryLimit = limits.job_memory_bytes;
        let information_size =
            u32::try_from(std::mem::size_of_val(&information)).map_err(|_| {
                io::Error::new(io::ErrorKind::InvalidInput, "job information is too large")
            })?;
        let configured = unsafe {
            SetInformationJobObject(
                self.handle,
                JobObjectExtendedLimitInformation,
                std::ptr::from_ref(&information).cast(),
                information_size,
            )
        };
        if configured == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    /// Creates a configured Job Object and assigns `child` to it.
    ///
    /// # Errors
    ///
    /// Returns an OS error when the limits are invalid or Windows cannot create,
    /// configure, or assign the Job Object.
    pub fn assign(child: &Child, limits: JobLimits) -> io::Result<Self> {
        let job = Self::create(limits)?;
        let process_handle = child.as_raw_handle().cast();
        if unsafe { AssignProcessToJobObject(job.handle, process_handle) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(job)
    }

    /// Terminates every process assigned to this Job Object.
    ///
    /// # Errors
    ///
    /// Returns the Windows error when the job cannot be terminated.
    pub fn terminate(&self) -> io::Result<()> {
        if unsafe { TerminateJobObject(self.handle, 1) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
}

/// A unique `AppContainer` identity with no capabilities.
///
/// Any profile store created by Windows is deleted when the value is dropped.
/// Hosts that refuse profile storage use the same storage-free derived package
/// SID. Callers should create one identity per worker run so two concurrent
/// parsers cannot read each other's staged files through a shared package SID.
pub struct AppContainerProfile {
    name: Vec<u16>,
    sid: PSID,
    created_profile_store: bool,
}

impl AppContainerProfile {
    /// Creates one unique, capability-free `AppContainer` identity.
    ///
    /// # Errors
    ///
    /// Returns an OS error if Windows cannot create the profile and SID.
    pub fn create_unique(prefix: &str) -> io::Result<Self> {
        if prefix.is_empty()
            || !prefix
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'.')
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "AppContainer prefix must contain only ASCII letters, numbers, and periods",
            ));
        }
        for attempt in 0..16_u32 {
            let nonce = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(io::Error::other)?
                .as_nanos();
            let name = format!("{prefix}.{}.{}.{attempt}", std::process::id(), nonce);
            if name.len() > 64 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "AppContainer profile name exceeds 64 characters",
                ));
            }
            let wide_name = wide_null(OsStr::new(&name))?;
            let display = wide_null(OsStr::new("Publisher V2 isolated parser"))?;
            let description = wide_null(OsStr::new(
                "Temporary capability-free profile for one untrusted parser run",
            ))?;
            let mut sid = std::ptr::null_mut();
            let result = unsafe {
                CreateAppContainerProfile(
                    wide_name.as_ptr(),
                    display.as_ptr(),
                    description.as_ptr(),
                    std::ptr::null(),
                    0,
                    &raw mut sid,
                )
            };
            if result >= 0 && !sid.is_null() {
                return Ok(Self {
                    name: wide_name,
                    sid,
                    created_profile_store: true,
                });
            }
            if result == hresult_from_win32(2) {
                let derived = unsafe {
                    DeriveAppContainerSidFromAppContainerName(wide_name.as_ptr(), &raw mut sid)
                };
                if derived >= 0 && !sid.is_null() {
                    return Ok(Self {
                        name: wide_name,
                        sid,
                        created_profile_store: false,
                    });
                }
                return Err(io::Error::from_raw_os_error(derived));
            }
            if result != hresult_from_win32(183) {
                return Err(io::Error::from_raw_os_error(result));
            }
        }
        Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "could not allocate a unique AppContainer profile",
        ))
    }

    /// Returns the exact unique profile name used to derive the package SID.
    #[must_use]
    pub fn profile_name(&self) -> OsString {
        OsString::from_wide(&self.name[..self.name.len() - 1])
    }

    /// Grants this profile read, execute, and directory-traverse access to a
    /// staged directory. New children inherit the same read/execute grant.
    ///
    /// # Errors
    ///
    /// Returns an OS error if the existing DACL cannot be read or updated.
    pub fn grant_read_execute(&self, path: &Path) -> io::Result<()> {
        self.grant_access(
            path,
            GENERIC_READ | GENERIC_EXECUTE,
            SUB_CONTAINERS_AND_OBJECTS_INHERIT,
        )
    }

    /// Grants this profile read/write access to one pre-created IPC file.
    ///
    /// # Errors
    ///
    /// Returns an OS error if the existing DACL cannot be read or updated.
    pub fn grant_read_write(&self, path: &Path) -> io::Result<()> {
        self.grant_access(path, GENERIC_READ | GENERIC_WRITE, 0)
    }

    /// Grants this profile read access to one existing file.
    ///
    /// # Errors
    ///
    /// Returns an OS error if the existing DACL cannot be read or updated.
    pub fn grant_read(&self, path: &Path) -> io::Result<()> {
        self.grant_access(path, GENERIC_READ, 0)
    }

    /// Grants this profile read, write, and traverse access to one private,
    /// host-created working directory without granting access to its parent.
    /// Callers must use a unique directory containing only one worker run.
    ///
    /// # Errors
    ///
    /// Returns an OS error if the existing DACL cannot be read or updated.
    pub fn grant_private_working_directory(&self, path: &Path) -> io::Result<()> {
        self.grant_access(path, GENERIC_READ | GENERIC_WRITE | GENERIC_EXECUTE, 0)
    }

    fn grant_access(&self, path: &Path, permissions: u32, inheritance: u32) -> io::Result<()> {
        let mut path = wide_null(path.as_os_str())?;
        let mut old_acl = std::ptr::null_mut();
        let mut security_descriptor = std::ptr::null_mut();
        let get_result = unsafe {
            GetNamedSecurityInfoW(
                path.as_ptr(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &raw mut old_acl,
                std::ptr::null_mut(),
                &raw mut security_descriptor,
            )
        };
        if get_result != 0 {
            return Err(io::Error::from_raw_os_error(get_result.cast_signed()));
        }
        let _descriptor = LocalAllocation(security_descriptor.cast());
        let trustee = TRUSTEE_W {
            pMultipleTrustee: std::ptr::null_mut(),
            MultipleTrusteeOperation: NO_MULTIPLE_TRUSTEE,
            TrusteeForm: TRUSTEE_IS_SID,
            TrusteeType: TRUSTEE_IS_GROUP,
            ptstrName: self.sid.cast(),
        };
        let entry = EXPLICIT_ACCESS_W {
            grfAccessPermissions: permissions,
            grfAccessMode: GRANT_ACCESS,
            grfInheritance: inheritance,
            Trustee: trustee,
        };
        let mut new_acl = std::ptr::null_mut();
        let acl_result =
            unsafe { SetEntriesInAclW(1, &raw const entry, old_acl, &raw mut new_acl) };
        if acl_result != 0 {
            return Err(io::Error::from_raw_os_error(acl_result.cast_signed()));
        }
        let _new_acl = LocalAllocation(new_acl.cast());
        let set_result = unsafe {
            SetNamedSecurityInfoW(
                path.as_mut_ptr(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                new_acl,
                std::ptr::null_mut(),
            )
        };
        if set_result != 0 {
            return Err(io::Error::from_raw_os_error(set_result.cast_signed()));
        }
        Ok(())
    }

    /// Launches an exact executable in this capability-free `AppContainer`.
    /// Only the three standard-I/O pipes are inherited through an explicit
    /// handle allowlist. The process enters its one-process Job Object before
    /// its first instruction executes.
    ///
    /// # Errors
    ///
    /// Returns an OS error if attribute, job, or process creation fails.
    pub fn spawn(
        self,
        executable: &Path,
        arguments: &[OsString],
        current_directory: &Path,
        limits: JobLimits,
    ) -> io::Result<SandboxedProcess> {
        spawn_appcontainer(self, executable, arguments, current_directory, limits)
    }
}

impl Drop for AppContainerProfile {
    fn drop(&mut self) {
        unsafe {
            if self.created_profile_store {
                DeleteAppContainerProfile(self.name.as_ptr());
            }
            FreeSid(self.sid);
        }
    }
}

/// A capability-free `AppContainer` process with allow-listed standard-I/O pipes.
pub struct SandboxedProcess {
    process: OwnedHandle,
    job: WorkerJob,
    profile: Option<AppContainerProfile>,
    stdin: Option<File>,
    stdout: Option<File>,
    stderr: Option<File>,
    exited: bool,
    cancellation: Option<Arc<AtomicBool>>,
}

impl SandboxedProcess {
    #[must_use]
    pub fn take_stdin(&mut self) -> Option<File> {
        self.stdin.take()
    }

    #[must_use]
    pub fn take_stdout(&mut self) -> Option<File> {
        self.stdout.take()
    }

    #[must_use]
    pub fn take_stderr(&mut self) -> Option<File> {
        self.stderr.take()
    }

    /// Returns the exit status if the process has stopped.
    ///
    /// # Errors
    ///
    /// Returns an OS error if the process state cannot be queried.
    pub fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        if !self.exited && self.cancellation.as_ref().is_some_and(|flag| flag.load(Ordering::Acquire)) {
            self.terminate()?;
        }
        let wait = unsafe { WaitForSingleObject(self.process.as_raw_handle().cast(), 0) };
        if wait == WAIT_TIMEOUT {
            return Ok(None);
        }
        if wait != WAIT_OBJECT_0 {
            return Err(io::Error::last_os_error());
        }
        let mut exit_code = 0_u32;
        if unsafe { GetExitCodeProcess(self.process.as_raw_handle().cast(), &raw mut exit_code) }
            == 0
        {
            return Err(io::Error::last_os_error());
        }
        self.exited = true;
        Ok(Some(ExitStatus::from_raw(exit_code)))
    }

    /// Terminates the whole job and waits for the process to stop.
    ///
    /// # Errors
    ///
    /// Returns an OS error when termination or waiting fails.
    pub fn terminate(&mut self) -> io::Result<()> {
        if !self.exited {
            self.job.terminate()?;
            let wait = unsafe { WaitForSingleObject(self.process.as_raw_handle().cast(), 5_000) };
            if wait != WAIT_OBJECT_0 {
                return Err(if wait == WAIT_TIMEOUT {
                    io::Error::new(
                        io::ErrorKind::TimedOut,
                        "sandboxed process did not terminate",
                    )
                } else {
                    io::Error::last_os_error()
                });
            }
            self.exited = true;
        }
        Ok(())
    }
}

impl Drop for SandboxedProcess {
    fn drop(&mut self) {
        if !self.exited {
            let _ignored = self.job.terminate();
            unsafe {
                WaitForSingleObject(self.process.as_raw_handle().cast(), 5_000);
            }
        }
        drop(self.stdin.take());
        drop(self.stdout.take());
        drop(self.stderr.take());
        drop(self.profile.take());
    }
}

fn spawn_appcontainer(
    profile: AppContainerProfile,
    executable: &Path,
    arguments: &[OsString],
    current_directory: &Path,
    limits: JobLimits,
) -> io::Result<SandboxedProcess> {
    let executable_wide = wide_null(executable.as_os_str())?;
    let current_directory_wide = wide_null(current_directory.as_os_str())?;
    let mut command_line = command_line(executable.as_os_str(), arguments)?;
    let environment = sanitized_environment(current_directory)?;
    let job = WorkerJob::create(limits)?;
    let (child_stdin, parent_stdin) = pipe_pair(PipeDirection::ParentWrites)?;
    let (child_stdout, parent_stdout) = pipe_pair(PipeDirection::ParentReads)?;
    let (child_stderr, parent_stderr) = pipe_pair(PipeDirection::ParentReads)?;
    let inherited_handles: [HANDLE; 3] = [
        child_stdin.as_raw_handle().cast(),
        child_stdout.as_raw_handle().cast(),
        child_stderr.as_raw_handle().cast(),
    ];
    let mut capabilities = SECURITY_CAPABILITIES {
        AppContainerSid: profile.sid,
        Capabilities: std::ptr::null_mut(),
        CapabilityCount: 0,
        Reserved: 0,
    };
    let mut attribute_bytes = 0_usize;
    unsafe {
        InitializeProcThreadAttributeList(std::ptr::null_mut(), 2, 0, &raw mut attribute_bytes);
    }
    if attribute_bytes == 0 {
        return Err(io::Error::last_os_error());
    }
    let words = attribute_bytes.div_ceil(std::mem::size_of::<usize>());
    let mut attribute_storage = vec![0_usize; words];
    let attribute_list = attribute_storage.as_mut_ptr().cast();
    if unsafe { InitializeProcThreadAttributeList(attribute_list, 2, 0, &raw mut attribute_bytes) }
        == 0
    {
        return Err(io::Error::last_os_error());
    }
    let _attributes = AttributeList(attribute_list);
    update_attribute(
        attribute_list,
        PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES as usize,
        std::ptr::from_mut(&mut capabilities).cast(),
        std::mem::size_of_val(&capabilities),
    )?;
    update_attribute(
        attribute_list,
        PROC_THREAD_ATTRIBUTE_HANDLE_LIST as usize,
        inherited_handles.as_ptr().cast(),
        std::mem::size_of_val(&inherited_handles),
    )?;
    let mut startup = STARTUPINFOEXW::default();
    startup.StartupInfo.cb = u32::try_from(std::mem::size_of::<STARTUPINFOEXW>())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "startup info is too large"))?;
    startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
    startup.StartupInfo.hStdInput = inherited_handles[0];
    startup.StartupInfo.hStdOutput = inherited_handles[1];
    startup.StartupInfo.hStdError = inherited_handles[2];
    startup.lpAttributeList = attribute_list;
    let mut process_information = PROCESS_INFORMATION::default();
    let created = unsafe {
        CreateProcessW(
            executable_wide.as_ptr(),
            command_line.as_mut_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            1,
            EXTENDED_STARTUPINFO_PRESENT
                | CREATE_NO_WINDOW
                | CREATE_SUSPENDED
                | CREATE_UNICODE_ENVIRONMENT,
            environment.as_ptr().cast(),
            current_directory_wide.as_ptr(),
            std::ptr::from_ref(&startup.StartupInfo),
            &raw mut process_information,
        )
    };
    if created == 0 {
        return Err(io::Error::last_os_error());
    }
    let process = unsafe { OwnedHandle::from_raw_handle(process_information.hProcess.cast()) };
    let thread = unsafe { OwnedHandle::from_raw_handle(process_information.hThread.cast()) };
    assign_and_resume(&job, &process, &thread)?;
    drop(thread);
    drop(child_stdin);
    drop(child_stdout);
    drop(child_stderr);
    Ok(SandboxedProcess {
        process,
        job,
        profile: Some(profile),
        stdin: Some(File::from(parent_stdin)),
        stdout: Some(File::from(parent_stdout)),
        stderr: Some(File::from(parent_stderr)),
        exited: false,
        cancellation: CANCELLATION.with(|current| current.borrow().clone()),
    })
}

fn assign_and_resume(
    job: &WorkerJob,
    process: &OwnedHandle,
    thread: &OwnedHandle,
) -> io::Result<()> {
    if unsafe { AssignProcessToJobObject(job.handle, process.as_raw_handle().cast()) } == 0 {
        let error = io::Error::last_os_error();
        terminate_raw_process(process);
        return Err(error);
    }
    if unsafe { ResumeThread(thread.as_raw_handle().cast()) } == u32::MAX {
        let error = io::Error::last_os_error();
        terminate_raw_process(process);
        return Err(error);
    }
    Ok(())
}

fn terminate_raw_process(process: &OwnedHandle) {
    unsafe {
        TerminateProcess(process.as_raw_handle().cast(), 1);
        WaitForSingleObject(process.as_raw_handle().cast(), 5_000);
    }
}

fn sanitized_environment(current_directory: &Path) -> io::Result<Vec<u16>> {
    const ALLOWED_NAMES: &[&str] = &[
        "ALLUSERSPROFILE",
        "APPDATA",
        "LOCALAPPDATA",
        "ProgramData",
        "ProgramFiles",
        "ProgramFiles(x86)",
        "ProgramW6432",
        "PUBLIC",
        "SystemDrive",
        "SystemRoot",
        "USERPROFILE",
        "windir",
    ];
    let mut entries = ALLOWED_NAMES
        .iter()
        .filter_map(|name| std::env::var_os(name).map(|value| (OsString::from(name), value)))
        .collect::<Vec<_>>();
    if !entries
        .iter()
        .any(|(name, _)| name.eq_ignore_ascii_case("SystemRoot"))
    {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "Windows system directory is unavailable",
        ));
    }
    entries.push((
        OsString::from("TEMP"),
        current_directory.as_os_str().to_owned(),
    ));
    entries.push((
        OsString::from("TMP"),
        current_directory.as_os_str().to_owned(),
    ));
    entries.sort_by_key(|(name, _)| name.to_string_lossy().to_ascii_uppercase());
    let mut block = Vec::new();
    for (name, value) in entries {
        block.extend(name.as_os_str().encode_wide());
        block.push(u16::from(b'='));
        let value = value.as_os_str().encode_wide().collect::<Vec<_>>();
        if value.contains(&0) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Windows environment value contains an interior NUL",
            ));
        }
        block.extend(value);
        block.push(0);
    }
    block.push(0);
    Ok(block)
}

fn update_attribute(
    list: *mut core::ffi::c_void,
    attribute: usize,
    value: *const core::ffi::c_void,
    bytes: usize,
) -> io::Result<()> {
    if unsafe {
        UpdateProcThreadAttribute(
            list,
            0,
            attribute,
            value,
            bytes,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[derive(Clone, Copy)]
enum PipeDirection {
    ParentReads,
    ParentWrites,
}

fn pipe_pair(direction: PipeDirection) -> io::Result<(OwnedHandle, OwnedHandle)> {
    let mut read = std::ptr::null_mut();
    let mut write = std::ptr::null_mut();
    let attributes = SECURITY_ATTRIBUTES {
        nLength: u32::try_from(std::mem::size_of::<SECURITY_ATTRIBUTES>()).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "security attributes are too large",
            )
        })?,
        lpSecurityDescriptor: std::ptr::null_mut(),
        bInheritHandle: 1,
    };
    if unsafe { CreatePipe(&raw mut read, &raw mut write, &raw const attributes, 0) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let read = unsafe { OwnedHandle::from_raw_handle(read.cast()) };
    let write = unsafe { OwnedHandle::from_raw_handle(write.cast()) };
    let (child, parent) = match direction {
        PipeDirection::ParentReads => (write, read),
        PipeDirection::ParentWrites => (read, write),
    };
    if unsafe { SetHandleInformation(parent.as_raw_handle().cast(), HANDLE_FLAG_INHERIT, 0) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok((child, parent))
}

fn wide_null(value: &OsStr) -> io::Result<Vec<u16>> {
    let mut wide = value.encode_wide().collect::<Vec<_>>();
    if wide.contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Windows string contains an interior NUL",
        ));
    }
    wide.push(0);
    Ok(wide)
}

fn command_line(executable: &OsStr, arguments: &[OsString]) -> io::Result<Vec<u16>> {
    let mut line = Vec::new();
    quote_argument(&mut line, executable);
    for argument in arguments {
        line.push(u16::from(b' '));
        quote_argument(&mut line, argument);
    }
    if line.contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Windows command argument contains an interior NUL",
        ));
    }
    line.push(0);
    Ok(line)
}

fn quote_argument(line: &mut Vec<u16>, argument: &OsStr) {
    let units = argument.encode_wide().collect::<Vec<_>>();
    let requires_quotes = units.is_empty()
        || units
            .iter()
            .any(|unit| matches!(*unit, 0x20 | 0x09 | 0x0a | 0x0b | 0x0c | 0x0d | 0x22));
    if !requires_quotes {
        line.extend_from_slice(&units);
        return;
    }
    line.push(u16::from(b'"'));
    let mut backslashes = 0_usize;
    for unit in units {
        if unit == u16::from(b'\\') {
            backslashes += 1;
            continue;
        }
        if unit == u16::from(b'"') {
            line.extend(std::iter::repeat_n(u16::from(b'\\'), backslashes * 2 + 1));
        } else {
            line.extend(std::iter::repeat_n(u16::from(b'\\'), backslashes));
        }
        backslashes = 0;
        line.push(unit);
    }
    line.extend(std::iter::repeat_n(u16::from(b'\\'), backslashes * 2));
    line.push(u16::from(b'"'));
}

const fn hresult_from_win32(error: i32) -> i32 {
    if error <= 0 {
        error
    } else {
        (error & 0x0000_FFFF) | 0x8007_0000_u32.cast_signed()
    }
}

struct AttributeList(*mut core::ffi::c_void);

impl Drop for AttributeList {
    fn drop(&mut self) {
        unsafe { DeleteProcThreadAttributeList(self.0) }
    }
}

struct LocalAllocation(*mut core::ffi::c_void);

impl Drop for LocalAllocation {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { LocalFree(self.0) };
        }
    }
}

impl Drop for WorkerJob {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.handle);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{JobLimits, WorkerJob};
    use std::os::windows::process::CommandExt;
    use std::{process::Command, time::Duration};

    #[test]
    fn invalid_memory_limits_fail_before_creating_a_job() {
        let mut child = Command::new("cmd.exe")
            .args(["/c", "exit", "0"])
            .creation_flags(0x0800_0000) // CREATE_NO_WINDOW
            .spawn()
            .expect("spawn test child");
        let result = WorkerJob::assign(
            &child,
            JobLimits {
                process_memory_bytes: 2,
                job_memory_bytes: 1,
                cpu_time: Duration::from_secs(1),
            },
        );
        child.wait().expect("reap test child");
        assert!(matches!(result, Err(error) if error.kind() == std::io::ErrorKind::InvalidInput));
    }
}
