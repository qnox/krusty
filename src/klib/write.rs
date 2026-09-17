//! Writing a KLIB container: the manifest, the module header and the `linkdata` fragments.
//!
//! The counterpart of the reader in [`super`], and deliberately the same shape: this writes the
//! container and takes each declaration fragment as BYTES. Producing those bytes is the metadata
//! encoder's job, exactly as decoding them is the metadata reader's.
//!
//! Correctness here is byte equality with the reference compiler, not merely "a klib the reader
//! accepts". A `.klib` is a distributed artifact: a dependent resolves against its `unique_name`, a
//! linker walks its fragments, and a fingerprint over its bytes decides whether a downstream module
//! needs recompiling. So the writer reproduces what kotlinc writes, down to key order in the
//! manifest and field order in the header, and the harness diffs the two artifacts entry by entry.
//!
//! The DIRECTORY shape is what this writes, because that is what `K2MetadataCompiler` produces for a
//! metadata-only klib: comparing directories makes byte equality a question about the content files
//! rather than about zip container metadata, which no consumer reads for meaning.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};

/// One package's declaration fragments, in the serializer's chunk order.
pub struct KlibPackageFragments {
    /// The package's fully qualified Kotlin name; empty for the root package.
    pub package_fqname: String,
    /// Each chunk's encoded `PackageFragment` bytes, in the order they are numbered on disk.
    pub chunks: Vec<Vec<u8>>,
}

/// A klib under construction.
pub struct KlibBuilder {
    manifest: BTreeMap<String, String>,
    module_header: Vec<u8>,
    packages: Vec<KlibPackageFragments>,
}

impl KlibBuilder {
    /// A metadata-only klib for `unique_name`, carrying the keys the reference compiler writes for
    /// one.
    ///
    /// A metadata klib names no platform and no targets: `builtins_platform` and `native_targets`
    /// appear only on a klib compiled FOR a target, and kotlinc omits them here rather than writing
    /// an empty value. Measured against `K2MetadataCompiler -Xmetadata-klib`, whose manifest is
    /// exactly these five keys.
    pub fn metadata_only(
        unique_name: &str,
        abi_version: &str,
        metadata_version: &str,
        compiler_version: &str,
        ir_signature_versions: &str,
    ) -> Self {
        let mut manifest = BTreeMap::new();
        manifest.insert("abi_version".to_string(), abi_version.to_string());
        manifest.insert("compiler_version".to_string(), compiler_version.to_string());
        manifest.insert(
            "ir_signature_versions".to_string(),
            ir_signature_versions.to_string(),
        );
        manifest.insert("metadata_version".to_string(), metadata_version.to_string());
        manifest.insert("unique_name".to_string(), unique_name.to_string());
        Self {
            manifest,
            module_header: Vec::new(),
            packages: Vec::new(),
        }
    }

    /// Set or replace one manifest key. A key whose value is empty is still written: a caller that
    /// means "absent" removes the key instead, because kotlinc distinguishes the two.
    pub fn manifest_entry(&mut self, key: &str, value: &str) -> &mut Self {
        self.manifest.insert(key.to_string(), value.to_string());
        self
    }

    pub fn remove_manifest_entry(&mut self, key: &str) -> &mut Self {
        self.manifest.remove(key);
        self
    }

    /// The `default/linkdata/module` header bytes.
    pub fn module_header(&mut self, bytes: Vec<u8>) -> &mut Self {
        self.module_header = bytes;
        self
    }

    pub fn package(&mut self, package: KlibPackageFragments) -> &mut Self {
        self.packages.push(package);
        self
    }

    /// The manifest exactly as it goes to disk.
    ///
    /// `key=value`, one per line, each line terminated — and the keys SORTED, which is what the
    /// reference compiler's manifests show and what makes the file reproducible. No comment header:
    /// `java.util.Properties.store` would write a timestamp, and a timestamp in a distributed
    /// artifact defeats the fingerprint over it.
    pub fn manifest_bytes(&self) -> Vec<u8> {
        let mut out = String::new();
        for (key, value) in &self.manifest {
            out.push_str(key);
            out.push('=');
            out.push_str(value);
            out.push('\n');
        }
        out.into_bytes()
    }

    /// Every entry this klib writes, as (archive-relative path, bytes), in path order.
    ///
    /// The fragment file name is `<chunk>_<last package segment>.knm`, and the root package's
    /// segment is empty — `root_package/0_.knm` — which is the spelling the reader accepts and every
    /// shipped klib uses.
    pub fn entries(&self) -> Vec<(String, Vec<u8>)> {
        let mut entries = vec![("default/manifest".to_string(), self.manifest_bytes())];
        entries.push((
            "default/linkdata/module".to_string(),
            self.module_header.clone(),
        ));
        for package in &self.packages {
            let directory = if package.package_fqname.is_empty() {
                "default/linkdata/root_package".to_string()
            } else {
                format!("default/linkdata/package_{}", package.package_fqname)
            };
            let segment = package
                .package_fqname
                .rsplit('.')
                .next()
                .filter(|_| !package.package_fqname.is_empty())
                .unwrap_or("");
            for (chunk, bytes) in package.chunks.iter().enumerate() {
                entries.push((format!("{directory}/{chunk}_{segment}.knm"), bytes.clone()));
            }
        }
        entries.sort_by(|left, right| left.0.cmp(&right.0));
        entries
    }

