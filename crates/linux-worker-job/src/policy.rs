//! Kernel policy definitions. No allocations or locks occur after fork.
use std::{
    fs::File,
    io,
    os::fd::{AsRawFd, FromRawFd},
    path::Path,
};

pub const OPEN_FILES: u64 = 64;
// CLOEXEC reserves sit above the final hard limit, so no worker can reuse them.
pub const EXEC_FD: i32 = 64;
pub const ERROR_FD: i32 = 65;
pub const BOOTSTRAP_FD: i32 = 66;
pub const POLICY_ID: &str = "linux-x86_64-landlock3-seccomp-allowlist-v1";
pub const READ: u64 = (1 << 2) | (1 << 3);
pub const READ_EXECUTE: u64 = READ | 1;
pub const READ_WRITE: u64 = READ | (1 << 1) | (1 << 14);
// Regular files/directories and removal only. Device/FIFO/socket creation and
// cross-directory reparenting are not needed by these workers.
pub const PRIVATE: u64 = READ_WRITE | (1 << 4) | (1 << 5) | (1 << 7) | (1 << 8);
const HANDLED: u64 = (1 << 15) - 1;
const FILE_ACCESS: u64 = 1 | (1 << 1) | (1 << 2) | (1 << 14);

pub struct Grant {
    pub anchor: File,
    pub access: u64,
}

#[repr(C, packed)]
struct PathRule {
    allowed: u64,
    parent_fd: i32,
}

pub fn abi() -> io::Result<u32> {
    let result = unsafe {
        libc::syscall(
            libc::SYS_landlock_create_ruleset,
            std::ptr::null::<u8>(),
            0,
            1,
        )
    };
    if result < 3 {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            format!(
                "Landlock ABI >= 3 is required (kernel returned {result}: {})",
                io::Error::last_os_error()
            ),
        ));
    }
    Ok(u32::try_from(result).expect("Landlock ABI fits u32"))
}

pub fn anchor(path: &Path) -> io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_PATH | libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(path)?;
    let kind = file.metadata()?.file_type();
    if !kind.is_file() && !kind.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "grant must name a regular file or directory, never a symlink/device",
        ));
    }
    Ok(file)
}

