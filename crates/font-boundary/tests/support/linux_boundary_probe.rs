//! Controlled ELF fixture for the real private font protocol and process boundary.
use std::io::{Read, Write};

fn main() {
    let mut input = Vec::new();
    let mut chunk = [0; 1024];
    loop {
        let count = std::io::stdin().read(&mut chunk).unwrap();
        if count == 0 {
            break;
        }
        input.extend_from_slice(&chunk[..count]);
        if input
            .windows(b"\"expected_source_sha256\":\"early-close".len())
            .any(|bytes| bytes == b"\"expected_source_sha256\":\"early-close")
        {
            std::fs::write("early-close-proven", input.len().to_string()).unwrap();
            std::process::exit(7);
        }
    }
    let request: serde_json::Value = serde_json::from_slice(&input).unwrap();
    let mode = request["expected_source_sha256"].as_str().unwrap();
    match mode {
        "malformed" => print!("not-json"),
        "oversized" => std::io::stdout()
            .write_all(&vec![b'x'; 4 * 1024 * 1024 + 2])
            .unwrap(),
        "stderr" => {
            std::io::stderr()
                .write_all(&vec![b'x'; 256 * 1024])
                .unwrap();
            print!("not-json");
        }
        "crash" => std::process::abort(),
        "sleep" => {
            std::fs::write("pid", std::process::id().to_string()).unwrap();
            std::fs::write("partial", b"untrusted partial output").unwrap();
            std::thread::sleep(std::time::Duration::from_secs(30));
        }
        "denials" => {
            let source = request["font_path"].as_str().unwrap();
            assert!(std::fs::read(source).is_ok());
            assert!(std::fs::write(source, b"overwritten").is_err());
            assert!(std::fs::read("/etc/passwd").is_err());
            assert!(std::net::TcpStream::connect(("127.0.0.1", 1)).is_err());
            assert!(std::thread::Builder::new().spawn(|| ()).is_err());
            assert!(std::process::Command::new("/bin/true").spawn().is_err());
            std::fs::write("denials-proven", b"yes").unwrap();
            print!("not-json");
        }
        _ => std::process::exit(7),
    }
}
