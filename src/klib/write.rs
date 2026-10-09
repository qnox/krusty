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

/// The platform a KLIB is compiled for, as its manifest names it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KlibPlatform {
    /// Kotlin/Native, for the listed Konan targets (`linux_x64`, …).
    Native { targets: Vec<String> },
}

/// A KLIB under construction.
pub struct KlibWriter {
    manifest: BTreeMap<String, String>,
    module_name: String,
    /// Each package's fragments in compilation order, keyed by the package's dotted name.
    packages: Vec<(String, Vec<Vec<u8>>)>,
}

impl KlibWriter {
    /// A library named `unique_name`, compiled by the Kotlin `compiler_version` for `platform`,
    /// whose declarations reference `depends` (other libraries' unique names, `stdlib` included).
    pub fn new(
        unique_name: &str,
        compiler_version: crate::kotlin_version::KotlinVersion,
        platform: &KlibPlatform,
        depends: &[String],
    ) -> Self {
        // The ABI and metadata versions are the compiler's language version.
        let language_version = format!("{}.{}.0", compiler_version.major, compiler_version.minor);
        let mut manifest = BTreeMap::new();
        manifest.insert("abi_version".to_string(), language_version.clone());
        manifest.insert(
            "compiler_version".to_string(),
            format!(
                "{}.{}.{}",
                compiler_version.major, compiler_version.minor, compiler_version.patch
            ),
        );
        manifest.insert("ir_signature_versions".to_string(), "1,2".to_string());
        manifest.insert("metadata_version".to_string(), language_version);
        manifest.insert("unique_name".to_string(), unique_name.to_string());
        if !depends.is_empty() {
            manifest.insert("depends".to_string(), depends.join(" "));
        }
        match platform {
            KlibPlatform::Native { targets } => {
                manifest.insert("builtins_platform".to_string(), "NATIVE".to_string());
                manifest.insert("native_targets".to_string(), targets.join(" "));
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
