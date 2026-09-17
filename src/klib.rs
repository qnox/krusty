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
/// Every per-package fragment path starts with this, followed by the package's fully qualified name.
const PACKAGE_PREFIX: &str = "default/linkdata/package_";
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

impl KlibArchive {
    /// Open `path` as a klib, accepting either a zip file or an unpacked directory.
    ///
    /// Returns `None` when the path is neither — including a zip that does not parse and a directory
    /// with no `default/` tree, both of which mean "this is not a library" rather than an error a
    /// caller can act on.
    pub fn open(path: &Path) -> Option<Self> {
        if path.is_dir() {
            if !path.join(ARCHIVE_COMPONENT).is_dir() {
                return None;
            }
            let mut entries = Vec::new();
            collect_directory_entries(path, String::new(), 0, &mut entries);
            entries.sort();
            return Some(Self {
                root: path.to_path_buf(),
                source: Source::Directory,
                entries,
            });
        }
        let file = std::fs::File::open(path).ok()?;
        let archive = zip::ZipArchive::new(file).ok()?;
        let mut entries: Vec<String> = archive
            .file_names()
            .filter(|name| !name.ends_with('/'))
            .map(str::to_string)
            .collect();
        entries.sort();
        Some(Self {
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

    /// Read one entry's bytes. `None` for an absent entry or an I/O failure — a klib that lists an
    /// entry it cannot produce is corrupt, and every caller here treats that as a missing symbol
    /// source rather than aborting the compilation.
    pub fn read(&self, entry: &str) -> Option<Vec<u8>> {
        match &self.source {
            Source::Zip(archive) => {
                let mut archive = archive
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                let mut file = archive.by_name(entry).ok()?;
                let mut bytes = Vec::with_capacity(file.size() as usize);
                file.read_to_end(&mut bytes).ok()?;
                Some(bytes)
            }
            Source::Directory => {
                // Reject anything that could escape the archive root before touching the filesystem:
                // an entry path is data from the library, not a caller-supplied path.
                if entry.split('/').any(|part| part == ".." || part.is_empty()) {
                    return None;
                }
                std::fs::read(self.root.join(entry)).ok()
            }
        }
    }

    /// The parsed `default/manifest`. An absent or unreadable manifest yields an empty one, which
    /// reports `None` for every field — the same answer a caller gets for a key the manifest omits.
    pub fn manifest(&self) -> KlibManifest {
        self.read("default/manifest")
            .map(|bytes| KlibManifest::parse(&bytes))
            .unwrap_or_default()
    }

    /// The `default/linkdata/module` header protobuf, which names the packages the library declares.
    pub fn module_header(&self) -> Option<Vec<u8>> {
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
            .filter(|entry| entry.starts_with(PACKAGE_PREFIX) && entry.ends_with(".knm"))
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
/// Both shapes the serializer writes are accepted: a per-package directory
/// (`default/linkdata/package_kotlin.collections/03_collections.knm`) and a single flat fragment
/// (`default/linkdata/package_kotlin.collections.knm`). The root package's directory is named
/// `package_`, which yields the empty name.
fn package_fqname(entry: &str) -> String {
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

fn collect_directory_entries(root: &Path, prefix: String, depth: usize, out: &mut Vec<String>) {
    if depth > MAX_DIRECTORY_DEPTH {
        return;
    }
    let Ok(directory) = std::fs::read_dir(root.join(&prefix)) else {
        return;
    };
    for child in directory.flatten() {
        let Ok(name) = child.file_name().into_string() else {
            continue;
        };
        let path = if prefix.is_empty() {
            name
        } else {
            format!("{prefix}/{name}")
        };
        match child.file_type() {
            Ok(kind) if kind.is_dir() => collect_directory_entries(root, path, depth + 1, out),
            Ok(_) => out.push(path),
            Err(_) => {}
        }
    }
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
        write(&root, "default/linkdata/package_/00_root.knm", b"root");
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
        assert_eq!(archive.manifest().unique_name(), Some("stdlib"));
        assert_eq!(archive.manifest().builtins_platform(), Some("NATIVE"));
        assert_eq!(
            archive.module_header().as_deref(),
            Some(&b"module-header"[..])
        );
        assert_eq!(archive.ir_entries(), vec!["default/ir/bodies.knb"]);

        let fragments = archive.package_fragments();
        assert_eq!(
            fragments
                .iter()
                .map(|fragment| (fragment.package_fqname.as_str(), fragment.entry.as_str()))
                .collect::<Vec<_>>(),
            vec![
                ("", "default/linkdata/package_/00_root.knm"),
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
            archive.read(&fragments[1].entry).as_deref(),
            Some(&b"three"[..])
        );
        assert_eq!(archive.read("default/ir/absent.knb"), None);
        assert_eq!(archive.read("../outside"), None, "no escape from the root");
    }

    #[test]
    fn a_directory_without_a_default_tree_is_not_a_klib() {
        let root = temp_dir("klib-not-a-klib");
        write(&root, "META-INF/MANIFEST.MF", b"Manifest-Version: 1.0\n");
        assert!(KlibArchive::open(&root).is_none());
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
