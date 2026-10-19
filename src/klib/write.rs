//! Writing a KLIB container: the manifest, module header, `linkdata`, and optional components.
//!
//! The counterpart of the reader in [`super`]. Each fragment arrives as the `PackageFragment` bytes
//! the metadata encoder produced ([`crate::metadata::klib_fragment`]); this module lays them out the
//! way the reference compiler does and admits standard optional files such as prepared inline IR.
//!
//! The layout, relative to the library root:
//!
//! | entry                                               | contents                              |
//! |-----------------------------------------------------|---------------------------------------|
//! | `default/manifest`                                  | sorted `key=value` lines              |
//! | `default/linkdata/module`                           | `Header`: module name, package names  |
//! | `default/linkdata/package_<fqname>/<n>_<seg>.knm`   | one fragment per file of the package  |
//! | `default/linkdata/root_package/<n>_.knm`            | the root package's fragments          |
//!
//! A package's files are numbered from zero in the order they were compiled, each package on its
//! own; `<seg>` is the package's last segment.

use std::collections::BTreeMap;
use std::io;
use std::path::Path;

use crate::kotlin_version::KotlinVersion;
use crate::language_version::LanguageVersion;

/// The platform a KLIB's declarations are compiled for, as its manifest names it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KlibPlatform {
    /// Kotlin/Native. A metadata library is not bound to a Konan target, so its manifest lists none.
    Native,
    /// Kotlin/Wasm for a JavaScript host.
    WasmJs,
    /// Kotlin/Wasm for a WASI host.
    WasmWasi,
}

/// The versions a KLIB's manifest is stamped with: the compiler release that wrote it and the
/// metadata version of its fragments.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KlibStamp {
    pub compiler: KotlinVersion,
    /// `metadata_version`: what the finalized language level stamps, or a selected
    /// `-Xmetadata-version`.
    pub metadata_version: [i32; 3],
}

impl KlibStamp {
    /// What `compiler` stamps on a library compiled at source-language level `language`.
    ///
    /// Read off `kotlinc-native -p library -Xmetadata-klib -language-version <language>`: 2.4.0 and
    /// 2.4.10 stamp `1.4.1` below language 2.3, and 2.4.20 stamps the language level itself. Keyed
    /// by exact releases, as the supported language levels are.
    pub fn new(compiler: KotlinVersion, language: LanguageVersion) -> Self {
        let legacy_stamp = matches!(compiler, KotlinVersion::V2_4_0 | KotlinVersion::V2_4_10)
            && language < LanguageVersion::V2_3;
        Self {
            compiler,
            metadata_version: if legacy_stamp {
                [1, 4, 1]
            } else {
                language.metadata_version()
            },
        }
    }

    /// Stamp `metadata_version` instead of the language level's (`-Xmetadata-version`).
    pub fn with_metadata_version(mut self, metadata_version: [i32; 3]) -> Self {
        self.metadata_version = metadata_version;
        self
    }

    /// `abi_version`: the KLIB ABI of the compiler release. Every supported release writes its
    /// own major and minor at every language level (2.4.0 at language 2.0 through 2.4).
    fn abi_version(self) -> String {
        format!("{}.{}.0", self.compiler.major, self.compiler.minor)
    }
}

/// A KLIB under construction. Its baseline is the `linkdata` layout produced by
/// `kotlinc-native -p library -Xmetadata-klib`; callers may add standard optional components.
pub struct KlibWriter {
    manifest: BTreeMap<String, String>,
    module_name: String,
    /// Each package's fragments in compilation order, keyed by the package's dotted name.
    packages: Vec<(String, Vec<Vec<u8>>)>,
    /// Packages that declare nothing themselves, such as an interop library's parent packages.
    empty_packages: Vec<String>,
    /// Directories the library holds even when they are empty, relative to its root.
    directories: Vec<String>,
    /// Other files, as (library-relative path, bytes).
    files: Vec<(String, Vec<u8>)>,
}

impl KlibWriter {
    /// A current Native metadata-library container for cross-module tests that only care about
    /// archive structure or manifest authority, not version stamping itself.
    #[cfg(test)]
    pub(crate) fn native_test_fixture(unique_name: &str) -> Self {
        Self::new(
            unique_name,
            KlibStamp::new(KotlinVersion::V2_4_20, LanguageVersion::V2_4),
            KlibPlatform::Native,
        )
    }

