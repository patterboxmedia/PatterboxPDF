//! Real command checks require the packaged Windows color/font/raster workers.
#![cfg(windows)]

use document_core::model::{
    Color, FontFaceBinding, ImageContentKind, ImageCrop, ImageFlip, Insets, ObjectDraft,
    ObjectKind, PhysicalSize, ShapeKind, ShapeStroke, TextFrame, TextStoryFormatting, TextStyle,
    Transform, WrapMode,
};
use document_core::{
    AssetStore, Command, Document, DocumentId, DocumentSession, ObjectLocation, ProjectSnapshot,
    Revision, Transaction,
};
use publisher_pdf::{ExportMode, ExportRequest, generate_pdf};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command as Process, Output};
use std::sync::atomic::{AtomicUsize, Ordering};

const PROFILE: &[u8] =
    include_bytes!("../../../crates/print-core/tests/fixtures/SWOP2006coated_300_mth.icc");
const FONT: &[u8] = include_bytes!("../../../crates/layout-core/tests/fixtures/DejaVuSans.ttf");
static NEXT: AtomicUsize = AtomicUsize::new(0);

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "publisher-pdf-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        fs::write(root.join("profile.icc"), PROFILE).unwrap();
        Self(root)
    }

    fn source(&self, kind: ObjectKind, assets: &AssetStore, size: PhysicalSize, x: i64) {
        let document = Document::new(
            DocumentId::parse("pdf-cli-test").unwrap(),
            "Saved publication for PDF command",
            PhysicalSize::new(100_000, 150_000).unwrap(),
            Insets::uniform(3_000).unwrap(),
        )
        .unwrap();
        let page = document.pages[0].id.clone();
        let mut session = DocumentSession::new(document).unwrap();
        session
            .execute(
                Revision::initial(),
                Transaction {
                    label: "Add content and a second page".into(),
                    commands: vec![
                        Command::AddObject {
                            location: ObjectLocation::Page(page.clone()),
                            object: ObjectDraft {
                                name: "Test content".into(),
                                size,
                                transform: Transform::identity_at(x, 10_000),
                                opacity_millionths: 500_000,
                                kind,
                            },
                        },
                        Command::AddPage {
                            after: Some(page),
                            trim: PhysicalSize::new(120_000, 160_000).unwrap(),
                            bleed: Insets::uniform(4_000).unwrap(),
                        },
                        Command::AddObject {
                            location: ObjectLocation::Pasteboard,
                            object: ObjectDraft {
                                name: "Excluded Pasteboard shape".into(),
                                size: PhysicalSize::new(15_000, 10_000).unwrap(),
                                transform: Transform::identity_at(5_000, 5_000),
                                opacity_millionths: 1_000_000,
                                kind: ObjectKind::Shape {
                                    shape: ShapeKind::Ellipse,
                                    fill: Color {
                                        red: 0,
                                        green: 0,
                                        blue: 0,
                                        alpha: 255,
                                    },
                                    stroke: ShapeStroke::default(),
                                    wrap_mode: WrapMode::None,
                                },
                            },
                        },
                    ],
                },
            )
            .unwrap();
        document_core::save_new_package(
            &self.0.join("source.publisher-v2"),
            &ProjectSnapshot::from_session(&session),
            assets,
        )
        .unwrap();
    }

    fn geometry(&self, x: i64) {
        self.source(
            ObjectKind::Shape {
                shape: ShapeKind::Rectangle,
                fill: Color {
                    red: 25,
                    green: 90,
                    blue: 180,
                    alpha: 220,
                },
                stroke: ShapeStroke::default(),
                wrap_mode: WrapMode::None,
            },
            &AssetStore::default(),
            PhysicalSize::new(30_000, 20_000).unwrap(),
            x,
        );
    }

    fn request(&self, destination: &str, mode: ExportMode) -> ExportRequest {
        ExportRequest {
            input: self.0.join("source.publisher-v2"),
            output: self.0.join(destination),
            profile: self.0.join("profile.icc"),
            mode,
            allow_warnings: false,
            metadata_utc: None,
        }
    }

    fn cli(&self, destination: &str, mode: &str, extra: &[&str]) -> Output {
        Process::new(binary())
            .arg("--input")
            .arg(self.0.join("source.publisher-v2"))
            .arg("--output")
            .arg(self.0.join(destination))
            .arg("--profile")
            .arg(self.0.join("profile.icc"))
            .arg("--mode")
            .arg(mode)
            .args(extra)
            .output()
            .unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn read_receipt(directory: &Path) -> Value {
    serde_json::from_slice(&fs::read(directory.join("receipt.json")).unwrap()).unwrap()
}

fn binary() -> PathBuf {
    let executable = std::env::current_exe().unwrap();
    let directory = executable.parent().unwrap();
    // A staged Windows test bundle keeps workers and the CLI beside the test.
    // Native cargo test normally puts test executables one level down in deps.
    let sibling = directory.join("publisher-pdf.exe");
    if sibling.is_file() {
        sibling
    } else {
        directory.parent().unwrap().join("publisher-pdf.exe")
    }
}

#[test]
fn separate_processes_generate_both_modes_deterministically_and_preserve_source() {
    let fixture = Fixture::new();
    fixture.geometry(10_000);
    let source = fs::read(fixture.0.join("source.publisher-v2")).unwrap();
    for mode in ["pdf", "pdfx4-candidate"] {
        let first = format!("{mode}-first");
        let second = format!("{mode}-second");
        for name in [&first, &second] {
            let output = fixture.cli(name, mode, &["--metadata-utc", "2026-10-03T00:00:00Z"]);
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            let receipt = read_receipt(&fixture.0.join(name));
            assert_eq!(
                serde_json::from_slice::<Value>(&output.stdout).unwrap(),
                receipt
            );
            let pdf = fs::read(fixture.0.join(name).join("publication.pdf")).unwrap();
            assert_eq!(receipt["source"]["sha256"], sha256(&source));
            assert_eq!(receipt["profile"]["sha256"], sha256(PROFILE));
            assert_eq!(receipt["pdf"]["sha256"], sha256(&pdf));
            assert_eq!(receipt["mode"], mode);
            assert_eq!(receipt["independent_verification"], "pending");
            assert_eq!(receipt["pasteboard_object_count_excluded"], 1);
            assert_eq!(receipt["pages"].as_array().unwrap().len(), 2);
            assert_eq!(receipt["pages"][0]["trim_box_um"]["width"], 100_000);
            assert_eq!(receipt["pages"][1]["trim_box_um"]["width"], 120_000);
            assert_eq!(
                receipt["pages"][0]["object_ids_in_paint_order"]
                    .as_array()
                    .unwrap()
                    .len(),
                1
            );
            assert_eq!(receipt["transparency"]["object_opacity"], true);
            assert!(receipt["warnings"].as_array().unwrap().is_empty());
            assert_eq!(
                receipt["resources"]["vector_colors"]["worker_executable_sha256"]
                    .as_str()
                    .unwrap()
                    .len(),
                64
            );
            assert_eq!(
                String::from_utf8_lossy(&pdf).contains("GTS_PDFXVersion"),
                mode == "pdfx4-candidate"
            );
        }
        assert_eq!(
            fs::read(fixture.0.join(first).join("publication.pdf")).unwrap(),
            fs::read(fixture.0.join(second).join("publication.pdf")).unwrap()
        );
    }
    assert_eq!(
        fs::read(fixture.0.join("source.publisher-v2")).unwrap(),
        source
    );
}

#[test]
fn existing_destinations_and_source_collisions_preserve_every_byte() {
    let fixture = Fixture::new();
    fixture.geometry(10_000);
    let original = fs::read(fixture.0.join("source.publisher-v2")).unwrap();
    fs::create_dir(fixture.0.join("existing")).unwrap();
    fs::write(
        fixture.0.join("existing/publication.pdf"),
        b"keep existing PDF",
    )
    .unwrap();
    fs::write(
        fixture.0.join("existing/receipt.json"),
        b"keep existing receipt",
    )
    .unwrap();
    fs::write(fixture.0.join("existing-file"), b"keep file").unwrap();
    for destination in [
        "existing",
        "existing-file",
        "source.publisher-v2",
        "profile.icc",
    ] {
        let error = generate_pdf(&fixture.request(destination, ExportMode::Pdf)).unwrap_err();
        assert_eq!(error.code(), "destination_exists");
    }
    assert_eq!(
        fs::read(fixture.0.join("source.publisher-v2")).unwrap(),
        original
    );
    assert_eq!(fs::read(fixture.0.join("profile.icc")).unwrap(), PROFILE);
    assert_eq!(
        fs::read(fixture.0.join("existing/publication.pdf")).unwrap(),
        b"keep existing PDF"
    );
    assert_eq!(
        fs::read(fixture.0.join("existing/receipt.json")).unwrap(),
        b"keep existing receipt"
    );
    assert_eq!(
        fs::read(fixture.0.join("existing-file")).unwrap(),
        b"keep file"
    );
}

#[test]
fn invalid_cli_and_profiles_create_no_output() {
    let fixture = Fixture::new();
    fixture.geometry(10_000);
    for timestamp in ["not-a-date", "2026-02-30T00:00:00Z"] {
        let output = fixture.cli(
            "invalid-date",
            "pdfx4-candidate",
            &["--metadata-utc", timestamp],
        );
        assert!(!output.status.success());
        assert!(!fixture.0.join("invalid-date").exists());
    }
    for extra in [vec![], vec!["--mode", "pdf"], vec!["--unknown"]] {
        let output = fixture.cli("invalid-mode", "pdfx4-certified", &extra);
        assert!(!output.status.success());
        assert!(!fixture.0.join("invalid-mode").exists());
        assert!(String::from_utf8_lossy(&output.stderr).contains("pdfx4-candidate"));
    }
    for (name, profile) in [
        ("missing", None),
        ("invalid", Some(b"invalid ICC".as_slice())),
    ] {
        let mut request = fixture.request(name, ExportMode::Pdf);
        request.profile = fixture.0.join(format!("{name}.icc"));
        if let Some(bytes) = profile {
            fs::write(&request.profile, bytes).unwrap();
        }
        let error = generate_pdf(&request).unwrap_err();
        assert!(error.to_string().contains("profile"));
        assert!(!request.output.exists());
    }
    let output = Process::new(binary())
        .args(["--input", "source.publisher-v2"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    for flag in ["--help", "--version"] {
        assert!(
            Process::new(binary())
                .arg(flag)
                .output()
                .unwrap()
                .status
                .success()
        );
    }
}

#[test]
fn warnings_require_explicit_acceptance_and_remain_in_receipt() {
    let fixture = Fixture::new();
    fixture.geometry(0);
    let output = fixture.cli("refused", "pdfx4-candidate", &[]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("--allow-warnings"));
    assert!(!fixture.0.join("refused").exists());
    let output = fixture.cli("accepted", "pdfx4-candidate", &["--allow-warnings"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let receipt = read_receipt(&fixture.0.join("accepted"));
    assert_eq!(receipt["warnings_accepted"], true);
    assert_eq!(receipt["warnings"][0]["code"], "PossibleBleedMissing");
    assert!(receipt["warnings"][0]["target"]["page_id"].is_string());
    assert_eq!(receipt["independent_verification"], "pending");
}

fn text(style: TextStyle, content: &str) -> ObjectKind {
    ObjectKind::Text {
        text: content.into(),
        style,
        frame: TextFrame::default(),
        story: TextStoryFormatting::default(),
    }
}

#[test]
fn unbound_fonts_overflow_and_unprepared_pictures_never_complete_a_bundle() {
    let unbound = Fixture::new();
    unbound.source(
        text(TextStyle::default(), "Exact font required"),
        &AssetStore::default(),
        PhysicalSize::new(70_000, 20_000).unwrap(),
        10_000,
    );
    let overflow = Fixture::new();
    let mut assets = AssetStore::default();
    let hash = assets.insert("font/ttf", FONT.to_vec());
    let style = TextStyle {
        font_family: "DejaVu Sans".into(),
        font_face: Some(FontFaceBinding {
            source: hash,
            face_index: 0,
        }),
        ..TextStyle::default()
    };
    overflow.source(
        text(style, &"Overflowing words ".repeat(60)),
        &assets,
        PhysicalSize::new(10_000, 5_000).unwrap(),
        10_000,
    );
    let picture = Fixture::new();
    let bytes =
        include_bytes!("../../../crates/raster-boundary/tests/fixtures/valid-3x2.png").to_vec();
    let inspection = document_core::inspect_raster(&bytes).unwrap();
    let mut picture_assets = AssetStore::default();
    let asset = picture_assets.insert_raster(bytes, inspection);
    picture.source(
        ObjectKind::Image {
            asset,
            wrap_mode: WrapMode::None,
            content_kind: ImageContentKind::ContinuousTone,
            crop: ImageCrop::default(),
            flip: ImageFlip::default(),
            lock_aspect_ratio: true,
        },
        &picture_assets,
        PhysicalSize::new(30_000, 20_000).unwrap(),
        10_000,
    );
    for (fixture, expected) in [
        (&unbound, "font"),
        (&overflow, "Overflow"),
        (&picture, "Picture"),
    ] {
        let original = fs::read(fixture.0.join("source.publisher-v2")).unwrap();
        for mode in ["pdf", "pdfx4-candidate"] {
            let output = fixture.cli(mode, mode, &["--allow-warnings"]);
            assert!(!output.status.success());
            assert!(
                String::from_utf8_lossy(&output.stderr)
                    .to_lowercase()
                    .contains(&expected.to_lowercase()),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(!fixture.0.join(mode).exists());
        }
        assert_eq!(
            fs::read(fixture.0.join("source.publisher-v2")).unwrap(),
            original
        );
    }
}

#[test]
fn candidate_generation_does_not_change_the_professional_gate() {
    use print_core::{
        ArtifactHash, EvaluationKey, ProfileHash, PublicationCandidatePreflight, PublicationGate,
    };
    let fixture = Fixture::new();
    fixture.geometry(10_000);
    let receipt = generate_pdf(&fixture.request("candidate", ExportMode::Pdfx4Candidate)).unwrap();
    let decode = |value: &str| -> [u8; 32] {
        std::array::from_fn(|i| u8::from_str_radix(&value[i * 2..i * 2 + 2], 16).unwrap())
    };
    // Reconstitute the exact successful candidate identities. Even a locally clean
    // report cannot authorize professional output without independent validation.
    let report = PublicationCandidatePreflight {
        evaluation_key: EvaluationKey::from_bytes(decode(&receipt.preflight_evaluation_key)),
        artifact_hash: ArtifactHash::of(
            &fs::read(fixture.0.join("candidate/publication.pdf")).unwrap(),
        ),
        export_identity: ArtifactHash::from_bytes(decode(&receipt.preflight_export_identity)),
        profile_hash: ProfileHash::of(PROFILE),
        findings: vec![],
    };
    let gate = PublicationGate::evaluate(&report, None);
    assert!(!gate.allowed);
    assert!(gate.authorization().is_none());
    assert!(gate.reason.contains("independent"));
    assert_eq!(receipt.independent_verification, "pending");
}
