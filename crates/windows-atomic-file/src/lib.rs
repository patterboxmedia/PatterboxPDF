//! Safe Windows handle-bound primitives for same-directory publication replacement.
//!
//! Every source mutation originates from an exact regular-file handle and names its destination
//! relative to an exact pinned parent-directory handle. The lexical ancestor chain is validated
//! before the parent is bound and its identity is checked again after binding; later ancestor
//! renames cannot redirect a relative mutation. This layer never enables replacement or POSIX
//! rename flags. Only fixed local NTFS paths and the narrow Cloud Files non-name-surrogate tag
//! family are accepted; an unreadable placeholder fails closed. It flushes exact writable files
//! where supported, but does not claim directory-metadata or power-loss durability.

#![cfg(windows)]

use std::ffi::{OsStr, OsString};
use std::fmt;
use std::fs::{File, OpenOptions};
use std::io;
use std::mem::{MaybeUninit, offset_of, size_of};
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
use std::os::windows::io::{AsRawHandle, FromRawHandle};
use std::path::{Component, Path, PathBuf, Prefix};
use windows_sys::Wdk::Storage::FileSystem::{FileRenameInformation, NtSetInformationFile};
use windows_sys::Win32::Foundation::{
    GENERIC_READ, GENERIC_WRITE, INVALID_HANDLE_VALUE, RtlNtStatusToDosError,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, DELETE, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT,
    FILE_ATTRIBUTE_TAG_INFO, FILE_DISPOSITION_INFO, FILE_FLAG_BACKUP_SEMANTICS,
    FILE_FLAG_OPEN_REPARSE_POINT, FILE_ID_INFO, FILE_LIST_DIRECTORY, FILE_NAME_NORMALIZED,
    FILE_READ_ATTRIBUTES, FILE_RENAME_INFO, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
    FILE_STANDARD_INFO, FILE_TRAVERSE, FileAttributeTagInfo, FileDispositionInfo, FileIdInfo,
    FileStandardInfo, FlushFileBuffers, GetDriveTypeW, GetFileInformationByHandleEx,
    GetFinalPathNameByHandleW, GetVolumeInformationByHandleW, OPEN_EXISTING,
    SetFileInformationByHandle, VOLUME_NAME_DOS,
};
use windows_sys::Win32::System::IO::IO_STATUS_BLOCK;

const NAME_SURROGATE_BIT: u32 = 0x2000_0000;
const CLOUD_TAG_FAMILY_MASK: u32 = 0xffff_0fff;
const CLOUD_TAG_FAMILY: u32 = 0x9000_001a;
const DRIVE_FIXED: u32 = 3;
const MAX_LEAF_UTF16: usize = 255;
const MAX_FINAL_PATH_UTF16: usize = 32_768;

/// Stable identity captured from an already-open file handle.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FileIdentity {
    pub volume_serial_number: u64,
    pub file_id: [u8; 16],
    pub byte_length: u64,
}

/// Filesystem state reached when a no-clobber replacement fails.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReplaceState {
    Unchanged,
    Restored,
    Ambiguous,
}

/// Structured failure from the two-rename replacement state machine.
#[derive(Debug)]
pub struct ReplaceError {
    state: ReplaceState,
    source: io::Error,
    rollback: Option<io::Error>,
}

impl ReplaceError {
    #[must_use]
    pub const fn state(&self) -> ReplaceState {
        self.state
    }

    #[must_use]
    pub fn rollback_error(&self) -> Option<&io::Error> {
        self.rollback.as_ref()
    }

    #[must_use]
    pub fn into_io_error(self) -> io::Error {
        io::Error::new(self.source.kind(), self.to_string())
    }
}

impl fmt::Display for ReplaceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "Windows replacement stopped at {:?}: {}",
            self.state, self.source
        )?;
        if let Some(rollback) = &self.rollback {
            write!(formatter, "; restore failed: {rollback}")?;
        }
        Ok(())
    }
}

impl std::error::Error for ReplaceError {}

/// Keeps the exact destination parent pinned while validating every lexical ancestor.
#[derive(Debug)]
pub struct PinnedDirectory {
    parent: PathBuf,
    parent_handle: File,
}

/// Exact source handles prepared for a no-clobber two-rename commit.
#[derive(Debug)]
pub struct PreparedReplacement<'a> {
    directory: &'a PinnedDirectory,
    destination: BoundFile,
    staged: BoundFile,
    destination_leaf: OsString,
    prior_leaf: OsString,
}

/// A replacement whose exact old destination has moved to the unique prior name.
#[derive(Debug)]
pub struct PriorPreservedReplacement<'a> {
    directory: &'a PinnedDirectory,
    destination: BoundFile,
    staged: BoundFile,
    destination_leaf: OsString,
    prior_leaf: OsString,
}

/// Exact committed handles retained until semantic verification and receipt finalization complete.
#[derive(Debug)]
pub struct CommittedReplacement {
    destination: BoundFile,
    prior: BoundFile,
    destination_path: PathBuf,
    prior_path: PathBuf,
}

/// Exact moved source retained so callers can verify receipt bytes from the same handle.
#[derive(Debug)]
pub struct MovedFile {
    file: BoundFile,
    destination_path: PathBuf,
}

/// An exact regular file held beneath a pinned ancestor chain.
#[derive(Debug)]
pub struct PinnedFile {
    file: BoundFile,
    path: PathBuf,
}

/// An exact source handle prepared for a no-replace sibling move.
#[derive(Debug)]
pub struct PreparedMove<'a> {
    directory: &'a PinnedDirectory,
    source: BoundFile,
    destination_leaf: OsString,
    destination_path: PathBuf,
}

#[derive(Debug)]
struct BoundFile {
    file: File,
    identity: FileIdentity,
}

/// An exclusively created sibling retained through writing, validation and installation.
/// Failed or abandoned writes remove only this exact file through its held handle.
#[derive(Debug)]
pub struct CreatedFile<'a> {
    directory: &'a PinnedDirectory,
    file: Option<BoundFile>,
    destination_leaf: OsString,
    destination_path: PathBuf,
}

impl<'a> CreatedFile<'a> {
    /// Duplicates the retained read/write handle for archive writing and validation.
    ///
    /// # Errors
    /// Returns an operating-system error if handle duplication fails.
    pub fn file(&self) -> io::Result<File> {
        self.file
            .as_ref()
            .ok_or_else(|| io::Error::other("created file handle is missing"))?
            .file
            .try_clone()
    }

