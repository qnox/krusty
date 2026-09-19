//! KLIB container reading: the platform-independent half of Kotlin library ingestion.
//!
//! A `.klib` is Kotlin's non-JVM library format and the symbol source every non-JVM target reads —
//! Native, JS and wasm all consume the same container. That is why the reader lives in the core and
//! not under a backend: the container layout, its manifest and its `linkdata` package fragments are
//! identical across targets, and only what a backend *does* with a decoded declaration differs.
//!
//! Two on-disk shapes are the same library. The Kotlin/JS and Kotlin/wasm distributions ship each
//! klib as a single zip file (`lib/kotlin-stdlib-js.klib`); the Kotlin/Native distribution ships its
//! klibs UNPACKED, as directories (`klib/common/stdlib/default/...`). A reader that understood only
//! zips would find no stdlib at all on a Native distribution, so [`KlibArchive`] abstracts over both
//! and its callers never branch on the shape.
//!
//! Layout, in both shapes, relative to the archive root:
//!
//! | entry                                               | contents                             |
//! |-----------------------------------------------------|--------------------------------------|
//! | `default/manifest`                                  | `java.util.Properties` key/values    |
//! | `default/linkdata/module`                            | module header protobuf               |
//! | `default/linkdata/package_<fqname>/<NN>_<seg>.knm`   | per-package declaration fragments    |
//! | `default/linkdata/root_package/<NN>_.knm`            | the root package's own fragments     |
//! | `default/ir/*.kn[bdft]`                              | serialized Kotlin IR (bodies)        |
//!
//! Declarations live in the `.knm` fragments, which carry the same Kotlin metadata protobuf the JVM
//! `@Metadata` reader already decodes — that decoder is what turns a fragment into declarations.
//! Bodies live under `default/ir/` in a separate, lower serialization; this module exposes those
//! entries as bytes and stops there.

use std::collections::BTreeMap;
use std::fmt;
use std::io::{Read as _, Seek as _};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// A klib's root directory tree, below which every entry path in this module is relative.
const ARCHIVE_COMPONENT: &str = "default";
/// A named package's fragments live under this, followed by the package's fully qualified name.
const PACKAGE_PREFIX: &str = "default/linkdata/package_";
/// The ROOT package's fragments live under their own directory instead — `package_` with an empty
/// name is not what the serializer writes. Every klib in the reference distribution has one.
const ROOT_PACKAGE_PREFIX: &str = "default/linkdata/root_package/";
/// Serialized Kotlin IR — the bodies. Read as opaque bytes here.
const IR_PREFIX: &str = "default/ir/";
/// Guard against a symlink cycle in an unpacked klib. A klib's own tree is three levels deep.
const MAX_DIRECTORY_DEPTH: usize = 8;

/// One `.knm` metadata fragment, resolved to the package it declares into.
///
/// A package's declarations are split across numbered fragments by the serializer, so several
/// fragments share one `package_fqname` and must all be decoded to see the whole package.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KlibFragment {
    /// The package's fully qualified Kotlin name; empty for the root package.
    pub package_fqname: String,
    /// The archive-relative entry path, to hand back to [`KlibArchive::read`].
    pub entry: String,
}

/// A klib, opened from either on-disk shape.
pub struct KlibArchive {
    root: PathBuf,
    source: Source,
    /// File entries, archive-relative with `/` separators, sorted. Directory entries are dropped:
    /// a zip klib records them and an unpacked one does not, and no caller distinguishes a library
    /// by which shape it came from.
    entries: Vec<String>,
}

enum Source {
    /// `zip::ZipArchive` reads through `&mut self`, so a shared archive needs the lock; the
    /// alternative is reopening the file per entry, which costs a central-directory parse each time.
    Zip(Mutex<zip::ZipArchive<std::fs::File>>),
    Directory {
        canonical_root: PathBuf,
    },
}

/// A structural or I/O failure while opening or reading a KLIB container.
///
/// Container failures are never translated into an empty library. A dependency selected by the
/// compiler is either a readable KLIB or an explicit error; otherwise corruption would surface much
/// later as an unrelated unresolved-reference diagnostic.
#[derive(Debug)]
pub enum KlibError {
    Io {
        operation: &'static str,
        path: PathBuf,
        source: std::io::Error,
    },
    InvalidZip {
        path: PathBuf,
        detail: String,
    },
    MissingDefaultTree {
        path: PathBuf,
    },
    InvalidEntryPath {
        archive: PathBuf,
        entry: String,
    },
    DuplicateEntry {
        archive: PathBuf,
        entry: String,
    },
    SymlinkEntry {
        archive: PathBuf,
        path: PathBuf,
    },
    UnsupportedEntry {
        archive: PathBuf,
        path: PathBuf,
    },
    NonUtf8Entry {
        archive: PathBuf,
        path: PathBuf,
    },
    DirectoryDepth {
        archive: PathBuf,
        path: PathBuf,
        maximum: usize,
    },
    MissingEntry {
        archive: PathBuf,
        entry: String,
    },
    PoisonedArchive {
        path: PathBuf,
    },
    PoisonedCache {
        path: PathBuf,
    },
    InvalidManifest {
        path: PathBuf,
        source: KlibManifestError,
    },
}

impl fmt::Display for KlibError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io {
                operation,
                path,
                source,
            } => write!(formatter, "cannot {operation} {}: {source}", path.display()),
            Self::InvalidZip { path, detail } => {
                write!(formatter, "invalid KLIB zip {}: {detail}", path.display())
            }
            Self::MissingDefaultTree { path } => {
                write!(formatter, "KLIB {} has no default/ tree", path.display())
            }
            Self::InvalidEntryPath { archive, entry } => write!(
                formatter,
                "KLIB {} contains unsafe entry path {entry:?}",
                archive.display()
            ),
            Self::DuplicateEntry { archive, entry } => write!(
                formatter,
                "KLIB {} contains duplicate entry {entry:?}",
                archive.display()
            ),
            Self::SymlinkEntry { archive, path } => write!(
                formatter,
                "KLIB {} contains symlink {}",
                archive.display(),
                path.display()
            ),
            Self::UnsupportedEntry { archive, path } => write!(
                formatter,
                "KLIB {} contains unsupported entry {}",
                archive.display(),
                path.display()
            ),
            Self::NonUtf8Entry { archive, path } => write!(
                formatter,
                "KLIB {} contains a non-UTF-8 entry name at {}",
                archive.display(),
                path.display()
            ),
            Self::DirectoryDepth {
                archive,
                path,
                maximum,
            } => write!(
                formatter,
                "KLIB {} exceeds directory depth {maximum} at {}",
                archive.display(),
                path.display()
            ),
            Self::MissingEntry { archive, entry } => write!(
                formatter,
                "KLIB {} has no entry {entry:?}",
                archive.display()
            ),
            Self::PoisonedArchive { path } => {
                write!(
                    formatter,
                    "KLIB archive lock is poisoned for {}",
                    path.display()
                )
            }
            Self::PoisonedCache { path } => {
                write!(
                    formatter,
                    "KLIB cache lock is poisoned for {}",
                    path.display()
                )
            }
            Self::InvalidManifest { path, source } => {
                write!(
                    formatter,
                    "invalid KLIB manifest {}: {source}",
                    path.display()
                )
            }
        }
    }
}

impl std::error::Error for KlibError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::InvalidManifest { source, .. } => Some(source),
            _ => None,
        }
    }
}

