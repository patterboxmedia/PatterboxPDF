//! Versioned single-file package save/open using a validated sibling stage.

use crate::assets::{AssetHash, AssetStore, EmbeddedAsset, RasterInspection};
use crate::commands::{CommandError, DocumentSession};
use crate::model::{Document, ModelError, ObjectKind, Revision};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use ttf_parser::Face;
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

/// Schema written by this build. Version eighteen adds selected-text families,
/// weights, colors and underline, including exact resources for additional families.
/// It also records Text Box interior margins/alignment and rectangle wrap clearance/side.
/// Version seventeen adds selected-text font size.
/// Version sixteen adds the open Line primitive.
/// Version fifteen adds publication-wide exact
/// font fallback chains; version fourteen added exact rich-text font variants.
pub const SCHEMA_VERSION: u32 = 18;
/// Oldest schema this build can open without losing authoring data.
pub const MIN_READABLE_SCHEMA_VERSION: u32 = 1;
static STAGE_SEQUENCE: AtomicU64 = AtomicU64::new(1);
const FONT_MEDIA_TYPES: &[&str] = &[
    "font/otf",
    "font/ttf",
    "font/collection",
    "application/font-sfnt",
    "application/vnd.ms-opentype",
];

/// Returns whether a package media type may identify an embedded OpenType resource.
///
/// This is the single allowlist used both while validating a package and while
/// handing exact font bytes to layout. Keeping the trust boundaries aligned
/// prevents an accepted publication from later losing its native preview.
#[must_use]
pub fn is_supported_font_media_type(media_type: &str) -> bool {
    FONT_MEDIA_TYPES.contains(&media_type)
}

/// Version-one resource budgets applied before expensive model or asset work.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PackageLimits {
    pub manifest_bytes: u64,
    pub json_depth: usize,
    pub string_bytes: usize,
    pub pages: usize,
    pub objects: usize,
    pub story_characters: usize,
    pub aggregate_asset_bytes: u64,
}

pub const PACKAGE_LIMITS_V1: PackageLimits = PackageLimits {
    manifest_bytes: 8 * 1024 * 1024,
    json_depth: 64,
    string_bytes: crate::model::MAX_TEXT_BYTES,
    pages: Document::MAX_INITIAL_PAGES as usize,
    objects: 50_000,
    story_characters: 5_000_000,
    aggregate_asset_bytes: 4 * 1024 * 1024 * 1024,
};
const MAX_ARCHIVE_ENTRIES: usize = PACKAGE_LIMITS_V1.objects + 1;
const MAX_ARCHIVE_ENTRY_NAME_BYTES: usize = 80;
const MAX_CENTRAL_DIRECTORY_BYTES: u64 = (MAX_ARCHIVE_ENTRIES as u64) * 200;
pub(crate) const MAX_ARCHIVE_FILE_BYTES: u64 = PACKAGE_LIMITS_V1.aggregate_asset_bytes
    + PACKAGE_LIMITS_V1.manifest_bytes
    + (MAX_ARCHIVE_ENTRIES as u64) * 400
    + 1024 * 1024;
const EOCD_MINIMUM_BYTES: u64 = 22;
const MAX_ZIP_COMMENT_BYTES: u64 = u16::MAX as u64;
const CENTRAL_HEADER_BYTES: usize = 46;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ProjectSnapshot {
    pub document: Document,
    pub revision: Revision,
}

impl ProjectSnapshot {
    #[must_use]
    pub fn from_session(session: &DocumentSession) -> Self {
        Self {
            document: session.document().clone(),
            revision: session.revision(),
        }
    }

    /// Restores an editing session from this package snapshot.
    ///
    /// # Errors
    ///
    /// Returns [`CommandError`] when the snapshot contains an invalid document.
    pub fn into_session(self) -> Result<DocumentSession, CommandError> {
        DocumentSession::from_saved(self.document, self.revision)
    }

