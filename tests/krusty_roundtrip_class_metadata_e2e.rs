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

/// A value-class-typed BODY property is DESCRIBED, and the record is byte-identical to kotlinc's:
/// `d2=[…,"k","LK;","getK-XLNMDGE","()Ljava/lang/String;","Ljava/lang/String;"]` — the Kotlin type
/// `LK;` (not the erased field type), the MANGLED accessor the class file actually defines, and an
/// explicit field descriptor a reader cannot derive from `K`. Its accessor is synthesized straight
/// from the property declaration and never appears in `IrClass::methods`, so the record has to take
/// the exact JVM spelling the value-class pass stamped on the `IrProperty`; the plain `getK` krusty
/// used to write advertised a method that does not exist.
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

#[test]
fn value_class_computed_property_round_trips_as_a_static_carrier_accessor() {
    let source = "@JvmInline value class Numbers(val values: IntArray) {\n\
                  \x20   val size: Int get() = values.size\n\
                  }\n";
    let Some((_, classes)) = krusty_lib_dir("vc_computed_property", source) else {
        eprintln!("skip (kotlin stdlib / JDK unavailable)");
        return;
    };
    let (_, bytes) = classes
        .iter()
        .find(|(name, _)| name == "Numbers")
        .expect("krusty emits Numbers.class");
    let info = parse_class(bytes).expect("Numbers.class parses");
    let accessor = info
        .methods
        .iter()
        .find(|method| method.name == "getSize-impl")
        .expect("computed value-class property is a static carrier accessor");
    assert_eq!(accessor.descriptor, "([I)I");
    let property = info
        .meta
        .class_properties
        .iter()
        .find(|property| property.name == "size")
        .expect("metadata describes the computed property");
    assert_eq!(
        property.getter.as_ref().map(|getter| getter.name.as_str()),
        Some("getSize-impl")
    );
    assert!(
        info.fields.iter().all(|field| field.name != "size"),
        "computed property has no backing field"
    );
    expect_roundtrip_ok(
        "vc_computed_property",
        source,
        "fun box(): String = if (Numbers(intArrayOf(3, 4)).size == 2) \"OK\" else \"wrong\"\n",
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

/// The classpath value-class RETURN, end to end. A member whose Kotlin return is a value class is
/// realized as a MANGLED method over the ERASED underlying (`make-<hash>()Ljava/lang/String;`), so a
/// caller that learns the Kotlin return `K` from `@Metadata` must NOT also emit kotlinc's boxed
/// sequence (`checkcast K; K.unbox-impl()`) — the `String` on the stack already IS the carrier, and
/// casting it is a ClassCastException. This runs `box()`, so a caller that merely COMPILES while
/// emitting the boxed form still fails here rather than in the box corpus.
#[test]
fn a_value_class_returning_member_round_trips() {
    const LIB: &str = "@JvmInline\nvalue class K(val v: String)\n\
class Holder {\n\
    fun make(): K = K(\"OK\")\n\
    fun echo(k: K): String = k.v\n\
}\n";
    // The RETURN feeding a value-class PARAMETER in one expression: both sides must agree that the
    // carrier, not a box, is what crosses the call boundary.
    const MAIN: &str = "fun box(): String {\n\
    val h = Holder()\n\
    if (h.echo(h.make()) != \"OK\") return \"f1\"\n\
    return h.make().v\n\
}\n";
    // The record must be PRESENT — the fix is the read half, so a regression that silently reinstates
    // the decline would otherwise pass this test by rejecting nothing and running nothing.
    let Some((_, classes)) = krusty_lib_dir("valueclass", LIB) else {
        eprintln!("skip (kotlin stdlib / JDK unavailable)");
        return;
    };
    let (_, bytes) = classes
        .iter()
        .find(|(name, _)| name == "Holder")
        .expect("krusty emits Holder.class");
    let meta = parse_class(bytes).expect("Holder.class parses").meta;
    assert!(
        meta.class_functions.iter().any(|f| f.kotlin_name == "make"),
        "the class's own @Metadata must describe the value-class-returning member",
    );
    expect_roundtrip_ok("valueclass", LIB, MAIN);
}

/// Admission is TRANSITIVE, and the value class need not be in the same FILE. A value class without a
/// record of its own reads downstream as an ordinary class — the caller casts the carrier to the box
/// and binds an instance accessor where kotlinc emits the static `-impl`, a ClassCastException.
/// Whether a sibling file's value class ends up described is decided by that file's own emit, so a
/// record here cannot assume it is: `Holder.make(): A` is withheld. (A CLASSPATH value class is
/// different — being known as one at all means its `@Metadata` inline record was read.)
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

/// A declared value-class return and a generic-slot read can have the SAME substituted Kotlin type
/// while requiring opposite representations. The declared member returns the erased carrier, whereas
/// `List<T>.get` returns a real box from its `Object` slot. Pin both in one consumer: this prevents the
/// checker-to-IR `declared_ret` handoff from being replaced by a broad "logical type is a value class"
/// rule that would make either the direct call over-unbox or the generic read skip its required unbox.
#[test]
fn declared_value_class_return_does_not_reclassify_generic_slot() {
    const LIB: &str = "@JvmInline\nvalue class Token(val value: String)\n\
object Factory {\n\
    fun direct(): Token = Token(\"OK\")\n\
}\n";
    const MAIN: &str = "fun box(): String {\n\
    val direct: Token = Factory.direct()\n\
    val values: List<Token> = listOf(direct)\n\
    val fromSlot: Token = values[0]\n\
    return if (direct.value == \"OK\" && fromSlot.value == \"OK\") \"OK\" else \"FAIL\"\n\
}\n";
    expect_roundtrip_ok("vc_generic_slot", LIB, MAIN);
}

/// The same read half for a member whose value class rides in a PARAMETER: the JVM method takes the
/// erased underlying, so the argument must be handed over unboxed.
#[test]
fn a_value_class_parameter_member_round_trips() {
    const LIB: &str = "@JvmInline\nvalue class K(val v: String)\n\
class Holder {\n\
    fun take(k: K): String = k.v\n\
}\n";
    const MAIN: &str = "fun box(): String = Holder().take(K(\"OK\"))\n";
    expect_roundtrip_ok("vcparam", LIB, MAIN);
}

/// A CONCRETE `suspend` member returning a value class over a REFERENCE underlying still withholds,
/// and unlike the shapes above the reason is in the BYTECODE, not the record. kotlinc boxes a
/// value-class CPS return only when the underlying is a PRIMITIVE; over a reference/nullable/generic
/// underlying it `areturn`s the raw carrier. krusty boxes unconditionally, so its `constructor-impl;
/// box-impl; areturn` disagrees with kotlinc's `constructor-impl; areturn` — while the record krusty
/// would write is byte-identical to kotlinc's. Describing it therefore advertises an ABI the class
/// file does not implement: a consumer doing `C().gk().v` gets "class K cannot be cast to class
/// java.lang.String". An ABSTRACT suspend member has no return expression to box and stays
/// describable, which is what `data_class_metadata_wiring_e2e`'s suspend interface cases pin.
#[test]
fn a_concrete_suspend_value_class_return_withholds_the_record() {
    const LIB: &str = "@JvmInline\nvalue class K(val v: String)\n\
class C {\n\
    suspend fun gk(): K = K(\"OK\")\n\
}\n";
    let Some((_, classes)) = krusty_lib_dir("suspendvcret", LIB) else {
        eprintln!("skip (kotlin stdlib / JDK unavailable)");
        return;
    };
    let (_, bytes) = classes
        .iter()
        .find(|(name, _)| name == "C")
        .expect("krusty emits C.class");
    assert!(
        !bytes
            .windows(b"Lkotlin/Metadata;".len())
            .any(|w| w == b"Lkotlin/Metadata;"),
        "a boxed value-class CPS return means the class carries NO @Metadata until the boxing matches",
    );
}

/// A VALUE class with a DECLARED member still withholds — for a WRITE-side reason the return model
/// does not reach. kotlinc realizes `fun k()` on a value class as the STATIC
/// `k-impl(Ljava/lang/String;)Ljava/lang/String;` over the unboxed carrier; krusty emits an INSTANCE
/// `k()` on the box. A caller reading the record puts the carrier under an `invokevirtual S.k()` —
/// "Type 'java/lang/String' is not assignable to 'S'", a VerifyError. The READ half is fine: against a
/// KOTLINC-built `S` the same `box()` runs, which is what makes this a member-ABI gap and not a
/// metadata one. Asserted on the emitted METHOD so it fails the day the ABI is corrected, prompting
/// the decline (and this test) to be replaced by a round-trip.
#[test]
fn a_value_class_with_a_declared_member_withholds_the_record() {
    const LIB: &str = "@JvmInline\nvalue class S(val v: String) {\n\
    fun k(): String = v + \"K\"\n\
}\n";
    let Some((_, classes)) = krusty_lib_dir("vcdeclared", LIB) else {
        eprintln!("skip (kotlin stdlib / JDK unavailable)");
        return;
    };
    let (_, bytes) = classes
        .iter()
        .find(|(name, _)| name == "S")
        .expect("krusty emits S.class");
    let info = parse_class(bytes).expect("S.class parses");
    assert!(
        info.methods.iter().any(|m| m.name == "k"),
        "pins the divergence being protected: krusty emits an instance `k`, kotlinc a static `k-impl`",
    );
    assert!(
        !bytes
            .windows(b"Lkotlin/Metadata;".len())
            .any(|w| w == b"Lkotlin/Metadata;"),
        "a value class with a declared member carries NO @Metadata until its member ABI matches",
    );
}

/// The FAKE OVERRIDE shape: the value-class-returning member is inherited, so the receiver the caller
/// lands on is the supertype's. Getting the return model right but the receiver wrong is a VerifyError
/// rather than a ClassCastException, which is why this shape is pinned separately.
#[test]
fn an_inherited_value_class_returning_member_round_trips() {
    const LIB: &str = "@JvmInline\nvalue class K(val v: String)\n\
open class A {\n\
    fun make(): K = K(\"OK\")\n\
}\n\
class C : A()\n";
    const MAIN: &str = "fun box(): String = C().make().v\n";
    expect_roundtrip_ok("vcfakeoverride", LIB, MAIN);
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
