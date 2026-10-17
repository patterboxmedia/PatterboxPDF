#[cfg(not(all(target_os = "linux", target_arch = "x86_64")))]
fn main() {
    std::process::exit(2);
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
#[allow(clippy::too_many_lines, clippy::items_after_statements)] // Independent test modes and local helpers.
fn main() {
    use std::{
        ffi::OsString,
        fs,
        io::{BufRead, Write},
        os::fd::AsRawFd,
        path::Path,
        time::Duration,
    };
    let args = std::env::args_os().skip(1).collect::<Vec<_>>();
    let mode = args.first().and_then(|value| value.to_str()).unwrap_or("");
    match mode {
        "timestamps" => {
            use std::{ffi::CString, os::unix::ffi::OsStrExt};
            for path in &args[1..] {
                let path = CString::new(path.as_os_str().as_bytes()).unwrap();
                let times = [libc::timespec {
                    tv_sec: 2,
                    tv_nsec: 0,
                }; 2];
                assert_eq!(
                    unsafe { libc::utimensat(libc::AT_FDCWD, path.as_ptr(), times.as_ptr(), 0) },
                    -1
                );
                assert_eq!(
                    std::io::Error::last_os_error().raw_os_error(),
                    Some(libc::EPERM)
                );
            }
            println!("timestamps-denied");
        }
        "low-fd-host" => {
            use publisher_linux_worker_job::SandboxProfile;
            let soft = args[1].to_string_lossy().parse::<u64>().unwrap();
            let mut inherited = libc::rlimit {
                rlim_cur: 0,
                rlim_max: 0,
            };
            assert_eq!(
                unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &raw mut inherited) },
                0
            );
            inherited.rlim_cur = soft;
            assert_eq!(
                unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &raw const inherited) },
                0
            );
            let result = SandboxProfile::new().unwrap().spawn(
                &std::env::current_exe().unwrap(),
                &[OsString::from("ready-only")],
                Path::new(&args[2]),
                limits(),
            );
            if soft < 73 {
                let error = result
                    .err()
                    .expect("insufficient bootstrap budget must fail");
                assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
                assert!(error.to_string().contains("inherited descriptor limit"));
                println!("descriptor-budget-denied");
                return;
            }
            let mut worker = result.expect("launch within ordinary inherited descriptor budget");
            drop(worker.take_stdin());
            std::io::copy(&mut worker.take_stdout().unwrap(), &mut std::io::stdout()).unwrap();
            loop {
                if let Some(status) = worker.try_wait().unwrap() {
                    assert!(status.success());
                    break;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
        }
        "ready-only" => println!("ready"),
        "sleep" | "cpu" | "memory" | "flood" => {
            println!("ready");
            std::io::stdout().flush().unwrap();
            match mode {
                "sleep" => std::thread::sleep(Duration::from_secs(30)),
                "cpu" => loop {
                    std::hint::black_box(1_u64.wrapping_add(std::hint::black_box(2)));
                },
                "memory" => {
                    let mut retained = Vec::new();
                    for _ in 0..32 {
                        let mut block = Vec::<u8>::new();
                        if block.try_reserve_exact(8 * 1024 * 1024).is_err() {
                            println!("memory-denied");
                            return;
                        }
                        block.resize(8 * 1024 * 1024, 0x5a);
                        retained.push(block);
                        std::hint::black_box(&retained);
                    }
                    std::process::exit(4);
                }
                "flood" => {
                    let bytes = [0x5a_u8; 4096];
                    while std::io::stdout().write_all(&bytes).is_ok() {}
                }
                _ => unreachable!(),
            }
        }
        "marker" => {
            fs::write(&args[1], b"parsed").unwrap();
        }
        "read-denied" => {
            assert_denied(fs::read(&args[1]));
            assert_denied(fs::write(&args[2], b"changed"));
            println!("substitution-denied");
        }
        "isolation" => {
            assert_eq!(fs::read(&args[1]).unwrap(), b"allowed");
            for path in &args[2..5] {
                assert_denied(fs::read(path));
                assert_denied(fs::write(path, b"changed"));
            }
            let output = Path::new(&args[5]);
            fs::write(output, b"authorized").unwrap();
            fs::write("private-new-file", b"private").unwrap();
            let fd = args[6].to_string_lossy().parse::<i32>().unwrap();
            assert_eq!(unsafe { libc::fcntl(fd, libc::F_GETFD) }, -1);
            assert_eq!(
                std::io::Error::last_os_error().raw_os_error(),
                Some(libc::EBADF)
            );
            assert_eq!(
                unsafe { libc::socket(libc::AF_INET, libc::SOCK_STREAM, 0) },
                -1
            );
            assert_eq!(
                std::io::Error::last_os_error().raw_os_error(),
                Some(libc::EPERM)
            );
            assert_eq!(
                unsafe {
                    libc::socketpair(libc::AF_UNIX, libc::SOCK_STREAM, 0, [0_i32; 2].as_mut_ptr())
                },
                -1
            );
            assert_eq!(
                std::io::Error::last_os_error().raw_os_error(),
                Some(libc::EPERM)
            );
            assert!(std::net::TcpStream::connect(("127.0.0.1", 1)).is_err());
            assert_eq!(unsafe { libc::fork() }, -1);
            assert_eq!(
                std::io::Error::last_os_error().raw_os_error(),
                Some(libc::EPERM)
            );
            assert!(std::process::Command::new("/bin/true").spawn().is_err());
            assert!(std::thread::Builder::new().spawn(|| ()).is_err());
            assert_eq!(
                unsafe { libc::prctl(libc::PR_SET_PDEATHSIG, 0, 0, 0, 0) },
                -1
            );
            assert_eq!(
                unsafe { libc::fcntl(1, libc::F_SETOWN, libc::getppid()) },
                -1
            );
            let exec_arguments = [c"/bin/true".as_ptr(), std::ptr::null()];
            let env = [std::ptr::null::<libc::c_char>()];
            assert_eq!(
                unsafe {
                    libc::execve(c"/bin/true".as_ptr(), exec_arguments.as_ptr(), env.as_ptr())
                },
                -1
            );
            assert_eq!(
                std::io::Error::last_os_error().raw_os_error(),
                Some(libc::EPERM)
            );
            assert_eq!(
                unsafe {
                    libc::syscall(
                        libc::SYS_execveat,
                        64,
                        c"".as_ptr(),
                        exec_arguments.as_ptr(),
                        env.as_ptr(),
                        libc::AT_EMPTY_PATH,
                    )
                },
                -1
            );
            assert_eq!(
                std::io::Error::last_os_error().raw_os_error(),
                Some(libc::EBADF)
            );
            let authorized = fs::File::open(&args[1]).unwrap();
            assert_eq!(unsafe { libc::dup2(authorized.as_raw_fd(), 64) }, -1);
            assert_eq!(
                std::io::Error::last_os_error().raw_os_error(),
                Some(libc::EBADF)
            );
            assert_eq!(unsafe { libc::memfd_create(c"escape".as_ptr(), 0) }, -1);
            assert_eq!(
                std::io::Error::last_os_error().raw_os_error(),
                Some(libc::EPERM)
            );
            assert_eq!(
                unsafe { libc::syscall(libc::SYS_io_uring_setup, 1, std::ptr::null::<u8>()) },
                -1
            );
            let mut variables = std::env::vars_os().map(|(key, _)| key).collect::<Vec<_>>();
            variables.sort();
            assert_eq!(
                variables,
                [OsString::from("LANG"), OsString::from("TMPDIR")]
            );
            println!(
                "files-denied;socket-denied;process-denied;thread-denied;fds-closed;exec-denied;environment-sanitized"
            );
        }
        "host" => {
            use publisher_linux_worker_job::SandboxProfile;
            let cwd = Path::new(&args[1]);
            let profile = SandboxProfile::new().unwrap();
            let mut worker = profile
                .spawn(
                    &std::env::current_exe().unwrap(),
                    &[OsString::from("sleep")],
                    cwd,
                    limits(),
                )
                .unwrap();
            drop(worker.take_stdin());
            let mut output = std::io::BufReader::new(worker.take_stdout().unwrap());
            let mut line = String::new();
            output.read_line(&mut line).unwrap();
            assert_eq!(line.trim(), "ready");
            println!("owner-ready {}", worker.id());
            std::io::stdout().flush().unwrap();
            std::io::copy(&mut output, &mut std::io::stdout()).unwrap();
        }
        "architecture" => {
            assert_eq!(unsafe { libc::syscall(0x4000_0027) }, -1);
            assert_eq!(
                std::io::Error::last_os_error().raw_os_error(),
                Some(libc::EPERM)
            );
            // Enter i386 compat ABI. The architecture guard must kill rather
            // than interpreting these numbers as native x86_64 calls.
            unsafe {
                std::arch::asm!("int 0x80", inout("eax") 20_i32 => _, options(nostack));
            }
            std::process::exit(5);
        }
        "setup-failure" => {
            use publisher_linux_worker_job::SandboxProfile;
            let syscall = args[1].to_string_lossy().parse::<u32>().unwrap();
            // Test process only: impose a real inherited kernel denial, then
            // invoke the unchanged production launcher. No injected backend.
            deny_syscall(syscall);
            let result = SandboxProfile::new().and_then(|profile| {
                profile.grant_read_write(Path::new(&args[2]))?;
                profile.spawn(
                    &std::env::current_exe()?,
                    &[OsString::from("marker"), args[2].clone()],
                    Path::new(&args[3]),
                    limits(),
                )
            });
            assert!(
                result.is_err(),
                "missing required primitive must fail setup"
            );
            println!("setup-denied");
        }
        _ => std::process::exit(2),
    }

    fn assert_denied<T>(result: std::io::Result<T>) {
        assert_eq!(
            result
                .err()
                .expect("operation unexpectedly allowed")
                .raw_os_error(),
            Some(libc::EACCES)
        );
    }
    fn limits() -> publisher_linux_worker_job::JobLimits {
        publisher_linux_worker_job::JobLimits {
            process_memory_bytes: 128 * 1024 * 1024,
            job_memory_bytes: 128 * 1024 * 1024,
            cpu_time: Duration::from_secs(3),
            wall_time: Duration::from_secs(20),
        }
    }
    fn deny_syscall(syscall: u32) {
        let mut filter = [
            libc::sock_filter {
                code: 0x20,
                jt: 0,
                jf: 0,
                k: 0,
            },
            libc::sock_filter {
                code: 0x15,
                jt: 0,
                jf: 1,
                k: syscall,
            },
            libc::sock_filter {
                code: 0x06,
                jt: 0,
                jf: 0,
                k: 0x0005_0000 | libc::EPERM as u32,
            },
            libc::sock_filter {
                code: 0x06,
                jt: 0,
                jf: 0,
                k: 0x7fff_0000,
            },
        ];
        let program = libc::sock_fprog {
            len: u16::try_from(filter.len()).unwrap(),
            filter: filter.as_mut_ptr(),
        };
        assert_eq!(
            unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) },
            0
        );
        assert_eq!(
            unsafe { libc::syscall(libc::SYS_seccomp, 1, 0, &raw const program) },
            0
        );
    }
}