    /// Returns a domain-separated digest of the complete canonical project
    /// snapshot. Struct field order and sequence order are preserved by the
    /// canonical document model; no unordered maps occur in this payload.
    ///
    /// # Errors
    ///
    /// Returns a serialization error if the canonical snapshot cannot be
    /// encoded for identity calculation.
    pub fn semantic_digest(&self) -> Result<[u8; 32], serde_json::Error> {
        let bytes = serde_json::to_vec(self)?;
        let mut digest = Sha256::new();
        digest.update(b"publisher-v2/project-snapshot-semantics/v2\0");
        digest.update((bytes.len() as u64).to_be_bytes());
        digest.update(bytes);
        Ok(digest.finalize().into())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OpenedPackage {
    /// Actual manifest version read from this package; opening never rewrites it.
    /// This compatibility fact stays outside canonical snapshot semantics.
    pub schema_version: u32,
    pub snapshot: ProjectSnapshot,
    pub assets: AssetStore,
    pub fingerprint: PackageFingerprint,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PackageFingerprint(String);

impl PackageFingerprint {
    /// Parses a canonical lowercase SHA-256 package fingerprint.
    ///
    /// # Errors
    ///
    /// Returns [`PackageError::InvalidFingerprint`] for malformed values.
    pub fn parse(value: impl Into<String>) -> Result<Self, PackageError> {
        let value = value.into();
        if value.len() == 64
            && value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            Ok(Self(value))
        } else {
            Err(PackageError::InvalidFingerprint)
        }
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Deserialize, Serialize)]
struct Manifest {
    schema_version: u32,
    snapshot: ProjectSnapshot,
    assets: BTreeMap<AssetHash, AssetDescriptor>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct AssetDescriptor {
    pub media_type: String,
    pub byte_length: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raster: Option<RasterInspection>,
}

#[derive(Debug, Deserialize)]
struct VersionProbe {
    schema_version: u32,
}

#[derive(Clone, Copy, Debug)]
struct ArchivedAssetEntry {
    index: usize,
    byte_length: u64,
}

type ArchivedAssetEntries = BTreeMap<AssetHash, ArchivedAssetEntry>;

/// Writes a new package through an exclusive sibling stage and installs without replacement.
///
/// Windows uses the same supported local-NTFS parent policy as repeat Save and retains the
/// exact stage handle through writing, validation, installation and destination verification.
/// Failed Windows writes clean only that held stage. Other hosts retain failed stages and do
/// not provide the Windows handle-bound path-substitution protection. Power-loss durability
/// of directory metadata remains unproved.
///
/// # Errors
///
/// Returns [`PackageError`] when the destination is unsuitable, the snapshot
/// or assets are invalid, or any staging, serialization, persistence, or
/// verification operation fails.
pub fn save_new_package(
    destination: &Path,
    snapshot: &ProjectSnapshot,
    assets: &AssetStore,
) -> Result<PackageFingerprint, PackageError> {
    save_new_package_with(destination, snapshot, assets, |_, _| Ok(()))
}

/// Stages a new publication with cancellation before its atomic, no-overwrite commit.
///
/// # Errors
/// Returns the normal package error or Interrupted when cancelled before publication.
pub fn save_new_package_with_cancel(
    destination: &Path,
    snapshot: &ProjectSnapshot,
    assets: &AssetStore,
    cancelled: impl Fn() -> bool,
) -> Result<PackageFingerprint, PackageError> {
    if cancelled() { return Err(cancelled_first_save()); }
    save_new_package_with(destination, snapshot, assets, |_, _| {
        if cancelled() { Err(cancelled_first_save()) } else { Ok(()) }
    })
}

fn cancelled_first_save() -> PackageError {
    PackageError::Io(std::io::Error::new(std::io::ErrorKind::Interrupted, "Save cancelled before the new publication was created"))
}

fn save_new_package_with(
    destination: &Path,
    snapshot: &ProjectSnapshot,
    assets: &AssetStore,
    mut inject: impl FnMut(&Path, FirstSaveFaultPoint) -> Result<(), PackageError>,
) -> Result<PackageFingerprint, PackageError> {
    #[cfg(windows)]
    let directory = publisher_windows_atomic_file::PinnedDirectory::for_destination(destination)?;
    if destination.exists() {
        return Err(PackageError::DestinationExists(destination.to_path_buf()));
    }
    let selected_assets = assets.for_document(&snapshot.document);
    let assets = &selected_assets;
    validate_package_budgets(snapshot, assets)?;
    validate_snapshot_assets(snapshot, assets)?;
    let parent = destination
        .parent()
        .ok_or(PackageError::DestinationHasNoParent)?;
    if !parent.is_dir() {
        return Err(PackageError::ParentMissing(parent.to_path_buf()));
    }
    let stage = sibling_stage_path(destination)?;
    if stage.exists() {
        return Err(PackageError::StageCollision(stage));
    }

    let result = (|| {
        let mut descriptors = BTreeMap::new();
        for (hash, asset) in assets.iter() {
            descriptors.insert(
                hash.clone(),
                AssetDescriptor {
                    media_type: asset.media_type.clone(),
                    byte_length: u64::try_from(asset.bytes.len())
                        .map_err(|_| PackageError::AssetTooLarge(hash.clone()))?,
                    raster: asset.raster.clone(),
                },
            );
        }

        let manifest = Manifest {
            schema_version: SCHEMA_VERSION,
            snapshot: snapshot.clone(),
            assets: descriptors,
        };
        let bytes = serde_json::to_vec_pretty(&manifest)?;
        #[cfg(windows)]
        let created = directory.create_file_for_destination(&stage, destination)?;
        #[cfg(windows)]
        let file = created.file()?;
        #[cfg(not(windows))]
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&stage)?;
        inject(&stage, FirstSaveFaultPoint::Created)?;
        write_archive(file.try_clone()?, &bytes, assets, |point| {
            inject(&stage, point)
        })?;
        let reopened = open_package_file(file)?;
        if reopened.snapshot != *snapshot || reopened.assets != *assets {
            return Err(PackageError::StageValidationMismatch);
        }
        inject(&stage, FirstSaveFaultPoint::Validated)?;
        #[cfg(windows)]
        let moved = created.commit()?;
        #[cfg(windows)]
        let committed = open_package_file(moved.reopened_destination_file()?)?;
        #[cfg(not(windows))]
        let committed = {
            install_new_archive(&stage, destination)?;
            open_package(destination)?
        };
        if committed != reopened {
            return Err(PackageError::CommittedValidationMismatch);
        }
        Ok(committed.fingerprint)
    })();

    // Windows cleanup is bound to CreatedFile, including failures before installation.
    // Other hosts retain failed stages rather than deleting a potentially substituted name.
    result
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum FirstSaveFaultPoint {
    Created,
    ManifestWritten,
    AssetWritten,
    Validated,
}

#[cfg(unix)]
fn install_new_archive(stage: &Path, destination: &Path) -> Result<(), PackageError> {
    rustix::fs::renameat_with(
        rustix::fs::CWD,
        stage,
        rustix::fs::CWD,
        destination,
        rustix::fs::RenameFlags::NOREPLACE,
    )
    .map_err(|error| PackageError::Io(error.into()))
}

#[cfg(all(not(windows), not(unix)))]
fn install_new_archive(_stage: &Path, _destination: &Path) -> Result<(), PackageError> {
    Err(PackageError::AtomicExchangeUnavailable)
}

pub(crate) fn write_replacement_archive(
    destination: &Path,
    snapshot: &ProjectSnapshot,
    assets: &AssetStore,
    inject: &mut impl FnMut(crate::save_transaction::SaveFaultPoint) -> Result<(), PackageError>,
) -> Result<OpenedPackage, PackageError> {
    use crate::save_transaction::SaveFaultPoint;

    if destination.exists() {
        return Err(PackageError::DestinationExists(destination.to_path_buf()));
    }
    let selected_assets = assets.for_document(&snapshot.document);
    let assets = &selected_assets;
    validate_package_budgets(snapshot, assets)?;
    validate_snapshot_assets(snapshot, assets)?;
    let parent = destination
        .parent()
        .ok_or(PackageError::DestinationHasNoParent)?;
    if !parent.is_dir() {
        return Err(PackageError::ParentMissing(parent.to_path_buf()));
    }

    let mut descriptors = BTreeMap::new();
    for (hash, asset) in assets.iter() {
        descriptors.insert(
            hash.clone(),
            AssetDescriptor {
                media_type: asset.media_type.clone(),
                byte_length: u64::try_from(asset.bytes.len())
                    .map_err(|_| PackageError::AssetTooLarge(hash.clone()))?,
                raster: asset.raster.clone(),
            },
        );
    }
    let manifest = Manifest {
        schema_version: SCHEMA_VERSION,
        snapshot: snapshot.clone(),
        assets: descriptors,
    };
    let manifest_bytes = serde_json::to_vec_pretty(&manifest)?;
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(destination)?;
    inject(SaveFaultPoint::ReplacementFileCreated)?;
    let mut archive = ZipWriter::new(file);
    let options = SimpleFileOptions::default()
        .compression_method(CompressionMethod::Stored)
        .unix_permissions(0o600);
    archive.start_file("manifest.json", options)?;
    archive.write_all(&manifest_bytes)?;
    inject(SaveFaultPoint::ReplacementManifestWritten)?;
    for (index, (hash, asset)) in assets.iter().enumerate() {
        let asset_options = options.large_file(asset.bytes.len() >= u32::MAX as usize);
        archive.start_file(format!("assets/{}.bin", hash.as_str()), asset_options)?;
        archive.write_all(&asset.bytes)?;
        inject(SaveFaultPoint::ReplacementAssetWritten(index))?;
    }
    let file = archive.finish()?;
    file.sync_all()?;
    inject(SaveFaultPoint::ReplacementFlushed)?;
    let reopened = open_package(destination)?;
    if reopened.snapshot != *snapshot || reopened.assets != *assets {
        return Err(PackageError::StageValidationMismatch);
    }
    inject(SaveFaultPoint::ReplacementValidated)?;
    Ok(reopened)
}

/// Replaces an existing package only when its validated bytes still match the opened generation.
///
/// The journal-named replacement is written directly, flushed, and reopened. On Unix, an atomic
/// exchange prevents a competing destination from being overwritten; an independently copied
/// prior and the exchanged-out original remain tied to a transaction-unique completed receipt.
/// Those artifacts await the later handle-bound cleanup policy. On Windows, source mutation is
/// handle-originated while a validated local-NTFS ancestor chain is pinned; the absolute sibling
/// target form is used because the documented relative `RootDirectory` form fails on the supported
/// host. Retained-artifact cleanup, unreadable cloud-placeholder acknowledgement, directory
/// metadata durability, and power-loss safety remain explicitly unproved. This API does not claim
/// power-loss safety on any platform yet.
///
/// # Errors
///
/// Returns [`PackageError::DestinationChanged`] when another process changed the package after it
/// was opened, or another package/IO error when staging, validation, or commit fails.
pub fn save_existing_package(
    destination: &Path,
    expected: &PackageFingerprint,
    snapshot: &ProjectSnapshot,
    assets: &AssetStore,
) -> Result<PackageFingerprint, PackageError> {
    crate::save_transaction::save_existing(destination, expected, snapshot, assets, |_| Ok(()))
}

/// Opens and validates every non-lossy schema supported by this build.
///
/// # Errors
///
/// Returns [`PackageError`] when package files cannot be read, decoded, or
/// validated against the supported schema and asset integrity rules.
pub fn open_package(path: &Path) -> Result<OpenedPackage, PackageError> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.file_type().is_file() {
        return Err(PackageError::NotRegularFile(path.to_path_buf()));
    }
    if metadata.len() > MAX_ARCHIVE_FILE_BYTES {
        return Err(PackageError::ArchiveTooLarge);
    }
    open_package_file(File::open(path)?)
}

pub(crate) fn open_package_file(mut file: File) -> Result<OpenedPackage, PackageError> {
    let metadata = file.metadata()?;
    if !metadata.file_type().is_file() {
        return Err(PackageError::InvalidArchiveEntry);
    }
    if metadata.len() > MAX_ARCHIVE_FILE_BYTES {
        return Err(PackageError::ArchiveTooLarge);
    }
    let fingerprint_before_validation = hash_archive_file(&mut file)?;
    open_hashed_archive(file, fingerprint_before_validation)
}

fn open_hashed_archive(
    mut file: File,
    fingerprint_before_validation: [u8; 32],
) -> Result<OpenedPackage, PackageError> {
    preflight_archive_directory(&mut file)?;
    let mut archive = ZipArchive::new(file)?;
    if archive.offset() != 0 {
        return Err(PackageError::InvalidArchiveEntry);
    }
    if archive.len() > MAX_ARCHIVE_ENTRIES {
        return Err(PackageError::ArchiveEntryLimitExceeded);
    }
    if archive.has_overlapping_files()? {
        return Err(PackageError::InvalidArchiveEntry);
    }
    let (manifest_bytes, archived_asset_entries) = index_archive_contents(&mut archive)?;
    validate_json_depth(&manifest_bytes)?;
    let version: VersionProbe = serde_json::from_slice(&manifest_bytes)?;
    if !(MIN_READABLE_SCHEMA_VERSION..=SCHEMA_VERSION).contains(&version.schema_version) {
        return Err(PackageError::UnsupportedSchema {
            found: version.schema_version,
            supported: SCHEMA_VERSION,
        });
    }
    let manifest: Manifest = serde_json::from_slice(&manifest_bytes)?;
    manifest.snapshot.document.validate()?;

    validate_manifest_budgets(&manifest)?;

    let assets = read_manifest_assets(&mut archive, &manifest, &archived_asset_entries)?;
    validate_snapshot_assets(&manifest.snapshot, &assets)?;
    let mut archive_file = archive.into_inner();
    let fingerprint_after_validation = hash_archive_file(&mut archive_file)?;
    if fingerprint_after_validation != fingerprint_before_validation {
        return Err(PackageError::ArchiveChangedDuringRead);
    }
    Ok(OpenedPackage {
        schema_version: version.schema_version,
        snapshot: manifest.snapshot,
        assets,
        fingerprint: PackageFingerprint(hex_digest(fingerprint_before_validation)),
    })
}

/// Opens a pre-archive recovery package without treating the legacy directory as a writable
/// project destination. New manual saves and autosaves always use the single-file archive.
pub(crate) fn open_legacy_recovery_package(path: &Path) -> Result<OpenedPackage, PackageError> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.file_type().is_dir() || metadata.file_type().is_symlink() {
        return Err(PackageError::NotRegularFile(path.to_path_buf()));
    }
    let manifest_path = path.join("manifest.json");
    let manifest_bytes = read_regular_file_bounded(
        &manifest_path,
        PACKAGE_LIMITS_V1.manifest_bytes,
        PackageError::ManifestTooLarge,
    )?;
    validate_json_depth(&manifest_bytes)?;
    let version: VersionProbe = serde_json::from_slice(&manifest_bytes)?;
    if !(MIN_READABLE_SCHEMA_VERSION..=SCHEMA_VERSION).contains(&version.schema_version) {
        return Err(PackageError::UnsupportedSchema {
            found: version.schema_version,
            supported: SCHEMA_VERSION,
        });
    }
    let manifest: Manifest = serde_json::from_slice(&manifest_bytes)?;
    manifest.snapshot.document.validate()?;
    validate_manifest_budgets(&manifest)?;

    let mut fingerprint = Sha256::new();
    fingerprint.update(b"publisher-v2-package-v1\0");
    fingerprint.update((manifest_bytes.len() as u64).to_le_bytes());
    fingerprint.update(&manifest_bytes);
    let mut assets = AssetStore::default();
    for (hash, descriptor) in manifest.assets {
        let asset_path = path.join("assets").join(format!("{}.bin", hash.as_str()));
        let bytes = read_regular_file_bounded(
            &asset_path,
            descriptor.byte_length,
            PackageError::AssetTooLarge(hash.clone()),
        )?;
        let actual_length =
            u64::try_from(bytes.len()).map_err(|_| PackageError::AssetTooLarge(hash.clone()))?;
        if actual_length != descriptor.byte_length {
            return Err(PackageError::AssetLengthMismatch {
                hash,
                expected: descriptor.byte_length,
                actual: actual_length,
            });
        }
        fingerprint.update(hash.as_str().as_bytes());
        fingerprint.update(descriptor.byte_length.to_le_bytes());
        fingerprint.update(&bytes);
        assets
            .insert_verified(
                hash.clone(),
                EmbeddedAsset {
                    media_type: descriptor.media_type,
                    bytes,
                    raster: descriptor.raster,
                },
            )
            .map_err(|_| PackageError::AssetHashMismatch(hash))?;
    }
    validate_snapshot_assets(&manifest.snapshot, &assets)?;
    Ok(OpenedPackage {
        schema_version: version.schema_version,
        snapshot: manifest.snapshot,
        assets,
        fingerprint: PackageFingerprint(format!("{:x}", fingerprint.finalize())),
    })
}

fn hash_archive_file(file: &mut File) -> Result<[u8; 32], PackageError> {
    file.seek(std::io::SeekFrom::Start(0))?;
    let mut digest = Sha256::new();
    let mut buffer = vec![0_u8; 64 * 1024];
    let mut hashed_bytes = 0_u64;
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hashed_bytes = hashed_bytes
            .checked_add(u64::try_from(count).map_err(|_| PackageError::ArchiveTooLarge)?)
            .ok_or(PackageError::ArchiveTooLarge)?;
        if hashed_bytes > MAX_ARCHIVE_FILE_BYTES {
            return Err(PackageError::ArchiveTooLarge);
        }
        digest.update(&buffer[..count]);
    }
    Ok(digest.finalize().into())
}

