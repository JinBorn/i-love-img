//! Lightweight queue entries preserve both the original and the input of this step.

use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::SystemTime;

static NEXT_ITEM_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceFingerprint {
    pub len: u64,
    pub modified: Option<SystemTime>,
    pub sha256: [u8; 32],
}

impl SourceFingerprint {
    /// Hashes in bounded chunks; matching size and timestamp alone is insufficient.
    pub fn capture(path: &Path) -> io::Result<Self> {
        let mut file = File::open(path)?;
        let before = file.metadata()?;
        if !before.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "input is not a regular file",
            ));
        }
        let mut hash = Sha256::new();
        let mut buffer = [0_u8; 64 * 1024];
        let mut read_bytes = 0_u64;
        loop {
            let count = file.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            hash.update(&buffer[..count]);
            read_bytes = read_bytes.saturating_add(count as u64);
        }
        let after = file.metadata()?;
        let at_path = fs::metadata(path)?;
        if before.len() != read_bytes
            || before.len() != after.len()
            || before.modified().ok() != after.modified().ok()
            || after.len() != at_path.len()
            || after.modified().ok() != at_path.modified().ok()
        {
            return Err(io::Error::other(
                "input changed while reading; reimport or retry",
            ));
        }
        Ok(Self {
            len: read_bytes,
            modified: after.modified().ok(),
            sha256: hash.finalize().into(),
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceRecord {
    pub path: PathBuf,
    pub fingerprint: SourceFingerprint,
}

impl SourceRecord {
    pub fn capture(path: &Path) -> io::Result<Self> {
        let path = fs::canonicalize(path)?;
        let fingerprint = SourceFingerprint::capture(&path)?;
        Ok(Self { path, fingerprint })
    }

    pub fn verify(&self) -> io::Result<()> {
        let actual = SourceFingerprint::capture(&self.path)?;
        if actual != self.fingerprint {
            return Err(io::Error::other(
                "input changed since import or the previous step; reimport or retry",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputSource {
    Original,
    PreviousOutput,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImportedImage {
    pub id: u64,
    pub original: SourceRecord,
    pub input: SourceRecord,
    /// Relative to its imported folder, including the file name.
    pub relative_path: PathBuf,
    pub input_source: InputSource,
}

impl ImportedImage {
    pub fn from_file(path: &Path, relative_path: PathBuf) -> io::Result<Self> {
        validate_relative_path(&relative_path)?;
        let original = SourceRecord::capture(path)?;
        Ok(Self {
            id: NEXT_ITEM_ID.fetch_add(1, Ordering::Relaxed),
            input: original.clone(),
            original,
            relative_path,
            input_source: InputSource::Original,
        })
    }

    /// Re-adjustment requires the original file still to match its imported content.
    pub fn readjust(&self) -> io::Result<Self> {
        self.original.verify()?;
        let mut next = self.clone();
        next.input = self.original.clone();
        next.input_source = InputSource::Original;
        Ok(next)
    }

    /// Only call this with the successful output record of the immediately preceding step.
    /// The original is deliberately not read: it may have been moved or deleted.
    pub fn continue_from(&self, output: &SourceRecord) -> io::Result<Self> {
        output.verify()?;
        let mut next = self.clone();
        next.input = output.clone();
        next.input_source = InputSource::PreviousOutput;
        Ok(next)
    }

    /// Explicit retry accepts the current on-disk input and takes a fresh fingerprint.
    pub fn refreshed_for_retry(&self) -> io::Result<Self> {
        let mut next = self.clone();
        next.input = SourceRecord::capture(&self.input.path)?;
        if self.input_source == InputSource::Original {
            next.original = next.input.clone();
        }
        Ok(next)
    }
}

#[derive(Clone, Debug)]
pub struct ImportOptions {
    pub recursive: bool,
    pub output_directory: PathBuf,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ImportSkipReason {
    Duplicate,
    OutputDirectory,
    DirectoryLink,
    NotRegularFile,
    Unreadable(String),
}

#[derive(Clone, Debug)]
pub struct ImportIssue {
    pub path: PathBuf,
    pub reason: ImportSkipReason,
}

#[derive(Clone, Debug, Default)]
pub struct ImportReport {
    pub items: Vec<ImportedImage>,
    pub skipped: Vec<ImportIssue>,
}

/// Imports regular files without decoding them. Unsupported/corrupt image contents
/// become per-item processing results, so a bad file never hides its neighbours.
pub fn import_paths(paths: &[PathBuf], options: &ImportOptions) -> ImportReport {
    let mut report = ImportReport::default();
    let mut seen = HashSet::new();
    let output = absolute_normalized(&options.output_directory)
        .and_then(|p| canonicalize_existing_prefix(&p));
    let output = match output {
        Ok(path) => path,
        Err(error) => {
            report.skipped.push(ImportIssue {
                path: options.output_directory.clone(),
                reason: ImportSkipReason::Unreadable(error.to_string()),
            });
            return report;
        }
    };
    for selected in paths {
        let metadata = match fs::symlink_metadata(selected) {
            Ok(metadata) => metadata,
            Err(error) => {
                issue(
                    &mut report,
                    selected,
                    ImportSkipReason::Unreadable(error.to_string()),
                );
                continue;
            }
        };
        if is_directory_link(selected, &metadata) {
            issue(&mut report, selected, ImportSkipReason::DirectoryLink);
            continue;
        }
        let resolved = match fs::canonicalize(selected) {
            Ok(path) => path,
            Err(error) => {
                issue(
                    &mut report,
                    selected,
                    ImportSkipReason::Unreadable(error.to_string()),
                );
                continue;
            }
        };
        if path_is_within(&resolved, &output) {
            issue(&mut report, selected, ImportSkipReason::OutputDirectory);
            continue;
        }
        if resolved.is_file() {
            let relative = resolved.file_name().map(PathBuf::from).unwrap_or_default();
            add_file(&resolved, relative, &mut seen, &mut report);
        } else if resolved.is_dir() {
            let mut pending = vec![resolved.clone()];
            while let Some(directory) = pending.pop() {
                let entries = match fs::read_dir(&directory) {
                    Ok(entries) => entries,
                    Err(error) => {
                        issue(
                            &mut report,
                            &directory,
                            ImportSkipReason::Unreadable(error.to_string()),
                        );
                        continue;
                    }
                };
                let mut children = Vec::new();
                for entry in entries {
                    match entry {
                        Ok(entry) => children.push(entry.path()),
                        Err(error) => issue(
                            &mut report,
                            &directory,
                            ImportSkipReason::Unreadable(error.to_string()),
                        ),
                    }
                }
                children.sort();
                for child in children {
                    let metadata = match fs::symlink_metadata(&child) {
                        Ok(metadata) => metadata,
                        Err(error) => {
                            issue(
                                &mut report,
                                &child,
                                ImportSkipReason::Unreadable(error.to_string()),
                            );
                            continue;
                        }
                    };
                    if is_directory_link(&child, &metadata) {
                        issue(&mut report, &child, ImportSkipReason::DirectoryLink);
                        continue;
                    }
                    let actual = match fs::canonicalize(&child) {
                        Ok(path) => path,
                        Err(error) => {
                            issue(
                                &mut report,
                                &child,
                                ImportSkipReason::Unreadable(error.to_string()),
                            );
                            continue;
                        }
                    };
                    if path_is_within(&actual, &output) {
                        issue(&mut report, &child, ImportSkipReason::OutputDirectory);
                    } else if actual.is_dir() {
                        if options.recursive {
                            pending.push(child);
                        }
                    } else if actual.is_file() {
                        let relative = child.strip_prefix(&resolved).unwrap_or(&child).to_owned();
                        add_file(&actual, relative, &mut seen, &mut report);
                    } else {
                        issue(&mut report, &child, ImportSkipReason::NotRegularFile);
                    }
                }
            }
        } else {
            issue(&mut report, selected, ImportSkipReason::NotRegularFile);
        }
    }
    report
}

fn add_file(
    path: &Path,
    relative: PathBuf,
    seen: &mut HashSet<PathBuf>,
    report: &mut ImportReport,
) {
    if !seen.insert(path_key(path)) {
        issue(report, path, ImportSkipReason::Duplicate);
        return;
    }
    match ImportedImage::from_file(path, relative) {
        Ok(item) => report.items.push(item),
        Err(error) => issue(
            report,
            path,
            ImportSkipReason::Unreadable(error.to_string()),
        ),
    }
}

fn issue(report: &mut ImportReport, path: &Path, reason: ImportSkipReason) {
    report.skipped.push(ImportIssue {
        path: path.to_owned(),
        reason,
    });
}

pub(crate) fn validate_relative_path(path: &Path) -> io::Result<()> {
    if path.as_os_str().is_empty() || !path.components().all(|c| matches!(c, Component::Normal(_)))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid relative output path",
        ));
    }
    Ok(())
}

pub(crate) fn path_is_within(path: &Path, directory: &Path) -> bool {
    path_key(path).starts_with(path_key(directory))
}

fn path_key(path: &Path) -> PathBuf {
    #[cfg(windows)]
    {
        PathBuf::from(path.to_string_lossy().to_lowercase())
    }
    #[cfg(not(windows))]
    {
        path.to_owned()
    }
}

fn absolute_normalized(path: &Path) -> io::Result<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_owned()
    } else {
        std::env::current_dir()?.join(path)
    };
    let mut normalized = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            other => normalized.push(other.as_os_str()),
        }
    }
    Ok(normalized)
}

fn canonicalize_existing_prefix(path: &Path) -> io::Result<PathBuf> {
    if path.exists() {
        return fs::canonicalize(path);
    }
    let parent = path.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "output directory has no existing ancestor",
        )
    })?;
    let name = path
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "invalid output directory"))?;
    Ok(canonicalize_existing_prefix(parent)?.join(name))
}

fn is_directory_link(path: &Path, metadata: &fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        // Junctions also carry the reparse-point attribute and must not be traversed.
        metadata.file_attributes() & 0x400 != 0 && path.is_dir()
    }
    #[cfg(not(windows))]
    {
        metadata.file_type().is_symlink() && path.is_dir()
    }
}
