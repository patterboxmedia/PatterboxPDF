#![allow(dead_code)]

#[path = "../src/main.rs"]
mod worker;

use std::fmt::Write as _;
use std::io::Write;
#[cfg(windows)]
use std::os::windows::process::CommandExt;
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

const WORKER: &str = env!("CARGO_BIN_EXE_publisher-font-worker");
const PROTOCOL_VERSION: u16 = 9;
const GLYPH_SET_DOMAIN: &[u8] = b"publisher-v2/font-glyph-set/v1\0";
const FONT: &[u8] = include_bytes!("../../../crates/layout-core/tests/fixtures/DejaVuSans.ttf");

static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

struct ProcessFixture {
    root: std::path::PathBuf,
}

impl ProcessFixture {
    fn create() -> Self {
        let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "publisher-font-worker-process-test-{}-{sequence}",
            std::process::id()
        ));
        std::fs::create_dir(&root).expect("create isolated worker-process directory");
        Self { root }
    }

    fn path(&self, name: &str) -> std::path::PathBuf {
        self.root.join(name)
    }
}

impl Drop for ProcessFixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn run_worker(input: &[u8]) -> Output {
    // Cross-compiled Cargo embeds the build host's path. The Windows evidence
    // runner stages the matching worker beside this executable instead.
    let staged_worker = std::env::current_exe()
        .expect("locate worker process test")
        .with_file_name(format!(
            "publisher-font-worker{}",
            std::env::consts::EXE_SUFFIX
        ));
    let worker = if staged_worker.is_file() {
        staged_worker
    } else {
        std::path::PathBuf::from(WORKER)
    };
    let mut command = Command::new(worker);
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    let mut child = command.spawn().expect("spawn packaged font worker");
    let mut stdin = child.stdin.take().expect("worker stdin is piped");
    stdin.write_all(input).expect("write worker request");
    drop(stdin);
    child.wait_with_output().expect("collect worker result")
}

fn assert_failure(output: &Output, error: &str) -> Value {
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stderr.is_empty(), "worker stderr must remain empty");
    assert!(
        output.stdout.len() <= 2_048,
        "failure JSON must remain bounded"
    );
    assert_eq!(output.stdout.first(), Some(&b'{'));
    assert_eq!(output.stdout.last(), Some(&b'}'));
    let value: Value = serde_json::from_slice(&output.stdout).expect("failure is one JSON value");
    let object = value.as_object().expect("failure is a JSON object");
    assert_eq!(
        object.len(),
        3,
        "failure has only protocol, code, and detail"
    );
    assert_eq!(value["protocol_version"], PROTOCOL_VERSION);
    assert_eq!(value["error"], error);
    assert!(value["detail"].is_string());
    value
}

fn hex_sha256(bytes: &[u8]) -> String {
    digest_to_hex(&Sha256::digest(bytes))
}

fn glyph_set_hash(glyph_ids: &[u16]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(GLYPH_SET_DOMAIN);
    hasher.update(
        u32::try_from(glyph_ids.len())
            .expect("test glyph set length fits u32")
            .to_be_bytes(),
    );
    for glyph_id in glyph_ids {
        hasher.update(glyph_id.to_be_bytes());
    }
    digest_to_hex(&hasher.finalize())
}

fn digest_to_hex(bytes: &[u8]) -> String {
    let mut digest = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        write!(&mut digest, "{byte:02x}").expect("writing to a String cannot fail");
    }
    digest
}

#[test]
fn empty_stdin_returns_one_bounded_structured_failure() {
    let output = run_worker(&[]);

    let value = assert_failure(&output, "no-request");
    assert_eq!(value["detail"], "NoRequest");
}

#[test]
fn unsupported_protocol_fails_before_attempting_to_open_the_source() {
    let input = serde_json::to_vec(&json!({
        "operation": "prepare",
        "protocol_version": PROTOCOL_VERSION + 1,
        "font_path": "this source must never be opened",
        "expected_source_sha256": "00".repeat(32),
        "prepared_output_path": "this output must never be opened",
        "preparation_policy": "subset-only",
        "face_index": 0,
        "glyph_ids": [36],
        "expected_glyph_set_sha256": "00".repeat(32)
    }))
    .expect("serialize unsupported protocol request");

    let value = assert_failure(&run_worker(&input), "unsupported-protocol");
    assert_eq!(value["detail"], "UnsupportedProtocol");
}