/// A malformed `java.util.Properties` document in `default/manifest`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KlibManifestError {
    InvalidUtf8 { valid_up_to: usize },
    InvalidUnicodeEscape { escape: String },
}

impl fmt::Display for KlibManifestError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidUtf8 { valid_up_to } => {
                write!(formatter, "invalid UTF-8 at byte {valid_up_to}")
            }
            Self::InvalidUnicodeEscape { escape } => {
                write!(formatter, "invalid Unicode escape {escape:?}")
            }
        }
    }
}

impl std::error::Error for KlibManifestError {}

impl KlibArchive {
    /// Open `path` as a klib, accepting either a zip file or an unpacked directory.
    ///
    pub fn open(path: &Path) -> Result<Self, KlibError> {
        let metadata = std::fs::symlink_metadata(path).map_err(|source| KlibError::Io {
            operation: "inspect",
            path: path.to_path_buf(),
            source,
        })?;
        if metadata.file_type().is_symlink() {
            return Err(KlibError::SymlinkEntry {
                archive: path.to_path_buf(),
                path: path.to_path_buf(),
            });
        }
        if metadata.is_dir() {
            if !path.join(ARCHIVE_COMPONENT).is_dir() {
                return Err(KlibError::MissingDefaultTree {
                    path: path.to_path_buf(),
                });
            }
            let canonical_root = std::fs::canonicalize(path).map_err(|source| KlibError::Io {
                operation: "canonicalize",
                path: path.to_path_buf(),
                source,
            })?;
            let mut entries = Vec::new();
            collect_directory_entries(path, path, String::new(), 0, &mut entries)?;
            entries.sort();
            reject_duplicate_entries(path, &entries)?;
            return Ok(Self {
                root: path.to_path_buf(),
                source: Source::Directory { canonical_root },
                entries,
            });
        }
        if !metadata.is_file() {
            return Err(KlibError::UnsupportedEntry {
                archive: path.to_path_buf(),
                path: path.to_path_buf(),
            });
        }
        let mut file = std::fs::File::open(path).map_err(|source| KlibError::Io {
            operation: "open",
            path: path.to_path_buf(),
            source,
        })?;
        let directory = zip_directory_layout(path, &mut file)?;
        file.seek(std::io::SeekFrom::Start(0))
            .map_err(|source| KlibError::Io {
                operation: "seek",
                path: path.to_path_buf(),
                source,
            })?;
        let mut archive = zip::ZipArchive::new(file).map_err(|error| KlibError::InvalidZip {
            path: path.to_path_buf(),
            detail: error.to_string(),
        })?;
        reject_duplicate_zip_entries(path, directory)?;
        if archive.central_directory_start() != directory.start
            || archive.len() != usize::from(directory.entry_count)
        {
            return Err(KlibError::InvalidZip {
                path: path.to_path_buf(),
                detail: "central-directory layout disagrees with EOCD".to_string(),
            });
        }
        let mut entries = Vec::new();
        for index in 0..archive.len() {
            let file = archive
                .by_index(index)
                .map_err(|error| KlibError::InvalidZip {
                    path: path.to_path_buf(),
                    detail: error.to_string(),
                })?;
            let name =
                std::str::from_utf8(file.name_raw()).map_err(|_| KlibError::NonUtf8Entry {
                    archive: path.to_path_buf(),
                    path: PathBuf::from(format!("zip entry #{index}")),
                })?;
            let entry_path = name.strip_suffix('/').unwrap_or(name);
            validate_entry_path(path, entry_path)?;
            if file
                .unix_mode()
                .is_some_and(|mode| mode & 0o170000 == 0o120000)
            {
                return Err(KlibError::SymlinkEntry {
                    archive: path.to_path_buf(),
                    path: PathBuf::from(name),
                });
            }
            if name.ends_with('/') {
                continue;
            }
            entries.push(name.to_string());
        }
        entries.sort();
        reject_duplicate_entries(path, &entries)?;
        if !entries.iter().any(|entry| entry.starts_with("default/")) {
            return Err(KlibError::MissingDefaultTree {
                path: path.to_path_buf(),
            });
        }
        Ok(Self {
            root: path.to_path_buf(),
            source: Source::Zip(Mutex::new(archive)),
            entries,
        })
    }

    /// Where this klib was opened from, for diagnostics.
    pub fn path(&self) -> &Path {
        &self.root
    }

    /// Every file entry, archive-relative and sorted.
    pub fn entries(&self) -> &[String] {
        &self.entries
    }

    /// Read one entry's bytes. Missing or unreadable entries are corruption, never an empty symbol
    /// source.
    pub fn read(&self, entry: &str) -> Result<Vec<u8>, KlibError> {
        validate_entry_path(&self.root, entry)?;
        if self
            .entries
            .binary_search_by(|known| known.as_str().cmp(entry))
            .is_err()
        {
            return Err(KlibError::MissingEntry {
                archive: self.root.clone(),
                entry: entry.to_string(),
            });
        }
        match &self.source {
            Source::Zip(archive) => {
                let mut archive = archive.lock().map_err(|_| KlibError::PoisonedArchive {
                    path: self.root.clone(),
                })?;
                let mut file = archive
                    .by_name(entry)
                    .map_err(|error| KlibError::InvalidZip {
                        path: self.root.clone(),
                        detail: error.to_string(),
                    })?;
                let mut bytes = Vec::with_capacity(file.size() as usize);
                file.read_to_end(&mut bytes)
                    .map_err(|source| KlibError::Io {
                        operation: "read",
                        path: self.root.join(entry),
                        source,
                    })?;
                Ok(bytes)
            }
            Source::Directory { canonical_root } => {
                let path = self.root.join(entry);
                reject_symlink_components(&self.root, &path)?;
                let canonical = std::fs::canonicalize(&path).map_err(|source| KlibError::Io {
                    operation: "canonicalize",
                    path: path.clone(),
                    source,
                })?;
                if !canonical.starts_with(canonical_root) {
                    return Err(KlibError::InvalidEntryPath {
                        archive: self.root.clone(),
                        entry: entry.to_string(),
                    });
                }
                std::fs::read(&canonical).map_err(|source| KlibError::Io {
                    operation: "read",
                    path: canonical,
                    source,
                })
            }
        }
    }

    /// The parsed `default/manifest`.
    pub fn manifest(&self) -> Result<KlibManifest, KlibError> {
        let bytes = self.read("default/manifest")?;
        KlibManifest::parse(&bytes).map_err(|source| KlibError::InvalidManifest {
            path: self.root.join("default/manifest"),
            source,
        })
    }

    /// The `default/linkdata/module` header protobuf, which names the packages the library declares.
    pub fn module_header(&self) -> Result<Vec<u8>, KlibError> {
        self.read("default/linkdata/module")
    }

    /// Every declaration fragment, ordered by package and then by the serializer's own chunk number.
    ///
    /// Chunk order is taken numerically rather than lexicographically so a library with more than a
    /// hundred fragments in one package does not sort `100_` before `11_`.
    pub fn package_fragments(&self) -> Vec<KlibFragment> {
        let mut fragments: Vec<(String, u32, String)> = self
            .entries
            .iter()
            .filter(|entry| {
                entry.ends_with(".knm")
                    && (entry.starts_with(PACKAGE_PREFIX) || entry.starts_with(ROOT_PACKAGE_PREFIX))
            })
            .map(|entry| {
                let fqname = package_fqname(entry);
                (fqname, chunk_number(entry), entry.clone())
            })
            .collect();
        fragments.sort();
        fragments
            .into_iter()
            .map(|(package_fqname, _, entry)| KlibFragment {
                package_fqname,
                entry,
            })
            .collect()
    }

