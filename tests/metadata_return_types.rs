//! `@Metadata` return-type decoding recovers Kotlin's read-only/mutable distinction (`mutableListOf`
//! returns `MutableList`, `listOf` returns `List`) — which the JVM descriptor/`Signature` both erase to
//! `java/util/List`. This is the foundation for distinguishing read-only vs mutable collections.

use krusty::jvm::classpath::Classpath;
use krusty::jvm::metadata::{builtins_supertypes, package_function_return_types};
use std::path::PathBuf;

fn stdlib() -> Option<PathBuf> {
    let kc = std::env::var("KRUSTY_KOTLINC")
        .ok()
        .filter(|s| !s.is_empty())?;
    let jar = PathBuf::from(&kc)
        .parent()?
        .parent()?
        .join("lib/kotlin-stdlib.jar");
    jar.exists().then_some(jar)
}

#[test]
fn collection_factory_return_types_distinguish_mutable() {
    let Some(jar) = stdlib() else {
        eprintln!("skip: set KRUSTY_KOTLINC");
        return;
    };
    let cp = Classpath::new(vec![jar]);
    // `listOf`/`mutableListOf`/`emptyList` live in this CollectionsKt facade part.
    let ci = cp
        .find("kotlin/collections/CollectionsKt__CollectionsKt")
        .expect("CollectionsKt part on classpath");
    let rets = package_function_return_types(&ci);
    assert_eq!(
        rets.get("listOf").map(String::as_str),
        Some("kotlin/collections/List"),
        "listOf must decode to the read-only List from @Metadata"
    );
    assert_eq!(
        rets.get("mutableListOf").map(String::as_str),
        Some("kotlin/collections/MutableList"),
        "mutableListOf must decode to MutableList from @Metadata (JVM signature erases both to java/util/List)"
    );
    assert_eq!(
        rets.get("emptyList").map(String::as_str),
        Some("kotlin/collections/List")
    );
    // A type stored directly in the d2 string table (not a predefined) still resolves.
    assert_eq!(
        rets.get("arrayListOf").map(String::as_str),
        Some("java/util/ArrayList")
    );
}

/// The Kotlin collection hierarchy is read from `collections.kotlin_builtins` exactly as kotlinc stores
/// it — the read-only/mutable supertyping (`MutableList : List, MutableCollection`) that exists in no JVM
/// descriptor. Parsed straight from the jar entry (`PackageFragment` + `QualifiedNameTable`).
#[test]
fn builtins_supertypes_decode_collection_hierarchy() {
    let Some(jar) = stdlib() else {
        eprintln!("skip: set KRUSTY_KOTLINC");
        return;
    };
    let mut zip = zip::ZipArchive::new(std::fs::File::open(&jar).unwrap()).unwrap();
    let mut entry = zip
        .by_name("kotlin/collections/collections.kotlin_builtins")
        .expect("collections.kotlin_builtins in stdlib jar");
    let mut bytes = Vec::new();
    std::io::Read::read_to_end(&mut entry, &mut bytes).unwrap();
    let h = builtins_supertypes(&bytes);
    assert_eq!(
        h.get("kotlin/collections/MutableList").map(Vec::as_slice),
        Some(
            &[
                "kotlin/collections/List".to_string(),
                "kotlin/collections/MutableCollection".to_string()
            ][..]
        )
    );
    assert_eq!(
        h.get("kotlin/collections/List").map(Vec::as_slice),
        Some(&["kotlin/collections/Collection".to_string()][..])
    );
    assert_eq!(
        h.get("kotlin/collections/MutableMap").map(Vec::as_slice),
        Some(&["kotlin/collections/Map".to_string()][..])
    );
}

