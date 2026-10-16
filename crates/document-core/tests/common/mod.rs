// Each integration-test crate imports only the fixtures it exercises.
#![allow(dead_code)]

use document_core::model::{
    Color, Document, DocumentId, Insets, ObjectDraft, ObjectKind, PhysicalSize, ShapeKind,
    ShapeStroke, Transform, WrapMode,
};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(1);

pub fn letter_document() -> Document {
    Document::new(
        DocumentId::parse("document-test").expect("valid fixture id"),
        "Test publication",
        PhysicalSize::new(215_900, 279_400).expect("positive letter size"),
        Insets::uniform(3_175).expect("nonnegative bleed"),
    )
    .expect("valid fixture document")
}

pub fn rectangle(name: &str, x: i64, y: i64) -> ObjectDraft {
    ObjectDraft {
        name: name.to_owned(),
        size: PhysicalSize::new(25_400, 25_400).expect("positive object size"),
        transform: Transform::identity_at(x, y),
        opacity_millionths: 1_000_000,
        kind: ObjectKind::Shape {
            shape: ShapeKind::Rectangle,
            fill: Color {
                red: 10,
                green: 20,
                blue: 30,
                alpha: 255,
            },
            stroke: ShapeStroke::default(),
            wrap_mode: WrapMode::None,
        },
    }
}

pub struct TempDirectory(PathBuf);

impl TempDirectory {
    pub fn new(label: &str) -> Self {
        let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "publisher-v2-{label}-{}-{sequence}",
            std::process::id()
        ));
        if path.exists() {
            std::fs::remove_dir_all(&path).expect("remove stale test directory");
        }
        std::fs::create_dir(&path).expect("create test directory");
        Self(path)
    }

    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDirectory {
    fn drop(&mut self) {
        let _ignored = std::fs::remove_dir_all(&self.0);
    }
}

#[derive(Clone, Debug)]
pub struct TestArchiveEntry {
    pub name: String,
    pub bytes: Vec<u8>,
}

pub fn read_archive_entries(path: &Path) -> Vec<TestArchiveEntry> {
    let mut archive = zip::ZipArchive::new(std::fs::File::open(path).expect("open test archive"))
        .expect("parse test archive");
    (0..archive.len())
        .map(|index| {
            let mut entry = archive.by_index(index).expect("read test archive entry");
            let name = entry.name().to_owned();
            let mut bytes = Vec::new();
            entry
                .read_to_end(&mut bytes)
                .expect("read test entry bytes");
            TestArchiveEntry { name, bytes }
        })
        .collect()
}

pub fn write_archive_entries(path: &Path, entries: &[TestArchiveEntry]) {
    let file = std::fs::File::create(path).expect("create test archive");
    let mut archive = zip::ZipWriter::new(file);
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Stored)
        .unix_permissions(0o600);
    for entry in entries {
        archive
            .start_file(&entry.name, options)
            .expect("start test archive entry");
        archive
            .write_all(&entry.bytes)
            .expect("write test archive entry");
    }
    archive
        .finish()
        .expect("finish test archive")
        .sync_all()
        .expect("sync test archive");
}

pub fn replace_archive_entry(path: &Path, name: &str, bytes: Vec<u8>) {
    let mut entries = read_archive_entries(path);
    let entry = entries
        .iter_mut()
        .find(|entry| entry.name == name)
        .expect("test archive entry exists");
    entry.bytes = bytes;
    write_archive_entries(path, &entries);
}

pub fn read_archive_entry(path: &Path, name: &str) -> Vec<u8> {
    read_archive_entries(path)
        .into_iter()
        .find(|entry| entry.name == name)
        .expect("test archive entry exists")
        .bytes
}

pub fn set_archive_comment(path: &Path, comment: &str) {
    let entries = read_archive_entries(path);
    let file = std::fs::File::create(path).expect("recreate commented archive");
    let mut archive = zip::ZipWriter::new(file);
    archive
        .set_comment(comment.to_owned())
        .expect("set test archive comment");
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Stored)
        .unix_permissions(0o600);
    for entry in entries {
        archive
            .start_file(entry.name, options)
            .expect("start entry");
        archive.write_all(&entry.bytes).expect("write entry");
    }
    archive
        .finish()
        .expect("finish commented archive")
        .sync_all()
        .expect("sync commented archive");
}

pub fn forge_duplicate_archive_entry_name(
    path: &Path,
    entries: &[TestArchiveEntry],
    duplicate_name: &str,
) {
    const PLACEHOLDER_NAME: &str = "duplicat.json";
    assert_eq!(
        PLACEHOLDER_NAME.len(),
        duplicate_name.len(),
        "forged duplicate names must preserve ZIP header lengths"
    );
    assert!(
        !entries.iter().any(|entry| entry.name == PLACEHOLDER_NAME),
        "placeholder name must be unique"
    );

    let mut forged_entries = entries.to_vec();
    let duplicate = entries
        .iter()
        .find(|entry| entry.name == duplicate_name)
        .expect("source duplicate entry exists");
    forged_entries.push(TestArchiveEntry {
        name: PLACEHOLDER_NAME.to_owned(),
        bytes: duplicate.bytes.clone(),
    });
    write_archive_entries(path, &forged_entries);

    let mut archive_bytes = std::fs::read(path).expect("read archive to forge duplicate name");
    let placeholder = PLACEHOLDER_NAME.as_bytes();
    let replacement = duplicate_name.as_bytes();
    let mut replacements = 0;
    for offset in 0..=archive_bytes.len() - placeholder.len() {
        if &archive_bytes[offset..offset + placeholder.len()] == placeholder {
            archive_bytes[offset..offset + placeholder.len()].copy_from_slice(replacement);
            replacements += 1;
        }
    }
    assert_eq!(
        replacements, 2,
        "placeholder should occur once in the local header and once in the central directory"
    );
    std::fs::write(path, archive_bytes).expect("write forged duplicate-name archive");
}

pub fn forge_archive_entry_count(path: &Path, entry_count: u16) {
    let mut archive_bytes = std::fs::read(path).expect("read archive to forge entry count");
    let eocd_offset = archive_bytes
        .windows(4)
        .rposition(|window| window == [0x50, 0x4b, 0x05, 0x06])
        .expect("test archive has an EOCD record");
    archive_bytes[eocd_offset + 8..eocd_offset + 10].copy_from_slice(&entry_count.to_le_bytes());
    archive_bytes[eocd_offset + 10..eocd_offset + 12].copy_from_slice(&entry_count.to_le_bytes());
    std::fs::write(path, archive_bytes).expect("write forged archive entry count");
}