fn hex_digest(bytes: [u8; 32]) -> String {
    let mut result = String::with_capacity(64);
    for byte in bytes {
        write!(&mut result, "{byte:02x}").expect("writing to a String cannot fail");
    }
    result
}

fn index_archive_contents(
    archive: &mut ZipArchive<File>,
) -> Result<(Vec<u8>, ArchivedAssetEntries), PackageError> {
    let mut seen = BTreeSet::new();
    let mut manifest_bytes = None;
    let mut archived_assets = ArchivedAssetEntries::new();
    let mut declared_asset_bytes = 0_u64;
    for index in 0..archive.len() {
        let entry = archive.by_index(index)?;
        let name = validate_archive_entry(&entry)?;
        if !seen.insert(name.clone()) {
            return Err(PackageError::DuplicateArchiveEntry(name));
        }
        if name == "manifest.json" {
            let expected_length = entry.size();
            if expected_length > PACKAGE_LIMITS_V1.manifest_bytes {
                return Err(PackageError::ManifestTooLarge);
            }
            let mut bytes =
                Vec::with_capacity(usize::try_from(expected_length.min(64 * 1024)).unwrap_or(0));
            entry
                .take(PACKAGE_LIMITS_V1.manifest_bytes.saturating_add(1))
                .read_to_end(&mut bytes)?;
            let actual_length =
                u64::try_from(bytes.len()).map_err(|_| PackageError::ManifestTooLarge)?;
            if actual_length > PACKAGE_LIMITS_V1.manifest_bytes {
                return Err(PackageError::ManifestTooLarge);
            }
            if actual_length != expected_length {
                return Err(PackageError::ArchiveEntryLengthMismatch {
                    path: name,
                    expected: expected_length,
                    actual: actual_length,
                });
            }
            manifest_bytes = Some(bytes);
        } else {
            declared_asset_bytes = declared_asset_bytes
                .checked_add(entry.size())
                .ok_or(PackageError::AggregateAssetLimitExceeded)?;
            if declared_asset_bytes > PACKAGE_LIMITS_V1.aggregate_asset_bytes {
                return Err(PackageError::AggregateAssetLimitExceeded);
            }
            let hash = archive_asset_hash(&name).ok_or(PackageError::InvalidArchiveEntry)?;
            archived_assets.insert(
                hash,
                ArchivedAssetEntry {
                    index,
                    byte_length: entry.size(),
                },
            );
        }
    }
    Ok((
        manifest_bytes.ok_or(PackageError::MissingManifest)?,
        archived_assets,
    ))
}

