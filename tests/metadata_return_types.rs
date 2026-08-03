//! `@Metadata` return-type decoding recovers Kotlin's read-only/mutable distinction (`mutableListOf`
//! returns `MutableList`, `listOf` returns `List`) — which the JVM descriptor/`Signature` both erase to
//! `java/util/List`. This is the foundation for distinguishing read-only vs mutable collections.

use krusty::jvm::classpath::Classpath;
use krusty::jvm::jvm_libraries::JvmLibraries;
use krusty::jvm::metadata::{package_functions, parse_builtins};
use krusty::symbol_resolver::{SymRecv, Symbol, SymbolResolver};
use krusty::types::{type_name, Ty};
use std::rc::Rc;

use super::common;

#[test]
fn collection_factory_return_types_distinguish_mutable() {
    let Some(jar) = common::stdlib_jar() else {
        eprintln!("skip: no kotlin-stdlib jar");
        return;
    };
    let cp = Classpath::new(vec![jar]);
    // `listOf`/`mutableListOf`/`emptyList` live in this CollectionsKt facade part.
    let ci = cp
        .find("kotlin/collections/CollectionsKt__CollectionsKt")
        .expect("CollectionsKt part on classpath");
    let fns = package_functions(&ci);
    let ret = |name: &str| {
        fns.iter()
            .find(|f| f.kotlin_name == name)
            .and_then(|f| f.ret_class)
    };
    assert_eq!(
        ret("listOf"),
        Some(type_name("kotlin/collections/List")),
        "listOf must decode to the read-only List from @Metadata"
    );
    assert_eq!(
        ret("mutableListOf"),
        Some(type_name("kotlin/collections/MutableList")),
        "mutableListOf must decode to MutableList from @Metadata (JVM signature erases both to java/util/List)"
    );
    assert_eq!(ret("emptyList"), Some(type_name("kotlin/collections/List")));
    // A type stored directly in the d2 string table (not a predefined) still resolves.
    assert_eq!(ret("arrayListOf"), Some(type_name("java/util/ArrayList")));
}

#[test]
fn nullable_type_parameter_return_metadata_is_kept() {
    let Some(jar) = common::stdlib_jar() else {
        eprintln!("skip: no kotlin-stdlib jar");
        return;
    };
    let cp = Classpath::new(vec![jar]);
    let ret = ["kotlin/StandardKt", "kotlin/StandardKt__StandardKt"]
        .iter()
        .find_map(|owner| {
            cp.meta_functions(owner)
                .iter()
                .find(|f| f.kotlin_name == "takeIf")
                .cloned()
        })
        .expect("takeIf metadata return");
    assert!(
        ret.ret_class.is_none(),
        "takeIf returns a type parameter, not a concrete class"
    );
    assert!(ret.ret_nullable(), "takeIf returns T? in metadata");
}

/// The Kotlin collection hierarchy is read from `collections.kotlin_builtins` exactly as kotlinc stores
/// it — the read-only/mutable supertyping (`MutableList : List, MutableCollection`) that exists in no JVM
/// descriptor. Parsed straight from the jar entry (`PackageFragment` + `QualifiedNameTable`).
#[test]
fn builtins_decode_collection_hierarchy() {
    let Some(jar) = common::stdlib_jar() else {
        eprintln!("skip: no kotlin-stdlib jar");
        return;
    };
    let mut zip = zip::ZipArchive::new(std::fs::File::open(&jar).unwrap()).unwrap();
    let mut entry = zip
        .by_name("kotlin/collections/collections.kotlin_builtins")
        .expect("collections.kotlin_builtins in stdlib jar");
    let mut bytes = Vec::new();
    std::io::Read::read_to_end(&mut entry, &mut bytes).unwrap();
    let h = parse_builtins(&bytes);
    assert_eq!(
        h.get("kotlin/collections/MutableList")
            .map(|c| c.supertypes.as_slice()),
        Some(
            &[
                "kotlin/collections/List".to_string(),
                "kotlin/collections/MutableCollection".to_string()
            ][..]
        )
    );
    assert_eq!(
        h.get("kotlin/collections/List")
            .map(|c| c.supertypes.as_slice()),
        Some(&["kotlin/collections/Collection".to_string()][..])
    );
    assert_eq!(
        h.get("kotlin/collections/MutableMap")
            .map(|c| c.supertypes.as_slice()),
        Some(&["kotlin/collections/Map".to_string()][..])
    );
}