    /// Binds a replacement without releasing or reopening the newly written stage.
    /// The caller must retain this creation guard until commit succeeds, then call
    /// `retain` so abandonment cleanup cannot remove the installed generation.
    ///
    /// # Errors
    /// Returns an error for an occupied prior name, unsafe destination, or failed
    /// handle duplication, flush or identity inspection.
    pub fn prepare_replace(&self, prior: &Path) -> io::Result<PreparedReplacement<'a>> {
        let prior_leaf = self.directory.leaf_for(prior)?;
        match prior.symlink_metadata() {
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Ok(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "prior path is occupied",
                ));
            }
            Err(error) => return Err(error),
        }
        let destination = BoundFile::open(&self.destination_path, false)?;
        require_direct_child(&destination.file, &self.directory.parent_handle)?;
        let file = self.file()?;
        file.sync_all()?;
        let identity = file_identity(&file)?;
        Ok(PreparedReplacement {
            directory: self.directory,
            destination,
            staged: BoundFile { file, identity },
            destination_leaf: self.destination_leaf.clone(),
            prior_leaf,
        })
    }

    /// Relinquishes abandonment cleanup after an independently held replacement
    /// transition has installed this exact generation. No path is deleted.
    pub fn retain(mut self) {
        drop(self.file.take());
    }

    /// Installs the exact written file, refusing any occupied destination.
    ///
    /// # Errors
    /// Returns an error if flush, identity inspection or the no-replace rename fails.
    pub fn commit(mut self) -> io::Result<MovedFile> {
        let source = self
            .file
            .as_mut()
            .ok_or_else(|| io::Error::other("created file handle is missing"))?;
        source.flush()?;
        source.identity = file_identity(&source.file)?;
        rename_handle(
            &source.file,
            &self.directory.parent_handle,
            &self.destination_leaf,
        )?;
        Ok(MovedFile {
            file: self
                .file
                .take()
                .ok_or_else(|| io::Error::other("installed file handle is missing"))?,
            destination_path: self.destination_path.clone(),
        })
    }
}

impl Drop for CreatedFile<'_> {
    fn drop(&mut self) {
        if let Some(file) = &self.file {
            // Cleanup failure leaves an identifiable stage; it never removes an unrelated path.
            let _ignored = delete_exact_file(&file.file);
        }
    }
}

impl PinnedDirectory {
    /// Exclusively creates a writable stage bound to this supported destination parent.
    ///
    /// # Errors
    /// Returns an error for invalid siblings, occupied stages or unsafe file handles.
    pub fn create_file_for_destination<'a>(
        &'a self,
        stage: &Path,
        destination: &Path,
    ) -> io::Result<CreatedFile<'a>> {
        let _stage_leaf = self.leaf_for(stage)?;
        let destination_leaf = self.leaf_for(destination)?;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .access_mode(GENERIC_READ | GENERIC_WRITE | DELETE)
            .share_mode(FILE_SHARE_READ)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
            .create_new(true)
            .open(stage)?;
        require_kind(&file, false)?;
        require_direct_child(&file, &self.parent_handle)?;
        let identity = file_identity(&file)?;
        Ok(CreatedFile {
            directory: self,
            file: Some(BoundFile { file, identity }),
            destination_leaf,
            destination_path: destination.to_path_buf(),
        })
    }
    /// Validates the lexical ancestor chain and pins the exact destination parent.
    ///
    /// # Errors
    ///
    /// Returns an error for a non-local/non-NTFS path, an inaccessible ancestor, an unsupported
    /// reparse point, or a parent that cannot be held without delete sharing.
    pub fn for_destination(destination: &Path) -> io::Result<Self> {
        Self::bind(destination, false)
    }

    /// Pins a directory with deletion rights from the outset. Its held handle prevents
    /// replacement until `remove_empty` either deletes that exact empty directory or fails.
    ///
    /// # Errors
    /// Returns an error for unsupported ancestry, sharing conflicts or inaccessible paths.
    pub fn for_removable_directory(directory: &Path) -> io::Result<Self> {
        Self::bind(&directory.join("identity-probe"), true)
    }

    /// Identity of this exact directory, including the volume identifier.
    ///
    /// # Errors
    /// Returns an operating-system error if handle inspection fails.
    pub fn identity(&self) -> io::Result<FileIdentity> {
        file_identity(&self.parent_handle)
    }

    /// Deletes only this exact directory and only if it is empty. No children are removed.
    ///
    /// # Errors
    /// Returns an error for a nonempty directory or a handle without deletion rights.
    pub fn remove_empty(self) -> io::Result<()> {
        delete_exact_file(&self.parent_handle)
    }

    fn bind(destination: &Path, removable: bool) -> io::Result<Self> {
        validate_absolute_path(destination)?;
        require_local_fixed_drive(destination)?;
        validate_leaf(destination.file_name().ok_or_else(invalid_name)?)?;
        let parent = destination.parent().ok_or_else(invalid_name)?.to_path_buf();
        let mut chain = parent.ancestors().collect::<Vec<_>>();
        chain.reverse();
        if chain.is_empty() {
            return Err(invalid_name());
        }
        for ancestor in &chain {
            validate_ancestor(ancestor).map_err(|error| {
                io::Error::new(
                    error.kind(),
                    format!(
                        "could not validate ancestor {}: {error}",
                        ancestor.display()
                    ),
                )
            })?;
        }
        let parent_handle = open_handle(
            &parent,
            FILE_LIST_DIRECTORY
                | FILE_TRAVERSE
                | FILE_READ_ATTRIBUTES
                | if removable { DELETE } else { 0 },
            FILE_FLAG_BACKUP_SEMANTICS,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
        )
        .map_err(|error| {
            io::Error::new(
                error.kind(),
                format!("could not pin parent {}: {error}", parent.display()),
            )
        })?;
        require_kind(&parent_handle, true)?;
        require_ntfs(&parent_handle)?;
        require_handle_path(&parent_handle, &parent)?;
        let rebound_parent = open_handle(
            &parent,
            FILE_READ_ATTRIBUTES,
            FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
        )?;
        require_kind(&rebound_parent, true)?;
        if file_identity(&rebound_parent)? != file_identity(&parent_handle)? {
            return Err(io::Error::other(
                "publication parent changed while it was being pinned",
            ));
        }
        Ok(Self {
            parent,
            parent_handle,
        })
    }

    /// Opens and binds the destination and staged archives before either name is mutated.
    ///
    /// # Errors
    ///
    /// Returns an error when a path escapes the pinned parent, the prior name is occupied, or an
    /// exact regular-file handle cannot be opened, flushed, or inspected safely.
    pub fn prepare_replace<'a>(
        &'a self,
        destination: &Path,
        staged: &Path,
        prior: &Path,
    ) -> io::Result<PreparedReplacement<'a>> {
        let destination_leaf = self.leaf_for(destination)?;
        let _staged_leaf = self.leaf_for(staged)?;
        let prior_leaf = self.leaf_for(prior)?;
        match prior.symlink_metadata() {
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Ok(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "prior path is occupied",
                ));
            }
            Err(error) => return Err(error),
        }
        let destination = BoundFile::open(destination, false)?;
        let staged = BoundFile::open(staged, true)?;
        require_direct_child(&destination.file, &self.parent_handle)?;
        require_direct_child(&staged.file, &self.parent_handle)?;
        staged.flush()?;
        Ok(PreparedReplacement {
            directory: self,
            destination,
            staged,
            destination_leaf,
            prior_leaf,
        })
    }

    /// Moves one exact regular file to a vacant sibling and retains its handle for verification.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid or cross-directory paths, unsafe file kinds, sharing
    /// conflicts, or an occupied destination name.
    pub fn prepare_move_file_no_replace<'a>(
        &'a self,
        source: &Path,
        destination: &Path,
    ) -> io::Result<PreparedMove<'a>> {
        let _source_leaf = self.leaf_for(source)?;
        let destination_leaf = self.leaf_for(destination)?;
        let source = BoundFile::open(source, false)?;
        require_direct_child(&source.file, &self.parent_handle)?;
        Ok(PreparedMove {
            directory: self,
            source,
            destination_leaf,
            destination_path: destination.to_path_buf(),
        })
    }

    /// Moves one exact regular file to a vacant sibling and retains its handle for verification.
    ///
    /// # Errors
    ///
    /// Returns an error if preparation or the no-replace handle rename fails.
    pub fn move_file_no_replace(&self, source: &Path, destination: &Path) -> io::Result<MovedFile> {
        self.prepare_move_file_no_replace(source, destination)?
            .commit()
    }

    /// Opens one exact regular sibling and keeps its identity bound to the pinned parent.
    ///
    /// # Errors
    ///
    /// Returns an error for an invalid/cross-directory path, unsupported file kind, or sharing
    /// conflict. The returned handle prevents concurrent writes, deletes, and renames.
    pub fn pin_file(&self, path: &Path) -> io::Result<PinnedFile> {
        let _leaf = self.leaf_for(path)?;
        let file = BoundFile::open(path, false)?;
        require_direct_child(&file.file, &self.parent_handle)?;
        Ok(PinnedFile {
            file,
            path: path.to_path_buf(),
        })
    }

    fn leaf_for(&self, path: &Path) -> io::Result<OsString> {
        validate_absolute_path(path)?;
        if path.parent() != Some(self.parent.as_path()) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "all mutation paths must share the pinned parent",
            ));
        }
        let leaf = path.file_name().ok_or_else(invalid_name)?;
        validate_leaf(leaf)?;
        Ok(leaf.to_os_string())
    }
}