fn read_manifest_assets(
    archive: &mut ZipArchive<File>,
    manifest: &Manifest,
    entries: &ArchivedAssetEntries,
) -> Result<AssetStore, PackageError> {
    if let Some(hash) = entries
        .keys()
        .find(|hash| !manifest.assets.contains_key(*hash))
    {
        return Err(PackageError::UnexpectedAsset(hash.clone()));
    }
    let mut assets = AssetStore::default();
    for (hash, descriptor) in &manifest.assets {
        let archived = entries
            .get(hash)
            .ok_or_else(|| PackageError::MissingAsset(hash.clone()))?;
        if archived.byte_length != descriptor.byte_length {
            return Err(PackageError::AssetLengthMismatch {
                hash: hash.clone(),
                expected: descriptor.byte_length,
                actual: archived.byte_length,
            });
        }
        let entry = archive.by_index(archived.index)?;
        let mut bytes =
            Vec::with_capacity(usize::try_from(archived.byte_length.min(64 * 1024)).unwrap_or(0));
        entry
            .take(descriptor.byte_length.saturating_add(1))
            .read_to_end(&mut bytes)?;
        let actual_length =
            u64::try_from(bytes.len()).map_err(|_| PackageError::AssetTooLarge(hash.clone()))?;
        if actual_length != descriptor.byte_length {
            return Err(PackageError::AssetLengthMismatch {
                hash: hash.clone(),
                expected: descriptor.byte_length,
                actual: actual_length,
            });
        }
        assets
            .insert_verified(
                hash.clone(),
                EmbeddedAsset {
                    media_type: descriptor.media_type.clone(),
                    bytes,
                    raster: descriptor.raster.clone(),
                },
            )
            .map_err(|_| PackageError::AssetHashMismatch(hash.clone()))?;
    }
    Ok(assets)
}

fn preflight_archive_directory(file: &mut File) -> Result<(), PackageError> {
    let file_length = file.metadata()?.len();
    if file_length < EOCD_MINIMUM_BYTES {
        return Err(PackageError::InvalidArchiveEntry);
    }
    let tail_length = (EOCD_MINIMUM_BYTES + MAX_ZIP_COMMENT_BYTES).min(file_length);
    file.seek(std::io::SeekFrom::Start(file_length - tail_length))?;
    let mut tail = vec![0_u8; usize::try_from(tail_length).unwrap_or(0)];
    file.read_exact(&mut tail)?;
    let eocd_index = (0..=tail.len().saturating_sub(22))
        .rev()
        .find(|&index| {
            tail[index..].starts_with(&[0x50, 0x4b, 0x05, 0x06])
                && index
                    + 22
                    + usize::from(u16::from_le_bytes([tail[index + 20], tail[index + 21]]))
                    == tail.len()
        })
        .ok_or(PackageError::InvalidArchiveEntry)?;
    let eocd_offset = file_length - tail_length + u64::try_from(eocd_index).unwrap_or(0);
    let eocd = &tail[eocd_index..eocd_index + 22];
    let disk = u16::from_le_bytes([eocd[4], eocd[5]]);
    let central_disk = u16::from_le_bytes([eocd[6], eocd[7]]);
    let entries_on_disk = u16::from_le_bytes([eocd[8], eocd[9]]);
    let entries = u16::from_le_bytes([eocd[10], eocd[11]]);
    if disk != 0 || central_disk != 0 || entries_on_disk != entries {
        return Err(PackageError::InvalidArchiveEntry);
    }
    let central_size_32 = u32::from_le_bytes(eocd[12..16].try_into().unwrap());
    let central_offset_32 = u32::from_le_bytes(eocd[16..20].try_into().unwrap());
    let requires_zip64 =
        entries == u16::MAX || central_size_32 == u32::MAX || central_offset_32 == u32::MAX;
    let (entry_count, central_size, central_offset, central_trailer_offset) = if requires_zip64 {
        if eocd_offset < 20 {
            return Err(PackageError::InvalidArchiveEntry);
        }
        file.seek(std::io::SeekFrom::Start(eocd_offset - 20))?;
        let mut locator = [0_u8; 20];
        file.read_exact(&mut locator)?;
        if !locator.starts_with(&[0x50, 0x4b, 0x06, 0x07])
            || u32::from_le_bytes(locator[4..8].try_into().unwrap()) != 0
            || u32::from_le_bytes(locator[16..20].try_into().unwrap()) != 1
        {
            return Err(PackageError::InvalidArchiveEntry);
        }
        let zip64_offset = u64::from_le_bytes(locator[8..16].try_into().unwrap());
        file.seek(std::io::SeekFrom::Start(zip64_offset))?;
        let mut zip64 = [0_u8; 56];
        file.read_exact(&mut zip64)?;
        if !zip64.starts_with(&[0x50, 0x4b, 0x06, 0x06])
            || u64::from_le_bytes(zip64[4..12].try_into().unwrap()) != 44
            || u32::from_le_bytes(zip64[16..20].try_into().unwrap()) != 0
            || u32::from_le_bytes(zip64[20..24].try_into().unwrap()) != 0
            || zip64_offset
                .checked_add(56)
                .is_none_or(|end| end != eocd_offset - 20)
        {
            return Err(PackageError::InvalidArchiveEntry);
        }
        let entries_on_disk = u64::from_le_bytes(zip64[24..32].try_into().unwrap());
        let entries = u64::from_le_bytes(zip64[32..40].try_into().unwrap());
        if entries_on_disk != entries {
            return Err(PackageError::InvalidArchiveEntry);
        }
        (
            entries,
            u64::from_le_bytes(zip64[40..48].try_into().unwrap()),
            u64::from_le_bytes(zip64[48..56].try_into().unwrap()),
            zip64_offset,
        )
    } else {
        (
            u64::from(entries),
            u64::from(central_size_32),
            u64::from(central_offset_32),
            eocd_offset,
        )
    };
    if entry_count > MAX_ARCHIVE_ENTRIES as u64 || central_size > MAX_CENTRAL_DIRECTORY_BYTES {
        return Err(PackageError::ArchiveEntryLimitExceeded);
    }
    if central_offset
        .checked_add(central_size)
        .is_none_or(|end| end != central_trailer_offset)
    {
        return Err(PackageError::InvalidArchiveEntry);
    }
    preflight_central_entries(file, entry_count, central_offset, central_size)?;
    file.seek(std::io::SeekFrom::Start(0))?;
    Ok(())
}

