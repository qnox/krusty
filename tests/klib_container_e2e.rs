//! The reference distribution's own KLIBs, read through the core container reader.
//!
//! The unit tests beside `src/klib.rs` pin the reader's rules against synthetic fixtures. These pin
//! it against the libraries Kotlin actually ships: the manifest fields a distribution klib carries,
//! the fragment-per-package split, and the fact that declarations and serialized IR travel in one
//! container. A klib is the symbol source every non-JVM target reads, so its shape has to be a
//! verified fact about the real artifact rather than about a fixture we wrote.

use std::collections::BTreeSet;

use krusty::klib::KlibArchive;

/// One klib from the reference distribution.
///
/// The distribution is a harness prerequisite, not an optional extra, so its absence fails these
/// tests rather than passing them: returning early here turned a missing toolchain — and a renamed
/// or corrupt klib — into a test that asserted nothing at all.
fn reference_klib(name: &str) -> KlibArchive {
    let directory = krusty::toolchain::kotlinc_lib_dir()
        .expect("the reference Kotlin distribution is a harness prerequisite");
    let path = directory.join(name);
    assert!(
        path.is_file(),
        "the distribution must ship {name}: {}",
        path.display()
    );
    KlibArchive::open(&path).unwrap_or_else(|error| panic!("{name} must open as a klib: {error}"))
}

#[test]
fn js_stdlib_klib_reports_its_identity_and_packages() {
    let archive = reference_klib("kotlin-stdlib-js.klib");
    let manifest = archive.manifest().expect("the manifest reads");
    assert_eq!(manifest.unique_name(), Some("kotlin"));
    assert_eq!(manifest.builtins_platform(), Some("JS"));
    assert!(
        manifest.abi_version().is_some() && manifest.metadata_version().is_some(),
        "a distribution klib names both versions: {manifest:?}"
    );

    let fragments = archive.package_fragments();
    let packages: BTreeSet<&str> = fragments
        .iter()
        .map(|fragment| fragment.package_fqname.as_str())
        .collect();
    for expected in [
        // The root package is spelled `root_package` on disk, not `package_` with an empty tail, so
        // it is listed here: a reader that only knew the `package_` prefix would report every other
        // package and silently skip this one.
        "",
        "kotlin",
        "kotlin.collections",
        "kotlin.ranges",
        "kotlin.text",
        "kotlin.js",
    ] {
        assert!(
            packages.contains(expected),
            "the JS stdlib declares {expected}: {packages:?}"
        );
    }
    assert!(
        fragments.len() > packages.len(),
        "a package's declarations are split across numbered fragments: \
         {} fragments over {} packages",
        fragments.len(),
        packages.len()
    );

    let first = fragments.first().expect("at least one fragment");
    assert!(
        !archive
            .read(&first.entry)
            .expect("read a fragment")
            .is_empty(),
        "a listed fragment reads back"
    );
    assert!(
        !archive
            .module_header()
            .expect("the module header reads")
            .is_empty(),
        "the module header is present"
    );
    assert!(
        archive
            .ir_entries()
            .contains(&"default/ir/irDeclarations.knd"),
        "declarations and serialized IR travel in one container: {:?}",
        archive.ir_entries()
    );
}

/// `builtins_platform` and the target list are what tell two klibs of the same library apart, so a
/// second platform's stdlib has to disagree with the first exactly there and nowhere else.
#[test]
fn the_wasm_stdlib_klib_differs_only_in_its_platform() {
    let js = reference_klib("kotlin-stdlib-js.klib")
        .manifest()
        .expect("the JS manifest reads");
    let wasm = reference_klib("kotlin-stdlib-wasm-js.klib")
        .manifest()
        .expect("the wasm manifest reads");
    assert_eq!(js.unique_name(), wasm.unique_name());
    assert_eq!(wasm.builtins_platform(), Some("WASM"));
    assert_eq!(wasm.targets(), vec!["wasm-js"]);
    assert!(
        js.targets().is_empty(),
        "a JS klib has one target and does not name it: {:?}",
        js.targets()
    );
}

/// A second library from the same distribution reads the same way and is told apart by its
/// `unique_name` and the packages it declares, not by its platform — `kotlin-test-js` shares
/// `builtins_platform=JS` with the stdlib beside it.
///
/// It records no `depends`: the distribution's klibs leave that key out entirely, so a resolver
/// cannot learn the library graph from the manifest alone. Pinning that here keeps a later linker
/// from being written against a field that is not there. (The manifest parser's own handling of
/// `depends` is pinned by the unit tests beside `src/klib.rs`, against a manifest that has one.)
#[test]
fn a_second_distribution_klib_is_told_apart_by_name_and_packages() {
    let archive = reference_klib("kotlin-test-js.klib");
    let manifest = archive.manifest().expect("the manifest reads");
    assert_eq!(manifest.unique_name(), Some("kotlin-test"));
    assert_eq!(manifest.builtins_platform(), Some("JS"));
    assert!(
        manifest.depends().is_empty(),
        "distribution klibs record no dependency list: {:?}",
        manifest.depends()
    );
    let fragments = archive.package_fragments();
    let packages: BTreeSet<&str> = fragments
        .iter()
        .map(|fragment| fragment.package_fqname.as_str())
        .collect();
    assert!(
        packages.contains("kotlin.test"),
        "kotlin-test declares kotlin.test: {packages:?}"
    );
    assert!(
        !packages.contains("kotlin.collections"),
        "and not the stdlib's packages: {packages:?}"
    );
}