impl PreparedMove<'_> {
    /// Stable generation bound by this prepared move, before any rename occurs.
    #[must_use]
    pub const fn identity(&self) -> FileIdentity {
        self.source.identity
    }

    /// Clones the exact source handle so callers can validate bytes before committing the move.
    ///
    /// # Errors
    ///
    /// Returns an operating-system error if the source handle cannot be duplicated.
    pub fn source_file(&self) -> io::Result<File> {
        self.source.file.try_clone()
    }

    /// Commits the no-replace move from the already-bound source handle.
    ///
    /// # Errors
    ///
    /// Returns an operating-system error if the destination became occupied or rename fails.
    pub fn commit(self) -> io::Result<MovedFile> {
        rename_handle(
            &self.source.file,
            &self.directory.parent_handle,
            &self.destination_leaf,
        )?;
        Ok(MovedFile {
            file: self.source,
            destination_path: self.destination_path,
        })
    }
}

impl<'a> PreparedReplacement<'a> {
    /// Clones the exact pre-commit destination handle for semantic validation.
    ///
    /// # Errors
    ///
    /// Returns an operating-system error if the handle cannot be duplicated.
    pub fn destination_file(&self) -> io::Result<File> {
        self.destination.file.try_clone()
    }

    /// Clones the exact staged handle for semantic validation.
    ///
    /// # Errors
    ///
    /// Returns an operating-system error if the handle cannot be duplicated.
    pub fn staged_file(&self) -> io::Result<File> {
        self.staged.file.try_clone()
    }

    #[must_use]
    pub const fn destination_identity(&self) -> FileIdentity {
        self.destination.identity
    }

    #[must_use]
    pub const fn staged_identity(&self) -> FileIdentity {
        self.staged.identity
    }

    /// Moves the exact destination to its vacant transaction-unique prior name.
    ///
    /// # Errors
    ///
    /// Returns [`ReplaceState::Unchanged`] if the prior name cannot be established without
    /// replacing an existing entry.
    pub fn preserve_prior(self) -> Result<PriorPreservedReplacement<'a>, ReplaceError> {
        if let Err(source) = rename_handle(
            &self.destination.file,
            &self.directory.parent_handle,
            &self.prior_leaf,
        ) {
            return Err(ReplaceError {
                state: ReplaceState::Unchanged,
                source,
                rollback: None,
            });
        }
        Ok(PriorPreservedReplacement {
            directory: self.directory,
            destination: self.destination,
            staged: self.staged,
            destination_leaf: self.destination_leaf,
            prior_leaf: self.prior_leaf,
        })
    }
}

impl PriorPreservedReplacement<'_> {
    /// Moves the exact staged generation into the now-vacant destination name.
    ///
    /// # Errors
    ///
    /// If the destination becomes occupied or the second rename fails, this attempts a no-clobber
    /// restore and reports whether the original generation was restored or the state is ambiguous.
    pub fn commit(self) -> Result<CommittedReplacement, ReplaceError> {
        let destination_path = self.directory.parent.join(&self.destination_leaf);
        let prior_path = self.directory.parent.join(&self.prior_leaf);
        if let Err(source) = rename_handle(
            &self.staged.file,
            &self.directory.parent_handle,
            &self.destination_leaf,
        ) {
            return match rename_handle(
                &self.destination.file,
                &self.directory.parent_handle,
                &self.destination_leaf,
            ) {
                Ok(()) => Err(ReplaceError {
                    state: ReplaceState::Restored,
                    source,
                    rollback: None,
                }),
                Err(rollback) => Err(ReplaceError {
                    state: ReplaceState::Ambiguous,
                    source,
                    rollback: Some(rollback),
                }),
            };
        }
        Ok(CommittedReplacement {
            destination: self.staged,
            prior: self.destination,
            destination_path,
            prior_path,
        })
    }

    /// Cancels after prior preservation and restores the original destination without clobbering.
    ///
    /// The returned structured error preserves the supplied cause and reports `Restored` when the
    /// old generation returned to the vacant destination, otherwise `Ambiguous`.
    #[must_use]
    pub fn restore_after_error(self, source: io::Error) -> ReplaceError {
        match rename_handle(
            &self.destination.file,
            &self.directory.parent_handle,
            &self.destination_leaf,
        ) {
            Ok(()) => ReplaceError {
                state: ReplaceState::Restored,
                source,
                rollback: None,
            },
            Err(rollback) => ReplaceError {
                state: ReplaceState::Ambiguous,
                source,
                rollback: Some(rollback),
            },
        }
    }
}

impl CommittedReplacement {
    /// Clones the exact committed destination handle.
    ///
    /// # Errors
    ///
    /// Returns an operating-system error if the handle cannot be duplicated.
    pub fn destination_file(&self) -> io::Result<File> {
        self.destination.file.try_clone()
    }

    /// Clones the exact retained-prior handle.
    ///
    /// # Errors
    ///
    /// Returns an operating-system error if the handle cannot be duplicated.
    pub fn prior_file(&self) -> io::Result<File> {
        self.prior.file.try_clone()
    }

    /// Reopens the destination name and proves it still names the exact committed file.
    ///
    /// # Errors
    ///
    /// Returns an error if the name cannot be opened safely or its file identity differs.
    pub fn reopened_destination_file(&self) -> io::Result<File> {
        reopen_exact_path(&self.destination_path, self.destination.identity)
    }

