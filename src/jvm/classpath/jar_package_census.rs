//! A jar's package catalog from its central directory: which packages hold classes or builtins,
//! and each `kotlin_module`'s facades. Only the module files are read.

use std::fs::File;
use std::path::Path;

use super::archive_entries::ArchiveEntries;
use super::{module_optional_annotations, record_pkg_entry_name, JarPackages};

/// Catalog the jar at `jar` into `jp`; `false` when the catalog is incomplete.
pub(super) fn catalog_jar(jar: &Path, jp: &mut JarPackages) -> bool {
    let file = match File::open(jar) {
        Ok(file) => file,
        // A missing entry is empty until its snapshot changes.
        Err(error) => return error.kind() == std::io::ErrorKind::NotFound,
    };
    let Ok(mut archive) = crate::archive::open_file(file) else {
        return false;
    };
    catalog_archive(&mut archive, jp)
}

/// Catalog `archive` into `jp`. An entry whose name does not decode or whose module file cannot
/// be read leaves the catalog incomplete, so a lookup never treats a missing package as absent.
fn catalog_archive(archive: &mut impl ArchiveEntries, jp: &mut JarPackages) -> bool {
    // Name pass over the central directory — no decompression. Defer reading `kotlin_module` bytes.
    let mut modules = Vec::new();
    let mut complete = true;
    for index in 0..archive.entry_count() {
        let Ok(name) = archive.entry_name(index) else {
            complete = false;
            continue;
        };
        if name.starts_with("META-INF/") && name.ends_with(".kotlin_module") {
            modules.push((index, name.into_owned()));
        } else {
            record_pkg_entry_name(&name, jp);
        }
    }
    for (index, name) in modules {
        match archive.entry_bytes(index) {
            Some(bytes) => module_optional_annotations::record_kotlin_module(&name, &bytes, jp),
            None => complete = false,
        }
    }
    complete
}

#[cfg(test)]
mod tests {
    use super::super::archive_entries::test_archives::{archive, UndecodableName};
    use super::*;

    #[test]
    fn an_undecodable_name_after_a_class_leaves_the_catalog_incomplete() {
        let mut jar = UndecodableName {
            inner: archive(&[("p/A.class", b""), ("p/B.class", b"")]),
            index: 1,
        };
        let mut packages = JarPackages::default();

        assert!(!catalog_archive(&mut jar, &mut packages));
        assert_eq!(packages.classes.len(), 1);
    }

    #[test]
    fn a_decodable_archive_catalogs_completely() {
        let mut jar = archive(&[("p/A.class", b""), ("p/B.class", b"")]);
        let mut packages = JarPackages::default();

        assert!(catalog_archive(&mut jar, &mut packages));
        assert_eq!(packages.classes.len(), 2);
    }
}