fn preflight_central_entries(
    file: &mut File,
    entry_count: u64,
    central_offset: u64,
    central_size: u64,
) -> Result<(), PackageError> {
    let central_end = central_offset
        .checked_add(central_size)
        .ok_or(PackageError::InvalidArchiveEntry)?;
    let mut cursor = central_offset;
    let mut seen_names = BTreeSet::new();
    file.seek(std::io::SeekFrom::Start(cursor))?;

    for _ in 0..entry_count {
        if cursor
            .checked_add(CENTRAL_HEADER_BYTES as u64)
            .is_none_or(|header_end| header_end > central_end)
        {
            return Err(PackageError::InvalidArchiveEntry);
        }
        let mut header = [0_u8; CENTRAL_HEADER_BYTES];
        file.read_exact(&mut header)?;
        if !header.starts_with(&[0x50, 0x4b, 0x01, 0x02]) {
            return Err(PackageError::InvalidArchiveEntry);
        }
        let name_length = u64::from(u16::from_le_bytes([header[28], header[29]]));
        let extra_length = u64::from(u16::from_le_bytes([header[30], header[31]]));
        let comment_length = u64::from(u16::from_le_bytes([header[32], header[33]]));
        if name_length == 0 || name_length > MAX_ARCHIVE_ENTRY_NAME_BYTES as u64 {
            return Err(PackageError::InvalidArchiveEntry);
        }
        let record_length = (CENTRAL_HEADER_BYTES as u64)
            .checked_add(name_length)
            .and_then(|length| length.checked_add(extra_length))
            .and_then(|length| length.checked_add(comment_length))
            .ok_or(PackageError::InvalidArchiveEntry)?;
        let next = cursor
            .checked_add(record_length)
            .filter(|next| *next <= central_end)
            .ok_or(PackageError::InvalidArchiveEntry)?;
        let mut name = vec![0_u8; usize::try_from(name_length).unwrap_or(0)];
        file.read_exact(&mut name)?;
        if !seen_names.insert(name.clone()) {
            return Err(PackageError::DuplicateArchiveEntry(
                String::from_utf8_lossy(&name).into_owned(),
            ));
        }
        cursor = next;
        file.seek(std::io::SeekFrom::Start(cursor))?;
    }

    if cursor != central_end {
        return Err(PackageError::InvalidArchiveEntry);
    }
    Ok(())
}

fn write_archive(
    file: File,
    manifest: &[u8],
    assets: &AssetStore,
    mut checkpoint: impl FnMut(FirstSaveFaultPoint) -> Result<(), PackageError>,
) -> Result<(), PackageError> {
    let mut archive = ZipWriter::new(file);
    let options = SimpleFileOptions::default()
        .compression_method(CompressionMethod::Stored)
        .unix_permissions(0o600);
    archive.start_file("manifest.json", options)?;
    archive.write_all(manifest)?;
    checkpoint(FirstSaveFaultPoint::ManifestWritten)?;
    for (hash, asset) in assets.iter() {
        let asset_options = options.large_file(asset.bytes.len() >= u32::MAX as usize);
        archive.start_file(format!("assets/{}.bin", hash.as_str()), asset_options)?;
        archive.write_all(&asset.bytes)?;
        checkpoint(FirstSaveFaultPoint::AssetWritten)?;
    }
    let file = archive.finish()?;
    file.sync_all()?;
    Ok(())
}

fn validate_archive_entry<R: Read + Seek>(
    entry: &zip::read::ZipFile<'_, R>,
) -> Result<String, PackageError> {
    let name = entry.name();
    if entry.name_raw() != name.as_bytes()
        || name.is_empty()
        || name.len() > MAX_ARCHIVE_ENTRY_NAME_BYTES
        || name.starts_with('/')
        || name.contains(['\\', ':', '\0'])
        || name
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
        || entry.encrypted()
        || entry.compression() != CompressionMethod::Stored
        || !entry.is_file()
        || entry.extra_data().is_some_and(|extra| !extra.is_empty())
        || entry.unix_mode().is_some_and(|mode| {
            let kind = mode & 0o170_000;
            kind != 0 && kind != 0o100_000
        })
    {
        return Err(PackageError::InvalidArchiveEntry);
    }
    if name != "manifest.json" && archive_asset_hash(name).is_none() {
        return Err(PackageError::UnexpectedArchiveEntry(name.to_owned()));
    }
    Ok(name.to_owned())
}

fn archive_asset_hash(name: &str) -> Option<AssetHash> {
    let hash = name.strip_prefix("assets/")?.strip_suffix(".bin")?;
    AssetHash::parse(hash.to_owned()).ok()
}

fn validate_manifest_budgets(manifest: &Manifest) -> Result<(), PackageError> {
    let declared_asset_bytes = manifest
        .assets
        .values()
        .try_fold(0_u64, |total, descriptor| {
            total.checked_add(descriptor.byte_length)
        })
        .ok_or(PackageError::AggregateAssetLimitExceeded)?;
    if declared_asset_bytes > PACKAGE_LIMITS_V1.aggregate_asset_bytes {
        return Err(PackageError::AggregateAssetLimitExceeded);
    }
    validate_snapshot_budget_fields(&manifest.snapshot)?;
    for descriptor in manifest.assets.values() {
        ensure_string_budget(&descriptor.media_type)?;
    }
    Ok(())
}

/// Returns manifest metadata without cloning embedded bytes.
#[must_use]
pub fn asset_descriptors(assets: &AssetStore) -> BTreeMap<AssetHash, AssetDescriptor> {
    assets
        .iter()
        .map(|(hash, asset)| {
            (
                hash.clone(),
                AssetDescriptor {
                    media_type: asset.media_type.clone(),
                    byte_length: asset.bytes.len() as u64,
                    raster: asset.raster.clone(),
                },
            )
        })
        .collect()
}

fn validate_package_budgets(
    snapshot: &ProjectSnapshot,
    assets: &AssetStore,
) -> Result<(), PackageError> {
    validate_document_saveability(
        &snapshot.document,
        snapshot.revision,
        &asset_descriptors(assets),
    )
}

#[derive(Serialize)]
struct SnapshotRef<'a> {
    document: &'a Document,
    revision: Revision,
}
#[derive(Serialize)]
struct ManifestRef<'a> {
    schema_version: u32,
    snapshot: SnapshotRef<'a>,
    assets: &'a BTreeMap<AssetHash, AssetDescriptor>,
}

/// Checks package budgets before admitting canonical changes.
/// Descriptors must describe trusted existing or pending embedded bytes.
///
/// # Errors
/// Returns an exceeded budget or serialization error.
pub fn validate_document_saveability(
    document: &Document,
    revision: Revision,
    assets: &BTreeMap<AssetHash, AssetDescriptor>,
) -> Result<(), PackageError> {
    validate_document_budget_fields(document)?;
    if assets.len() >= MAX_ARCHIVE_ENTRIES {
        return Err(PackageError::ArchiveEntryLimitExceeded);
    }
    let mut total = 0_u64;
    for asset in assets.values() {
        ensure_string_budget(&asset.media_type)?;
        total = total
            .checked_add(asset.byte_length)
            .ok_or(PackageError::AggregateAssetLimitExceeded)?;
        if total > PACKAGE_LIMITS_V1.aggregate_asset_bytes {
            return Err(PackageError::AggregateAssetLimitExceeded);
        }
    }
    let manifest = ManifestRef {
        schema_version: SCHEMA_VERSION,
        snapshot: SnapshotRef { document, revision },
        assets,
    };
    let mut counter = ManifestByteCounter { bytes: 0 };
    if let Err(error) = serde_json::to_writer_pretty(&mut counter, &manifest) {
        return if counter.bytes > PACKAGE_LIMITS_V1.manifest_bytes {
            Err(PackageError::ManifestTooLarge)
        } else {
            Err(error.into())
        };
    }
    Ok(())
}