    /// Reopens the prior name and proves it still names the exact displaced file.
    ///
    /// # Errors
    ///
    /// Returns an error if the name cannot be opened safely or its file identity differs.
    pub fn reopened_prior_file(&self) -> io::Result<File> {
        reopen_exact_path(&self.prior_path, self.prior.identity)
    }

    /// Removes the exact displaced directory entry through its retained handle.
    ///
    /// This never truncates the displaced file. If the entry was a hard link, only the link moved
    /// into the transaction-unique prior name is removed; its other names and bytes remain.
    ///
    /// # Errors
    ///
    /// Returns an operating-system error if Windows cannot mark the exact prior handle for
    /// deletion. The committed destination remains installed.
    pub fn remove_prior(self) -> io::Result<()> {
        delete_exact_file(&self.prior.file)
    }

    #[must_use]
    pub const fn destination_identity(&self) -> FileIdentity {
        self.destination.identity
    }

    #[must_use]
    pub const fn prior_identity(&self) -> FileIdentity {
        self.prior.identity
    }
}

fn delete_exact_file(file: &File) -> io::Result<()> {
    let disposition = FILE_DISPOSITION_INFO { DeleteFile: true };
    let disposition_size = u32::try_from(size_of::<FILE_DISPOSITION_INFO>())
        .map_err(|_| io::Error::other("file disposition structure is too large"))?;
    // SAFETY: `file` is an owned, open handle created with DELETE access. The disposition
    // structure is initialized and remains alive for the duration of the call.
    if unsafe {
        SetFileInformationByHandle(
            file.as_raw_handle().cast(),
            FileDispositionInfo,
            (&raw const disposition).cast(),
            disposition_size,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

impl MovedFile {
    /// Clones the exact moved source handle.
    ///
    /// # Errors
    ///
    /// Returns an operating-system error if the handle cannot be duplicated.
    pub fn file(&self) -> io::Result<File> {
        self.file.file.try_clone()
    }

    #[must_use]
    pub const fn identity(&self) -> FileIdentity {
        self.file.identity
    }

    /// Reopens the destination name and proves it still names the exact moved file.
    ///
    /// # Errors
    ///
    /// Returns an error if the name cannot be opened safely or its file identity differs.
    pub fn reopened_destination_file(&self) -> io::Result<File> {
        reopen_exact_path(&self.destination_path, self.file.identity)
    }
}

impl PinnedFile {
    /// Deletes only the exact held file. Dropping this handle completes the deletion.
    ///
    /// # Errors
    /// Returns an operating-system error if the deletion cannot be marked.
    pub fn remove(self) -> io::Result<()> {
        delete_exact_file(&self.file.file)
    }
    /// Clones the exact held file handle for bounded content validation.
    ///
    /// # Errors
    ///
    /// Returns an operating-system error if the handle cannot be duplicated.
    pub fn file(&self) -> io::Result<File> {
        self.file.file.try_clone()
    }

    #[must_use]
    pub const fn identity(&self) -> FileIdentity {
        self.file.identity
    }

    /// Reopens the bound name and verifies that it still identifies this exact file.
    ///
    /// # Errors
    ///
    /// Returns an error if the name changed, disappeared, or became unsafe.
    pub fn reopened_file(&self) -> io::Result<File> {
        reopen_exact_path(&self.path, self.file.identity)
    }
}

impl BoundFile {
    fn open(path: &Path, writable: bool) -> io::Result<Self> {
        let access = GENERIC_READ | DELETE | if writable { GENERIC_WRITE } else { 0 };
        let file = open_handle(path, access, FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_READ)?;
        require_kind(&file, false)?;
        let identity = file_identity(&file)?;
        Ok(Self { file, identity })
    }

    fn flush(&self) -> io::Result<()> {
        // SAFETY: the handle is owned, open, and was created with GENERIC_WRITE for staged files.
        if unsafe { FlushFileBuffers(self.file.as_raw_handle().cast()) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
}

fn reopen_exact_path(path: &Path, expected: FileIdentity) -> io::Result<File> {
    // The original exact handle requests DELETE access and a staged handle can also request write
    // access, so this read-only verification handle must share both with those already-held rights.
    // The original handle itself omits write and delete sharing, therefore no third party can
    // mutate, rename, or delete the file while this check is in progress.
    let file = open_handle(
        path,
        GENERIC_READ,
        FILE_FLAG_OPEN_REPARSE_POINT,
        FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
    )?;
    require_kind(&file, false)?;
    if file_identity(&file)? != expected {
        return Err(io::Error::other(
            "publication name no longer identifies the exact held file",
        ));
    }
    Ok(file)
}

/// True only for redirecting reparse-point tags, not `OneDrive` cloud tags.
#[must_use]
pub const fn is_name_surrogate_tag(tag: u32) -> bool {
    tag & NAME_SURROGATE_BIT != 0
}

/// True for the narrowly documented Cloud Files placeholder tag family.
#[must_use]
pub const fn is_cloud_files_tag(tag: u32) -> bool {
    tag & CLOUD_TAG_FAMILY_MASK == CLOUD_TAG_FAMILY
}

fn open_handle(path: &Path, access: u32, flags: u32, share: u32) -> io::Result<File> {
    let wide = wide_local_path(path)?;
    // SAFETY: `wide` is NUL-terminated and lives across the call. The returned raw handle is
    // checked against INVALID_HANDLE_VALUE and transferred exactly once into `File` ownership.
    let raw = unsafe {
        CreateFileW(
            wide.as_ptr(),
            access,
            share,
            std::ptr::null(),
            OPEN_EXISTING,
            flags,
            std::ptr::null_mut(),
        )
    };
    if raw == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `raw` is a newly returned valid owned HANDLE and is not used elsewhere.
    Ok(unsafe { File::from_raw_handle(raw.cast()) })
}

fn validate_ancestor(path: &Path) -> io::Result<()> {
    let metadata = path.symlink_metadata()?;
    if !metadata.file_type().is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "publication ancestor is not a directory",
        ));
    }
    if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT == 0 {
        return Ok(());
    }
    let share = FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE;
    let handle = open_handle(
        path,
        0,
        FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
        share,
    )?;
    require_kind(&handle, true)
}

fn require_kind(file: &File, directory: bool) -> io::Result<()> {
    let info: FILE_ATTRIBUTE_TAG_INFO = query_info(file, FileAttributeTagInfo)?;
    if info.FileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0
        && (is_name_surrogate_tag(info.ReparseTag) || !is_cloud_files_tag(info.ReparseTag))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "unsupported reparse point in publication path",
        ));
    }
    let is_directory = info.FileAttributes & FILE_ATTRIBUTE_DIRECTORY != 0;
    if is_directory != directory {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "publication path entry has the wrong file kind",
        ));
    }
    Ok(())
}

