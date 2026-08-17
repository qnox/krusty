//! `@JvmField` on a class property — the annotation that REPLACES the accessor pair with a plain
//! field.
//!
//! Measured against kotlinc 2.4.10, `@JvmField val ys: Array<String>` in `class H`:
//!
//! * the backing field carries the PROPERTY's declared visibility (`public`, or `protected` for a
//!   `protected` declaration) instead of Kotlin's default `private`,
//! * NO `getX()`/`setX()` method is emitted at all, and
//! * `@Metadata` records the property with an EMPTY `JvmPropertySignature.field` and no
//!   `JvmPropertySignature.getter`/`setter`.
//!
//! krusty synthesized the accessor pair from the backing field for every non-private property, so it
//! emitted a `getYs()` the annotation forbids, kept the field private, and advertised that getter in
//! the metadata — a consumer compiled against the record would call a method the class file does not
//! declare.

use super::common;
use std::fs;

/// Assert one same-module fixture's `@Metadata` is byte-identical to kotlinc's for `class_internal`.
///
/// The stdlib is on the classpath because kotlinc's always is — and here it also supplies
/// `kotlin.jvm.JvmField` itself, whose resolved identity is what the emitter keys on.
fn assert_identical(stem: &str, src: &str, class_internal: &str) {
    let classpath = [common::stdlib_jar()];
    let Some(result) =
        common::metadata_diff_against_kotlinc_cp(stem, src, class_internal, &classpath)
    else {
        eprintln!("skip ({stem}: provisioned kotlinc unavailable)");
        return;
    };
    result.unwrap_or_else(|diff| panic!("{diff}"));
}

/// The smallest shape: a `val` and a `var`, both `@JvmField`, read from a member function. Neither
/// records an accessor, and the `var` records no setter either.
#[test]
fn a_jvm_field_property_records_no_accessor() {
    const SRC: &str = "package app\n\
        \n\
        class S {\n\
        \x20   @JvmField val a: String = \"x\"\n\
        \x20   @JvmField var b: Int = 1\n\
        \x20   fun n(): Int = a.length + b\n\
        }\n";
    assert_identical("Sfield", SRC, "app/S");
}

/// A primary-constructor `val`/`var` is a property too, and its annotation reaches the FIELD site
/// through the constructor-parameter branch of use-site defaulting (`@JvmField` targets `FIELD`
/// only, so it is not a parameter annotation). Alongside them: a nullable property, and the two
/// non-public visibilities `@JvmField` admits — `protected`, which stays `protected` on the field,
/// and `internal`, which is public on the JVM.
#[test]
fn constructor_nullable_and_non_public_jvm_fields_record_no_accessor() {
    assert_identical("Gfield", MIXED_SRC, "app/G");
}

const MIXED_SRC: &str = "package app\n\
    \n\
    class G(@JvmField val p: String, @JvmField var q: Int) {\n\
    \x20   @JvmField val n: String? = null\n\
    \x20   @JvmField protected val pr: Int = 2\n\
    \x20   @JvmField internal val it2: Long = 3L\n\
    \x20   fun use(): Int = p.length + q + pr + it2.toInt() + (n?.length ?: 0)\n\
    }\n";

/// The metadata comparison above cannot see the CLASS FILE: it would still pass while krusty emitted
/// the forbidden accessor beside the record, and while the field stayed private (which makes every
/// cross-class read an `IllegalAccessError`). Assert the emitted members directly, against kotlinc's
/// own output for the same source.
#[test]
fn a_jvm_field_property_emits_a_visible_field_and_no_accessor() {
    let classes = common::expect_classes_with_stdlib(MIXED_SRC, "Gemit");
    let (_, bytes) = classes
        .iter()
        .find(|(name, _)| name == "app/G")
        .expect("krusty emitted app/G");
    let dir = common::scratch_dir().expect("scratch dir");
    let emitted = dir.join("G.class");
    fs::write(&emitted, bytes).expect("write krusty class");
    let krusty =
        common::javap(&["-p", &emitted.to_string_lossy()]).expect("pooled javap available");

    let reference = dir.join("kotlinc");
    fs::create_dir_all(&reference).expect("create kotlinc output");
    let source = dir.join("Gemit.kt");
    fs::write(&source, MIXED_SRC).expect("write kotlinc source");
    let args = vec![
        "-d".to_string(),
        reference.to_string_lossy().into_owned(),
        source.to_string_lossy().into_owned(),
    ];
    let Some((code, stderr)) = common::kotlinc_compile(&args) else {
        eprintln!("skip (provisioned kotlinc unavailable)");
        return;
    };
    assert_eq!(code, 0, "kotlinc rejected the reference source: {stderr}");
    let kotlinc = common::javap(&["-p", &reference.join("app/G.class").to_string_lossy()])
        .expect("pooled javap available");

    assert_eq!(
        members(&krusty),
        members(&kotlinc),
        "krusty:\n{krusty}\nkotlinc:\n{kotlinc}"
    );
}