struct ManifestByteCounter {
    bytes: u64,
}
impl Write for ManifestByteCounter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.bytes = self.bytes.saturating_add(bytes.len() as u64);
        if self.bytes > PACKAGE_LIMITS_V1.manifest_bytes {
            return Err(std::io::Error::other("manifest budget exceeded"));
        }
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn validate_snapshot_budget_fields(snapshot: &ProjectSnapshot) -> Result<(), PackageError> {
    validate_document_budget_fields(&snapshot.document)
}

fn validate_document_budget_fields(document: &Document) -> Result<(), PackageError> {
    if document.pages.len() > PACKAGE_LIMITS_V1.pages {
        return Err(PackageError::PageLimitExceeded);
    }
    ensure_string_budget(&document.title)?;
    for master in &document.masters { ensure_string_budget(&master.name)?; }
    let mut expanded = 0_usize;
    for page in &document.pages {
        ensure_string_budget(&page.name)?;
        let inherited = page.master_id.as_ref().and_then(|id| document.masters.iter().find(|master| &master.id == id))
            .map_or(0, |master| master.objects.len());
        expanded = expanded.checked_add(page.objects.len()).and_then(|count| count.checked_add(inherited))
            .ok_or(PackageError::ExpandedObjectLimitExceeded)?;
        if expanded > PACKAGE_LIMITS_V1.objects { return Err(PackageError::ExpandedObjectLimitExceeded); }
    }

    let objects = document.authored_objects();
    let mut object_count = 0_usize;
    let mut story_characters = 0_usize;
    for object in objects {
        object_count = object_count
            .checked_add(1)
            .ok_or(PackageError::ObjectLimitExceeded)?;
        if object_count > PACKAGE_LIMITS_V1.objects {
            return Err(PackageError::ObjectLimitExceeded);
        }
        ensure_string_budget(&object.name)?;
        if let ObjectKind::Text {
            text, style, story, ..
        } = &object.kind
        {
            ensure_string_budget(text)?;
            validate_text_style_budget_fields(style)?;
            for run in &story.inline_spans {
                if let Some(family) = &run.style.font_family {
                    ensure_string_budget(family)?;
                }
            }
            story_characters = story_characters
                .checked_add(text.chars().count())
                .ok_or(PackageError::StoryLimitExceeded)?;
            if story_characters > PACKAGE_LIMITS_V1.story_characters {
                return Err(PackageError::StoryLimitExceeded);
            }
        }
        if let ObjectKind::Table { table } = &object.kind {
            validate_text_style_budget_fields(&table.style)?;
            for formatting in &table.cell_formatting {
                if let Some(family) = &formatting.style.font_family {ensure_string_budget(family)?;}
            }
            for cell in &table.cells {
                ensure_string_budget(cell)?;
                story_characters = story_characters
                    .checked_add(cell.chars().count())
                    .ok_or(PackageError::StoryLimitExceeded)?;
                if story_characters > PACKAGE_LIMITS_V1.story_characters {
                    return Err(PackageError::StoryLimitExceeded);
                }
            }
        }
    }
    Ok(())
}

fn validate_text_style_budget_fields(style: &crate::model::TextStyle) -> Result<(), PackageError> {
    ensure_string_budget(&style.font_family)?;
    for binding in &style.exact_font_families {
        ensure_string_budget(&binding.font_family)?;
    }
    Ok(())
}

fn ensure_string_budget(value: &str) -> Result<(), PackageError> {
    if value.len() > PACKAGE_LIMITS_V1.string_bytes {
        Err(PackageError::StringLimitExceeded)
    } else {
        Ok(())
    }
}

fn read_regular_file_bounded(
    path: &Path,
    maximum: u64,
    too_large: PackageError,
) -> Result<Vec<u8>, PackageError> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
        return Err(PackageError::NotRegularFile(path.to_path_buf()));
    }
    if metadata.len() > maximum {
        return Err(too_large);
    }
    let mut bytes = Vec::with_capacity(usize::try_from(metadata.len().min(64 * 1024)).unwrap_or(0));
    File::open(path)?
        .take(maximum.saturating_add(1))
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > maximum {
        return Err(too_large);
    }
    Ok(bytes)
}