pub fn ruleset(grants: &[Grant]) -> io::Result<File> {
    abi()?;
    let access = HANDLED;
    let fd = unsafe {
        libc::syscall(
            libc::SYS_landlock_create_ruleset,
            &raw const access,
            size_of::<u64>(),
            0,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    let result = unsafe { File::from_raw_fd(i32::try_from(fd).expect("file descriptor")) };
    for grant in grants {
        let access = if grant.anchor.metadata()?.is_file() {
            grant.access & FILE_ACCESS
        } else {
            grant.access
        };
        let rule = PathRule {
            allowed: access,
            parent_fd: grant.anchor.as_raw_fd(),
        };
        if unsafe {
            libc::syscall(
                libc::SYS_landlock_add_rule,
                result.as_raw_fd(),
                1,
                &raw const rule,
                0,
            )
        } < 0
        {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(result)
}

fn stmt(code: u16, k: u32) -> libc::sock_filter {
    libc::sock_filter {
        code,
        jt: 0,
        jf: 0,
        k,
    }
}
fn jump(k: u32, jt: u8, jf: u8) -> libc::sock_filter {
    libc::sock_filter {
        code: 0x15,
        jt,
        jf,
        k,
    }
}
const ALLOW: u32 = 0x7fff_0000;
const KILL: u32 = 0x8000_0000;
const EPERM: u32 = 0x0005_0000 | libc::EPERM as u32;

/// Only named `x86_64` syscalls are admitted. Unknown/new syscalls fail closed;
/// x32 syscall numbers and a changed audit architecture cannot bypass the list.
pub fn filter() -> Vec<libc::sock_filter> {
    let mut code = vec![
        stmt(0x20, 4),
        jump(0xc000_003e, 1, 0),
        stmt(0x06, KILL),
        stmt(0x20, 0),
    ];
    // Initial execveat uses an existing sealed FD above the *hard* NOFILE
    // limit. CLOEXEC removes it. No later open/dup can recreate that FD, and
    // no allowed syscall can raise the hard limit. This avoids an FD-reuse
    // escape while applying the complete filter before the initial exec.
    code.push(jump(322, 0, 8));
    code.extend([
        stmt(0x20, 16),
        jump(EXEC_FD as u32, 0, 5),
        stmt(0x20, 20),
        jump(0, 0, 3),
        stmt(0x20, 48),
        jump(libc::AT_EMPTY_PATH as u32, 0, 1),
        stmt(0x06, ALLOW),
        stmt(0x06, EPERM),
    ]);
    code.push(stmt(0x20, 0));
    // fcntl may duplicate descriptors or adjust normal I/O flags. F_SETOWN /
    // F_SETSIG would let pipe-driven SIGIO target another host process, so no
    // asynchronous-I/O ownership commands are admitted.
    code.push(jump(72, 0, 12));
    code.extend([
        stmt(0x20, 24),
        jump(libc::F_DUPFD as u32, 9, 0),
        jump(libc::F_DUPFD_CLOEXEC as u32, 8, 0),
        jump(libc::F_GETFD as u32, 7, 0),
        jump(libc::F_SETFD as u32, 6, 0),
        jump(libc::F_GETFL as u32, 5, 0),
        jump(libc::F_SETFL as u32, 0, 3),
        stmt(0x20, 32),
        stmt(0x54, libc::O_ASYNC as u32),
        jump(0, 1, 0),
        stmt(0x06, EPERM),
        stmt(0x06, ALLOW),
    ]);
    code.push(stmt(0x20, 0));
    // prlimit64 is query-only (glibc startup); its new-limit argument is NULL.
    code.push(jump(302, 0, 6));
    code.extend([
        stmt(0x20, 32),
        jump(0, 0, 3),
        stmt(0x20, 36),
        jump(0, 0, 1),
        stmt(0x06, ALLOW),
        stmt(0x06, EPERM),
    ]);
    code.push(stmt(0x20, 0));
    // tgkill is needed for Rust abort/pthread signals; constrain its target
    // group below in the child. The placeholder is replaced before fork.
    code.push(jump(234, 0, 6));
    code.extend([
        stmt(0x20, 16),
        jump(u32::MAX, 0, 3),
        stmt(0x20, 20),
        jump(0, 0, 1),
        stmt(0x06, ALLOW),
        stmt(0x06, EPERM),
    ]);
    code.push(stmt(0x20, 0));
    // Read/write and memory operations, loader/runtime bookkeeping, harmless
    // metadata and Landlock-controlled regular-file/directory operations.
    // Not admitted: sockets, io_uring, ptrace/process_vm, bpf, namespaces,
    // mounts, keyrings, perf, sendfile/splice, chmod/chown/xattr, prctl,
    // execve, fork/vfork, resource-limit changes or signal-other-process.
    let allowed = [
        0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 17, 18, 19, 20, 21, 23, 24, 25, 26,
        27, 28, 32, 33, 35, 39, 60, 63, 74, 75, 76, 77, 78, 79, 80, 81, 82, 83, 84, 85, 86, 87, 89,
        95, 96, 97, 98, 99, 100, 102, 104, 107, 108, 110, 111, 131, 137, 138, 158, 186, 201, 202,
        204, 218, 219, 228, 229, 230, 231, 232, 233, 257, 258, 262, 263, 264, 265, 267, 269, 270,
        271, 273, 281, 291, 292, 293, 295, 296, 318, 324, 332, 334, 436, 439, 441,
    ];
    for nr in allowed {
        code.extend([jump(nr, 0, 1), stmt(0x06, ALLOW)]);
    }
    code.push(stmt(0x06, EPERM));
    code
}

pub fn set_tgid(code: &mut [libc::sock_filter], tgid: u32) {
    for instruction in code {
        if instruction.code == 0x15 && instruction.k == u32::MAX {
            instruction.k = tgid;
        }
    }
}