fn require_ntfs(file: &File) -> io::Result<()> {
    let mut filesystem = [0_u16; 32];
    // SAFETY: the directory handle remains open, optional output pointers are null, and the
    // filesystem-name buffer is writable for the exact element count passed to Windows.
    let ok = unsafe {
        GetVolumeInformationByHandleW(
            file.as_raw_handle().cast(),
            std::ptr::null_mut(),
            0,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            filesystem.as_mut_ptr(),
            u32::try_from(filesystem.len()).expect("filesystem buffer length fits u32"),
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    let end = filesystem
        .iter()
        .position(|unit| *unit == 0)
        .unwrap_or(filesystem.len());
    let name = String::from_utf16(&filesystem[..end]).map_err(|_| invalid_name())?;
    if !name.eq_ignore_ascii_case("NTFS") {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "publication replacement is currently proved only on NTFS",
        ));
    }
    Ok(())
}

fn file_identity(file: &File) -> io::Result<FileIdentity> {
    let id: FILE_ID_INFO = query_info(file, FileIdInfo)?;
    let standard: FILE_STANDARD_INFO = query_info(file, FileStandardInfo)?;
    let byte_length = u64::try_from(standard.EndOfFile).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "negative file length from Windows",
        )
    })?;
    Ok(FileIdentity {
        volume_serial_number: id.VolumeSerialNumber,
        file_id: id.FileId.Identifier,
        byte_length,
    })
}

fn final_handle_path(file: &File) -> io::Result<PathBuf> {
    let flags = FILE_NAME_NORMALIZED | VOLUME_NAME_DOS;
    // SAFETY: the first call intentionally supplies no output buffer and asks Windows for the
    // required UTF-16 capacity for this valid, open handle.
    let required = unsafe {
        GetFinalPathNameByHandleW(file.as_raw_handle().cast(), std::ptr::null_mut(), 0, flags)
    };
    if required == 0 {
        return Err(io::Error::last_os_error());
    }
    let capacity = usize::try_from(required).map_err(|_| invalid_name())?;
    if capacity > MAX_FINAL_PATH_UTF16 {
        return Err(invalid_name());
    }
    let mut buffer = vec![0_u16; capacity];
    // SAFETY: `buffer` is writable for the exact capacity passed, and the handle remains open.
    let written = unsafe {
        GetFinalPathNameByHandleW(
            file.as_raw_handle().cast(),
            buffer.as_mut_ptr(),
            required,
            flags,
        )
    };
    if written == 0 || written >= required {
        return Err(if written == 0 {
            io::Error::last_os_error()
        } else {
            invalid_name()
        });
    }
    buffer.truncate(usize::try_from(written).map_err(|_| invalid_name())?);
    Ok(PathBuf::from(OsString::from_wide(&buffer)))
}

fn require_handle_path(file: &File, expected: &Path) -> io::Result<()> {
    let actual = final_handle_path(file)?;
    if !windows_paths_equal(&actual, expected) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "publication path resolves through an unsupported redirect",
        ));
    }
    Ok(())
}

fn require_direct_child(file: &File, parent: &File) -> io::Result<()> {
    let file_path = final_handle_path(file)?;
    let actual_parent = file_path.parent().ok_or_else(invalid_name)?;
    let expected_parent = final_handle_path(parent)?;
    if !windows_paths_equal(actual_parent, &expected_parent) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "publication file is not inside the exact pinned parent",
        ));
    }
    Ok(())
}

fn windows_paths_equal(left: &Path, right: &Path) -> bool {
    fn display(path: &Path) -> String {
        let path = path.to_string_lossy();
        path.strip_prefix(r"\\?\").unwrap_or(&path).to_owned()
    }
    display(left).eq_ignore_ascii_case(&display(right))
}

fn query_info<T>(file: &File, class: i32) -> io::Result<T> {
    let mut info = MaybeUninit::<T>::uninit();
    // SAFETY: the output points to writable storage of exactly `size_of::<T>()`; the selected
    // information classes above are each paired with their documented fixed-size structure.
    let ok = unsafe {
        GetFileInformationByHandleEx(
            file.as_raw_handle().cast(),
            class,
            info.as_mut_ptr().cast(),
            u32::try_from(size_of::<T>()).expect("Windows info structures fit u32"),
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: successful GetFileInformationByHandleEx initialized the complete fixed-size output.
    Ok(unsafe { info.assume_init() })
}

fn rename_handle(file: &File, parent: &File, leaf: &OsStr) -> io::Result<()> {
    validate_leaf(leaf)?;
    let name = leaf.encode_wide().collect::<Vec<_>>();
    let name_bytes = name
        .len()
        .checked_mul(size_of::<u16>())
        .ok_or_else(invalid_name)?;
    let encoded_len = offset_of!(FILE_RENAME_INFO, FileName)
        .checked_add(name_bytes)
        .ok_or_else(invalid_name)?;
    let info_len = encoded_len.max(size_of::<FILE_RENAME_INFO>());
    let allocation_len = info_len
        .checked_add(size_of::<u16>())
        .ok_or_else(invalid_name)?;
    let words = allocation_len.div_ceil(size_of::<usize>());
    let mut storage = vec![0_usize; words];
    let info = storage.as_mut_ptr().cast::<FILE_RENAME_INFO>();
    // SAFETY: `storage` is usize-aligned and uses the documented fixed structure size plus the
    // flexible UTF-16 tail. No reference to the flexible member is formed; writes stay in-bounds.
    unsafe {
        (*info).Anonymous.ReplaceIfExists = false;
        // The parent was opened with FILE_TRAVERSE and remains pinned without delete sharing.
        // RootDirectory therefore makes the validated leaf relative to that exact directory,
        // independent of the process current directory or later lexical-ancestor renames.
        (*info).RootDirectory = parent.as_raw_handle().cast();
        (*info).FileNameLength =
            u32::try_from(name.len() * size_of::<u16>()).expect("validated leaf length fits u32");
        std::ptr::copy_nonoverlapping(
            name.as_ptr(),
            std::ptr::addr_of_mut!((*info).FileName).cast::<u16>(),
            name.len(),
        );
        let mut status_block = IO_STATUS_BLOCK::default();
        let status = NtSetInformationFile(
            file.as_raw_handle().cast(),
            &raw mut status_block,
            info.cast_const().cast(),
            u32::try_from(info_len).expect("validated rename record fits u32"),
            FileRenameInformation,
        );
        if status < 0 {
            let win32 = RtlNtStatusToDosError(status);
            return Err(io::Error::from_raw_os_error(
                i32::try_from(win32).unwrap_or(i32::MAX),
            ));
        }
    }
    Ok(())
}

fn validate_absolute_path(path: &Path) -> io::Result<()> {
    let mut components = path.components();
    let is_local_disk = matches!(
        components.next(),
        Some(Component::Prefix(prefix)) if matches!(prefix.kind(), Prefix::Disk(_))
    );
    if !is_local_disk || components.next() != Some(Component::RootDir) {
        return Err(invalid_name());
    }
    for component in components {
        match component {
            Component::Normal(name) => validate_leaf(name)?,
            Component::Prefix(_)
            | Component::RootDir
            | Component::CurDir
            | Component::ParentDir => return Err(invalid_name()),
        }
    }
    Ok(())
}

fn require_local_fixed_drive(path: &Path) -> io::Result<()> {
    let Some(Component::Prefix(prefix)) = path.components().next() else {
        return Err(invalid_name());
    };
    let Prefix::Disk(letter) = prefix.kind() else {
        return Err(invalid_name());
    };
    let root = [u16::from(letter), u16::from(b':'), u16::from(b'\\'), 0];
    // SAFETY: `root` is a complete NUL-terminated local drive-root string.
    if unsafe { GetDriveTypeW(root.as_ptr()) } != DRIVE_FIXED {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "publication replacement requires a fixed local drive",
        ));
    }
    Ok(())
}

fn validate_leaf(leaf: &OsStr) -> io::Result<()> {
    let encoded = leaf.encode_wide().collect::<Vec<_>>();
    if encoded.is_empty()
        || encoded.len() > MAX_LEAF_UTF16
        || encoded
            .iter()
            .any(|unit| matches!(*unit, 0..=0x1f | 0x22 | 0x2a | 0x2f | 0x3a | 0x3c..=0x3e | 0x3f | 0x5c | 0x7c))
        || encoded
            .last()
            .is_some_and(|unit| matches!(*unit, 0x20 | 0x2e))
    {
        return Err(invalid_name());
    }
    let display = String::from_utf16(&encoded).map_err(|_| invalid_name())?;
    if display == "." || display == ".." || is_dos_device_name(&display) {
        return Err(invalid_name());
    }
    Ok(())
}

fn is_dos_device_name(name: &str) -> bool {
    let stem = name.split('.').next().unwrap_or(name).to_ascii_uppercase();
    matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || stem
            .strip_prefix("COM")
            .or_else(|| stem.strip_prefix("LPT"))
            .is_some_and(|suffix| {
                matches!(
                    suffix,
                    "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
                )
            })
}

fn wide_local_path(path: &Path) -> io::Result<Vec<u16>> {
    validate_absolute_path(path)?;
    let encoded = path.as_os_str().encode_wide();
    let mut wide = Vec::new();
    wide.extend([0x5c, 0x5c, 0x3f, 0x5c]);
    wide.extend(encoded);
    wide.push(0);
    Ok(wide)
}

fn invalid_name() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        "invalid Windows publication path",
    )
}