fn validate_json_depth(bytes: &[u8]) -> Result<(), PackageError> {
    let mut depth = 0_usize;
    let mut in_string = false;
    let mut escaped = false;
    for &byte in bytes {
        if in_string {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                in_string = false;
            }
            continue;
        }
        match byte {
            b'"' => in_string = true,
            b'{' | b'[' => {
                depth = depth
                    .checked_add(1)
                    .ok_or(PackageError::JsonDepthExceeded)?;
                if depth > PACKAGE_LIMITS_V1.json_depth {
                    return Err(PackageError::JsonDepthExceeded);
                }
            }
            b'}' | b']' => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    Ok(())
}

fn validate_snapshot_assets(
    snapshot: &ProjectSnapshot,
    assets: &AssetStore,
) -> Result<(), PackageError> {
    snapshot.document.validate()?;
    for chain in &snapshot.document.exact_font_fallbacks {
        for face in &chain.faces {
            validate_font_face_binding(face, assets)?;
        }
    }
    let objects = snapshot
        .document
        .authored_objects();
    for object in objects {
        match &object.kind {
            ObjectKind::Image { asset, .. } if !assets.contains(asset) => {
                return Err(PackageError::MissingAsset(asset.clone()));
            }
            ObjectKind::Text { style, .. } => validate_font_face_asset(style, assets)?,
            ObjectKind::Table { table } => validate_font_face_asset(&table.style, assets)?,
            _ => {}
        }
    }
    Ok(())
}

fn validate_font_face_asset(
    style: &crate::model::TextStyle,
    assets: &AssetStore,
) -> Result<(), PackageError> {
    if let Some(font_face) = &style.font_face {
        validate_font_face_binding(font_face, assets)?;
    }
    for variant in &style.exact_font_variants {
        validate_font_face_binding(&variant.face, assets)?;
    }
    for binding in &style.exact_font_families {
        validate_font_face_binding(&binding.face, assets)?;
    }
    Ok(())
}

fn validate_font_face_binding(
    font_face: &crate::model::FontFaceBinding,
    assets: &AssetStore,
) -> Result<(), PackageError> {
    let asset = assets
        .get(&font_face.source)
        .ok_or_else(|| PackageError::MissingAsset(font_face.source.clone()))?;
    if !is_supported_font_media_type(&asset.media_type) {
        return Err(PackageError::InvalidFontMediaType {
            source: font_face.source.clone(),
            media_type: asset.media_type.clone(),
        });
    }
    if Face::parse(&asset.bytes, font_face.face_index).is_err() {
        return Err(PackageError::InvalidFontFace {
            source: font_face.source.clone(),
            face_index: font_face.face_index,
        });
    }
    Ok(())
}

fn sibling_stage_path(destination: &Path) -> Result<PathBuf, PackageError> {
    sibling_named_path(destination, "stage")
}

fn sibling_named_path(destination: &Path, kind: &str) -> Result<PathBuf, PackageError> {
    let parent = destination
        .parent()
        .ok_or(PackageError::DestinationHasNoParent)?;
    let name = destination
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or(PackageError::InvalidDestinationName)?;
    let sequence = STAGE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    Ok(parent.join(format!(".{name}.{kind}-{}-{sequence}", std::process::id())))
}

#[derive(Debug)]
pub enum PackageError {
    Io(std::io::Error),
    Zip(zip::result::ZipError),
    Json(serde_json::Error),
    InvalidModel(ModelError),
    UnsupportedSchema {
        found: u32,
        supported: u32,
    },
    DestinationExists(PathBuf),
    DestinationChanged,
    DestinationHasNoParent,
    ParentMissing(PathBuf),
    InvalidDestinationName,
    InvalidFingerprint,
    StageCollision(PathBuf),
    MissingAsset(AssetHash),
    InvalidFontMediaType {
        source: AssetHash,
        media_type: String,
    },
    InvalidFontFace {
        source: AssetHash,
        face_index: u32,
    },
    AssetHashMismatch(AssetHash),
    AssetLengthMismatch {
        hash: AssetHash,
        expected: u64,
        actual: u64,
    },
    AssetTooLarge(AssetHash),
    ManifestTooLarge,
    JsonDepthExceeded,
    StringLimitExceeded,
    PageLimitExceeded,
    ObjectLimitExceeded,
    ExpandedObjectLimitExceeded,
    StoryLimitExceeded,
    AggregateAssetLimitExceeded,
    ArchiveEntryLimitExceeded,
    ArchiveTooLarge,
    ArchiveChangedDuringRead,
    InterruptedSavePresent(PathBuf),
    InvalidSaveJournal,
    InterruptedSaveRecoveryReadOnly,
    InterruptedSaveRecoveryUnsupported,
    InterruptedSaveRecoveryAmbiguous,
    InterruptedSaveRecoveryChanged,
    InterruptedSaveRecoveryChoiceUnavailable,
    RandomIdentityUnavailable,
    AtomicExchangeUnavailable,
    WindowsAtomicReplace {
        state: WindowsReplaceState,
        source: std::io::Error,
    },
    InvalidArchiveEntry,
    UnexpectedArchiveEntry(String),
    DuplicateArchiveEntry(String),
    MissingManifest,
    UnexpectedAsset(AssetHash),
    ArchiveEntryTooLarge(String),
    ArchiveEntryLengthMismatch {
        path: String,
        expected: u64,
        actual: u64,
    },
    NotRegularFile(PathBuf),
    StageValidationMismatch,
    CommittedValidationMismatch,
    RollbackFailed {
        commit: std::io::Error,
        rollback: std::io::Error,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WindowsReplaceState {
    Unchanged,
    Restored,
    Ambiguous,
}

impl From<std::io::Error> for PackageError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<zip::result::ZipError> for PackageError {
    fn from(error: zip::result::ZipError) -> Self {
        Self::Zip(error)
    }
}

impl From<serde_json::Error> for PackageError {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error)
    }
}

impl From<ModelError> for PackageError {
    fn from(error: ModelError) -> Self {
        Self::InvalidModel(error)
    }
}

impl std::fmt::Display for PackageError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::PageLimitExceeded => write!(
                formatter,
                "This publication supports at most 500 pages. Remove a page before adding another."
            ),
            Self::ObjectLimitExceeded => write!(
                formatter,
                "This publication supports at most 50,000 objects. Remove objects before adding more."
            ),
            Self::ExpandedObjectLimitExceeded => formatter.write_str("Master assignments produce more than 50,000 published objects. Reduce Master content or apply it to fewer Pages."),
            Self::StoryLimitExceeded => write!(
                formatter,
                "This publication supports at most 5,000,000 text characters. Shorten text before adding more."
            ),
            Self::ManifestTooLarge => write!(
                formatter,
                "This publication has reached its saved document size limit. Remove content before adding more."
            ),
            Self::AggregateAssetLimitExceeded => write!(
                formatter,
                "Embedded pictures and fonts exceed the 4 GiB publication limit. Use smaller resources."
            ),
            Self::UnsupportedSchema { found, supported } => write!(
                formatter,
                "package schema {found} is unsupported; this build supports schema {supported}"
            ),
            other => write!(formatter, "package operation failed: {other:?}"),
        }
    }
}

impl std::error::Error for PackageError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Zip(error) => Some(error),
            Self::Json(error) => Some(error),
            Self::InvalidModel(error) => Some(error),
            Self::WindowsAtomicReplace { source, .. } => Some(source),
            _ => None,
        }
    }
}

#[cfg(test)]
mod budget_tests {
    use super::{
        AssetStore, PACKAGE_LIMITS_V1, PackageError, ProjectSnapshot, STAGE_SEQUENCE,
        hash_archive_file, open_hashed_archive, save_new_package, validate_json_depth,
    };
    use crate::model::{Document, DocumentId, Insets, PhysicalSize, Revision};
    use std::fs::{self, OpenOptions};
    use std::io::{Seek, SeekFrom, Write};
    use std::sync::atomic::Ordering;