    /// The serialized-IR entries, as archive-relative paths. The bodies live here; decoding them is
    /// a separate, lower serialization this module deliberately does not enter.
    pub fn ir_entries(&self) -> Vec<&str> {
        self.entries
            .iter()
            .filter(|entry| entry.starts_with(IR_PREFIX))
            .map(String::as_str)
            .collect()
    }
}

/// The package a fragment declares into, from its entry path.
///
/// Three spellings the serializer writes are accepted. A named package lives in its own directory
/// (`default/linkdata/package_kotlin.collections/03_collections.knm`) or, in a library with one
/// fragment per package, as a single flat entry (`default/linkdata/package_kotlin.collections.knm`).
/// The ROOT package is `default/linkdata/root_package/0_.knm` — a directory of its own rather than
/// `package_` with nothing after it — and yields the empty name.
fn package_fqname(entry: &str) -> String {
    if entry.starts_with(ROOT_PACKAGE_PREFIX) {
        return String::new();
    }
    let rest = &entry[PACKAGE_PREFIX.len()..];
    match rest.split_once('/') {
        Some((fqname, _)) => fqname.to_string(),
        None => rest.trim_end_matches(".knm").to_string(),
    }
}

/// The serializer's chunk number from a fragment file name (`03_collections.knm` → 3).
///
/// A flat fragment has no number; it sorts last, behind every numbered chunk of its package.
fn chunk_number(entry: &str) -> u32 {
    let name = entry.rsplit('/').next().unwrap_or(entry);
    let digits: String = name.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().unwrap_or(u32::MAX)
}

fn validate_entry_path(archive: &Path, entry: &str) -> Result<(), KlibError> {
    let path = Path::new(entry);
    let portable_windows_prefix = entry
        .as_bytes()
        .get(..2)
        .is_some_and(|prefix| prefix[0].is_ascii_alphabetic() && prefix[1] == b':');
    let only_normal_components = path
        .components()
        .all(|component| matches!(component, std::path::Component::Normal(_)));
    if entry.is_empty()
        || path.is_absolute()
        || portable_windows_prefix
        || entry.contains('\\')
        || !only_normal_components
        || entry
            .split('/')
            .any(|component| component.is_empty() || component == "." || component == "..")
    {
        return Err(KlibError::InvalidEntryPath {
            archive: archive.to_path_buf(),
            entry: entry.to_string(),
        });
    }
    Ok(())
}

fn reject_duplicate_entries(archive: &Path, entries: &[String]) -> Result<(), KlibError> {
    for pair in entries.windows(2) {
        if pair[0] == pair[1] {
            return Err(KlibError::DuplicateEntry {
                archive: archive.to_path_buf(),
                entry: pair[0].clone(),
            });
        }
    }
    Ok(())
}

#[derive(Clone, Copy)]
struct ZipDirectoryLayout {
    start: u64,
    end: u64,
    entry_count: u16,
}

fn invalid_zip(path: &Path, detail: impl Into<String>) -> KlibError {
    KlibError::InvalidZip {
        path: path.to_path_buf(),
        detail: detail.into(),
    }
}

/// Read the non-ZIP64 end record and turn its relative directory offset into an absolute file
/// range. The range and count are the authority for the later census; neither an early signature
/// nor EOF may silently shorten it.
fn zip_directory_layout(
    path: &Path,
    file: &mut std::fs::File,
) -> Result<ZipDirectoryLayout, KlibError> {
    const EOCD_SIGNATURE: [u8; 4] = 0x0605_4b50u32.to_le_bytes();
    const ZIP64_LOCATOR_SIGNATURE: [u8; 4] = 0x0706_4b50u32.to_le_bytes();
    const EOCD_FIXED: usize = 22;
    const MAX_COMMENT: usize = u16::MAX as usize;

    let length = file
        .metadata()
        .map_err(|source| KlibError::Io {
            operation: "inspect",
            path: path.to_path_buf(),
            source,
        })?
        .len();
    let tail_length = length.min((EOCD_FIXED + MAX_COMMENT) as u64) as usize;
    file.seek(std::io::SeekFrom::Start(length - tail_length as u64))
        .map_err(|source| KlibError::Io {
            operation: "seek",
            path: path.to_path_buf(),
            source,
        })?;
    let mut tail = vec![0; tail_length];
    file.read_exact(&mut tail).map_err(|source| KlibError::Io {
        operation: "read ZIP end record",
        path: path.to_path_buf(),
        source,
    })?;
    let Some(relative_eocd) = (0..=tail.len().saturating_sub(EOCD_FIXED))
        .rev()
        .find(|&offset| {
            tail.get(offset..offset + 4) == Some(&EOCD_SIGNATURE)
                && tail
                    .get(offset + 20..offset + 22)
                    .map(|bytes| u16::from_le_bytes([bytes[0], bytes[1]]) as usize)
                    .is_some_and(|comment| offset + EOCD_FIXED + comment == tail.len())
        })
    else {
        return Err(invalid_zip(
            path,
            "invalid Zip archive: Could not find EOCD",
        ));
    };
    let eocd = &tail[relative_eocd..relative_eocd + EOCD_FIXED];
    let absolute_eocd = length - tail_length as u64 + relative_eocd as u64;
    let has_zip64_locator = if relative_eocd >= 20 {
        tail.get(relative_eocd - 20..relative_eocd - 16) == Some(&ZIP64_LOCATOR_SIGNATURE)
    } else if absolute_eocd >= 20 {
        file.seek(std::io::SeekFrom::Start(absolute_eocd - 20))
            .map_err(|source| KlibError::Io {
                operation: "seek",
                path: path.to_path_buf(),
                source,
            })?;
        let mut signature = [0; 4];
        file.read_exact(&mut signature)
            .map_err(|source| KlibError::Io {
                operation: "read ZIP64 locator",
                path: path.to_path_buf(),
                source,
            })?;
        signature == ZIP64_LOCATOR_SIGNATURE
    } else {
        false
    };
    if has_zip64_locator {
        return Err(invalid_zip(path, "ZIP64 KLIB archives are unsupported"));
    }
    let disk = u16::from_le_bytes([eocd[4], eocd[5]]);
    let directory_disk = u16::from_le_bytes([eocd[6], eocd[7]]);
    let disk_entries = u16::from_le_bytes([eocd[8], eocd[9]]);
    let entry_count = u16::from_le_bytes([eocd[10], eocd[11]]);
    let directory_size = u32::from_le_bytes([eocd[12], eocd[13], eocd[14], eocd[15]]);
    let directory_offset = u32::from_le_bytes([eocd[16], eocd[17], eocd[18], eocd[19]]);
    if disk == u16::MAX
        || directory_disk == u16::MAX
        || disk_entries == u16::MAX
        || entry_count == u16::MAX
        || directory_size == u32::MAX
        || directory_offset == u32::MAX
    {
        return Err(invalid_zip(path, "ZIP64 KLIB archives are unsupported"));
    }
    if disk != 0 || directory_disk != 0 || disk_entries != entry_count {
        return Err(invalid_zip(
            path,
            "multi-disk KLIB ZIP archives are unsupported",
        ));
    }
    let relative_end = u64::from(directory_offset)
        .checked_add(u64::from(directory_size))
        .ok_or_else(|| invalid_zip(path, "central-directory range overflows"))?;
    let archive_prefix = absolute_eocd
        .checked_sub(relative_end)
        .ok_or_else(|| invalid_zip(path, "central-directory range exceeds the archive"))?;
    let start = archive_prefix + u64::from(directory_offset);
    let end = start + u64::from(directory_size);
    if end != absolute_eocd {
        return Err(invalid_zip(
            path,
            "central-directory range does not end at EOCD",
        ));
    }
    Ok(ZipDirectoryLayout {
        start,
        end,
        entry_count,
    })
}