/// `String`'s members read straight from `kotlin/kotlin.kotlin_builtins` (no hardcoded member table):
/// the `get(Int): Char` operator, `length: Int`, `plus(Any?): String`, `compareTo(String): Int`.
#[test]
fn builtins_string_members_from_metadata() {
    let Some(jar) = stdlib() else {
        eprintln!("skip: set KRUSTY_KOTLINC");
        return;
    };
    let mut zip = zip::ZipArchive::new(std::fs::File::open(&jar).unwrap()).unwrap();
    let mut entry = zip
        .by_name("kotlin/kotlin.kotlin_builtins")
        .expect("kotlin.kotlin_builtins in stdlib jar");
    let mut bytes = Vec::new();
    std::io::Read::read_to_end(&mut entry, &mut bytes).unwrap();
    let members = krusty::jvm::metadata::builtins_class_members(&bytes, "kotlin/String");
    let find = |name: &str| members.iter().find(|m| m.name == name);
    // Functions: `get(Int): Char` (the `s[i]` operator), `plus(Any?): String`, `compareTo(String): Int`.
    let get = find("get").expect("String.get");
    assert_eq!(get.params, vec!["kotlin/Int".to_string()]);
    assert_eq!(get.ret, "kotlin/Char");
    assert_eq!(find("plus").expect("String.plus").ret, "kotlin/String");
    assert_eq!(
        find("compareTo").expect("String.compareTo").ret,
        "kotlin/Int"
    );
    // The `length: Int` PROPERTY (Class.property = field 10) also resolves from builtins.
    assert_eq!(find("length").expect("String.length").ret, "kotlin/Int");
}

/// The Classpath subtype helpers built on that hierarchy: `MutableList <: MutableCollection`, but the
/// read-only `List` is NOT — which is exactly what makes `MutableCollection.plusAssign` apply to a
/// `MutableList` receiver and not to a `List`. A non-builtin name (`ArrayList`) is not in the hierarchy.
#[test]
fn kotlin_collection_subtyping() {
    let Some(jar) = stdlib() else {
        eprintln!("skip: set KRUSTY_KOTLINC");
        return;
    };
    let cp = Classpath::new(vec![jar]);
    assert!(cp.is_kotlin_collection("kotlin/collections/MutableList"));
    assert!(cp.is_kotlin_collection("kotlin/collections/List"));
    assert!(!cp.is_kotlin_collection("java/util/ArrayList"));
    assert!(cp.kotlin_subtype(
        "kotlin/collections/MutableList",
        "kotlin/collections/MutableCollection"
    ));
    assert!(cp.kotlin_subtype(
        "kotlin/collections/MutableMap",
        "kotlin/collections/MutableMap"
    ));
    assert!(!cp.kotlin_subtype(
        "kotlin/collections/List",
        "kotlin/collections/MutableCollection"
    ));
}

/// `@Metadata` carries the Kotlin extension-receiver of `plusAssign` — `MutableCollection`/`MutableMap`
/// — which the JVM signature erases to a `java/util/Collection`/`Map` parameter. This is what lets
/// overload resolution reject `plusAssign` on a read-only `List` (no `Mutable*` receiver is its supertype).
#[test]
fn plus_assign_receiver_is_mutable() {
    let Some(jar) = stdlib() else {
        eprintln!("skip: set KRUSTY_KOTLINC");
        return;
    };
    let cp = Classpath::new(vec![jar]);
    let krs = cp.metadata_receiver_types("kotlin/collections/CollectionsKt", "plusAssign");
    assert!(
        krs.iter()
            .any(|k| k == "kotlin/collections/MutableCollection"),
        "plusAssign must have a MutableCollection receiver, got {krs:?}"
    );
    // `plus` (read-only) must NOT carry a Mutable receiver — else it would be wrongly rejected on `List`.
    let plus = cp.metadata_receiver_types("kotlin/collections/CollectionsKt", "plus");
    assert!(
        !plus
            .iter()
            .any(|k| k.starts_with("kotlin/collections/Mutable")),
        "plus must not be a Mutable* extension, got {plus:?}"
    );
}