#[cfg(test)]
mod tests {
    use super::{
        DELETE, FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
        GENERIC_READ, PinnedDirectory, ReplaceState, is_cloud_files_tag, is_name_surrogate_tag,
        open_handle, validate_leaf,
    };
    use std::ffi::OsStr;
    use std::fs::OpenOptions;
    use std::io::Read as _;
    use std::os::windows::ffi::OsStrExt as _;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    static SEQUENCE: AtomicU64 = AtomicU64::new(1);

    #[test]
    fn removable_directory_refuses_substitution_and_removes_only_exact_empty_directory() {
        let temp = TempDirectory::new("empty-retirement");
        let path = temp.path().join("envelope");
        std::fs::create_dir(&path).unwrap();
        let held = PinnedDirectory::for_removable_directory(&path).unwrap();
        assert!(std::fs::rename(&path, temp.path().join("moved")).is_err());
        assert!(std::fs::remove_dir(&path).is_err());
        held.remove_empty().unwrap();
        assert!(!path.exists());
    }

    #[test]
    fn removable_directory_preserves_unknown_children_and_exact_file_removal_finishes() {
        let temp = TempDirectory::new("nonempty-retirement");
        let path = temp.path().join("envelope");
        std::fs::create_dir(&path).unwrap();
        let child = path.join("owned.bin");
        std::fs::write(&child, b"owned").unwrap();
        let held = PinnedDirectory::for_removable_directory(&path).unwrap();
        assert!(held.remove_empty().is_err());
        assert_eq!(std::fs::read(&child).unwrap(), b"owned");
        let held = PinnedDirectory::for_removable_directory(&path).unwrap();
        let file = held.pin_file(&child).unwrap();
        assert!(std::fs::rename(&child, path.join("substitution.bin")).is_err());
        file.remove().unwrap();
        assert!(!child.exists());
        held.remove_empty().unwrap();
        assert!(!path.exists());
    }

    struct TempDirectory(PathBuf);

    impl TempDirectory {
        fn new(label: &str) -> Self {
            let sequence = SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "publisher-windows-handles-{label}-{}-{sequence}",
                std::process::id()
            ));
            if path.exists() {
                std::fs::remove_dir_all(&path).expect("remove stale Windows handle fixture");
            }
            std::fs::create_dir(&path).expect("create Windows handle fixture");
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDirectory {
        fn drop(&mut self) {
            let _ignored = std::fs::remove_dir_all(&self.0);
        }
    }

    fn publication_paths(label: &str) -> (TempDirectory, PathBuf, PathBuf, PathBuf) {
        let temp = TempDirectory::new(label);
        let destination = temp.path().join("book.publisher-v2");
        let staged = temp.path().join("replacement.publisher-v2");
        let prior = temp.path().join("prior.publisher-v2");
        std::fs::write(&destination, b"old publication").expect("write destination");
        std::fs::write(&staged, b"new publication").expect("write stage");
        (temp, destination, staged, prior)
    }

    #[test]
    fn exclusive_creation_preserves_an_occupied_stage() {
        let (temp, destination, stage, _prior) = publication_paths("exclusive-create");
        let directory = PinnedDirectory::for_destination(&destination).unwrap();
        assert!(
            directory
                .create_file_for_destination(&stage, &destination)
                .is_err()
        );
        assert_eq!(std::fs::read(&stage).unwrap(), b"new publication");
        let vacant_stage = temp.path().join("vacant.stage");
        let created = directory
            .create_file_for_destination(&vacant_stage, &destination)
            .unwrap();
        assert!(
            created.commit().is_err(),
            "occupied destination must survive"
        );
        assert!(
            !vacant_stage.exists(),
            "failed installation cleans only its held stage"
        );
        assert_eq!(std::fs::read(&destination).unwrap(), b"old publication");
    }

    #[test]
    fn created_stage_enters_replacement_without_releasing_its_handle() {
        use std::io::Write;
        let (temp, destination, occupied_stage, prior) = publication_paths("created-replacement");
        let stage = temp.path().join("held.stage");
        let directory = PinnedDirectory::for_destination(&destination).unwrap();
        let created = directory
            .create_file_for_destination(&stage, &destination)
            .unwrap();
        created
            .file()
            .unwrap()
            .write_all(b"new retained generation")
            .unwrap();
        assert!(std::fs::rename(&stage, temp.path().join("substitute.stage")).is_err());
        let prepared = created.prepare_replace(&prior).unwrap();
        let identity = prepared.staged_identity();
        let committed = prepared.preserve_prior().unwrap().commit().unwrap();
        created.retain();
        assert_eq!(committed.destination_identity(), identity);
        drop(committed);
        assert_eq!(
            std::fs::read(&destination).unwrap(),
            b"new retained generation"
        );
        assert_eq!(std::fs::read(&occupied_stage).unwrap(), b"new publication");
        assert!(!stage.exists());
    }

