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
use std::io::Read as _;
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
    Directory,
}

/// Why a klib could not be ingested.
///
/// The distinction the variants draw is the point of the type: [`Self::NotALibrary`] is an OPTIONAL
/// absence — the path is simply not a klib, which every caller may skip — while every other variant
/// is a library that exists and cannot be read. Collapsing the two, as an `Option` does, turns a
/// corrupt archive, an unreadable file and an absent optional dependency into one answer, and a
/// caller that skips absences then silently skips corruption too.
#[derive(Debug)]
pub enum KlibError {
    /// The path is not a klib: not a zip, and not a directory with a `default/` tree. Callers for
    /// which a klib is optional stop here; nothing is wrong.
    NotALibrary { path: PathBuf },
    /// The path exists but could not be opened or walked.
    Unreadable {
        path: PathBuf,
        cause: std::io::Error,
    },
    /// The file is a zip whose central directory does not parse, or whose entry cannot be inflated.
    Malformed { path: PathBuf, cause: String },
    /// An entry the archive LISTS could not be produced. A library that advertises a fragment it
    /// cannot hand back is corrupt, whatever the cause.
    MissingEntry { path: PathBuf, entry: String },
    /// An entry names a location outside the archive root — through `..`, an absolute or
    /// platform-prefixed path, or a symlink that resolves out of the tree. This is refused before
    /// the filesystem is touched, or after resolution for a symlink, and never read.
    EscapesArchive { path: PathBuf, entry: String },
}

impl std::fmt::Display for KlibError {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotALibrary { path } => {
                write!(out, "'{}' is not a Kotlin library", path.display())
            }
            Self::Unreadable { path, cause } => {
                write!(out, "'{}' cannot be read: {cause}", path.display())
            }
            Self::Malformed { path, cause } => {
                write!(
                    out,
                    "'{}' is not a readable archive: {cause}",
                    path.display()
                )
            }
            Self::MissingEntry { path, entry } => write!(
                out,
                "'{}' lists '{entry}' but cannot produce it",
                path.display()
            ),
            Self::EscapesArchive { path, entry } => write!(
                out,
                "'{}' contains an entry that resolves outside it: '{entry}'",
                path.display()
            ),
        }
    }
}

impl KlibError {
    /// Whether this is the optional absence rather than a library that failed to read.
    pub fn is_absence(&self) -> bool {
        matches!(self, Self::NotALibrary { .. })
    }
}