    /// A library named `unique_name`, stamped with `stamp`, for `platform`.
    ///
    /// The manifest is the reference compiler's for a metadata library: no dependencies, no Konan
    /// target, and `ir_signature_versions=1,2`, which the reference compiler writes on a library
    /// with no serialized IR too. The entry therefore says nothing about IR being present; the
    /// absence of `default/ir` does.
    pub fn new(unique_name: &str, stamp: KlibStamp, platform: KlibPlatform) -> Self {
        let version = |[major, minor, patch]: [i32; 3]| format!("{major}.{minor}.{patch}");
        let mut manifest = BTreeMap::new();
        manifest.insert("abi_version".to_string(), stamp.abi_version());
        manifest.insert(
            "compiler_version".to_string(),
            format!(
                "{}.{}.{}",
                stamp.compiler.major, stamp.compiler.minor, stamp.compiler.patch
            ),
        );
        manifest.insert("ir_signature_versions".to_string(), "1,2".to_string());
        manifest.insert(
            "metadata_version".to_string(),
            version(stamp.metadata_version),
        );
        manifest.insert("unique_name".to_string(), unique_name.to_string());
        match platform {
            KlibPlatform::Native => {
                manifest.insert("builtins_platform".to_string(), "NATIVE".to_string());
                manifest.insert("native_targets".to_string(), String::new());
            }
            KlibPlatform::WasmJs => {
                manifest.insert("builtins_platform".to_string(), "WASM".to_string());
                manifest.insert("wasm_targets".to_string(), "wasm-js".to_string());
            }
            KlibPlatform::WasmWasi => {
                manifest.insert("builtins_platform".to_string(), "WASM".to_string());
                manifest.insert("wasm_targets".to_string(), "wasm-wasi".to_string());
            }
        }
        Self {
            manifest,
            module_name: unique_name.to_string(),
            packages: Vec::new(),
            empty_packages: Vec::new(),
            directories: Vec::new(),
            files: Vec::new(),
        }
    }

    /// Set the manifest property `key`, replacing what the writer stamped.
    pub fn set_property(&mut self, key: &str, value: &str) {
        self.manifest.insert(key.to_string(), value.to_string());
    }

    /// Add the fragment of a package that declares nothing: the module header lists the package
    /// among its empty ones too.
    pub fn add_empty_package(&mut self, package: &str, fragment: Vec<u8>) {
        self.add_fragment(package, fragment);
        if !self.empty_packages.iter().any(|name| name == package) {
            self.empty_packages.push(package.to_string());
        }
    }

    /// Keep the directory `path` (library-relative) in the written library even when empty.
    pub fn add_directory(&mut self, path: &str) {
        self.directories.push(path.to_string());
    }

    /// Add the file `path` (library-relative) holding `bytes`.
    pub fn add_file(&mut self, path: &str, bytes: Vec<u8>) {
        self.files.push((path.to_string(), bytes));
    }

    /// Add one file's fragment to `package` (dotted; empty for the root package).
    pub fn add_fragment(&mut self, package: &str, fragment: Vec<u8>) {
        match self.packages.iter_mut().find(|(name, _)| name == package) {
            Some((_, fragments)) => fragments.push(fragment),
            None => self.packages.push((package.to_string(), vec![fragment])),
        }
    }

    /// The manifest as it goes to disk: `key=value`, one per line, keys sorted, escaped as
    /// `java.util.Properties.store` escapes them. No comment header: `store` would write a
    /// timestamp, and the reference compiler omits it.
    fn manifest_bytes(&self) -> Vec<u8> {
        let mut out = String::new();
        for (key, value) in &self.manifest {
            escape_property(&mut out, key, true);
            out.push('=');
            escape_property(&mut out, value, false);
            out.push('\n');
        }
        out.into_bytes()
    }