/// `zip` indexes central-directory records by raw name, so a later duplicate replaces the earlier
/// record before `ZipArchive::len`/`by_index` can expose it. Census the exact EOCD-owned range first.
fn reject_duplicate_zip_entries(
    archive: &Path,
    directory: ZipDirectoryLayout,
) -> Result<(), KlibError> {
    const CENTRAL_DIRECTORY_HEADER: u32 = 0x0201_4b50;
    const FIXED_AFTER_SIGNATURE: usize = 42;

    let mut file = std::fs::File::open(archive).map_err(|source| KlibError::Io {
        operation: "open",
        path: archive.to_path_buf(),
        source,
    })?;
    file.seek(std::io::SeekFrom::Start(directory.start))
        .map_err(|source| KlibError::Io {
            operation: "seek",
            path: archive.to_path_buf(),
            source,
        })?;
    let mut names = std::collections::HashSet::<Vec<u8>>::new();
    for index in 0..directory.entry_count {
        let mut signature = [0u8; 4];
        file.read_exact(&mut signature).map_err(|_| {
            invalid_zip(
                archive,
                format!("truncated central-directory entry {index}"),
            )
        })?;
        if u32::from_le_bytes(signature) != CENTRAL_DIRECTORY_HEADER {
            return Err(invalid_zip(
                archive,
                format!("invalid central-directory entry {index} signature"),
            ));
        }
        let mut fixed = [0u8; FIXED_AFTER_SIGNATURE];
        file.read_exact(&mut fixed).map_err(|_| {
            invalid_zip(
                archive,
                format!("truncated central-directory entry {index}"),
            )
        })?;
        let name_length = u16::from_le_bytes([fixed[24], fixed[25]]) as usize;
        let extra_length = u16::from_le_bytes([fixed[26], fixed[27]]) as usize;
        let comment_length = u16::from_le_bytes([fixed[28], fixed[29]]) as usize;
        let variable_length = name_length + extra_length + comment_length;
        let variable_start = file.stream_position().map_err(|source| KlibError::Io {
            operation: "inspect central directory",
            path: archive.to_path_buf(),
            source,
        })?;
        if variable_start
            .checked_add(variable_length as u64)
            .is_none_or(|end| end > directory.end)
        {
            return Err(invalid_zip(
                archive,
                format!("central-directory entry {index} exceeds EOCD bounds"),
            ));
        }
        let mut name = vec![0u8; name_length];
        file.read_exact(&mut name).map_err(|_| {
            invalid_zip(
                archive,
                format!("truncated central-directory entry {index} name"),
            )
        })?;
        if !names.insert(name.clone()) {
            let entry = String::from_utf8(name).map_err(|_| KlibError::NonUtf8Entry {
                archive: archive.to_path_buf(),
                path: PathBuf::from("duplicate zip entry"),
            })?;
            return Err(KlibError::DuplicateEntry {
                archive: archive.to_path_buf(),
                entry,
            });
        }
        let trailing = i64::from(extra_length as u32 + comment_length as u32);
        file.seek(std::io::SeekFrom::Current(trailing))
            .map_err(|_| {
                invalid_zip(
                    archive,
                    format!("truncated central-directory entry {index} trailer"),
                )
            })?;
    }
    let position = file.stream_position().map_err(|source| KlibError::Io {
        operation: "inspect central directory",
        path: archive.to_path_buf(),
        source,
    })?;
    if position != directory.end {
        return Err(invalid_zip(
            archive,
            format!(
                "central-directory count ends at byte {position}, expected {}",
                directory.end
            ),
        ));
    }
    Ok(())
}

fn collect_directory_entries(
    archive: &Path,
    root: &Path,
    prefix: String,
    depth: usize,
    out: &mut Vec<String>,
) -> Result<(), KlibError> {
    if depth > MAX_DIRECTORY_DEPTH {
        return Err(KlibError::DirectoryDepth {
            archive: archive.to_path_buf(),
            path: root.join(&prefix),
            maximum: MAX_DIRECTORY_DEPTH,
        });
    }
    let directory = std::fs::read_dir(root.join(&prefix)).map_err(|source| KlibError::Io {
        operation: "list",
        path: root.join(&prefix),
        source,
    })?;
    for child in directory {
        let child = child.map_err(|source| KlibError::Io {
            operation: "list",
            path: root.join(&prefix),
            source,
        })?;
        let child_path = child.path();
        let name = child
            .file_name()
            .into_string()
            .map_err(|_| KlibError::NonUtf8Entry {
                archive: archive.to_path_buf(),
                path: child_path.clone(),
            })?;
        let path = if prefix.is_empty() {
            name
        } else {
            format!("{prefix}/{name}")
        };
        validate_entry_path(archive, &path)?;
        let kind = child.file_type().map_err(|source| KlibError::Io {
            operation: "inspect",
            path: child_path.clone(),
            source,
        })?;
        if kind.is_symlink() {
            return Err(KlibError::SymlinkEntry {
                archive: archive.to_path_buf(),
                path: child_path,
            });
        }
        if kind.is_dir() {
            collect_directory_entries(archive, root, path, depth + 1, out)?;
        } else if kind.is_file() {
            out.push(path);
        } else {
            return Err(KlibError::UnsupportedEntry {
                archive: archive.to_path_buf(),
                path: child_path,
            });
        }
    }
    Ok(())
}

fn reject_symlink_components(archive: &Path, path: &Path) -> Result<(), KlibError> {
    let relative = path
        .strip_prefix(archive)
        .map_err(|_| KlibError::InvalidEntryPath {
            archive: archive.to_path_buf(),
            entry: path.display().to_string(),
        })?;
    let mut current = archive.to_path_buf();
    for component in relative.components() {
        current.push(component);
        let metadata = std::fs::symlink_metadata(&current).map_err(|source| KlibError::Io {
            operation: "inspect",
            path: current.clone(),
            source,
        })?;
        if metadata.file_type().is_symlink() {
            return Err(KlibError::SymlinkEntry {
                archive: archive.to_path_buf(),
                path: current,
            });
        }
    }
    Ok(())
}

/// A klib's `default/manifest`: the library's identity, versions and target set.
///
/// The file is a `java.util.Properties` document. The distribution's own manifests happen to be plain
/// `key=value` lines, but that is a property of those files and not of the format, so the separators,
/// escapes and line continuations `Properties` defines are honoured rather than assumed absent — a
/// manifest written by anything else may use them.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct KlibManifest {
    values: BTreeMap<String, String>,
}

impl KlibManifest {
    pub fn parse(bytes: &[u8]) -> Result<Self, KlibManifestError> {
        let text = std::str::from_utf8(bytes).map_err(|error| KlibManifestError::InvalidUtf8 {
            valid_up_to: error.valid_up_to(),
        })?;
        let mut values = BTreeMap::new();
        for logical in logical_lines(&text) {
            let Some((key, value)) = split_property(&logical)? else {
                continue;
            };
            values.insert(key, value);
        }
        Ok(Self { values })
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        self.values.get(key).map(String::as_str)
    }

    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    /// The library's module name — what a dependent records in its own `depends` and what an
    /// `IdSignature` resolves against.
    pub fn unique_name(&self) -> Option<&str> {
        self.get("unique_name")
    }