impl KlibArchive {
    /// Open `path` as a klib, accepting either a zip file or an unpacked directory.
    pub fn open(path: &Path) -> Result<Self, KlibError> {
        if path.is_dir() {
            if !path.join(ARCHIVE_COMPONENT).is_dir() {
                return Err(KlibError::NotALibrary {
                    path: path.to_path_buf(),
                });
            }
            // The root is canonicalized ONCE, here, and every read is checked against it. An entry
            // path is data the library supplies, so containment cannot be a property of how that
            // path is spelled: a symlink spelled with no `..` in it still resolves wherever it
            // points.
            let root = path.canonicalize().map_err(|cause| KlibError::Unreadable {
                path: path.to_path_buf(),
                cause,
            })?;
            let mut entries = Vec::new();
            collect_directory_entries(&root, String::new(), 0, &mut entries).map_err(|cause| {
                KlibError::Unreadable {
                    path: path.to_path_buf(),
                    cause,
                }
            })?;
            entries.sort();
            return Ok(Self {
                root,
                source: Source::Directory,
                entries,
            });
        }
        let file = std::fs::File::open(path).map_err(|cause| {
            if cause.kind() == std::io::ErrorKind::NotFound {
                KlibError::NotALibrary {
                    path: path.to_path_buf(),
                }
            } else {
                KlibError::Unreadable {
                    path: path.to_path_buf(),
                    cause,
                }
            }
        })?;
        let archive = zip::ZipArchive::new(file).map_err(|cause| KlibError::Malformed {
            path: path.to_path_buf(),
            cause: cause.to_string(),
        })?;
        let mut entries: Vec<String> = archive
            .file_names()
            .filter(|name| !name.ends_with('/'))
            .map(str::to_string)
            .collect();
        entries.sort();
        // The same test the directory shape applies, so the two shapes answer alike: a zip with no
        // `default/` tree is some other archive — a jar on the wrong path, say — and is an absent
        // library rather than a corrupt one. Without this only the directory shape could say "not a
        // klib", and any zip at all opened as one.
        if !entries.iter().any(|entry| {
            entry.starts_with(ARCHIVE_COMPONENT)
                && entry[ARCHIVE_COMPONENT.len()..].starts_with('/')
        }) {
            return Err(KlibError::NotALibrary {
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

    /// Read one entry's bytes.
    ///
    /// An entry this archive LISTS and cannot produce is corruption, not absence, and says so. An
    /// entry that resolves outside the archive root is refused rather than read.
    pub fn read(&self, entry: &str) -> Result<Vec<u8>, KlibError> {
        match &self.source {
            Source::Zip(archive) => {
                let mut archive = archive
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                let mut file = archive
                    .by_name(entry)
                    .map_err(|_| KlibError::MissingEntry {
                        path: self.root.clone(),
                        entry: entry.to_string(),
                    })?;
                let mut bytes = Vec::with_capacity(file.size() as usize);
                file.read_to_end(&mut bytes)
                    .map_err(|cause| KlibError::Malformed {
                        path: self.root.clone(),
                        cause: format!("entry '{entry}': {cause}"),
                    })?;
                Ok(bytes)
            }
            Source::Directory => {
                let resolved = self.resolve(entry)?;
                std::fs::read(resolved).map_err(|_| KlibError::MissingEntry {
                    path: self.root.clone(),
                    entry: entry.to_string(),
                })
            }
        }
    }

    /// Where an entry of an UNPACKED klib really lives, or a refusal.
    ///
    /// Two checks, because neither alone is containment. The path must be made only of ordinary
    /// components, which refuses `..`, an absolute path and a platform prefix (`C:`, `\\?\`) — the
    /// spellings a textual `/`-split test misses on a platform whose separator is not `/`. Then the
    /// resolved location must still sit under the canonical root, which is the only thing that
    /// refuses a symlink: a symlink's own spelling is perfectly ordinary, and `read` follows it.
    fn resolve(&self, entry: &str) -> Result<PathBuf, KlibError> {
        let escapes = || KlibError::EscapesArchive {
            path: self.root.clone(),
            entry: entry.to_string(),
        };
        let relative = Path::new(entry);
        if relative.components().count() == 0
            || !relative
                .components()
                .all(|component| matches!(component, std::path::Component::Normal(_)))
        {
            return Err(escapes());
        }
        let joined = self.root.join(relative);
        // A path that does not exist has nothing to resolve and nothing to escape through; report
        // it as the missing entry it is, not as an escape.
        let Ok(canonical) = joined.canonicalize() else {
            return Err(KlibError::MissingEntry {
                path: self.root.clone(),
                entry: entry.to_string(),
            });
        };
        if !canonical.starts_with(&self.root) {
            return Err(escapes());
        }
        Ok(canonical)
    }

    /// The parsed `default/manifest`.
    ///
    /// A klib without a manifest is not a klib; one whose manifest cannot be read is corrupt. Both
    /// are reported, because an empty manifest answers `None` for every field and is therefore
    /// indistinguishable from one that simply omits the key the caller asked about.
    pub fn manifest(&self) -> Result<KlibManifest, KlibError> {
        self.read("default/manifest")
            .map(|bytes| KlibManifest::parse(&bytes))
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

/// Walk an unpacked klib, recording every ordinary file below `root`.
///
/// A directory that cannot be read is reported rather than skipped: a library whose tree is half
/// unreadable would otherwise present as a library with fewer declarations. Depth is bounded so a
/// symlink cycle terminates; a klib's own tree is three levels deep.
///
/// `DirEntry::file_type` does not follow symlinks, so a symlink is neither `is_dir` nor `is_file`
/// here and is left out of the listing entirely. `read` refuses one that slipped in anyway, by
/// resolution rather than by spelling.
fn collect_directory_entries(
    root: &Path,
    prefix: String,
    depth: usize,
    out: &mut Vec<String>,
) -> std::io::Result<()> {
    if depth > MAX_DIRECTORY_DEPTH {
        return Ok(());
    }
    for child in std::fs::read_dir(root.join(&prefix))? {
        let child = child?;
        let Ok(name) = child.file_name().into_string() else {
            continue;
        };
        let path = if prefix.is_empty() {
            name
        } else {
            format!("{prefix}/{name}")
        };
        let kind = child.file_type()?;
        if kind.is_dir() {
            collect_directory_entries(root, path, depth + 1, out)?;
        } else if kind.is_file() {
            out.push(path);
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
    pub fn parse(bytes: &[u8]) -> Self {
        let text = String::from_utf8_lossy(bytes);
        let mut values = BTreeMap::new();
        for logical in logical_lines(&text) {
            let Some((key, value)) = split_property(&logical) else {
                continue;
            };
            values.insert(key, value);
        }
        Self { values }
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
fn split_property(line: &str) -> Option<(String, String)> {
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
        return None;
    }
    if !separator_seen {
        return Some((unescape(&key), String::new()));
    }
    let value: String = characters.collect();
    Some((unescape(&key), unescape(value.trim_start())))
}

fn unescape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut characters = text.chars();
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
                let digits: String = characters.by_ref().take(4).collect();
                match u32::from_str_radix(&digits, 16)
                    .ok()
                    .and_then(char::from_u32)
                {
                    Some(decoded) => out.push(decoded),
                    // An unpaired surrogate or short escape is written through verbatim rather than
                    // silently dropped, so a malformed manifest stays diagnosable.
                    None => {
                        out.push_str("\\u");
                        out.push_str(&digits);
                    }
                }
            }
            Some(other) => out.push(other),
            None => out.push('\\'),
        }
    }
    out
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

    #[test]
    fn manifest_reads_properties_separators_and_escapes() {
        let manifest = KlibManifest::parse(
            b"# a comment\n\
              ! another\n\
              unique_name=kotlin\n\
              abi_version:2.4.0\n\
              metadata_version 2.4.0\n\
              depends=kotlin \\\n  kotlinx\n\
              escaped\\=key=value\n\
              unicode=\\u004b\n",
        );
        assert_eq!(manifest.unique_name(), Some("kotlin"));
        assert_eq!(manifest.abi_version(), Some("2.4.0"));
        assert_eq!(manifest.metadata_version(), Some("2.4.0"));
        assert_eq!(manifest.depends(), vec!["kotlin", "kotlinx"]);
        assert_eq!(manifest.get("escaped=key"), Some("value"));
        assert_eq!(manifest.get("unicode"), Some("K"));
        assert_eq!(manifest.get("absent"), None);
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
        let native = KlibManifest::parse(b"native_targets=linux_x64 macos_arm64\n");
        assert_eq!(native.targets(), vec!["linux_x64", "macos_arm64"]);
        let wasm = KlibManifest::parse(b"wasm_targets=wasm-js\n");
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
        let manifest = archive.manifest().expect("the manifest reads");
        assert_eq!(manifest.unique_name(), Some("stdlib"));
        assert_eq!(manifest.builtins_platform(), Some("NATIVE"));
        assert_eq!(
            archive.module_header().expect("the module header reads"),
            b"module-header"
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
                .expect("a listed fragment"),
            b"three"
        );
        assert!(
            matches!(
                archive.read("default/ir/absent.knb"),
                Err(KlibError::MissingEntry { .. })
            ),
            "an entry the archive does not have is missing, not an escape",
        );
    }

    #[test]
    fn a_directory_without_a_default_tree_is_not_a_klib() {
        let root = temp_dir("klib-not-a-klib");
        write(&root, "META-INF/MANIFEST.MF", b"Manifest-Version: 1.0\n");
        let error = KlibArchive::open(&root).err().expect("not a klib");
        assert!(
            error.is_absence(),
            "a directory that is not a klib is an ABSENT library, not a failed one: {error}",
        );
    }

    /// Containment is not a property of how an entry path is SPELLED.
    ///
    /// A symlink's own spelling is perfectly ordinary — no `..`, no prefix, nothing a textual test
    /// would object to — and `std::fs::read` follows it. Only resolving the path and checking it
    /// against the canonical root refuses this, which is why the check is there and not in the
    /// spelling. The entry is also absent from the listing, because the walk records ordinary files
    /// and a symlink is neither a file nor a directory to `DirEntry::file_type`.
    #[test]
    #[cfg(unix)]
    fn a_symlink_out_of_the_archive_is_refused_and_unlisted() {
        // The archive root is a SUBDIRECTORY here, so the target genuinely sits outside it.
        let enclosing = temp_dir("klib-symlink-escape");
        let secret = enclosing.join("outside-the-archive");
        std::fs::write(&secret, b"not the library's to hand out").expect("write the outside file");
        let root = enclosing.join("library");
        write(&root, "default/manifest", b"unique_name=stdlib\n");
        let inside = root.join("default").join("linkdata");
        std::fs::create_dir_all(&inside).expect("create linkdata");
        std::os::unix::fs::symlink(&secret, inside.join("escape.knm"))
            .expect("create the escaping symlink");

        let archive = KlibArchive::open(&root).expect("open unpacked klib");
        assert!(
            !archive
                .entries()
                .iter()
                .any(|entry| entry.ends_with("escape.knm")),
            "a symlink is not an ordinary file and is not listed: {:?}",
            archive.entries(),
        );
        assert!(
            matches!(
                archive.read("default/linkdata/escape.knm"),
                Err(KlibError::EscapesArchive { .. })
            ),
            "the symlink resolves outside the root and is refused",
        );
    }

    /// The spellings a `/`-split test misses. `..` it catches; an absolute path, a platform prefix
    /// and a backslash-separated path it does not, and on a platform whose separator is `\\` the
    /// last of those is a real traversal.
    #[test]
    fn every_escaping_spelling_is_refused_before_the_filesystem() {
        let root = temp_dir("klib-escaping-spellings");
        write(&root, "default/manifest", b"unique_name=stdlib\n");
        let archive = KlibArchive::open(&root).expect("open unpacked klib");
        for entry in [
            "../outside",
            "default/../../outside",
            "/etc/passwd",
            "",
            ".",
            "./default/manifest",
        ] {
            assert!(
                matches!(archive.read(entry), Err(KlibError::EscapesArchive { .. })),
                "'{entry}' must be refused as an escape, not read",
            );
        }
        #[cfg(windows)]
        for entry in ["..\\outside", "C:\\Windows", "\\\\?\\C:\\Windows"] {
            assert!(
                matches!(archive.read(entry), Err(KlibError::EscapesArchive { .. })),
                "'{entry}' must be refused as an escape, not read",
            );
        }
    }

    /// A file that is not a zip is a MALFORMED archive, not an absent library: something is there
    /// and it cannot be read. Collapsing the two is what let a corrupt distribution look exactly
    /// like a distribution without one.
    #[test]
    fn a_file_that_is_not_an_archive_is_malformed_not_absent() {
        let root = temp_dir("klib-not-an-archive");
        std::fs::create_dir_all(&root).expect("create the directory");
        let path = root.join("broken.klib");
        std::fs::write(&path, b"this is not a zip central directory").expect("write the file");
        let error = KlibArchive::open(&path).err().expect("not an archive");
        assert!(
            matches!(error, KlibError::Malformed { .. }),
            "a file that is not a zip is malformed: {error}",
        );
        assert!(!error.is_absence(), "and it is not an absence: {error}");
    }

    /// A path that does not exist at all IS an absence, and the one case a caller may skip.
    #[test]
    fn a_path_that_does_not_exist_is_an_absence() {
        let root = temp_dir("klib-absent");
        let error = KlibArchive::open(&root.join("never-written.klib"))
            .err()
            .expect("nothing there");
        assert!(error.is_absence(), "{error}");
    }

    /// A klib with no manifest is reported rather than answering an empty one. An empty manifest
    /// says `None` for every field, which is exactly what a manifest that omits one key says, so a
    /// caller could not tell "this library declares no unique name" from "there is no manifest".
    #[test]
    fn a_missing_manifest_is_reported_not_answered_empty() {
        let root = temp_dir("klib-no-manifest");
        write(&root, "default/linkdata/module", b"module-header");
        let archive = KlibArchive::open(&root).expect("open unpacked klib");
        assert!(
            matches!(archive.manifest(), Err(KlibError::MissingEntry { .. })),
            "an absent manifest is a missing entry, not an empty manifest",
        );
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