    /// The `default/linkdata/module` header: `module_name` (1) as `<name>`, then the name of each
    /// package that has fragments (`package_fragment_name` = 7), sorted, the root package's empty
    /// name included, then those that declare nothing (`empty_package` = 8).
    fn module_header(&self) -> Vec<u8> {
        let mut header = crate::metadata::protobuf::Pb::new();
        header.field_bytes(1, format!("<{}>", self.module_name).as_bytes());
        let mut packages = self
            .packages
            .iter()
            .map(|(package, _)| package.as_str())
            .collect::<Vec<_>>();
        packages.sort_unstable();
        for package in packages {
            header.field_bytes(7, package.as_bytes());
        }
        for package in &self.empty_packages {
            header.field_bytes(8, package.as_bytes());
        }
        header.into_bytes()
    }

    /// Every entry, as (library-relative path, bytes), in path order.
    pub fn entries(&self) -> Vec<(String, Vec<u8>)> {
        let mut entries = vec![
            ("default/manifest".to_string(), self.manifest_bytes()),
            ("default/linkdata/module".to_string(), self.module_header()),
        ];
        for (package, fragments) in &self.packages {
            let (directory, segment) = if package.is_empty() {
                ("default/linkdata/root_package".to_string(), "")
            } else {
                (
                    format!("default/linkdata/package_{package}"),
                    package.rsplit('.').next().unwrap_or(package),
                )
            };
            for (index, fragment) in fragments.iter().enumerate() {
                entries.push((
                    format!("{directory}/{index}_{segment}.knm"),
                    fragment.clone(),
                ));
            }
        }
        entries.extend(self.files.iter().cloned());
        entries.sort_by(|left, right| left.0.cmp(&right.0));
        entries
    }

    /// Write the library as a directory tree rooted at `root` (the unpacked shape).
    pub fn write_directory(&self, root: &Path) -> io::Result<()> {
        for directory in &self.directories {
            std::fs::create_dir_all(root.join(directory))?;
        }
        for (entry, bytes) in self.entries() {
            let path = root.join(&entry);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&path, &bytes)?;
        }
        Ok(())
    }

    /// Write the library packed into the `.klib` archive at `path`: every directory, then every
    /// file, in path order.
    pub fn write_archive(&self, path: &Path) -> io::Result<()> {
        let file = std::fs::File::create(path)?;
        let mut archive = zip::ZipWriter::new(file);
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        let entries = self.entries();
        let mut directories = std::collections::BTreeSet::new();
        for directory in self.directories.iter().map(String::as_str).chain(
            entries
                .iter()
                .filter_map(|(entry, _)| Some(entry.rsplit_once('/')?.0)),
        ) {
            let mut prefix = String::new();
            for segment in directory.split('/') {
                prefix.push_str(segment);
                prefix.push('/');
                directories.insert(prefix.clone());
            }
        }
        for directory in directories {
            archive
                .add_directory(directory, options)
                .map_err(io::Error::other)?;
        }
        for (entry, bytes) in entries {
            archive
                .start_file(entry, options)
                .map_err(io::Error::other)?;
            io::Write::write_all(&mut archive, &bytes)?;
        }
        archive.finish().map_err(io::Error::other)?;
        Ok(())
    }
}