#[test]
fn malformed_json_returns_one_bounded_structured_failure() {
    let value = assert_failure(&run_worker(b"{not-json"), "invalid-request");

    let detail = value["detail"].as_str().expect("failure detail is text");
    assert!(!detail.is_empty());
    assert!(detail.len() <= 1_024);
}

#[test]
fn existing_output_file_fails_after_processing_without_overwriting_it() {
    let fixture = ProcessFixture::create();
    let font_path = fixture.path("source.font");
    let output_path = fixture.path("prepared.font");
    std::fs::write(&font_path, FONT).expect("write exact source font");
    std::fs::write(&output_path, b"keep me").expect("write output sentinel");
    let glyph_ids = [0_u16, 36];
    let input = serde_json::to_vec(&json!({
        "operation": "prepare",
        "protocol_version": PROTOCOL_VERSION,
        "font_path": font_path,
        "expected_source_sha256": hex_sha256(FONT),
        "prepared_output_path": output_path,
        "preparation_policy": "subset-only",
        "face_index": 0,
        "glyph_ids": glyph_ids,
        "expected_glyph_set_sha256": glyph_set_hash(&glyph_ids)
    }))
    .expect("serialize exact worker request");

    let value = assert_failure(&run_worker(&input), "output-unavailable");
    assert!(!value["detail"].as_str().unwrap_or_default().is_empty());
    assert_eq!(
        std::fs::read(fixture.path("prepared.font")).expect("read untouched sentinel"),
        b"keep me"
    );
}

#[test]
fn inspect_process_returns_bounded_path_free_face_facts_without_an_output_file() {
    let fixture = ProcessFixture::create();
    let font_path = fixture.path("source.font");
    std::fs::write(&font_path, FONT).expect("write exact source font");
    let input = serde_json::to_vec(&json!({
        "operation": "inspect",
        "protocol_version": PROTOCOL_VERSION,
        "font_path": font_path,
        "expected_source_sha256": hex_sha256(FONT),
        "face_index": 0
    }))
    .expect("serialize inspection request");

    let output = run_worker(&input);

    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    assert!(output.stdout.len() <= 4 * 1024 * 1024);
    let value: Value = serde_json::from_slice(&output.stdout).expect("inspection report is JSON");
    assert_eq!(value["operation"], "inspect");
    let report = &value["report"];
    assert_eq!(report["protocol_version"], PROTOCOL_VERSION);
    assert_eq!(report["source_sha256"], hex_sha256(FONT));
    assert_eq!(report["source_byte_length"], FONT.len());
    assert_eq!(report["face_index"], 0);
    assert_eq!(report["container_kind"], "single-font");
    assert_eq!(report["face_count"], 1);
    assert_eq!(report["family_names"], serde_json::json!(["DejaVu Sans"]));
    assert_eq!(report["weight"], 400);
    assert_eq!(report["style"], "normal");
    assert_eq!(report["italic"], false);
    assert!(report.get("font_path").is_none());
    assert!(report.get("prepared_output_path").is_none());
    let rendered = String::from_utf8(output.stdout).expect("inspection report is UTF-8");
    assert!(!rendered.contains(&fixture.root.display().to_string()));
    assert!(!fixture.path("prepared.font").exists());
}

#[test]
fn inspect_process_rejects_malformed_font_with_path_free_structured_failure() {
    let fixture = ProcessFixture::create();
    let font_path = fixture.path("private-source.font");
    let malformed = b"not a font";
    std::fs::write(&font_path, malformed).expect("write malformed font");
    let input = serde_json::to_vec(&json!({
        "operation": "inspect",
        "protocol_version": PROTOCOL_VERSION,
        "font_path": font_path,
        "expected_source_sha256": hex_sha256(malformed),
        "face_index": 0
    }))
    .expect("serialize inspection request");

    let value = assert_failure(&run_worker(&input), "malformed-font");

    let rendered = serde_json::to_string(&value).expect("serialize failure");
    assert!(!rendered.contains(&fixture.root.display().to_string()));
    assert!(!rendered.contains("private-source.font"));
}