    #[test]
    fn failed_created_replacement_cleans_held_stage_and_preserves_occupants() {
        use std::io::Write;
        let (temp, destination, occupied_stage, prior) =
            publication_paths("created-replacement-failure");
        let stage = temp.path().join("held.stage");
        let directory = PinnedDirectory::for_destination(&destination).unwrap();
        let created = directory
            .create_file_for_destination(&stage, &destination)
            .unwrap();
        created
            .file()
            .unwrap()
            .write_all(b"unfinished generation")
            .unwrap();
        std::fs::write(&prior, b"independent prior occupant").unwrap();
        assert!(created.prepare_replace(&prior).is_err());
        drop(created);
        assert!(!stage.exists());
        assert_eq!(
            std::fs::read(&prior).unwrap(),
            b"independent prior occupant"
        );
        assert_eq!(std::fs::read(&destination).unwrap(), b"old publication");
        assert_eq!(std::fs::read(&occupied_stage).unwrap(), b"new publication");
    }

    #[test]
    fn ordinary_replace_and_receipt_move_are_handle_bound_and_no_clobber() {
        let (temp, destination, staged, prior) = publication_paths("ordinary");
        let directory = PinnedDirectory::for_destination(&destination).expect("pin ancestors");
        let prepared = directory
            .prepare_replace(&destination, &staged, &prior)
            .expect("prepare exact files");
        let staged_identity = prepared.staged_identity();
        assert!(
            std::fs::rename(&staged, temp.path().join("swapped.publisher-v2")).is_err(),
            "an exact staged handle opened without delete sharing blocks path swaps"
        );
        let committed = prepared
            .preserve_prior()
            .expect("preserve prior")
            .commit()
            .expect("commit replacement");
        assert_eq!(committed.destination_identity(), staged_identity);
        committed
            .reopened_destination_file()
            .expect("destination name still binds the exact staged identity");
        committed
            .reopened_prior_file()
            .expect("prior name still binds the exact old identity");
        let mut exact_destination = committed
            .destination_file()
            .expect("clone exact destination");
        let mut bytes = Vec::new();
        exact_destination
            .read_to_end(&mut bytes)
            .expect("read exact committed handle");
        assert_eq!(bytes, b"new publication");
        assert_eq!(
            std::fs::read(&prior).expect("read prior"),
            b"old publication"
        );

        let journal = temp.path().join("active-journal.json");
        let receipt = temp.path().join("completed-receipt.json");
        std::fs::write(&journal, b"receipt bytes").expect("write journal");
        let moved = directory
            .move_file_no_replace(&journal, &receipt)
            .expect("move exact journal");
        assert_eq!(moved.identity().byte_length, 13);
        moved
            .reopened_destination_file()
            .expect("receipt name still binds the exact journal identity");
        assert!(!journal.exists());
        assert_eq!(
            std::fs::read(receipt).expect("read receipt"),
            b"receipt bytes"
        );
    }

    #[test]
    fn removing_a_displaced_hard_link_never_changes_its_other_name() {
        let (temp, destination, staged, prior) = publication_paths("hard-link-prior");
        let other_name = temp.path().join("unrelated.json");
        std::fs::hard_link(&destination, &other_name).expect("create second hard-link name");
        let committed = PinnedDirectory::for_destination(&destination)
            .expect("pin exact parent")
            .prepare_replace(&destination, &staged, &prior)
            .expect("prepare hard-link replacement")
            .preserve_prior()
            .expect("move only the destination name")
            .commit()
            .expect("install replacement");
        committed
            .remove_prior()
            .expect("remove exact displaced directory entry");
        assert!(!prior.exists());
        assert_eq!(
            std::fs::read(other_name).expect("read other hard-link name"),
            b"old publication"
        );
        assert_eq!(
            std::fs::read(destination).expect("read replacement"),
            b"new publication"
        );
    }

    #[test]
    fn one_character_sibling_names_have_a_complete_rename_record() {
        let temp = TempDirectory::new("one-character-names");
        let destination = temp.path().join("d");
        let staged = temp.path().join("s");
        let prior = temp.path().join("p");
        std::fs::write(&destination, b"old").expect("write one-character destination");
        std::fs::write(&staged, b"new").expect("write one-character stage");

        let committed = PinnedDirectory::for_destination(&destination)
            .expect("pin one-character parent")
            .prepare_replace(&destination, &staged, &prior)
            .expect("prepare one-character paths")
            .preserve_prior()
            .expect("rename destination to one-character prior")
            .commit()
            .expect("rename stage to one-character destination");
        committed
            .reopened_destination_file()
            .expect("reopen one-character destination");
        committed
            .reopened_prior_file()
            .expect("reopen one-character prior");
        assert_eq!(
            std::fs::read(destination).expect("read destination"),
            b"new"
        );
        assert_eq!(std::fs::read(prior).expect("read prior"), b"old");
    }

    #[test]
    fn working_directory_profile_supports_parent_relative_replacement() {
        let sequence = SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let path = std::env::current_dir()
            .expect("read native working directory")
            .join(".tmp")
            .join(format!(
                "publisher-windows-working-profile-{}-{sequence}",
                std::process::id()
            ));
        std::fs::create_dir_all(&path).expect("create working-profile fixture");
        let temp = TempDirectory(path);
        let destination = temp.path().join("book.publisher-v2");
        let staged = temp.path().join("replacement.publisher-v2");
        let prior = temp.path().join("prior.publisher-v2");
        std::fs::write(&destination, b"old working-profile publication")
            .expect("write working-profile destination");
        std::fs::write(&staged, b"new working-profile publication")
            .expect("write working-profile stage");

        let committed = PinnedDirectory::for_destination(&destination)
            .expect("pin exact working-profile parent")
            .prepare_replace(&destination, &staged, &prior)
            .expect("bind working-profile archives")
            .preserve_prior()
            .expect("preserve working-profile prior")
            .commit()
            .expect("commit working-profile replacement");
        committed
            .reopened_destination_file()
            .expect("reopen working-profile destination");
        assert_eq!(
            std::fs::read(&destination).expect("read working-profile destination"),
            b"new working-profile publication"
        );
        assert_eq!(
            std::fs::read(&prior).expect("read working-profile prior"),
            b"old working-profile publication"
        );
    }

    #[test]
    fn ancestor_redirection_is_blocked_while_exact_parent_is_pinned() {
        let temp = TempDirectory::new("ancestor-redirection");
        let lexical = temp.path().join("lexical");
        let moved = temp.path().join("moved");
        let parent = lexical.join("publication");
        std::fs::create_dir_all(&parent).expect("create original publication parent");
        let destination = parent.join("book.publisher-v2");
        let staged = parent.join("replacement.publisher-v2");
        let prior = parent.join("prior.publisher-v2");
        std::fs::write(&destination, b"exact old publication").expect("write exact old");
        std::fs::write(&staged, b"exact new publication").expect("write exact new");

        let directory = PinnedDirectory::for_destination(&destination).expect("pin exact parent");
        let prepared = directory
            .prepare_replace(&destination, &staged, &prior)
            .expect("bind exact siblings");
        assert!(
            std::fs::rename(&lexical, &moved).is_err(),
            "the no-delete parent handle must prevent an ancestor path redirect"
        );
        let committed = prepared
            .preserve_prior()
            .expect("preserve inside exact parent")
            .commit()
            .expect("commit inside exact parent");
        assert_eq!(
            std::fs::read(&destination).expect("read exact destination"),
            b"exact new publication"
        );
        assert_eq!(
            std::fs::read(&prior).expect("read exact prior"),
            b"exact old publication"
        );
        drop(committed);
    }