/// The declared members javap prints for a class, in order, normalized to one line each. Enough to
/// compare a field's access flags and an accessor's presence; the `Compiled from` header and the
/// class header itself are dropped.
fn members(javap: &str) -> Vec<String> {
    javap
        .lines()
        .map(str::trim)
        .filter(|line| line.ends_with(';') && !line.contains("class "))
        .map(str::to_string)
        .collect()
}

/// The realization has to RUN: with no accessor to call, every read and write — from inside the
/// declaring class, from a sibling class in the same compilation, and from another file — must be a
/// field access. A stale `invokevirtual getP()` links against nothing and is a `NoSuchMethodError`.
#[test]
fn jvm_field_properties_are_read_and_written_as_fields() {
    const SRC: &str = "class Holder(@JvmField val p: String) {\n\
        \x20   @JvmField var q: Int = 1\n\
        \x20   fun inside(): String = p + q\n\
        }\n\
        \n\
        class Peer {\n\
        \x20   fun outside(h: Holder): String {\n\
        \x20       h.q = 7\n\
        \x20       return h.p + h.q\n\
        \x20   }\n\
        }\n\
        \n\
        fun box(): String {\n\
        \x20   val h = Holder(\"a\")\n\
        \x20   if (h.inside() != \"a1\") return \"fail inside: \" + h.inside()\n\
        \x20   val outside = Peer().outside(h)\n\
        \x20   return if (outside == \"a7\" && h.q == 7) \"OK\" else \"fail outside: \" + outside\n\
        }\n";
    common::expect_box_ok_with_stdlib(SRC, "JvmFieldRun");
}

/// The same reads across a FILE boundary, where the consumer sees the declaration through the
/// module's symbols rather than its own IR.
#[test]
fn a_cross_file_jvm_field_property_is_read_as_a_field() {
    const LIB: &str = "package lib\n\
        \n\
        class Holder(@JvmField val p: String) {\n\
        \x20   @JvmField var q: Int = 1\n\
        }\n";
    const USE: &str = "import lib.Holder\n\
        \n\
        fun box(): String {\n\
        \x20   val h = Holder(\"a\")\n\
        \x20   h.q = 7\n\
        \x20   return if (h.p == \"a\" && h.q == 7) \"OK\" else \"fail\"\n\
        }\n";
    common::expect_box_ok_files_with_stdlib(
        &[("Lib.kt", LIB), ("Use.kt", USE)],
        "cross-file @JvmField property access",
    );
}

/// The originally reported shape, `@JvmField val ys: Array<String>`, asserted on the two facts the
/// annotation owns: the field is `public final` and NO accessor is emitted beside it.
///
/// It is deliberately not a byte-identity row. An `Array<String>` PROPERTY still differs from
/// kotlinc in two annotation-independent ways — `@Metadata` omits the `JvmFieldSignature.desc`
/// (`[Ljava/lang/String;`) kotlinc records for an array-typed backing field, and the emitted field
/// carries a `Signature` attribute (`Lkotlin/Array<Ljava/lang/String;>;`) kotlinc does not write at
/// all. Both reproduce with the annotation removed, so a whole-payload comparison here would measure
/// the array gaps rather than `@JvmField`.
#[test]
fn an_array_typed_jvm_field_emits_a_public_field_and_no_accessor() {
    const SRC: &str = "package app\n\
        \n\
        class H {\n\
        \x20   @JvmField val ys: Array<String> = arrayOf()\n\
        \x20   fun n(): Int = ys.size\n\
        }\n";
    let classes = common::expect_classes_with_stdlib(SRC, "Harray");
    let (_, bytes) = classes
        .iter()
        .find(|(name, _)| name == "app/H")
        .expect("krusty emitted app/H");
    let dir = common::scratch_dir().expect("scratch dir");
    let emitted = dir.join("H.class");
    fs::write(&emitted, bytes).expect("write krusty class");
    let dumped =
        common::javap(&["-p", &emitted.to_string_lossy()]).expect("pooled javap available");

    let field = members(&dumped)
        .into_iter()
        .find(|member| member.ends_with(" ys;"))
        .unwrap_or_else(|| panic!("no `ys` field emitted:\n{dumped}"));
    assert!(
        field.starts_with("public final "),
        "a @JvmField backing field is public: {field}\n{dumped}"
    );
    assert!(
        !dumped.contains("getYs"),
        "@JvmField suppresses the accessor:\n{dumped}"
    );
}

/// The control: the SAME shapes WITHOUT the annotation keep their accessors and their private
/// fields. Removing the getter must be keyed on the annotation, not on the property being a plain
/// backing field.
#[test]
fn a_plain_property_still_records_its_accessors() {
    const SRC: &str = "package app\n\
        \n\
        class P {\n\
        \x20   val a: String = \"x\"\n\
        \x20   var b: Int = 1\n\
        \x20   fun n(): Int = a.length + b\n\
        }\n";
    assert_identical("Pplain", SRC, "app/P");
}