    #[test]
    fn first_save_does_not_replace_a_racing_destination() {
        let sequence = STAGE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "publisher-first-save-{}-{sequence}",
            std::process::id()
        ));
        fs::create_dir(&root).unwrap();
        let destination = root.join("new.publisher-v2");
        let snapshot = ProjectSnapshot {
            document: Document::new(
                DocumentId::parse("first-save").unwrap(),
                "First save",
                PhysicalSize::new(215_900, 279_400).unwrap(),
                Insets::uniform(3_175).unwrap(),
            )
            .unwrap(),
            revision: Revision::initial(),
        };
        let result = super::save_new_package_with(
            &destination,
            &snapshot,
            &AssetStore::default(),
            |_, validated| {
                if validated == super::FirstSaveFaultPoint::Validated {
                    fs::write(&destination, b"concurrent publication")?;
                }
                Ok(())
            },
        );
        let bytes = fs::read(&destination).unwrap();
        fs::remove_dir_all(root).unwrap();
        assert!(
            result.is_err(),
            "first Save must refuse a concurrent destination"
        );
        assert_eq!(bytes, b"concurrent publication");
    }

    #[cfg(windows)]
    #[test]
    fn first_save_holds_stage_through_write_validation_and_cleans_failed_attempt() {
        let sequence = STAGE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "publisher-first-save-handle-{}-{sequence}",
            std::process::id()
        ));
        fs::create_dir(&root).unwrap();
        let destination = root.join("new.publisher-v2");
        let snapshot = ProjectSnapshot {
            document: Document::new(
                DocumentId::parse("first-save").unwrap(),
                "First save",
                PhysicalSize::new(215_900, 279_400).unwrap(),
                Insets::uniform(3_175).unwrap(),
            )
            .unwrap(),
            revision: Revision::initial(),
        };
        for fail_at in [
            super::FirstSaveFaultPoint::Created,
            super::FirstSaveFaultPoint::ManifestWritten,
            super::FirstSaveFaultPoint::Validated,
        ] {
            let result = super::save_new_package_with(
                &destination,
                &snapshot,
                &AssetStore::default(),
                |stage, validated| {
                    assert!(fs::rename(stage, root.join("substituted")).is_err());
                    assert!(fs::write(stage, b"substituted bytes").is_err());
                    assert!(fs::remove_file(stage).is_err());
                    if validated == fail_at {
                        return Err(PackageError::Io(std::io::Error::other(
                            "injected first-Save interruption",
                        )));
                    }
                    Ok(())
                },
            );
            assert!(result.is_err());
            assert!(!destination.exists());
            assert_eq!(
                fs::read_dir(&root).unwrap().count(),
                0,
                "abandoned exact stage is cleaned"
            );
        }
        save_new_package(&destination, &snapshot, &AssetStore::default()).unwrap();
        assert_eq!(
            super::open_package(&destination).unwrap().snapshot,
            snapshot
        );
        assert_eq!(fs::read_dir(&root).unwrap().count(), 1);
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn first_save_process_exit_preserves_staging_and_allows_retry() {
        use std::os::windows::process::CommandExt;

        for point in ["created", "manifest", "validated"] {
            let sequence = STAGE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let root = std::env::temp_dir().join(format!(
                "publisher-first-save-exit-{}-{sequence}",
                std::process::id()
            ));
            fs::create_dir(&root).unwrap();
            let destination = root.join("new.publisher-v2");
            let marker = root.join("reached.txt");
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--ignored",
                    "--exact",
                    "package::budget_tests::first_save_exit_child",
                ])
                .env("PUBLISHER_FIRST_SAVE_EXIT_DESTINATION", &destination)
                .env("PUBLISHER_FIRST_SAVE_EXIT_POINT", point)
                .env("PUBLISHER_FIRST_SAVE_EXIT_MARKER", &marker)
                .creation_flags(0x0800_0000) // CREATE_NO_WINDOW for the test-only child.
                .output()
                .unwrap();
            assert_eq!(output.status.code(), Some(91), "{point}: {output:?}");
            let stage = std::path::PathBuf::from(fs::read_to_string(&marker).unwrap());
            assert_eq!(stage.parent(), Some(root.as_path()));
            assert!(
                !destination.exists(),
                "{point}: first Save installed too soon"
            );
            let staged_bytes = fs::read(&stage).unwrap();
            let snapshot = first_save_exit_snapshot();
            if point == "validated" {
                assert_eq!(super::open_package(&stage).unwrap().snapshot, snapshot);
            } else {
                assert!(super::open_package(&stage).is_err());
            }
            // Process exit skips Rust Drop. Keep that exact orphan, including incomplete
            // bytes, while a new attempt creates and installs its own distinct stage.
            save_new_package(&destination, &snapshot, &AssetStore::default()).unwrap();
            assert_eq!(
                super::open_package(&destination).unwrap().snapshot,
                snapshot
            );
            assert_eq!(fs::read(&stage).unwrap(), staged_bytes);
            assert_eq!(fs::read_dir(&root).unwrap().count(), 3);
            fs::remove_dir_all(root).unwrap();
        }
    }

    #[cfg(windows)]
    fn first_save_exit_snapshot() -> ProjectSnapshot {
        ProjectSnapshot {
            document: Document::new(
                DocumentId::parse("first-save-exit").unwrap(),
                "First Save interrupted",
                PhysicalSize::new(215_900, 279_400).unwrap(),
                Insets::uniform(3_175).unwrap(),
            )
            .unwrap(),
            revision: Revision::initial(),
        }
    }

    #[cfg(windows)]
    #[test]
    #[ignore = "spawned by first_save_process_exit_preserves_staging_and_allows_retry"]
    fn first_save_exit_child() {
        let destination = std::path::PathBuf::from(
            std::env::var_os("PUBLISHER_FIRST_SAVE_EXIT_DESTINATION").unwrap(),
        );
        let point = std::env::var("PUBLISHER_FIRST_SAVE_EXIT_POINT").unwrap();
        let marker = std::env::var_os("PUBLISHER_FIRST_SAVE_EXIT_MARKER").unwrap();
        super::save_new_package_with(
            &destination,
            &first_save_exit_snapshot(),
            &AssetStore::default(),
            |stage, boundary| {
                let reached = match boundary {
                    super::FirstSaveFaultPoint::Created => "created",
                    super::FirstSaveFaultPoint::ManifestWritten => "manifest",
                    super::FirstSaveFaultPoint::AssetWritten => "asset",
                    super::FirstSaveFaultPoint::Validated => "validated",
                };
                if reached == point {
                    let mut receipt = fs::File::create(&marker).unwrap();
                    receipt
                        .write_all(stage.to_str().unwrap().as_bytes())
                        .unwrap();
                    receipt.sync_all().unwrap();
                    // Exit without unwinding: CreatedFile::drop must not run.
                    std::process::exit(91);
                }
                Ok(())
            },
        )
        .expect("child must exit at the requested boundary");
        panic!("first Save child did not exit at {point}");
    }

    #[cfg(windows)]
    #[test]
    fn first_save_refuses_unsupported_destination_before_creating_a_stage() {
        let snapshot = ProjectSnapshot {
            document: Document::new(
                DocumentId::parse("first-save").unwrap(),
                "First save",
                PhysicalSize::new(215_900, 279_400).unwrap(),
                Insets::uniform(3_175).unwrap(),
            )
            .unwrap(),
            revision: Revision::initial(),
        };
        for path in [
            r"relative.publisher-v2",
            r"\\server\share\book.publisher-v2",
        ] {
            let result = super::save_new_package_with(
                std::path::Path::new(path),
                &snapshot,
                &AssetStore::default(),
                |_, _| panic!("unsupported destination reached staging"),
            );
            assert!(result.is_err());
        }
    }

    #[test]
    fn json_depth_ignores_braces_inside_strings() {
        let json = br#"{"text":"{{[[not structure]]}}","value":[1]}"#;
        assert!(validate_json_depth(json).is_ok());
    }

    #[test]
    fn json_depth_stops_before_deserialization() {
        let mut json = vec![b'['; PACKAGE_LIMITS_V1.json_depth + 1];
        json.extend(std::iter::repeat_n(b']', PACKAGE_LIMITS_V1.json_depth + 1));
        assert!(matches!(
            validate_json_depth(&json),
            Err(PackageError::JsonDepthExceeded)
        ));
    }

    #[test]
    fn open_rejects_an_archive_changed_after_its_initial_fingerprint() {
        let sequence = STAGE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "publisher-v2-open-race-{}-{sequence}",
            std::process::id()
        ));
        fs::create_dir(&root).expect("create race fixture directory");
        let package = root.join("race.publisher-v2");
        let snapshot = ProjectSnapshot {
            document: Document::new(
                DocumentId::parse("open-race").expect("valid document id"),
                "Open race",
                PhysicalSize::new(215_900, 279_400).expect("valid trim"),
                Insets::uniform(3_175).expect("valid bleed"),
            )
            .expect("valid document"),
            revision: Revision::initial(),
        };
        save_new_package(&package, &snapshot, &AssetStore::default()).expect("write race fixture");

        let bytes = fs::read(&package).expect("read race fixture");
        let central_header = bytes
            .windows(4)
            .position(|window| window == b"PK\x01\x02")
            .expect("central directory header");
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&package)
            .expect("open mutable race fixture");
        let initial_fingerprint = hash_archive_file(&mut file).expect("hash initial archive");
        let changed_offset = central_header + 12;
        file.seek(SeekFrom::Start(changed_offset as u64))
            .expect("seek to harmless central timestamp byte");
        file.write_all(&[bytes[changed_offset] ^ 1])
            .expect("change archive between fingerprint and validation");
        file.sync_all().expect("flush changed archive");

        let result = open_hashed_archive(file, initial_fingerprint);
        fs::remove_dir_all(&root).expect("remove race fixture directory");
        assert!(matches!(
            result,
            Err(PackageError::ArchiveChangedDuringRead)
        ));
    }
}