/// `String`'s members read straight from `kotlin/kotlin.kotlin_builtins` (no hardcoded member table):
/// the `get(Int): Char` operator, `length: Int`, `plus(Any?): String`, `compareTo(String): Int`.
#[test]
fn builtins_string_members_from_metadata() {
    let Some(jar) = common::stdlib_jar() else {
        eprintln!("skip: no kotlin-stdlib jar");
        return;
    };
    let mut zip = zip::ZipArchive::new(std::fs::File::open(&jar).unwrap()).unwrap();
    let mut entry = zip
        .by_name("kotlin/kotlin.kotlin_builtins")
        .expect("kotlin.kotlin_builtins in stdlib jar");
    let mut bytes = Vec::new();
    std::io::Read::read_to_end(&mut entry, &mut bytes).unwrap();
    let string = krusty::jvm::metadata::parse_builtins(&bytes)
        .remove("kotlin/String")
        .expect("String builtin class");
    // `kotlin/String` omits `Class.flags` in the shipped fragment. The protobuf default is the
    // semantic PUBLIC FINAL word (`6`), so the parser must produce the same JVM access as an explicit
    // public-final class rather than leaking the wire omission downstream as INTERNAL (`0`).
    assert_eq!(string.access, 0x0019, "public static final");
    let members = string.members;
    let find = |name: &str| members.iter().find(|m| m.name == name);
    // Functions: `get(Int): Char` (the `s[i]` operator), `plus(Any?): String`, `compareTo(String): Int`.
    let get = find("get").expect("String.get");
    assert_eq!(
        get.params.iter().map(|p| p.render()).collect::<Vec<_>>(),
        vec!["kotlin/Int".to_string()]
    );
    assert_eq!(get.ret.render(), "kotlin/Char");
    assert_eq!(
        find("plus").expect("String.plus").ret.render(),
        "kotlin/String"
    );
    assert_eq!(
        find("compareTo").expect("String.compareTo").ret.render(),
        "kotlin/Int"
    );
    // The `length: Int` PROPERTY (Class.property = field 10) also resolves from builtins.
    assert_eq!(
        find("length").expect("String.length").ret.render(),
        "kotlin/Int"
    );
}

/// Read one `.kotlin_builtins` fragment out of the stdlib jar.
fn builtins_fragment(jar: &std::path::Path, path: &str) -> Vec<u8> {
    let mut zip = zip::ZipArchive::new(std::fs::File::open(jar).unwrap()).unwrap();
    let mut entry = zip.by_name(path).expect("builtins fragment in stdlib jar");
    let mut bytes = Vec::new();
    std::io::Read::read_to_end(&mut entry, &mut bytes).unwrap();
    bytes
}

/// A builtin member whose type is a TYPE PARAMETER (`List<E>.get(index: Int): E`) must decode — not be
/// dropped — and a type ARGUMENT must survive the decode (`Map<K, V>.entries: Set<Map.Entry<K, V>>`).
/// Both are the only record of these signatures when the mapped JVM class is off the classpath.
#[test]
fn builtins_decode_type_parameters_and_arguments() {
    let Some(jar) = common::stdlib_jar() else {
        eprintln!("skip: no kotlin-stdlib jar");
        return;
    };
    let bytes = builtins_fragment(&jar, "kotlin/collections/collections.kotlin_builtins");
    let classes = parse_builtins(&bytes);

    let list = classes
        .get("kotlin/collections/List")
        .expect("List builtin");
    assert_eq!(
        list.type_params
            .iter()
            .map(|p| p.name.clone())
            .collect::<Vec<_>>(),
        vec!["E".to_string()]
    );
    let get = list
        .members
        .iter()
        .find(|m| m.name == "get")
        .expect("List.get must not be dropped for its type-parameter return");
    assert_eq!(
        get.params.iter().map(|p| p.render()).collect::<Vec<_>>(),
        vec!["kotlin/Int".to_string()]
    );
    assert_eq!(get.ret.render(), "E");

    let map = classes.get("kotlin/collections/Map").expect("Map builtin");
    let entries = map
        .members
        .iter()
        .find(|m| m.name == "entries")
        .expect("Map.entries");
    assert_eq!(
        entries.ret.render(),
        "kotlin/collections/Set<kotlin/collections/Map.Entry<K,V>>",
        "the element type argument must survive the builtins decode"
    );
}

/// The end of that chain: with NO JDK on the classpath (only the stdlib jar), a builtin member whose
/// return is a type parameter still resolves AND binds against the receiver's type argument —
/// `List<String>.get(Int)` is `String`, not a dropped member. The `.kotlin_builtins` fallback is a
/// supported configuration, so its members must carry a generic signature like any other.
#[test]
fn builtin_generic_member_binds_receiver_argument_without_jdk() {
    let Some(jar) = common::stdlib_jar() else {
        eprintln!("skip: no kotlin-stdlib jar");
        return;
    };
    // Only the stdlib jar: no JDK, so `java/util/List` is absent and the builtins fallback is taken.
    let libs = JvmLibraries::new(Rc::new(Classpath::new(vec![jar])));
    let scope = [type_name("kotlin/collections")];
    let resolver = SymbolResolver::new_scoped(&libs, &scope);
    let call = resolver
        .resolve_symbol(
            SymRecv::Value(Ty::obj_args("kotlin/collections/List", &[Ty::String])),
            "get",
            &[Ty::Int],
            &[],
        )
        .and_then(Symbol::call)
        .expect("List.get must resolve from .kotlin_builtins with no JDK on the classpath");
    assert_eq!(
        call.ret,
        Ty::String,
        "the type-parameter return must bind to the receiver's type argument"
    );
    // Interface-ness comes from the builtin's own `CLASS_KIND`, not a curated JVM-name table — with
    // no `java/util/List` to read the flag off, a curated table that omits `java/util/*` answered
    // "class" for every collection member.
    let members =
        krusty::symbol_source::SymbolSource::resolve_type(&libs, "kotlin/collections/List")
            .expect("List resolves from .kotlin_builtins")
            .members;
    assert!(
        members
            .iter()
            .find(|m| m.name == "get")
            .expect("List.get member")
            .is_interface(),
        "a member of the `List` builtin must be marked interface-dispatched"
    );
}

