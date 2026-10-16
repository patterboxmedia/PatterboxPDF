mod common;

use common::{TempDirectory, TestArchiveEntry, write_archive_entries};
use document_core::package::{
    MIN_READABLE_SCHEMA_VERSION, PackageError, SCHEMA_VERSION, open_package,
};

#[test]
fn every_supported_schema_opens_unchanged_and_reports_its_actual_version() {
    let mut manifest: serde_json::Value =
        serde_json::from_slice(include_bytes!("fixtures/minimal/manifest.json"))
            .expect("parse checked-in schema-one fixture");
    for version in MIN_READABLE_SCHEMA_VERSION..=SCHEMA_VERSION {
        let temp = TempDirectory::new(&format!("schema-{version}-fixture"));
        let fixture = temp.path().join("minimal.publisher-v2");
        manifest["schema_version"] = serde_json::json!(version);
        write_archive_entries(
            &fixture,
            &[TestArchiveEntry {
                name: "manifest.json".to_owned(),
                bytes: serde_json::to_vec(&manifest).expect("encode versioned fixture"),
            }],
        );
        let before = std::fs::read(&fixture).expect("read original fixture");
        let opened = open_package(&fixture).expect("supported schema remains readable");
        assert_eq!(opened.schema_version, version);
        assert_eq!(opened.snapshot.document.title, "Minimal fixture");
        assert_eq!(opened.snapshot.document.pages.len(), 1);
        assert!(opened.snapshot.document.pages[0].guides.is_empty());
        assert!(opened.assets.is_empty());
        assert_eq!(
            std::fs::read(&fixture).unwrap(),
            before,
            "Open preserves original bytes"
        );
    }
}

#[test]
fn pre_archive_directory_is_not_mistaken_for_a_current_project_file() {
    // Materialize the checked-in fixture on the executing OS; a cross-compiled
    // test must not try to open its compiler's Linux source path on Windows.
    let temp = TempDirectory::new("legacy-schema-one");
    let fixture = temp.path().join("minimal");
    std::fs::create_dir(&fixture).expect("create legacy fixture directory");
    let before = include_bytes!("fixtures/minimal/manifest.json");
    std::fs::write(fixture.join("manifest.json"), before).expect("write legacy fixture");

    assert!(matches!(
        open_package(&fixture).expect_err("manual projects now require one archive file"),
        PackageError::NotRegularFile(path) if path == fixture
    ));
    assert_eq!(
        std::fs::read(fixture.join("manifest.json")).expect("re-read legacy fixture"),
        before,
        "rejecting a pre-archive project directory never mutates it"
    );
}

#[test]
fn current_writer_is_newer_than_the_legacy_readable_fixture() {
    assert_eq!(MIN_READABLE_SCHEMA_VERSION, 1);
    assert_eq!(SCHEMA_VERSION, 18);
}

#[test]
fn unsupported_past_and_future_versions_are_read_only_failures() {
    for unsupported in [MIN_READABLE_SCHEMA_VERSION - 1, SCHEMA_VERSION + 1] {
        let temp = TempDirectory::new(&format!("schema-{unsupported}"));
        let package = temp.path().join("unsupported.publisher-v2");
        let manifest = format!("{{\"schema_version\":{unsupported}}}");
        write_archive_entries(
            &package,
            &[TestArchiveEntry {
                name: "manifest.json".to_owned(),
                bytes: manifest.into_bytes(),
            }],
        );
        let before = std::fs::read(&package).expect("read original bytes");

        let error = open_package(&package).expect_err("unsupported schema must reject");
        assert!(matches!(
            error,
            PackageError::UnsupportedSchema {
                found,
                supported: SCHEMA_VERSION
            } if found == unsupported
        ));
        assert_eq!(
            std::fs::read(&package).expect("source remains readable"),
            before,
            "open never mutates an unsupported package"
        );
    }
}
