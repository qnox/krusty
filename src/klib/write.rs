//! Writing a KLIB container: the manifest, the module header and the `linkdata` fragments.
//!
//! The counterpart of the reader in [`super`]. Each fragment arrives as the `PackageFragment` bytes
//! the metadata encoder produced ([`crate::metadata::klib_fragment`]); this module only lays them out
//! the way the reference compiler does, and writes the two module-level files beside them.
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

/// A metadata KLIB under construction: the `linkdata` declarations of a module and no serialized
/// IR, laid out as `kotlinc-native -p library -Xmetadata-klib` lays one out.
pub struct KlibWriter {
    manifest: BTreeMap<String, String>,
    module_name: String,
    /// Each package's fragments in compilation order, keyed by the package's dotted name.
    packages: Vec<(String, Vec<Vec<u8>>)>,
}

impl KlibWriter {
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
        }
        Self {
            manifest,
            module_name: unique_name.to_string(),
            packages: Vec::new(),
        }
    }

    /// Add one file's fragment to `package` (dotted; empty for the root package).
    pub fn add_fragment(&mut self, package: &str, fragment: Vec<u8>) {
        match self.packages.iter_mut().find(|(name, _)| name == package) {
            Some((_, fragments)) => fragments.push(fragment),
            None => self.packages.push((package.to_string(), vec![fragment])),
        }
    }

    /// The manifest as it goes to disk: `key=value`, one per line, keys sorted. No comment header:
    /// `java.util.Properties.store` would write a timestamp, and the reference compiler omits it.
    fn manifest_bytes(&self) -> Vec<u8> {
        let mut out = String::new();
        for (key, value) in &self.manifest {
            out.push_str(key);
            out.push('=');
            out.push_str(value);
            out.push('\n');
        }
        out.into_bytes()
    }

    /// The `default/linkdata/module` header: `module_name` (1) as `<name>`, then the name of each
    /// package that has fragments (`package_fragment_name` = 7), sorted, the root package's empty
    /// name included.
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
        entries.sort_by(|left, right| left.0.cmp(&right.0));
        entries
    }

    /// Write the library as a directory tree rooted at `root` (the unpacked shape).
    pub fn write_directory(&self, root: &Path) -> io::Result<()> {
        for (entry, bytes) in self.entries() {
            let path = root.join(&entry);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&path, &bytes)?;
        }
        Ok(())
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

    /// The `abi_version` and `metadata_version` the reference compilers write with
    /// `-language-version <level> -Xmetadata-klib`, for every supported release and language level.
    #[test]
    fn the_stamp_follows_the_language_level_as_each_release_does() {
        let releases = [
            KotlinVersion::V2_4_0,
            KotlinVersion::V2_4_10,
            KotlinVersion::V2_4_20,
        ];
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
        assert_eq!(
            stamps,
            [
                "2.4.0 2.0: abi_version=2.4.0 metadata_version=1.4.1",
                "2.4.0 2.1: abi_version=2.4.0 metadata_version=1.4.1",
                "2.4.0 2.2: abi_version=2.4.0 metadata_version=1.4.1",
                "2.4.0 2.3: abi_version=2.4.0 metadata_version=2.3.0",
                "2.4.0 2.4: abi_version=2.4.0 metadata_version=2.4.0",
                "2.4.10 2.0: abi_version=2.4.0 metadata_version=1.4.1",
                "2.4.10 2.1: abi_version=2.4.0 metadata_version=1.4.1",
                "2.4.10 2.2: abi_version=2.4.0 metadata_version=1.4.1",
                "2.4.10 2.3: abi_version=2.4.0 metadata_version=2.3.0",
                "2.4.10 2.4: abi_version=2.4.0 metadata_version=2.4.0",
                "2.4.20 2.0: abi_version=2.4.0 metadata_version=2.0.0",
                "2.4.20 2.1: abi_version=2.4.0 metadata_version=2.1.0",
                "2.4.20 2.2: abi_version=2.4.0 metadata_version=2.2.0",
                "2.4.20 2.3: abi_version=2.4.0 metadata_version=2.3.0",
                "2.4.20 2.4: abi_version=2.4.0 metadata_version=2.4.0",
            ]
        );
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