/// The same fix seen through the checker, with NO JDK on the classpath: an indexed read, a
/// type-argument-carrying property chain, and a destructured lambda parameter over `Map.entries` all
/// depend on the builtins decode keeping type parameters AND type arguments.
#[test]
fn builtin_generic_members_type_check_without_jdk() {
    let Some(jar) = common::stdlib_jar() else {
        eprintln!("skip: no kotlin-stdlib jar");
        return;
    };
    let src = r#"
fun a(l: List<String>): String = l.get(1)
fun b(l: List<String>): String = l[1]
fun c(m: Map<String, Int>): String = m.entries.first().key
fun d(m: Map<String, Int>): List<Int> = m.entries.map { (k, v) -> k.length + v }
"#;
    // `jdk_modules = None`: the mapped JVM classes are absent, so every one of these resolves
    // through the `.kotlin_builtins` fallback.
    let diags = common::front_end_diagnostics(src, &[jar], None);
    assert!(
        diags.is_empty(),
        "builtin generic members must type-check with no JDK on the classpath, got: {diags:?}"
    );
}

/// `MutableList.removeAt(Int)` is `java.util.List.remove(int)`, so a class implementing `MutableList`
/// gets a `remove(int)` bridge to its `removeAt` override. That renaming exists ONLY on the mutable
/// side: a READ-ONLY `List` implementation declaring an unrelated `removeAt` must get no such bridge,
/// or an unrelated method silently becomes the `java.util.List.remove(int)` implementation.
#[test]
fn read_only_list_impl_gets_no_remove_bridge() {
    let (Some(jar), Some(jdk)) = (common::stdlib_jar(), common::jdk_modules()) else {
        eprintln!("skip: no kotlin-stdlib jar / JDK");
        return;
    };
    let cp = [jar];
    let bridge_of = |src: &str| -> bool {
        let classes = common::compile_in_process(src, "Ro", &cp, Some(&jdk)).expect("compiles");
        let bytes = classes
            .iter()
            .find(|(name, _)| name == "Ro")
            .map(|(_, b)| b.clone())
            .expect("Ro class emitted");
        krusty::jvm::classreader::parse_class(&bytes)
            .expect("parse Ro")
            .methods
            .iter()
            .any(|m| m.name == "remove" && m.descriptor.starts_with("(I)"))
    };
    assert!(
        !bridge_of(
            r#"
abstract class Ro : List<String> {
    fun removeAt(index: Int): String = "x"
}
"#
        ),
        "a read-only List implementation must not get a remove(int) bridge"
    );
    assert!(
        bridge_of(
            r#"
abstract class Ro : MutableList<String> {
    fun removeAt(index: Int): String = "x"
}
"#
        ),
        "a MutableList implementation must expose removeAt as remove(int)"
    );
}

/// The Classpath subtype helpers built on that hierarchy: `MutableList <: MutableCollection`, but the
/// read-only `List` is NOT — which is exactly what makes `MutableCollection.plusAssign` apply to a
/// `MutableList` receiver and not to a `List`. A non-builtin name (`ArrayList`) is not in the hierarchy.
#[test]
fn kotlin_collection_subtyping() {
    let Some(jar) = common::stdlib_jar() else {
        eprintln!("skip: no kotlin-stdlib jar");
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
/// — which the JVM signature erases to a `java/util/Collection`/`Map` parameter. Assert through the
/// resolver surface, not a name-only metadata probe: the selected callable should exist only for a
/// mutable receiver.
#[test]
fn plus_assign_receiver_is_mutable() {
    let Some(jar) = common::stdlib_jar() else {
        eprintln!("skip: no kotlin-stdlib jar");
        return;
    };
    let libs = JvmLibraries::new(Rc::new(Classpath::new(vec![jar])));
    let scope = [type_name("kotlin/collections")];
    let resolver = SymbolResolver::new_scoped(&libs, &scope);
    let mutable = resolver
        .resolve_symbol(
            SymRecv::Value(Ty::obj("kotlin/collections/MutableCollection")),
            "plusAssign",
            &[Ty::Int],
            &[],
        )
        .and_then(Symbol::extension_call);
    assert!(
        mutable.is_some(),
        "plusAssign must resolve for a MutableCollection receiver"
    );
    assert!(
        resolver
            .resolve_symbol(
                SymRecv::Value(Ty::obj("kotlin/collections/List")),
                "plusAssign",
                &[Ty::Int],
                &[],
            )
            .and_then(Symbol::extension_call)
            .is_none(),
        "plusAssign must not bind a read-only List receiver"
    );
}
