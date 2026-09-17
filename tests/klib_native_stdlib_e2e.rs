//! The Kotlin/Native stdlib KLIB, read through the core.
//!
//! This is the library a native target resolves against, and the reason the klib work exists: until
//! now krusty's native backend inferred Kotlin signatures from the JVM stdlib JAR, which is a
//! different library with different spellings. Here the signatures come from the artifact the
//! distribution actually ships for that target.
//!
//! It is also the artifact that settles the container's two shapes. `klib/common/stdlib` is an
//! UNPACKED DIRECTORY, not a zip, so a reader that understood only zip archives would find no stdlib
//! at all on a Native distribution.
//!
//! Provision with `just kotlin-native`; the tests return without asserting when it is absent.

use krusty::klib::KlibArchive;
use krusty::metadata::reader::parse_package_fragment;

fn native_stdlib() -> Option<KlibArchive> {
    KlibArchive::open(&krusty::toolchain::kotlin_native_stdlib()?)
}

#[test]
fn the_native_stdlib_is_an_unpacked_directory_naming_its_targets() {
    let Some(stdlib) = krusty::toolchain::kotlin_native_stdlib() else {
        return;
    };
    assert!(
        stdlib.is_dir(),
        "the Native distribution ships its stdlib unpacked: {}",
        stdlib.display()
    );
    let archive = KlibArchive::open(&stdlib).expect("open the Native stdlib klib");
    let manifest = archive.manifest();
    assert_eq!(manifest.unique_name(), Some("stdlib"));
    assert_eq!(manifest.builtins_platform(), Some("NATIVE"));
    let targets = manifest.targets();
    assert!(
        targets.len() > 10 && targets.contains(&"linux_x64"),
        "native_targets names every target the prebuilt supports: {targets:?}"
    );
    assert!(
        !archive.ir_entries().is_empty(),
        "and the bodies travel in the same container"
    );
}

/// The signatures a native backend needs, from the library its own target ships.
///
/// `isNaN` is here under its MEMBER spelling on `Double` and on `Float` separately — the shape whose
/// JVM-facade mismatch had to be handled by hand while the signature source was the JVM stdlib jar.
#[test]
fn the_native_stdlib_declares_its_whole_api() {
    let Some(archive) = native_stdlib() else {
        return;
    };
    let mut classes = std::collections::HashMap::new();
    let mut functions = Vec::new();
    let mut packages = std::collections::BTreeSet::new();
    for fragment in archive.package_fragments() {
        packages.insert(fragment.package_fqname.clone());
        let Some(bytes) = archive.read(&fragment.entry) else {
            continue;
        };
        let package = parse_package_fragment(&bytes);
        classes.extend(package.classes);
        functions.extend(package.functions);
    }
    assert!(
        classes.len() > 800 && functions.len() > 5000,
        "the Native stdlib declares its whole API: {} classifiers, {} functions",
        classes.len(),
        functions.len()
    );
    assert!(
        packages.contains("kotlin.native.concurrent"),
        "including the packages only this target has: {packages:?}"
    );

    let string = classes.get("kotlin/String").expect("kotlin.String");
    assert_eq!(
        string.supertypes,
        vec!["kotlin/Comparable", "kotlin/CharSequence"],
        "String's supertypes come from the metadata, not a curated table"
    );

    let atomic = classes
        .get("kotlin/native/concurrent/AtomicInt")
        .expect("a Native-only classifier");
    assert_eq!(atomic.kind, krusty::libraries::TypeKind::Class);
    assert!(!atomic.members.is_empty());

    let is_nan: Vec<String> = functions
        .iter()
        .filter(|function| function.name == "isNaN")
        .filter_map(|function| function.receiver.as_ref().map(|receiver| receiver.render()))
        .collect();
    assert!(
        is_nan.contains(&"kotlin/Double".to_string())
            && is_nan.contains(&"kotlin/Float".to_string()),
        "isNaN is declared once per receiver type, under its member spelling: {is_nan:?}"
    );

    let list = classes
        .get("kotlin/collections/List")
        .expect("kotlin.collections.List");
    let get = list
        .members
        .iter()
        .find(|member| member.name == "get")
        .expect("List.get");
    assert_eq!(
        get.param_names,
        vec!["index".to_string()],
        "a shipped library names its parameters, which a named argument needs"
    );
    assert_eq!(
        get.param_defaults,
        vec![false],
        "and says which declare a default"
    );

    let mem_scoped = functions
        .iter()
        .find(|function| function.name == "memScoped")
        .expect("memScoped, which only a native target has");
    assert_eq!(mem_scoped.ret.render(), "R");
    assert_eq!(
        mem_scoped
            .params
            .iter()
            .map(|parameter| parameter.render())
            .collect::<Vec<_>>(),
        vec!["kotlin/Function1<kotlinx/cinterop/MemScope,R>"],
        "with its cinterop scope receiver intact"
    );
}