    #[test]
    fn occupied_prior_and_commit_gap_destination_are_never_clobbered() {
        let (_temp, destination, staged, prior) = publication_paths("occupied-prior");
        std::fs::write(&prior, b"external prior").expect("occupy prior");
        let directory = PinnedDirectory::for_destination(&destination).expect("pin ancestors");
        let error = directory
            .prepare_replace(&destination, &staged, &prior)
            .expect_err("occupied prior must refuse");
        assert_eq!(error.kind(), std::io::ErrorKind::AlreadyExists);
        assert_eq!(
            std::fs::read(&destination).expect("read old"),
            b"old publication"
        );
        assert_eq!(
            std::fs::read(&staged).expect("read new"),
            b"new publication"
        );
        assert_eq!(
            std::fs::read(&prior).expect("read external"),
            b"external prior"
        );

        let (temp, destination, staged, prior) = publication_paths("occupied-gap");
        let directory = PinnedDirectory::for_destination(&destination).expect("pin ancestors");
        let prepared = directory
            .prepare_replace(&destination, &staged, &prior)
            .expect("prepare exact files");
        let preserved = prepared.preserve_prior().expect("preserve prior");
        std::fs::write(&destination, b"concurrent destination")
            .expect("create concurrent destination");
        let error = preserved
            .commit()
            .expect_err("concurrent destination must block commit and restore");
        assert_eq!(error.state(), ReplaceState::Ambiguous);
        assert!(error.rollback_error().is_some());
        assert_eq!(
            std::fs::read(&destination).expect("read concurrent destination"),
            b"concurrent destination"
        );
        assert_eq!(
            std::fs::read(&prior).expect("read retained old"),
            b"old publication"
        );
        assert_eq!(
            std::fs::read(&staged).expect("read retained new"),
            b"new publication"
        );
        drop(temp);
    }

    #[test]
    fn existing_writer_or_delete_handle_fails_closed_before_any_rename() {
        let (_temp, destination, staged, prior) = publication_paths("existing-writer");
        let writer = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&destination)
            .expect("open competing writer");
        let directory = PinnedDirectory::for_destination(&destination).expect("pin ancestors");
        let error = directory
            .prepare_replace(&destination, &staged, &prior)
            .expect_err("existing writer must create a sharing violation");
        assert_eq!(error.raw_os_error(), Some(32));
        assert_eq!(
            std::fs::read(&destination).expect("read old"),
            b"old publication"
        );
        assert_eq!(
            std::fs::read(&staged).expect("read new"),
            b"new publication"
        );
        drop(writer);

        let (_temp, destination, staged, prior) = publication_paths("existing-deleter");
        let deleter = open_handle(
            &destination,
            GENERIC_READ | DELETE,
            FILE_FLAG_OPEN_REPARSE_POINT,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
        )
        .expect("open competing delete handle");
        let directory = PinnedDirectory::for_destination(&destination).expect("pin ancestors");
        let error = directory
            .prepare_replace(&destination, &staged, &prior)
            .expect_err("existing delete handle must create a sharing violation");
        assert_eq!(error.raw_os_error(), Some(32));
        assert_eq!(
            std::fs::read(&destination).expect("read old"),
            b"old publication"
        );
        assert_eq!(
            std::fs::read(&staged).expect("read new"),
            b"new publication"
        );
        drop(deleter);
    }

    #[test]
    fn long_local_ntfs_paths_replace_through_exact_handles() {
        let temp = TempDirectory::new("long-path");
        let deep = temp
            .path()
            .join("a".repeat(90))
            .join("b".repeat(90))
            .join("c".repeat(90));
        std::fs::create_dir_all(&deep).expect("create extended-length directory");
        let destination = deep.join("book.publisher-v2");
        let staged = deep.join("replacement.publisher-v2");
        let prior = deep.join("prior.publisher-v2");
        assert!(destination.as_os_str().encode_wide().count() > 260);
        std::fs::write(&destination, b"old long publication").expect("write long destination");
        std::fs::write(&staged, b"new long publication").expect("write long stage");
        let directory = PinnedDirectory::for_destination(&destination).expect("pin long path");
        let committed = directory
            .prepare_replace(&destination, &staged, &prior)
            .expect("prepare long path")
            .preserve_prior()
            .expect("preserve long prior")
            .commit()
            .expect("commit long replacement");
        committed
            .reopened_destination_file()
            .expect("reopen exact long destination");
        assert_eq!(
            std::fs::read(&destination).expect("read long destination"),
            b"new long publication"
        );
        assert_eq!(
            std::fs::read(&prior).expect("read long prior"),
            b"old long publication"
        );
    }

    #[test]
    fn redirecting_reparse_points_and_ambiguous_windows_names_are_rejected() {
        assert!(is_name_surrogate_tag(0xa000_000c));
        assert!(!is_name_surrogate_tag(0x9000_701a));
        assert!(is_cloud_files_tag(0x9000_001a));
        assert!(is_cloud_files_tag(0x9000_701a));
        assert!(!is_cloud_files_tag(0x9000_001c));
        assert!(!is_cloud_files_tag(0x8000_0017));
        for invalid in [
            "book:stream",
            "book?.pub",
            "CON",
            "lpt1.txt",
            "COM¹.txt",
            "trailing.",
            "trailing ",
        ] {
            assert!(
                validate_leaf(OsStr::new(invalid)).is_err(),
                "accepted {invalid}"
            );
        }
        assert!(validate_leaf(OsStr::new(&"a".repeat(256))).is_err());
        assert!(
            PinnedDirectory::for_destination(Path::new(r"\\server\share\book.publisher-v2"))
                .is_err()
        );
        assert!(PinnedDirectory::for_destination(Path::new(r"\\?\C:\book.publisher-v2")).is_err());
        assert!(PinnedDirectory::for_destination(Path::new(r"\\.\C:\book.publisher-v2")).is_err());
        assert!(
            PinnedDirectory::for_destination(Path::new(r"C:\valid\trailing.\book.publisher-v2"))
                .is_err()
        );

        let temp = TempDirectory::new("symlinks");
        let real_directory = temp.path().join("real");
        let linked_directory = temp.path().join("linked");
        std::fs::create_dir(&real_directory).expect("create real directory");
        if std::os::windows::fs::symlink_dir(&real_directory, &linked_directory).is_ok() {
            let destination = linked_directory.join("book.publisher-v2");
            assert!(PinnedDirectory::for_destination(&destination).is_err());
        }

        let real_file = temp.path().join("real.publisher-v2");
        let linked_file = temp.path().join("linked.publisher-v2");
        std::fs::write(&real_file, b"real").expect("write real file");
        if std::os::windows::fs::symlink_file(&real_file, &linked_file).is_ok() {
            let directory = PinnedDirectory::for_destination(&linked_file).expect("pin parent");
            assert!(
                directory
                    .move_file_no_replace(&linked_file, &temp.path().join("moved.publisher-v2"))
                    .is_err()
            );
        }
    }
}