/// The ZIP shape's own rules, on fixtures this repository writes.
///
/// The unit tests beside `src/klib.rs` pin the UNPACKED shape — containment, escapes, an absent
/// manifest — because that is the shape whose entries touch the filesystem. These pin the zip
/// shape's four answers, which no distribution klib can exercise: a distribution ships only valid
/// ones, so "not a zip", "not a klib" and "lists an entry it cannot produce" would never be reached.
mod synthetic {
    use super::*;
    use std::io::Write as _;
    use std::path::{Path, PathBuf};

    fn scratch(tag: &str) -> PathBuf {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos())
            .unwrap_or_default();
        let directory =
            std::env::temp_dir().join(format!("krusty-{tag}-{}-{unique}", std::process::id()));
        std::fs::create_dir_all(&directory).expect("create the fixture directory");
        directory
    }

    fn write_zip(path: &Path, entries: &[(&str, &[u8])]) {
        let file = std::fs::File::create(path).expect("create the fixture archive");
        let mut archive = zip::ZipWriter::new(file);
        for (name, contents) in entries {
            archive
                .start_file(
                    *name,
                    zip::write::SimpleFileOptions::default()
                        .compression_method(zip::CompressionMethod::Deflated),
                )
                .expect("start the fixture entry");
            archive
                .write_all(contents)
                .expect("write the fixture entry");
        }
        archive.finish().expect("finish the fixture archive");
    }

    /// The shape a distribution ships, written here so the three failures below are told apart from
    /// a success rather than only from each other.
    #[test]
    fn a_zip_klib_reads_its_manifest_fragments_and_ir() {
        let directory = scratch("klib-zip-valid");
        let path = directory.join("fixture.klib");
        write_zip(
            &path,
            &[
                (
                    "default/manifest",
                    b"unique_name=fixture\nbuiltins_platform=JS\n",
                ),
                ("default/linkdata/module", b"module-header"),
                ("default/linkdata/root_package/00_.knm", b"root"),
                ("default/linkdata/package_fixture.core/01_core.knm", b"core"),
                ("default/ir/irDeclarations.knd", b"ir"),
            ],
        );

        let archive = KlibArchive::open(&path).expect("the fixture opens as a klib");
        let manifest = archive.manifest().expect("the manifest reads");
        assert_eq!(manifest.unique_name(), Some("fixture"));
        assert_eq!(manifest.builtins_platform(), Some("JS"));
        assert_eq!(
            archive.module_header().expect("the module header reads"),
            b"module-header"
        );
        assert_eq!(
            archive
                .package_fragments()
                .iter()
                .map(|fragment| fragment.package_fqname.clone())
                .collect::<Vec<_>>(),
            ["", "fixture.core"],
            "the root package is listed beside the named one"
        );
        assert_eq!(archive.ir_entries(), vec!["default/ir/irDeclarations.knd"]);
    }

    /// A file that is not a zip at all. Something is there and cannot be read, which is not the
    /// same answer as nothing being there — and both produced `None` before.
    #[test]
    fn a_file_that_does_not_parse_as_a_zip_is_malformed() {
        let directory = scratch("klib-zip-invalid");
        let path = directory.join("fixture.klib");
        std::fs::write(
            &path,
            b"PK\x03\x04 and then nothing that follows the format",
        )
        .expect("write the fixture");
        let error = KlibArchive::open(&path).err().expect("not a zip");
        assert!(
            !error.is_absence(),
            "a corrupt archive is not an absent library: {error}"
        );
    }

    /// A zip with no `default/` tree is some other archive — a jar on the wrong path — and is an
    /// ABSENT library. Both on-disk shapes answer this the same way.
    #[test]
    fn a_zip_without_a_default_tree_is_not_a_klib() {
        let directory = scratch("klib-zip-not-a-klib");
        let path = directory.join("fixture.klib");
        write_zip(
            &path,
            &[("META-INF/MANIFEST.MF", b"Manifest-Version: 1.0\n")],
        );
        let error = KlibArchive::open(&path).err().expect("not a klib");
        assert!(
            error.is_absence(),
            "a zip that is not a klib is an absent library, not a failed one: {error}"
        );
    }

    /// An entry the central directory LISTS and whose payload does not inflate. The archive
    /// advertises a fragment it cannot hand back, which is corruption; skipping it would publish a
    /// library with silently fewer declarations than it claims.
    #[test]
    fn an_entry_that_cannot_be_inflated_is_reported_not_skipped() {
        let directory = scratch("klib-zip-bad-entry");
        let path = directory.join("fixture.klib");
        write_zip(
            &path,
            &[
                ("default/manifest", b"unique_name=fixture\n"),
                (
                    "default/linkdata/package_fixture.core/01_core.knm",
                    b"a payload long enough that corrupting it cannot still inflate",
                ),
            ],
        );
        // Corrupt the deflate stream in place, leaving the central directory — and so the entry
        // listing — intact. The archive still says it has the fragment.
        let mut bytes = std::fs::read(&path).expect("read the fixture back");
        let start = bytes.len() / 3;
        for byte in &mut bytes[start..start + 24] {
            *byte ^= 0xff;
        }
        std::fs::write(&path, &bytes).expect("write the corrupted fixture");

        let archive = KlibArchive::open(&path).expect("the archive still opens");
        let entry = "default/linkdata/package_fixture.core/01_core.knm";
        assert!(
            archive.entries().iter().any(|listed| listed == entry),
            "the archive still lists the fragment: {:?}",
            archive.entries()
        );
        assert!(
            archive.read(entry).is_err(),
            "and reading it is a failure, not an empty fragment"
        );
    }
}