/// Append `text` escaped as `java.util.Properties.store` writes a key (`key`) or a value: the
/// separators and comment characters, backslashes, control characters, a value's leading space
/// and every space of a key, and everything outside printable ASCII as `\uXXXX`.
fn escape_property(out: &mut String, text: &str, key: bool) {
    for (index, c) in text.chars().enumerate() {
        match c {
            ' ' if key || index == 0 => out.push_str("\\ "),
            '\\' => out.push_str("\\\\"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\x0c' => out.push_str("\\f"),
            '=' | ':' | '#' | '!' => {
                out.push('\\');
                out.push(c);
            }
            ' '..='~' => out.push(c),
            other => {
                let mut units = [0u16; 2];
                for unit in other.encode_utf16(&mut units) {
                    out.push_str(&format!("\\u{unit:04X}"));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn writer() -> KlibWriter {
        let mut writer = KlibWriter::new(
            "lib",
            KlibStamp::new(KotlinVersion::V2_4_10, LanguageVersion::V2_4),
            KlibPlatform::Native,
        );
        writer.add_fragment("p.two", b"two-0".to_vec());
        writer.add_fragment("p.one", b"one-0".to_vec());
        writer.add_fragment("", b"root-0".to_vec());
        writer.add_fragment("p.one", b"one-1".to_vec());
        writer
    }

    #[test]
    fn each_package_numbers_its_own_fragments_in_compilation_order() {
        let entries = writer().entries();
        assert_eq!(
            entries
                .iter()
                .map(|(name, bytes)| (name.as_str(), String::from_utf8_lossy(bytes).into_owned()))
                .filter(|(name, _)| name.ends_with(".knm"))
                .collect::<Vec<_>>(),
            [
                (
                    "default/linkdata/package_p.one/0_one.knm",
                    "one-0".to_string()
                ),
                (
                    "default/linkdata/package_p.one/1_one.knm",
                    "one-1".to_string()
                ),
                (
                    "default/linkdata/package_p.two/0_two.knm",
                    "two-0".to_string()
                ),
                ("default/linkdata/root_package/0_.knm", "root-0".to_string()),
            ]
        );
    }

    fn manifest(writer: &KlibWriter) -> String {
        String::from_utf8(writer.manifest_bytes()).expect("a UTF-8 manifest")
    }

    /// `kotlinc-native 2.4.10 -p library -Xmetadata-klib -o lib`, byte for byte.
    #[test]
    fn the_manifest_is_the_reference_metadata_library_manifest() {
        assert_eq!(
            manifest(&writer()),
            "abi_version=2.4.0\n\
             builtins_platform=NATIVE\n\
             compiler_version=2.4.10\n\
             ir_signature_versions=1,2\n\
             metadata_version=2.4.0\n\
             native_targets=\n\
             unique_name=lib\n"
        );
    }

    #[test]
    fn wasm_manifests_name_the_exact_target() {
        let stamp = KlibStamp::new(KotlinVersion::V2_4_10, LanguageVersion::V2_4);
        for (platform, target) in [
            (KlibPlatform::WasmJs, "wasm-js"),
            (KlibPlatform::WasmWasi, "wasm-wasi"),
        ] {
            let manifest = manifest(&KlibWriter::new("lib", stamp, platform));
            assert_eq!(
                manifest,
                format!(
                    "abi_version=2.4.0\n\
                     builtins_platform=WASM\n\
                     compiler_version=2.4.10\n\
                     ir_signature_versions=1,2\n\
                     metadata_version=2.4.0\n\
                     unique_name=lib\n\
                     wasm_targets={target}\n"
                )
            );
        }
    }

    /// The `abi_version` and `metadata_version` the reference compilers write with
    /// `-language-version <level> -Xmetadata-klib`, for every supported release and language level.
    #[test]
    fn the_stamp_follows_the_language_level_as_each_release_does() {
        let releases = KotlinVersion::supported();
        let languages = [
            LanguageVersion::V2_0,
            LanguageVersion::V2_1,
            LanguageVersion::V2_2,
            LanguageVersion::V2_3,
            LanguageVersion::V2_4,
        ];
        let stamps = releases
            .iter()
            .flat_map(|&compiler| {
                languages.iter().map(move |&language| {
                    let writer = KlibWriter::new(
                        "lib",
                        KlibStamp::new(compiler, language),
                        KlibPlatform::Native,
                    );
                    let versions = manifest(&writer)
                        .lines()
                        .filter(|line| {
                            line.starts_with("abi_version=")
                                || line.starts_with("metadata_version=")
                        })
                        .collect::<Vec<_>>()
                        .join(" ");
                    format!("{compiler} {language}: {versions}")
                })
            })
            .collect::<Vec<_>>();
        // Measured with kotlinc-native, recorded at the release where they change; a later release
        // writes the stamps of the newest record at or below it.
        let recorded: &[(KotlinVersion, [&str; 5])] = &[
            (
                KotlinVersion::V2_4_0,
                [
                    "2.0: abi_version=2.4.0 metadata_version=1.4.1",
                    "2.1: abi_version=2.4.0 metadata_version=1.4.1",
                    "2.2: abi_version=2.4.0 metadata_version=1.4.1",
                    "2.3: abi_version=2.4.0 metadata_version=2.3.0",
                    "2.4: abi_version=2.4.0 metadata_version=2.4.0",
                ],
            ),
            (
                KotlinVersion::V2_4_20,
                [
                    "2.0: abi_version=2.4.0 metadata_version=2.0.0",
                    "2.1: abi_version=2.4.0 metadata_version=2.1.0",
                    "2.2: abi_version=2.4.0 metadata_version=2.2.0",
                    "2.3: abi_version=2.4.0 metadata_version=2.3.0",
                    "2.4: abi_version=2.4.0 metadata_version=2.4.0",
                ],
            ),
        ];
        let expected = releases
            .iter()
            .flat_map(|&compiler| {
                let (_, rows) = recorded
                    .iter()
                    .rev()
                    .find(|(release, _)| *release <= compiler)
                    .expect("a stamp record at or below every supported release");
                rows.iter().map(move |row| format!("{compiler} {row}"))
            })
            .collect::<Vec<_>>();
        assert_eq!(stamps, expected);
    }

    #[test]
    fn a_selected_metadata_version_replaces_the_language_stamp() {
        let stamp = KlibStamp::new(KotlinVersion::V2_4_20, LanguageVersion::V2_4)
            .with_metadata_version([2, 3, 0]);
        let writer = KlibWriter::new("lib", stamp, KlibPlatform::Native);
        assert_eq!(
            manifest(&writer),
            "abi_version=2.4.0\n\
             builtins_platform=NATIVE\n\
             compiler_version=2.4.20\n\
             ir_signature_versions=1,2\n\
             metadata_version=2.3.0\n\
             native_targets=\n\
             unique_name=lib\n"
        );
    }

    #[test]
    fn manifest_properties_are_escaped_as_java_properties_store_writes_them() {
        let mut writer = KlibWriter::new(
            "lib",
            KlibStamp::new(KotlinVersion::V2_4_20, LanguageVersion::V2_4),
            KlibPlatform::Native,
        );
        writer.set_property("compilerOpts", "-DByte=uByte -DX:1 #! \\é");
        writer.set_property("headers", " lead");
        assert_eq!(
            manifest(&writer),
            "abi_version=2.4.0\n\
             builtins_platform=NATIVE\n\
             compilerOpts=-DByte\\=uByte -DX\\:1 \\#\\! \\\\\\u00E9\n\
             compiler_version=2.4.20\n\
             headers=\\ lead\n\
             ir_signature_versions=1,2\n\
             metadata_version=2.4.0\n\
             native_targets=\n\
             unique_name=lib\n"
        );
    }

    #[test]
    fn an_empty_package_is_listed_after_the_packages_with_fragments() {
        let mut writer = writer();
        writer.add_empty_package("p", b"empty".to_vec());
        let mut expected = crate::metadata::protobuf::Pb::new();
        expected.field_bytes(1, b"<lib>");
        for package in ["", "p", "p.one", "p.two"] {
            expected.field_bytes(7, package.as_bytes());
        }
        expected.field_bytes(8, b"p");
        assert_eq!(writer.module_header(), expected.into_bytes());
    }

    #[test]
    fn the_module_header_names_the_module_and_its_sorted_packages() {
        let mut expected = crate::metadata::protobuf::Pb::new();
        expected.field_bytes(1, b"<lib>");
        for package in ["", "p.one", "p.two"] {
            expected.field_bytes(7, package.as_bytes());
        }
        assert_eq!(writer().module_header(), expected.into_bytes());
    }

    #[test]
    fn the_klib_reader_opens_a_written_library() {
        let root = std::env::temp_dir().join(format!("klib-write-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        writer().write_directory(&root).expect("write the library");
        let archive = crate::klib::KlibArchive::open(&root).expect("open the written library");
        let manifest = archive.manifest().expect("a parsable manifest");
        assert_eq!(manifest.unique_name(), Some("lib"));
        assert_eq!(manifest.depends(), Vec::<&str>::new());
        assert_eq!(manifest.targets(), Vec::<&str>::new());
        assert_eq!(
            archive
                .package_fragments()
                .iter()
                .map(|fragment| (fragment.package_fqname.as_str(), fragment.entry.as_str()))
                .collect::<Vec<_>>(),
            [
                ("", "default/linkdata/root_package/0_.knm"),
                ("p.one", "default/linkdata/package_p.one/0_one.knm"),
                ("p.one", "default/linkdata/package_p.one/1_one.knm"),
                ("p.two", "default/linkdata/package_p.two/0_two.knm"),
            ]
        );
        std::fs::remove_dir_all(&root).expect("remove the library");
    }
}