    /// Write the klib as a directory tree rooted at `root`, creating it if absent.
    pub fn write_directory(&self, root: &Path) -> io::Result<Vec<PathBuf>> {
        let mut written = Vec::new();
        for (entry, bytes) in self.entries() {
            let path = root.join(&entry);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&path, &bytes)?;
            written.push(path);
        }
        Ok(written)
    }
}

/// The `default/linkdata/module` header: the module's name and the packages it declares.
///
/// `ModuleHeader { module_name = 1, package_fragment_name = 7 (repeated), empty_package = 8
/// (repeated) }`. Measured from a reference metadata klib, whose header for one `package lib` file
/// is `module_name = "<main>"`, `package_fragment_name = ["", "lib"]` and one empty `empty_package`:
/// the root package is named even when it declares nothing, because a resolver walking a qualifier
/// must be able to see that the package exists.
pub fn module_header(module_name: &str, package_fqnames: &[String]) -> Vec<u8> {
    let mut out = Vec::new();
    push_tagged_bytes(&mut out, 1, module_name.as_bytes());
    for fqname in package_fqnames {
        push_tagged_bytes(&mut out, 7, fqname.as_bytes());
    }
    push_tagged_bytes(&mut out, 8, b"");
    out
}

fn push_tagged_bytes(out: &mut Vec<u8>, field: u32, bytes: &[u8]) {
    push_varint(out, u64::from(field) << 3 | 2);
    push_varint(out, bytes.len() as u64);
    out.extend_from_slice(bytes);
}

fn push_varint(out: &mut Vec<u8>, mut value: u64) {
    loop {
        let byte = (value & 0x7f) as u8;
        value >>= 7;
        if value == 0 {
            out.push(byte);
            return;
        }
        out.push(byte | 0x80);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_metadata_manifest_is_sorted_key_value_lines() {
        let builder = KlibBuilder::metadata_only("main", "2.4.0", "2.4.0", "2.4.10", "1,2");
        assert_eq!(
            String::from_utf8(builder.manifest_bytes()).expect("utf-8"),
            "abi_version=2.4.0\n\
             compiler_version=2.4.10\n\
             ir_signature_versions=1,2\n\
             metadata_version=2.4.0\n\
             unique_name=main\n",
            "the five keys a metadata-only klib carries, sorted, no comment header"
        );
    }

    /// The header a reference metadata klib carries for a single `package lib` file, byte for byte.
    #[test]
    fn the_module_header_names_every_package_including_the_root() {
        let header = module_header("<main>", &[String::new(), "lib".to_string()]);
        assert_eq!(
            header,
            b"\n\x06<main>:\x00:\x03libB\x00".to_vec(),
            "module_name, then each package_fragment_name, then the empty_package entry"
        );
    }

    #[test]
    fn fragment_paths_follow_the_serializer_naming() {
        let mut builder = KlibBuilder::metadata_only("main", "2.4.0", "2.4.0", "2.4.10", "1,2");
        builder
            .module_header(module_header("<main>", &[String::new(), "lib".to_string()]))
            .package(KlibPackageFragments {
                package_fqname: String::new(),
                chunks: vec![vec![1]],
            })
            .package(KlibPackageFragments {
                package_fqname: "a.b.lib".to_string(),
                chunks: vec![vec![2], vec![3]],
            });
        assert_eq!(
            builder
                .entries()
                .into_iter()
                .map(|(entry, _)| entry)
                .collect::<Vec<_>>(),
            vec![
                "default/linkdata/module",
                "default/linkdata/package_a.b.lib/0_lib.knm",
                "default/linkdata/package_a.b.lib/1_lib.knm",
                "default/linkdata/root_package/0_.knm",
                "default/manifest",
            ],
            "a fragment is <chunk>_<last segment>.knm, and the root package's segment is empty"
        );
    }

    /// The reader accepts what the writer produces: the round trip is the writer's first gate, and
    /// byte equality with the reference compiler is the second.
    #[test]
    fn what_is_written_reads_back() {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos())
            .unwrap_or_default();
        let root =
            std::env::temp_dir().join(format!("krusty-klib-write-{}-{unique}", std::process::id()));
        let mut builder = KlibBuilder::metadata_only("mylib", "2.4.0", "2.4.0", "2.4.10", "1,2");
        builder
            .module_header(module_header("<main>", &[String::new(), "lib".to_string()]))
            .package(KlibPackageFragments {
                package_fqname: "lib".to_string(),
                chunks: vec![b"first".to_vec(), b"second".to_vec()],
            })
            .package(KlibPackageFragments {
                package_fqname: String::new(),
                chunks: vec![b"root".to_vec()],
            });
        builder.write_directory(&root).expect("write the klib");

        let archive = super::super::KlibArchive::open(&root).expect("the writer produced a klib");
        assert_eq!(archive.manifest().unique_name(), Some("mylib"));
        assert!(archive.manifest().builtins_platform().is_none());
        let fragments = archive.package_fragments();
        assert_eq!(
            fragments
                .iter()
                .map(|fragment| (
                    fragment.package_fqname.as_str(),
                    archive.read(&fragment.entry).expect("read back")
                ))
                .collect::<Vec<_>>(),
            vec![
                ("", b"root".to_vec()),
                ("lib", b"first".to_vec()),
                ("lib", b"second".to_vec()),
            ]
        );
        std::fs::remove_dir_all(&root).ok();
    }
}