    /// The KLIB ABI version: which layout the container and its linkdata follow.
    pub fn abi_version(&self) -> Option<&str> {
        self.get("abi_version")
    }

    /// The Kotlin metadata protobuf version of the `.knm` fragments.
    pub fn metadata_version(&self) -> Option<&str> {
        self.get("metadata_version")
    }

    pub fn compiler_version(&self) -> Option<&str> {
        self.get("compiler_version")
    }

    /// Which set of builtins the library was compiled against (`JS`, `WASM`, `NATIVE`, `COMMON`).
    /// This is the field that tells a common klib apart from a platform one.
    pub fn builtins_platform(&self) -> Option<&str> {
        self.get("builtins_platform")
    }

    /// The `unique_name`s this library depends on, in manifest order.
    ///
    /// The distribution's klibs omit the key entirely, so an empty result means the manifest is
    /// silent about the library graph, not that the library stands alone.
    pub fn depends(&self) -> Vec<&str> {
        self.words("depends")
    }

    /// The concrete targets this library was compiled for.
    ///
    /// Native records `native_targets`, wasm records `wasm_targets`, and JS records neither — a JS
    /// klib has exactly one target and does not name it. An empty result therefore means
    /// "unconstrained", not "no target".
    pub fn targets(&self) -> Vec<&str> {
        let native = self.words("native_targets");
        if native.is_empty() {
            self.words("wasm_targets")
        } else {
            native
        }
    }

    fn words(&self, key: &str) -> Vec<&str> {
        self.get(key)
            .map(|value| value.split_whitespace().collect())
            .unwrap_or_default()
    }
}

/// Join `Properties` continuation lines: a line whose trailing run of backslashes is odd continues
/// into the next, and the continuation's leading whitespace is dropped.
fn logical_lines(text: &str) -> Vec<String> {
    let mut lines = Vec::new();
    let mut pending: Option<String> = None;
    for raw in text.lines() {
        let line = match &pending {
            Some(_) => raw.trim_start(),
            None => {
                let trimmed = raw.trim_start();
                if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with('!') {
                    continue;
                }
                trimmed
            }
        };
        let continues = trailing_backslashes(line) % 2 == 1;
        let body = if continues {
            &line[..line.len() - 1]
        } else {
            line
        };
        let mut accumulated = pending.take().unwrap_or_default();
        accumulated.push_str(body);
        if continues {
            pending = Some(accumulated);
        } else {
            lines.push(accumulated);
        }
    }
    if let Some(accumulated) = pending {
        lines.push(accumulated);
    }
    lines
}

fn trailing_backslashes(line: &str) -> usize {
    line.chars().rev().take_while(|c| *c == '\\').count()
}

/// Split one logical line at its first UNESCAPED `=`, `:` or run of whitespace, then unescape both
/// halves. `Properties` allows all three separators, which is why a key cannot simply be the text
/// before the first `=`.
fn split_property(line: &str) -> Result<Option<(String, String)>, KlibManifestError> {
    let mut key = String::new();
    let mut characters = line.chars().peekable();
    let mut separator_seen = false;
    while let Some(character) = characters.next() {
        match character {
            '\\' => {
                key.push('\\');
                if let Some(escaped) = characters.next() {
                    key.push(escaped);
                }
            }
            '=' | ':' => {
                separator_seen = true;
                break;
            }
            c if c.is_whitespace() => {
                // Whitespace ends the key; a single `=`/`:` may still follow it as the separator.
                while let Some(next) = characters.peek() {
                    if next.is_whitespace() {
                        characters.next();
                    } else {
                        break;
                    }
                }
                if matches!(characters.peek(), Some('=') | Some(':')) {
                    characters.next();
                }
                separator_seen = true;
                break;
            }
            c => key.push(c),
        }
    }
    if key.is_empty() {
        return Ok(None);
    }
    if !separator_seen {
        return Ok(Some((unescape(&key)?, String::new())));
    }
    let value: String = characters.collect();
    Ok(Some((unescape(&key)?, unescape(value.trim_start())?)))
}

fn unescape(text: &str) -> Result<String, KlibManifestError> {
    let mut out = String::with_capacity(text.len());
    let mut characters = text.chars().peekable();
    while let Some(character) = characters.next() {
        if character != '\\' {
            out.push(character);
            continue;
        }
        match characters.next() {
            Some('t') => out.push('\t'),
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some('f') => out.push('\u{c}'),
            Some('u') => {
                let (high, high_escape) = unicode_code_unit(&mut characters)?;
                if (0xd800..=0xdbff).contains(&high) {
                    if characters.next() != Some('\\') || characters.next() != Some('u') {
                        return Err(KlibManifestError::InvalidUnicodeEscape {
                            escape: high_escape,
                        });
                    }
                    let (low, low_escape) = unicode_code_unit(&mut characters)?;
                    if !(0xdc00..=0xdfff).contains(&low) {
                        return Err(KlibManifestError::InvalidUnicodeEscape {
                            escape: format!("{high_escape}{low_escape}"),
                        });
                    }
                    let scalar =
                        0x10000 + ((u32::from(high) - 0xd800) << 10) + (u32::from(low) - 0xdc00);
                    out.push(char::from_u32(scalar).expect("validated surrogate pair"));
                } else if (0xdc00..=0xdfff).contains(&high) {
                    return Err(KlibManifestError::InvalidUnicodeEscape {
                        escape: high_escape,
                    });
                } else {
                    out.push(char::from_u32(u32::from(high)).expect("non-surrogate code unit"));
                }
            }
            Some(other) => out.push(other),
            None => out.push('\\'),
        }
    }
    Ok(out)
}

