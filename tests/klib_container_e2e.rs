//! The reference distribution's own KLIBs, read through the core container reader.
//!
//! The unit tests beside `src/klib.rs` pin the reader's rules against synthetic fixtures. These pin
//! it against the libraries Kotlin actually ships: the manifest fields a distribution klib carries,
//! the fragment-per-package split, and the fact that declarations and serialized IR travel in one
//! container. A klib is the symbol source every non-JVM target reads, so its shape has to be a
//! verified fact about the real artifact rather than about a fixture we wrote.

use std::collections::BTreeSet;

use krusty::klib::KlibArchive;

fn reference_klib(name: &str) -> Option<KlibArchive> {
    let directory = krusty::toolchain::kotlinc_lib_dir()?;
    let path = directory.join(name);
    if !path.is_file() {
        return None;
    }
    KlibArchive::open(&path)
}

#[test]
fn js_stdlib_klib_reports_its_identity_and_packages() {
    let Some(archive) = reference_klib("kotlin-stdlib-js.klib") else {
        return;
    };
    let manifest = archive.manifest();
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
        archive
            .module_header()
            .is_some_and(|bytes| !bytes.is_empty()),
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
    let (Some(js), Some(wasm)) = (
        reference_klib("kotlin-stdlib-js.klib"),
        reference_klib("kotlin-stdlib-wasm-js.klib"),
    ) else {
        return;
    };
    assert_eq!(js.manifest().unique_name(), wasm.manifest().unique_name());
    assert_eq!(wasm.manifest().builtins_platform(), Some("WASM"));
    assert_eq!(wasm.manifest().targets(), vec!["wasm-js"]);
    assert!(
        js.manifest().targets().is_empty(),
        "a JS klib has one target and does not name it: {:?}",
        js.manifest().targets()
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
    let Some(archive) = reference_klib("kotlin-test-js.klib") else {
        return;
    };
    let manifest = archive.manifest();
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
