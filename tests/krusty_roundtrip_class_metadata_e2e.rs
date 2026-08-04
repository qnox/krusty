//! krusty → krusty round-trips that need per-CLASS `@kotlin.Metadata`.
//!
//! The facade `@Metadata` describes a file's TOP-LEVEL declarations only. Everything a reader needs
//! about a *class* — its constructor's parameter NAMES (named arguments), its members' Kotlin
//! signatures, and the `operator` marks on `componentN` (destructuring) — lives in the `@Metadata`
//! attached to the class file itself. These tests compile a library with krusty's SHIPPING default
//! options (`common::compile_in_process` → `JvmBackend`, no env overrides), write the emitted classes
//! to a directory, and compile+run a second source set against them. They fail — not skip — when
//! krusty rejects either half, so a missing metadata record cannot pass as a skip.
use super::common;
use krusty::jvm::classreader::parse_class;
use std::path::PathBuf;

/// A compiled library: the directory its classes were written to, plus the `(internal name, bytes)`
/// pairs themselves (so a test can decode the `@Metadata` without re-reading the files).
type CompiledLib = (PathBuf, Vec<(String, Vec<u8>)>);

/// Compile `lib_src` with krusty's default backend options and write the classes into a fresh
/// directory. `None` only when the external toolchain (stdlib jar / JDK jimage) is unavailable.
fn krusty_lib_dir(tag: &str, lib_src: &str) -> Option<CompiledLib> {
    let stdlib = common::stdlib_jar();
    let jdk = common::jdk_modules();
    let classpath = [stdlib];
    let classes = common::compile_in_process(lib_src, "Lib", &classpath, Some(&jdk))
        .unwrap_or_else(|| {
            let diagnostics = common::front_end_diagnostics(lib_src, &classpath, Some(&jdk));
            panic!("{tag}: krusty rejected the library source; diagnostics: {diagnostics:?}")
        });
    let dir = std::env::temp_dir().join(format!("krusty_clsmeta_{tag}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    for (name, bytes) in &classes {
        let path = dir.join(format!("{name}.class"));
        std::fs::create_dir_all(path.parent()?).ok()?;
        std::fs::write(&path, bytes).ok()?;
    }
    Some((dir, classes))
}

/// Compile+run `main` against a krusty-built `lib_src`, asserting `"OK"`. Skips only when the
/// toolchain is missing; a compiler rejection is a FAILURE with the diagnostics attached.
fn expect_roundtrip_ok(tag: &str, lib_src: &str, main: &str) {
    let Some((dir, _)) = krusty_lib_dir(tag, lib_src) else {
        eprintln!("skip ({tag}: kotlin stdlib / JDK unavailable)");
        return;
    };
    let stdlib = common::stdlib_jar();
    let jdk = common::jdk_modules();
    let classpath = [dir, stdlib];
    let output =
        common::compile_and_run_box(main, "Main", &classpath, Some(&jdk)).unwrap_or_else(|| {
            let diagnostics = common::front_end_diagnostics(main, &classpath, Some(&jdk));
            panic!("{tag}: compiling/running against krusty's own output failed; diagnostics: {diagnostics:?}")
        });
    assert_eq!(output, "OK", "{tag}");
}

/// The write side, pinned directly: krusty's DEFAULT emit puts a `@kotlin.Metadata` on the class file
/// (not only on the file facade), and it names the class's Kotlin members. Without this the reader has
/// nothing to read, whatever the reader does.
#[test]
fn a_class_carries_its_own_metadata_by_default() {
    let Some((_, classes)) = krusty_lib_dir("write", "data class Point(val x: Int, val y: Int)\n")
    else {
        eprintln!("skip (kotlin stdlib / JDK unavailable)");
        return;
    };
    let (_, bytes) = classes
        .iter()
        .find(|(name, _)| name == "Point")
        .expect("krusty emits Point.class");
    let meta = parse_class(bytes).expect("Point.class parses").meta;
    let functions: Vec<&str> = meta
        .class_functions
        .iter()
        .map(|f| f.kotlin_name.as_str())
        .collect();
    assert_eq!(
        functions,
        [
            "component1",
            "component2",
            "copy",
            "equals",
            "hashCode",
            "toString"
        ],
        "the class's own @Metadata names its synthesized data-class members",
    );
    let properties: Vec<&str> = meta
        .class_properties
        .iter()
        .map(|p| p.name.as_str())
        .collect();
    assert_eq!(properties, ["x", "y"], "and its constructor properties");
}

/// A value-class-typed BODY property is physically exposed through a MANGLED accessor whose descriptor
/// returns the value class's erased underlying. Its accessor is synthesized straight from the property
/// declaration (never an `IrClass::methods` entry), so the record has to take the exact JVM spelling
/// the value-class pass stamped on the `IrProperty` — describing a plain `getK` advertises a method the
/// class file does not define, and a reader binding it gets a NoSuchMethodError.
#[test]
fn value_class_body_property_round_trips() {
    let source = "@JvmInline value class K(val v: String)\n\
                  class Holder { val k: K = K(\"OK\") }\n";
    let Some((_, classes)) = krusty_lib_dir("vc_body_property", source) else {
        eprintln!("skip (kotlin stdlib / JDK unavailable)");
        return;
    };
    let (_, bytes) = classes
        .iter()
        .find(|(name, _)| name == "Holder")
        .expect("krusty emits Holder.class");
    let info = parse_class(bytes).expect("Holder.class parses");
    let mangled = info
        .methods
        .iter()
        .find(|method| method.name.starts_with("getK-"))
        .expect("the value-class pass must realize the body property with its mangled getter")
        .name
        .clone();
    let described = info
        .meta
        .class_properties
        .iter()
        .find(|p| p.name == "k")
        .expect("the record describes the property");
    assert_eq!(
        described.getter.as_ref().map(|g| g.name.as_str()),
        Some(mangled.as_str()),
        "the record must name the accessor the class file defines, not the plain convention",
    );
    expect_roundtrip_ok(
        "vc_body_property",
        source,
        "fun box(): String = Holder().k.v\n",
    );
}

/// The end-to-end gap this closes: `copy(y = 4)` needs `copy`'s PARAMETER NAMES and `val (a, b) = q`
/// needs `component1`/`component2`'s `operator` marks. Both methods were always emitted into the class
/// file; only the `@Metadata` describing them was missing, so a second krusty compilation reported
/// "named arguments are only supported for top-level functions…" and "no operator 'component1'".
#[test]
fn data_class_copy_and_destructuring_round_trip() {
    const LIB: &str = "data class Point(val x: Int, val y: Int)\n";
    const MAIN: &str = "fun box(): String {\n\
    val p = Point(3, -4)\n\
    val q = p.copy(y = 4)\n\
    if (q.x != 3 || q.y != 4) return \"f1\"\n\
    val (a, b) = q\n\
    if (a != 3 || b != 4) return \"f2\"\n\
    if (p == q) return \"f3\"\n\
    if (p != Point(3, -4)) return \"f4\"\n\
    if (q.toString() != \"Point(x=3, y=4)\") return \"f5\"\n\
    return \"OK\"\n\
}\n";
    expect_roundtrip_ok("data", LIB, MAIN);
}

/// A class whose declared member's signature mentions a VALUE class round-trips. The record describes
/// the member in KOTLIN terms (`make(): K`) with a `JvmMethodSignature` carrying the mangled name and
/// the erased descriptor, and the reader binds that: the physical call already leaves the underlying
/// carrier on the stack, so the result needs neither `checkcast K` nor `unbox-impl`. This is the shape
/// that used to be withheld — describing it before the read side existed produced exactly that boxed
/// sequence and a ClassCastException.
#[test]
fn a_value_class_returning_member_round_trips() {
    const LIB: &str = "@JvmInline\nvalue class K(val v: String)\n\
class Holder {\n\
    fun make(): K = K(\"OK\")\n\
    fun echo(k: K): String = k.v\n\
}\n";
    const MAIN: &str = "fun box(): String {\n\
    val h = Holder()\n\
    if (h.echo(h.make()) != \"OK\") return \"f1\"\n\
    return h.make().v\n\
}\n";
    let Some((_, classes)) = krusty_lib_dir("valueclass", LIB) else {
        eprintln!("skip (kotlin stdlib / JDK unavailable)");
        return;
    };
    let (_, bytes) = classes
        .iter()
        .find(|(name, _)| name == "Holder")
        .expect("krusty emits Holder.class");
    let info = parse_class(bytes).expect("Holder.class parses");
    let described = info
        .meta
        .class_functions
        .iter()
        .find(|f| f.kotlin_name == "make")
        .expect("the record names the member in Kotlin terms");
    assert_eq!(
        described.jvm_name.as_str(),
        info.methods
            .iter()
            .find(|m| m.name.starts_with("make-"))
            .expect("the class file defines the mangled method")
            .name,
        "the record's JVM name must be the method the class file actually defines",
    );
    expect_roundtrip_ok("valueclass", LIB, MAIN);
}

/// Admission is TRANSITIVE, and the value class need not be in the same FILE. A value class with a
/// declared member carries no record of its own, so a downstream compilation reads it as an ordinary
/// class — it casts the carrier to the box and binds an instance accessor where kotlinc emits the
/// static `-impl`, a ClassCastException. Whether a sibling file's value class ends up described is
/// decided by that file's own emit, so a record here cannot assume it is: `Holder.make(): A` is
/// withheld. (A CLASSPATH value class is different — being known as one at all means its `@Metadata`
/// inline record was read.)
#[test]
fn a_sibling_files_undescribed_value_class_withholds_the_record() {
    let stdlib = common::stdlib_jar();
    let jdk = common::jdk_modules();
    let sources = [
        (
            "A.kt",
            "@JvmInline value class A(val v: String) { fun shout(): String = v + \"!\" }\n",
        ),
        ("B.kt", "class Holder { fun make(): A = A(\"O\") }\n"),
    ];
    let Some(classes) = common::compile_in_process_files(&sources, &[stdlib], Some(&jdk)) else {
        eprintln!("skip (kotlin stdlib / JDK unavailable)");
        return;
    };
    let (_, holder) = classes
        .iter()
        .find(|(name, _)| name == "Holder")
        .expect("krusty emits Holder.class");
    let (_, value_class) = classes
        .iter()
        .find(|(name, _)| name == "A")
        .expect("krusty emits A.class");
    let carries_metadata = |bytes: &[u8]| bytes.windows(17).any(|w| w == b"Lkotlin/Metadata;");
    assert!(
        !carries_metadata(value_class),
        "precondition: a value class with a declared member is itself withheld",
    );
    assert!(
        !carries_metadata(holder),
        "so the class whose member RETURNS it must be withheld too",
    );
}

/// `data` synthesizes over the PRIMARY-CONSTRUCTOR properties only. `c.fields` also holds a BODY
/// property's backing field, so counting all of them described a `component2` and a `copy(II)` that
/// the class file does not define — real kotlinc reading that record accepts `val (a, b) = p` and
/// binds a method that is not there. Pins the record against kotlinc's own for the same source.
#[test]
fn a_body_property_adds_no_component_or_copy_parameter() {
    let Some((_, classes)) =
        krusty_lib_dir("bodyprop", "data class P(val x: Int) { val y: Int = 1 }\n")
    else {
        eprintln!("skip (kotlin stdlib / JDK unavailable)");
        return;
    };
    let (_, bytes) = classes
        .iter()
        .find(|(name, _)| name == "P")
        .expect("krusty emits P.class");
    let meta = parse_class(bytes).expect("P.class parses").meta;
    let functions: Vec<&str> = meta
        .class_functions
        .iter()
        .map(|f| f.kotlin_name.as_str())
        .collect();
    assert_eq!(
        functions,
        ["component1", "copy", "equals", "hashCode", "toString"],
        "only the primary-ctor property gets a componentN",
    );
    let copy_params = meta
        .class_functions
        .iter()
        .find(|f| f.kotlin_name == "copy")
        .map(|f| f.value_params.len());
    assert_eq!(copy_params, Some(1), "copy takes only the ctor property");
}

/// A VALUE-CLASS-typed constructor parameter gives the class kotlinc's private-primary +
/// `DefaultConstructorMarker` ABI, which the record cannot describe: krusty named the PRIVATE
/// `<init>(Ljava/lang/String;)V`, typed `id` as `String` rather than `ItemId`, and dropped the
/// getter's mangled name. `ir.has_value_param_ctor` is the signal (recorded before erasure).
#[test]
fn a_value_class_constructor_parameter_withholds_the_record() {
    let Some((_, classes)) = krusty_lib_dir(
        "vcctor",
        "@JvmInline\nvalue class ItemId(val v: String)\nclass Holder(val id: ItemId)\n",
    ) else {
        eprintln!("skip (kotlin stdlib / JDK unavailable)");
        return;
    };
    let (_, bytes) = classes
        .iter()
        .find(|(name, _)| name == "Holder")
        .expect("krusty emits Holder.class");
    assert!(
        !bytes
            .windows(b"Lkotlin/Metadata;".len())
            .any(|w| w == b"Lkotlin/Metadata;"),
        "a value-class ctor parameter means the class carries NO @Metadata at all",
    );
}

/// A plain class's DECLARED members also round-trip: a named argument on a member function needs that
/// member's parameter names, which only the class `@Metadata` carries (the JVM descriptor does not).
#[test]
fn plain_class_member_named_arguments_round_trip() {
    const LIB: &str = "class Calc(val base: Int) {\n\
    fun blend(lo: Int, hi: Int): Int = base + lo * 10 + hi\n\
}\n";
    const MAIN: &str = "fun box(): String {\n\
    val c = Calc(1)\n\
    if (c.blend(hi = 3, lo = 2) != 24) return \"f1\"\n\
    return \"OK\"\n\
}\n";
    expect_roundtrip_ok("member", LIB, MAIN);
}
