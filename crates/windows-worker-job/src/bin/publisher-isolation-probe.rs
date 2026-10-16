#[cfg(not(windows))]
fn main() {}

#[cfg(windows)]
fn main() {
    use std::{
        fs,
        net::{Ipv4Addr, SocketAddr, SocketAddrV4, TcpStream},
        path::Path,
        time::Duration,
    };
    let arguments = std::env::args_os().skip(1).collect::<Vec<_>>();
    if arguments.len() == 1 {
        run_resource_probe(&arguments[0]);
        return;
    }
    if arguments.len() != 5 {
        std::process::exit(2);
    }
    let allowed_path = Path::new(&arguments[0]);
    let denied_path = Path::new(&arguments[1]);
    let port = arguments[2].to_string_lossy().parse::<u16>().unwrap_or(0);
    let report_path = Path::new(&arguments[3]);
    let profile_name = &arguments[4];

    let allowed_read = fs::read(allowed_path).is_ok_and(|bytes| bytes == b"allowed");
    let denied_read = fs::read(denied_path).is_err();
    let denied_write = fs::write(denied_path.with_extension("write"), b"blocked").is_err();
    let network_denied = TcpStream::connect_timeout(
        &SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, port)),
        Duration::from_millis(500),
    )
    .is_err();
    let (environment_sanitized, environment_keys) = environment_is_sanitized(profile_name);
    let appcontainer = process_is_appcontainer();

    let report_prefix = format!(
        "appcontainer={appcontainer};allowed_read={allowed_read};denied_read={denied_read};denied_write={denied_write};network_denied={network_denied}"
    );
    let report_suffix = format!(
        "environment_sanitized={environment_sanitized};environment_keys={environment_keys}"
    );
    if fs::write(
        report_path,
        format!("{report_prefix};child_denied=attempted;{report_suffix}"),
    )
    .is_err()
    {
        std::process::exit(3);
    }
    let child_denied = child_process_creation_is_denied();
    if fs::write(
        report_path,
        format!("{report_prefix};child_denied={child_denied};{report_suffix}"),
    )
    .is_err()
    {
        std::process::exit(3);
    }
    println!("pipe-ok");
    if ![
        appcontainer,
        allowed_read,
        denied_read,
        denied_write,
        network_denied,
        child_denied,
        environment_sanitized,
    ]
    .into_iter()
    .all(|passed| passed)
    {
        std::process::exit(1);
    }
}

// Test-only modes exercise the real Job limits without parsing hostile files or
// allocating more than 256 MiB even if the memory limit unexpectedly fails.
#[cfg(windows)]
fn run_resource_probe(mode: &std::ffi::OsStr) {
    use std::io::Write;
    println!("ready");
    std::io::stdout().flush().expect("flush readiness");
    match mode.to_str() {
        Some("memory") => {
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
        Some("cpu") => loop {
            std::hint::black_box(1_u64.wrapping_add(std::hint::black_box(2)));
        },
        Some("sleep") => std::thread::sleep(std::time::Duration::from_secs(30)),
        _ => std::process::exit(2),
    }
}

#[cfg(windows)]
fn child_process_creation_is_denied() -> bool {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::{
        Foundation::CloseHandle,
        System::Threading::{
            CREATE_NO_WINDOW, CreateProcessW, PROCESS_INFORMATION, STARTUPINFOW, TerminateProcess,
        },
    };

    let Ok(executable) = std::env::current_exe() else {
        return false;
    };
    let mut wide = executable.as_os_str().encode_wide().collect::<Vec<_>>();
    if wide.contains(&0) {
        return false;
    }
    wide.push(0);
    let mut startup = STARTUPINFOW {
        cb: u32::try_from(std::mem::size_of::<STARTUPINFOW>()).unwrap_or(0),
        ..STARTUPINFOW::default()
    };
    let mut information = PROCESS_INFORMATION::default();
    let created = unsafe {
        CreateProcessW(
            wide.as_ptr(),
            std::ptr::null_mut(),
            std::ptr::null(),
            std::ptr::null(),
            0,
            CREATE_NO_WINDOW,
            std::ptr::null(),
            std::ptr::null(),
            &raw mut startup,
            &raw mut information,
        )
    };
    if created == 0 {
        return true;
    }
    unsafe {
        TerminateProcess(information.hProcess, 1);
        CloseHandle(information.hThread);
        CloseHandle(information.hProcess);
    }
    false
}

#[cfg(windows)]
fn environment_is_sanitized(expected_profile_name: &std::ffi::OsStr) -> (bool, String) {
    let variables = std::env::vars_os().collect::<std::collections::HashMap<_, _>>();
    let keys = variables
        .keys()
        .map(|key| key.to_string_lossy().to_ascii_uppercase())
        .collect::<std::collections::HashSet<_>>();
    let allowed = [
        "ALLUSERSPROFILE",
        "APPDATA",
        "LOCALAPPDATA",
        "PROGRAMDATA",
        "PROGRAMFILES",
        "PROGRAMFILES(X86)",
        "PROGRAMW6432",
        "PUBLIC",
        "SYSTEMDRIVE",
        "SYSTEMROOT",
        "TEMP",
        "TMP",
        "USERPROFILE",
        "WINDIR",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect::<std::collections::HashSet<_>>();
    let mut ordered_keys = keys.iter().cloned().collect::<Vec<_>>();
    ordered_keys.sort_unstable();
    let primary_temporary_path = std::env::var_os("TEMP");
    let secondary_temporary_path = std::env::var_os("TMP");
    let expected_suffix = format!(
        "\\packages\\{}\\ac\\temp",
        expected_profile_name.to_string_lossy().to_ascii_lowercase()
    );
    let temporary_paths_match = primary_temporary_path.as_ref().is_some_and(|primary| {
        secondary_temporary_path.as_ref().is_some_and(|secondary| {
            primary == secondary
                && primary
                    .to_string_lossy()
                    .replace('/', "\\")
                    .to_ascii_lowercase()
                    .ends_with(&expected_suffix)
        })
    });
    let summary = format!(
        "{};expected_profile={};temp={};tmp={}",
        ordered_keys.join(","),
        expected_profile_name.to_string_lossy(),
        primary_temporary_path.as_deref().map_or_else(
            || "<missing>".into(),
            |value| value.to_string_lossy().into_owned(),
        ),
        secondary_temporary_path.as_deref().map_or_else(
            || "<missing>".into(),
            |value| value.to_string_lossy().into_owned(),
        )
    );
    let accepted = keys.is_subset(&allowed)
        && keys.contains("SYSTEMROOT")
        && keys.contains("TEMP")
        && keys.contains("TMP")
        && temporary_paths_match;
    (accepted, summary)
}

#[cfg(windows)]
fn process_is_appcontainer() -> bool {
    use windows_sys::Win32::{
        Foundation::CloseHandle,
        Security::{GetTokenInformation, TOKEN_QUERY, TokenIsAppContainer},
        System::Threading::{GetCurrentProcess, OpenProcessToken},
    };

    let mut token = std::ptr::null_mut();
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &raw mut token) } == 0 {
        return false;
    }
    let mut is_appcontainer = 0_u32;
    let mut returned = 0_u32;
    let queried = unsafe {
        GetTokenInformation(
            token,
            TokenIsAppContainer,
            std::ptr::from_mut(&mut is_appcontainer).cast(),
            u32::try_from(std::mem::size_of_val(&is_appcontainer)).unwrap_or(0),
            &raw mut returned,
        )
    };
    unsafe { CloseHandle(token) };
    queried != 0 && is_appcontainer != 0
}