fn unicode_code_unit<I>(characters: &mut I) -> Result<(u16, String), KlibManifestError>
where
    I: Iterator<Item = char>,
{
    let mut digits = String::with_capacity(4);
    for _ in 0..4 {
        match characters.next() {
            Some(digit) if digit.is_ascii_hexdigit() => digits.push(digit),
            _ => {
                return Err(KlibManifestError::InvalidUnicodeEscape {
                    escape: format!("\\u{digits}"),
                });
            }
        }
    }
    let escape = format!("\\u{digits}");
    let value =
        u16::from_str_radix(&digits, 16).expect("four ASCII hexadecimal digits always fit in u16");
    Ok((value, escape))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos())
            .unwrap_or_default();
        let directory =
            std::env::temp_dir().join(format!("krusty-{tag}-{}-{unique}", std::process::id()));
        std::fs::create_dir_all(&directory).expect("create test directory");
        directory
    }

    fn write(root: &Path, entry: &str, contents: &[u8]) {
        let path = root.join(entry);
        std::fs::create_dir_all(path.parent().expect("entry has a parent"))
            .expect("create entry directory");
        std::fs::write(path, contents).expect("write entry");
    }

    // Raw stored ZIPs owned by this repository. They are intentionally not produced with
    // `zip::ZipWriter` in the test: the reader is exercised against fixed central-directory bytes,
    // including shapes a conforming writer refuses to create or silently normalizes.
    const VALID_ZIP: &str = "504b0304140000000000000021004395ff6914000000140000001000000064656661756c742f6d616e6966657374756e697175655f6e616d653d666978747572650a504b030414000000000000002100191e210c19000000190000001700000064656661756c742f6c696e6b646174612f6d6f64756c650a153c756e7061636b65644578616d706c654b6c69623e3a00504b030414000000000000002100d5e7263a51000000510000002400000064656661756c742f6c696e6b646174612f726f6f745f7061636b6167652f305f2e6b6e6d0a1d0a046d61696e0a066b6f746c696e0a04556e69740a076d61696e2e6b74120c0a0210010a060800100218001a1c1a0710003800e00a03f201040a023001d80affffffffffffffffff01e00a00ea0a00504b01021403140000000000000021004395ff691400000014000000100000000000000000000000a4810000000064656661756c742f6d616e6966657374504b0102140314000000000000002100191e210c1900000019000000170000000000000000000000a4814200000064656661756c742f6c696e6b646174612f6d6f64756c65504b0102140314000000000000002100d5e7263a5100000051000000240000000000000000000000a4819000000064656661756c742f6c696e6b646174612f726f6f745f7061636b6167652f305f2e6b6e6d504b05060000000003000300d5000000230100000000";
    const UNSAFE_PATH_ZIP: &str = "504b03041400000000008e69335d8316dc8c0100000001000000090000002e2e2f65736361706578504b03041400000000008e69335d5ff15c150e0000000e0000001000000064656661756c742f6d616e6966657374756e697175655f6e616d653d780a504b010214031400000000008e69335d8316dc8c01000000010000000900000000000000000000008001000000002e2e2f657363617065504b010214031400000000008e69335d5ff15c150e0000000e00000010000000000000000000000080012800000064656661756c742f6d616e6966657374504b0506000000000200020075000000640000000000";
    const DUPLICATE_ENTRY_ZIP: &str = "504b03041400000000008e69335d43beb7e801000000010000001000000064656661756c742f6d616e696665737461504b03041400000000008e69335df9efbe7101000000010000001000000064656661756c742f6d616e696665737462504b010214031400000000008e69335d43beb7e8010000000100000010000000000000000000000080010000000064656661756c742f6d616e6966657374504b010214031400000000008e69335df9efbe71010000000100000010000000000000000000000080012f00000064656661756c742f6d616e6966657374504b050600000000020002007c0000005e0000000000";
    const CORRUPT_CRC_ZIP: &str = "504b03041400000000008e69335d4395ff6914000000140000001000000064656661756c742f6d616e6966657374746e697175655f6e616d653d666978747572650a504b010214031400000000008e69335d4395ff69140000001400000010000000000000000000000080010000000064656661756c742f6d616e6966657374504b050600000000010001003e000000420000000000";
    const CORRUPT_CENTRAL_DIRECTORY_ZIP: &str = "504b03041400000000008e69335d4395ff6914000000140000001000000064656661756c742f6d616e6966657374756e697175655f6e616d653d666978747572650a504b03041400000000008e69335d61a807fe02000000020000001700000064656661756c742f6c696e6b646174612f6d6f64756c650801504b010214031400000000008e69335d4395ff69140000001400000010000000000000000000000080010000000064656661756c742f6d616e6966657374504b010214031400000000008e69335d61a807fe020000000200000017000000000000000000000080014200000064656661756c742f6c696e6b646174612f6d6f64756c6542414421000000000200020083000000790000000000";

    fn raw_zip(hex: &str) -> Vec<u8> {
        assert_eq!(hex.len() % 2, 0);
        hex.as_bytes()
            .chunks_exact(2)
            .map(|pair| {
                let digit = |byte: u8| match byte {
                    b'0'..=b'9' => byte - b'0',
                    b'a'..=b'f' => byte - b'a' + 10,
                    _ => panic!("non-hex raw ZIP fixture byte"),
                };
                digit(pair[0]) << 4 | digit(pair[1])
            })
            .collect()
    }

    fn write_raw_zip(root: &Path, name: &str, bytes: &str) -> PathBuf {
        let path = root.join(name);
        std::fs::write(&path, raw_zip(bytes)).expect("write raw ZIP fixture");
        path
    }

    fn write_raw_zip_bytes(root: &Path, name: &str, bytes: &[u8]) -> PathBuf {
        let path = root.join(name);
        std::fs::write(&path, bytes).expect("write raw ZIP fixture");
        path
    }

    #[test]
    fn manifest_reads_properties_separators_and_escapes() {
        let manifest = KlibManifest::parse(
            "# a comment\n\
             ! another\n\
             unique_name=mód\n\
             abi_version:2.4.0\n\
             metadata_version 2.4.0\n\
             depends=kotlin \\\n  kotlinx\n\
             escaped\\=key=value\n\
             controls=tab\\tline\\ncarriage\\rform\\f\n\
             unicode=\\u004b\n"
                .as_bytes(),
        )
        .expect("valid manifest");
        // Kotlin 2.4.10's metadata compiler writes the non-ASCII module name as raw UTF-8
        // (`mód` = `6d c3 b3 64`), while the surrounding grammar retains Properties escapes.
        assert_eq!(manifest.unique_name(), Some("mód"));
        assert_eq!(manifest.abi_version(), Some("2.4.0"));
        assert_eq!(manifest.metadata_version(), Some("2.4.0"));
        assert_eq!(manifest.depends(), vec!["kotlin", "kotlinx"]);
        assert_eq!(manifest.get("escaped=key"), Some("value"));
        assert_eq!(
            manifest.get("controls"),
            Some("tab\tline\ncarriage\rform\u{c}")
        );
        assert_eq!(manifest.get("unicode"), Some("K"));
        assert_eq!(manifest.get("absent"), None);
    }

    #[test]
    fn manifest_rejects_invalid_utf8_and_unicode_escapes_exactly() {
        assert_eq!(
            KlibManifest::parse(b"unique_name=\xff\n").unwrap_err(),
            KlibManifestError::InvalidUtf8 { valid_up_to: 12 }
        );
        assert_eq!(
            KlibManifest::parse(b"unique_name=\\u12\n").unwrap_err(),
            KlibManifestError::InvalidUnicodeEscape {
                escape: "\\u12".to_string()
            }
        );
        assert_eq!(
            KlibManifest::parse(b"unique_name=\\uD83Dx\n").unwrap_err(),
            KlibManifestError::InvalidUnicodeEscape {
                escape: "\\uD83D".to_string()
            }
        );
        assert_eq!(
            KlibManifest::parse(b"unique_name=\\uDE03\n").unwrap_err(),
            KlibManifestError::InvalidUnicodeEscape {
                escape: "\\uDE03".to_string()
            }
        );
        let manifest =
            KlibManifest::parse(b"unique_name=\\uD83D\\uDE03\n").expect("valid surrogate pair");
        assert_eq!(manifest.unique_name(), Some("😃"));
    }

    #[test]
    fn absent_manifest_reports_no_fields() {
        let manifest = KlibManifest::default();
        assert!(manifest.is_empty());
        assert_eq!(manifest.unique_name(), None);
        assert!(manifest.depends().is_empty());
        assert!(manifest.targets().is_empty());
    }

    #[test]
    fn native_targets_win_over_wasm_targets() {
        let native = KlibManifest::parse(b"native_targets=linux_x64 macos_arm64\n")
            .expect("valid native manifest");
        assert_eq!(native.targets(), vec!["linux_x64", "macos_arm64"]);
        let wasm = KlibManifest::parse(b"wasm_targets=wasm-js\n").expect("valid Wasm manifest");
        assert_eq!(wasm.targets(), vec!["wasm-js"]);
    }

    /// An unpacked klib is what a Kotlin/Native distribution ships, so the directory shape is a
    /// first-class input rather than a convenience.
    #[test]
    fn unpacked_directory_klib_reads_like_a_zip_one() {
        let root = temp_dir("klib-directory");
        write(
            &root,
            "default/manifest",
            b"unique_name=stdlib\nbuiltins_platform=NATIVE\n",
        );
        write(&root, "default/linkdata/module", b"module-header");
        write(&root, "default/linkdata/root_package/00_.knm", b"root");
        write(
            &root,
            "default/linkdata/package_kotlin.collections/03_collections.knm",
            b"three",
        );
        write(
            &root,
            "default/linkdata/package_kotlin.collections/100_collections.knm",
            b"hundred",
        );
        write(
            &root,
            "default/linkdata/package_kotlin.collections/11_collections.knm",
            b"eleven",
        );
        write(&root, "default/ir/bodies.knb", b"bodies");

        let archive = KlibArchive::open(&root).expect("open unpacked klib");
        let manifest = archive.manifest().expect("read manifest");
        assert_eq!(manifest.unique_name(), Some("stdlib"));
        assert_eq!(manifest.builtins_platform(), Some("NATIVE"));
        assert_eq!(
            archive
                .module_header()
                .expect("read module header")
                .as_slice(),
            &b"module-header"[..]
        );
        assert_eq!(archive.ir_entries(), vec!["default/ir/bodies.knb"]);

        let fragments = archive.package_fragments();
        assert_eq!(
            fragments
                .iter()
                .map(|fragment| (fragment.package_fqname.as_str(), fragment.entry.as_str()))
                .collect::<Vec<_>>(),
            vec![
                ("", "default/linkdata/root_package/00_.knm"),
                (
                    "kotlin.collections",
                    "default/linkdata/package_kotlin.collections/03_collections.knm"
                ),
                (
                    "kotlin.collections",
                    "default/linkdata/package_kotlin.collections/11_collections.knm"
                ),
                (
                    "kotlin.collections",
                    "default/linkdata/package_kotlin.collections/100_collections.knm"
                ),
            ],
            "fragments order by package, then by the serializer's chunk number"
        );
        assert_eq!(
            archive
                .read(&fragments[1].entry)
                .expect("read fragment")
                .as_slice(),
            &b"three"[..]
        );
        assert!(matches!(
            archive.read("default/ir/absent.knb"),
            Err(KlibError::MissingEntry { .. })
        ));
        assert!(matches!(
            archive.read("../outside"),
            Err(KlibError::InvalidEntryPath { .. })
        ));
    }

    #[test]
    fn repository_owned_zip_klib_reads_the_same_container_contract() {
        let root = temp_dir("klib-zip");
        let path = write_raw_zip(&root, "fixture.klib", VALID_ZIP);

        let archive = KlibArchive::open(&path).expect("open fixture KLIB");
        assert_eq!(
            archive.entries(),
            [
                "default/linkdata/module",
                "default/linkdata/root_package/0_.knm",
                "default/manifest"
            ]
        );
        let manifest = archive.manifest().expect("read fixture manifest");
        assert_eq!(manifest.unique_name(), Some("fixture"));
        assert_eq!(
            archive.module_header().expect("read fixture module"),
            raw_zip("0a153c756e7061636b65644578616d706c654b6c69623e3a00")
        );
        assert_eq!(archive.package_fragments().len(), 1);
        assert_eq!(archive.package_fragments()[0].package_fqname, "");
    }

    #[test]
    fn raw_zip_failures_preserve_the_exact_container_error() {
        let root = temp_dir("klib-raw-zip-errors");

        let unsafe_path = write_raw_zip(&root, "unsafe.klib", UNSAFE_PATH_ZIP);
        match KlibArchive::open(&unsafe_path) {
            Err(KlibError::InvalidEntryPath { archive, entry }) => {
                assert_eq!(archive, unsafe_path);
                assert_eq!(entry, "../escape");
            }
            Err(error) => panic!("unexpected unsafe-path error: {error}"),
            Ok(_) => panic!("unsafe raw ZIP opened"),
        }

        let duplicate = write_raw_zip(&root, "duplicate.klib", DUPLICATE_ENTRY_ZIP);
        match KlibArchive::open(&duplicate) {
            Err(KlibError::DuplicateEntry { archive, entry }) => {
                assert_eq!(archive, duplicate);
                assert_eq!(entry, "default/manifest");
            }
            Err(error) => panic!("unexpected duplicate-entry error: {error}"),
            Ok(_) => panic!("duplicate-entry raw ZIP opened"),
        }

        let corrupt_crc = write_raw_zip(&root, "corrupt-crc.klib", CORRUPT_CRC_ZIP);
        let archive = KlibArchive::open(&corrupt_crc).expect("CRC is checked while reading entry");
        match archive.manifest() {
            Err(KlibError::Io {
                operation,
                path,
                source,
            }) => {
                assert_eq!(operation, "read");
                assert_eq!(path, corrupt_crc.join("default/manifest"));
                assert_eq!(source.kind(), std::io::ErrorKind::InvalidData);
                assert_eq!(source.to_string(), "Invalid checksum");
            }
            Err(error) => panic!("unexpected corrupt-CRC error: {error}"),
            Ok(_) => panic!("corrupt-CRC manifest read successfully"),
        }

        let corrupt_central =
            write_raw_zip(&root, "corrupt-central.klib", CORRUPT_CENTRAL_DIRECTORY_ZIP);
        match KlibArchive::open(&corrupt_central) {
            Err(KlibError::InvalidZip { path, detail }) => {
                assert_eq!(path, corrupt_central);
                assert_eq!(detail, "invalid Zip archive: Could not find EOCD");
            }
            Err(error) => panic!("unexpected central-directory error: {error}"),
            Ok(_) => panic!("corrupt-central-directory raw ZIP opened"),
        }
    }

    #[test]
    fn eocd_count_bounds_and_truncation_are_authoritative() {
        let root = temp_dir("klib-eocd-errors");

        let mut payload_signature = raw_zip(VALID_ZIP);
        payload_signature[46..50].copy_from_slice(&0x0706_4b50u32.to_le_bytes());
        let payload_signature =
            write_raw_zip_bytes(&root, "zip64-signature-in-payload.klib", &payload_signature);
        KlibArchive::open(&payload_signature)
            .expect("a ZIP64 signature in entry data is not a ZIP64 locator");

        let mut zip64_locator = raw_zip(VALID_ZIP);
        let eocd = zip64_locator.len() - 22;
        let mut locator = [0u8; 20];
        locator[..4].copy_from_slice(&0x0706_4b50u32.to_le_bytes());
        zip64_locator.splice(eocd..eocd, locator);
        let zip64_locator = write_raw_zip_bytes(&root, "zip64-locator.klib", &zip64_locator);
        match KlibArchive::open(&zip64_locator) {
            Err(KlibError::InvalidZip { path, detail }) => {
                assert_eq!(path, zip64_locator);
                assert_eq!(detail, "ZIP64 KLIB archives are unsupported");
            }
            Err(error) => panic!("unexpected ZIP64-locator error: {error}"),
            Ok(_) => panic!("ZIP64 locator was accepted"),
        }

        let mut zip64_sentinel = raw_zip(VALID_ZIP);
        let eocd = zip64_sentinel.len() - 22;
        zip64_sentinel[eocd + 8..eocd + 12].copy_from_slice(&u16::MAX.to_le_bytes().repeat(2));
        let zip64_sentinel = write_raw_zip_bytes(&root, "zip64-sentinel.klib", &zip64_sentinel);
        match KlibArchive::open(&zip64_sentinel) {
            Err(KlibError::InvalidZip { path, detail }) => {
                assert_eq!(path, zip64_sentinel);
                assert_eq!(detail, "ZIP64 KLIB archives are unsupported");
            }
            Err(error) => panic!("unexpected ZIP64-sentinel error: {error}"),
            Ok(_) => panic!("ZIP64 sentinel was accepted"),
        }

        let mut wrong_count = raw_zip(VALID_ZIP);
        let eocd = wrong_count.len() - 22;
        wrong_count[eocd + 8..eocd + 12].copy_from_slice(&4u16.to_le_bytes().repeat(2));
        let wrong_count = write_raw_zip_bytes(&root, "wrong-count.klib", &wrong_count);
        match KlibArchive::open(&wrong_count) {
            Err(KlibError::InvalidZip { path, detail }) => {
                assert_eq!(path, wrong_count);
                assert_eq!(detail, "invalid central-directory entry 3 signature");
            }
            Err(error) => panic!("unexpected wrong-count error: {error}"),
            Ok(_) => panic!("wrong-count ZIP opened"),
        }

        let mut out_of_bounds = raw_zip(VALID_ZIP);
        let eocd = out_of_bounds.len() - 22;
        let central = u32::from_le_bytes(
            out_of_bounds[eocd + 16..eocd + 20]
                .try_into()
                .expect("EOCD directory offset"),
        ) as usize;
        let third =
            central + (46 + "default/manifest".len()) + (46 + "default/linkdata/module".len());
        out_of_bounds[third + 28..third + 30].copy_from_slice(&u16::MAX.to_le_bytes());
        let out_of_bounds = write_raw_zip_bytes(&root, "out-of-bounds.klib", &out_of_bounds);
        match KlibArchive::open(&out_of_bounds) {
            Err(KlibError::InvalidZip { path, detail }) => {
                assert_eq!(path, out_of_bounds);
                assert_eq!(detail, "central-directory entry 2 exceeds EOCD bounds");
            }
            Err(error) => panic!("unexpected out-of-bounds error: {error}"),
            Ok(_) => panic!("out-of-bounds ZIP opened"),
        }

        let mut truncated = raw_zip(VALID_ZIP);
        let eocd = truncated.len() - 22;
        truncated.remove(eocd - 1);
        let truncated = write_raw_zip_bytes(&root, "truncated-directory.klib", &truncated);
        match KlibArchive::open(&truncated) {
            Err(KlibError::InvalidZip { path, detail }) => {
                assert_eq!(path, truncated);
                assert_eq!(detail, "central-directory range exceeds the archive");
            }
            Err(error) => panic!("unexpected truncated-directory error: {error}"),
            Ok(_) => panic!("truncated central-directory ZIP opened"),
        }
    }

    #[test]
    fn archive_entry_validation_rejects_portable_prefix_and_separator_forms() {
        let archive = Path::new("fixture.klib");
        for entry in [
            "C:/default/manifest",
            "C:default/manifest",
            r"\\server\share\default\manifest",
            r"default\manifest",
        ] {
            match validate_entry_path(archive, entry) {
                Err(KlibError::InvalidEntryPath {
                    archive: actual_archive,
                    entry: actual_entry,
                }) => {
                    assert_eq!(actual_archive, archive);
                    assert_eq!(actual_entry, entry);
                }
                Err(error) => panic!("unexpected entry error for {entry:?}: {error}"),
                Ok(()) => panic!("unsafe entry accepted: {entry:?}"),
            }
        }
    }

    #[test]
    fn a_directory_without_a_default_tree_is_not_a_klib() {
        let root = temp_dir("klib-not-a-klib");
        write(&root, "META-INF/MANIFEST.MF", b"Manifest-Version: 1.0\n");
        assert!(matches!(
            KlibArchive::open(&root),
            Err(KlibError::MissingDefaultTree { .. })
        ));
    }

    #[test]
    fn missing_and_corrupt_containers_are_explicit_errors() {
        let root = temp_dir("klib-container-errors");
        let missing = root.join("missing.klib");
        match KlibArchive::open(&missing) {
            Err(KlibError::Io {
                operation,
                path,
                source,
            }) => {
                assert_eq!(operation, "inspect");
                assert_eq!(path, missing);
                assert_eq!(source.kind(), std::io::ErrorKind::NotFound);
            }
            Err(error) => panic!("unexpected missing-container error: {error}"),
            Ok(_) => panic!("missing container opened"),
        }

        let corrupt = root.join("corrupt.klib");
        std::fs::write(&corrupt, b"this is not a zip").expect("write corrupt container");
        match KlibArchive::open(&corrupt) {
            Err(KlibError::InvalidZip { path, detail }) => {
                assert_eq!(path, corrupt);
                assert_eq!(detail, "invalid Zip archive: Could not find EOCD");
            }
            Err(error) => panic!("unexpected corrupt-container error: {error}"),
            Ok(_) => panic!("corrupt container opened"),
        }
    }

    #[test]
    fn missing_manifest_and_disappearing_entry_do_not_become_empty_data() {
        let root = temp_dir("klib-entry-errors");
        write(&root, "default/linkdata/module", b"module");
        let archive = KlibArchive::open(&root).expect("open container without manifest");
        assert!(matches!(
            archive.manifest(),
            Err(KlibError::MissingEntry { ref entry, .. }) if entry == "default/manifest"
        ));

        let module = root.join("default/linkdata/module");
        std::fs::remove_file(&module).expect("remove inventoried entry");
        match archive.module_header().unwrap_err() {
            KlibError::Io {
                operation,
                path,
                source,
            } => {
                assert_eq!(operation, "inspect");
                assert_eq!(path, module);
                assert_eq!(source.kind(), std::io::ErrorKind::NotFound);
            }
            error => panic!("unexpected unreadable-entry error: {error}"),
        }
    }

    #[cfg(unix)]
    #[test]
    fn unpacked_container_rejects_symlinks_instead_of_escaping_its_root() {
        use std::os::unix::fs::symlink;

        let root = temp_dir("klib-symlink");
        let outside = temp_dir("klib-outside");
        write(&outside, "manifest", b"unique_name=outside\n");
        std::fs::create_dir_all(root.join("default")).expect("create default tree");
        let link = root.join("default/manifest");
        symlink(outside.join("manifest"), &link).expect("create archive symlink");

        match KlibArchive::open(&root) {
            Err(KlibError::SymlinkEntry { archive, path }) => {
                assert_eq!(archive, root);
                assert_eq!(path, link);
            }
            Err(error) => panic!("unexpected symlink error: {error}"),
            Ok(_) => panic!("container with a symlink opened"),
        }
    }

    /// The root package has its own directory name, which is NOT `package_` with an empty tail: every
    /// klib the reference distribution ships spells it `root_package`, and a reader that only knew the
    /// `package_` prefix skipped the root package's declarations entirely.
    #[test]
    fn the_root_package_has_its_own_directory() {
        assert_eq!(package_fqname("default/linkdata/root_package/0_.knm"), "");
        assert_eq!(chunk_number("default/linkdata/root_package/0_.knm"), 0);
    }

    #[test]
    fn a_flat_fragment_names_its_package_too() {
        assert_eq!(
            package_fqname("default/linkdata/package_kotlin.collections.knm"),
            "kotlin.collections"
        );
        assert_eq!(
            chunk_number("default/linkdata/package_kotlin.knm"),
            u32::MAX
        );
        assert_eq!(
            chunk_number("default/linkdata/package_kotlin/07_kotlin.knm"),
            7
        );
    }
}
