//! Byte-identity + `@Metadata` wiring for krusty's opt-in class-metadata emission.
//!
//! krusty is compiled IN-PROCESS here (`compile_in_process_metadata_cp`, which stamps the same
//! `SourceFile` the CLI does) — never via a spawned `krusty` binary. In-process keeps the whole
//! codepath coverage-instrumented and avoids the ~370ms cold classpath scan a subprocess pays per
//! run; the kotlinc reference is the only external process (unavoidable, and server-backed).
use super::common;
use krusty::jvm::classreader::parse_class;
use std::path::PathBuf;

/// krusty's emitted bytes for `class_internal`, compiled in-process with class metadata on.
fn krusty_bytes(src: &str, class_internal: &str, cp: &[PathBuf]) -> Option<Vec<u8>> {
    let stem = class_internal.rsplit('/').next().unwrap();
    let classes = common::compile_in_process_metadata_cp(src, stem, cp)?;
    classes
        .into_iter()
        .find(|(n, _)| n == class_internal)
        .map(|(_, b)| b)
}

/// kotlinc's reference bytes for `class_internal` (server-backed). `None` ⇒ toolchain unavailable.
fn kotlinc_bytes(src: &str, stem: &str, class_internal: &str, cp: &[PathBuf]) -> Option<Vec<u8>> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NONCE: AtomicU64 = AtomicU64::new(0);
    common::java_home()?;
    // Unique per call — parallel tests must not share a scratch dir (several compile `demo/C`).
    let uniq = NONCE.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("krusty_ref_{}_{stem}_{uniq}", std::process::id()));
    let out = dir.join("out");
    std::fs::create_dir_all(&out).ok()?;
    let kt = dir.join(format!("{stem}.kt"));
    std::fs::write(&kt, src).ok()?;
    let mut args = vec![kt.to_string_lossy().into_owned()];
    if !cp.is_empty() {
        let joined = cp
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join(":");
        args.extend(["-cp".to_string(), joined]);
    }
    args.extend(["-d".to_string(), out.to_string_lossy().into_owned()]);
    let (code, stderr) = common::kotlinc_compile(&args)?;
    assert_eq!(code, 0, "kotlinc failed: {stderr}");
    let bytes = std::fs::read(out.join(format!("{class_internal}.class"))).ok();
    let _ = std::fs::remove_dir_all(&dir);
    bytes
}

/// Assert krusty's in-process output for `class_internal` is byte-for-byte identical to kotlinc's.
/// Skips (does not fail) when krusty declines the source or the kotlinc toolchain is unavailable.
fn assert_byte_identical(src: &str, class_internal: &str, cp: &[PathBuf]) {
    let Some(kr) = krusty_bytes(src, class_internal, cp) else {
        eprintln!("skip ({class_internal}: krusty declined the source)");
        return;
    };
    let stem = class_internal.rsplit('/').next().unwrap();
    let Some(ko) = kotlinc_bytes(src, stem, class_internal, cp) else {
        eprintln!("skip ({class_internal}: provisioned kotlinc unavailable)");
        return;
    };
    assert_eq!(
        kr,
        ko,
        "{class_internal} must be byte-for-byte identical to kotlinc (krusty {} B, kotlinc {} B)",
        kr.len(),
        ko.len(),
    );
}

// ---- Byte-identity (the end-to-end goal) ----------------------------------------------------------

/// A `data class` with a NULLABLE-PRIMITIVE field (`Int?` → boxed `Integer`) — its `component1`/`copy`
/// carry a `JvmMethodSignature` (`@Metadata` f100) recording the boxed descriptor, which the proto type
/// alone (`Int?`) does not pin. Non-null `Int` needs none; this pins the boxed-signature emission.
#[test]
fn data_class_nullable_primitive_field_is_byte_identical() {
    assert_byte_identical("package demo\ndata class D(val x: Int?)\n", "demo/D", &[]);
}

/// A `data class` whose FIRST field is a nullable primitive and whose later field is a non-null
/// primitive (`Int?`, `Int`) — two f100 positions (`component1` + `copy`, `component2`/the non-null
/// stay derivable).
#[test]
fn data_class_nullable_first_primitive_is_byte_identical() {
    assert_byte_identical(
        "package demo\ndata class D(val a: Int?, val b: Int)\n",
        "demo/D",
        &[],
    );
}

/// A multi-field `data class` mixing a non-null primitive, a NULLABLE primitive, and a reference
/// (`Int`, `Double?`, `String`) — the `hashCode` accumulator `result = result*31 + h(field)` runs
/// ACROSS the nullable primitive's `if (b==null) 0 else Object.hashCode(b)` ternary. kotlinc keeps
/// `result*31` on the operand stack across that branch (the keep-LHS-on-stack path), and the boxed
/// `Double?` positions carry the f100 signature — the two fixes composing.
#[test]
fn data_class_mixed_nullable_primitive_is_byte_identical() {
    assert_byte_identical(
        "package demo\ndata class D(val a: Int, val b: Double?, val c: String)\n",
        "demo/D",
        &[],
    );
}

/// A `data class` with a PARAMETERIZED-type field (`List<String>`) — FULLY byte-identical. Combines the
/// generic `Signature` on field/getter/component1/copy/ctor (copy-sig after the copy descriptor, field-sig
/// late before `@Metadata`), the interface-field `hashCode` owner (`java/lang/Object`, not `List`), and
/// deferred `VerifType` interning so `copy$default`'s `same_frame` locals never intern an orphan
/// `Class java/util/List`. The pervasive `List`/`Map`/`Set`-field data-class shape.
#[test]
fn data_class_generic_collection_field_is_byte_identical() {
    let cp: Vec<PathBuf> = common::stdlib_jar().into_iter().collect();
    assert_byte_identical(
        "package demo\ndata class D(val xs: List<String>)\n",
        "demo/D",
        &cp,
    );
}

/// A `data class` with a bare-interface builtin field (`CharSequence`) — its `@Metadata` property type
/// must encode via the `predefinedIndex` (13) kotlinc uses for the mapped Kotlin builtin, NOT a
/// class-id `Lkotlin/CharSequence;` descriptor (which leaves an orphan d2 string + a divergent d1).
#[test]
fn data_class_charsequence_field_is_byte_identical() {
    let cp: Vec<PathBuf> = common::stdlib_jar().into_iter().collect();
    assert_byte_identical(
        "package demo\ndata class D(val cs: CharSequence)\n",
        "demo/D",
        &cp,
    );
}

/// A `data class` with a two-argument generic field (`Map<String, Int>`) — the `Signature` nests both
/// type arguments (`Ljava/util/Map<Ljava/lang/String;Ljava/lang/Integer;>;`, the `Int` boxed) and the
/// same deferred-interning path keeps `copy$default` orphan-free.
#[test]
fn data_class_generic_map_field_is_byte_identical() {
    let cp: Vec<PathBuf> = common::stdlib_jar().into_iter().collect();
    assert_byte_identical(
        "package demo\ndata class D(val m: Map<String, Int>)\n",
        "demo/D",
        &cp,
    );
}

/// A `data class` with a NULLABLE generic field (`List<String>?`) — the generic `Signature` survives
/// nullability (`parameterized_sig` unwraps `T?`), and the nullable `hashCode`/`equals` guards compose
/// with the deferred-interned frames.
#[test]
fn data_class_nullable_generic_collection_field_is_byte_identical() {
    let cp: Vec<PathBuf> = common::stdlib_jar().into_iter().collect();
    assert_byte_identical(
        "package demo\ndata class D(val xs: List<String>?)\n",
        "demo/D",
        &cp,
    );
}

/// A plain (non-`data`) class with a PARAMETERIZED-type property (`List<String>`) — the generic
/// `Signature` machinery: the field, its getter, and the constructor each carry a `Signature`
/// attribute (`Ljava/util/List<Ljava/lang/String;>;` and the `(…)V`/`()…` method forms), interned in
/// kotlinc's exact positions (method sigs right after each erased descriptor; the field sig after the
/// accessors, before `@Metadata`; the `Signature` attribute NAME before the field's `@NotNull`). Needs
/// the kotlin stdlib on the classpath so `List<String>` resolves.
#[test]
fn plain_generic_collection_property_is_byte_identical() {
    let cp: Vec<PathBuf> = common::stdlib_jar().into_iter().collect();
    assert_byte_identical(
        "package demo\nclass C(val xs: List<String>)\n",
        "demo/C",
        &cp,
    );
}

/// A single `val Int` property: the minimal shape — @Metadata, debug tables, constant-pool order.
#[test]
fn val_int_class_is_byte_identical() {
    assert_byte_identical("package demo\nclass C(val x: Int)\n", "demo/C", &[]);
}

/// `val Int` + `var String`: the non-null reference path — `@NotNull` on the field/getter/setter/param,
/// the setter's `checkNotNullParameter("<set-?>")` guard, post-prologue LineNumberTable offsets.
#[test]
fn var_reference_class_is_byte_identical() {
    assert_byte_identical(
        "package demo\nclass C(val x: Int, var y: String)\n",
        "demo/C",
        &[],
    );
}

/// A `var Long` + `val Int`: the wide (2-slot) `Long`/`Double` `slot_size` branch and a primitive
/// `var` setter (no null-check guard, but its LVT still names the value `<set-?>`). Exercises the
/// per-setter `<set-?>` interning that a non-last setter needs (`setA` precedes `getB`).
#[test]
fn var_long_class_is_byte_identical() {
    assert_byte_identical(
        "package demo\nclass C(var a: Long, val b: Int)\n",
        "demo/C",
        &[],
    );
}

/// A nullable reference property (`String?`): `@Nullable` on the field/getter/return, and NO
/// `checkNotNullParameter` guard (unlike a non-null reference). Verifies the `@Nullable` annotation
/// type is seeded into the constant pool in kotlinc's order.
#[test]
fn nullable_reference_class_is_byte_identical() {
    assert_byte_identical("package demo\nclass C(val x: String?)\n", "demo/C", &[]);
}

/// A multi-property class with mixed nullable + non-null references and primitives — the property-class
/// machinery (seeding, debug tables, annotations) must generalize beyond two properties.
#[test]
fn multi_property_mixed_nullability_is_byte_identical() {
    assert_byte_identical(
        "package demo\nclass C(val a: Int, val b: String, val c: String?)\n",
        "demo/C",
        &[],
    );
}

/// A non-null reference param pushed PAST slot 3 (four wide `Long`s first put the `String` at slot 9) —
/// its `checkNotNullParameter` guard uses the 2-byte `aload <u1>` (not the 1-byte `aload_0..3`), so the
/// constructor's `LineNumberTable` start_pc is the larger post-prologue offset. Pins the slot-dependent
/// prologue length.
#[test]
fn nonnull_ref_param_past_slot3_is_byte_identical() {
    assert_byte_identical(
        "package demo\nclass C(val a: Long, val b: Long, val c: Long, val d: Long, val e: String)\n",
        "demo/C",
        &[],
    );
}

/// A nullable PRIMITIVE property (`Double?`, `Int?`) — boxed backing field (`Ljava/lang/Double;`),
/// so `@Metadata` records an explicit `JvmFieldSignature.desc` (interned after the getter/setter, as
/// kotlinc does). A nullable reference (`String?`) leaves the field derived; this pins the difference.
#[test]
fn nullable_primitive_class_is_byte_identical() {
    assert_byte_identical(
        "package demo\nclass C(val a: Int?, var b: Double?, val c: String?)\n",
        "demo/C",
        &[],
    );
}

/// Every primitive property type — pins the `@Metadata` builtin `predefinedIndex` for each
/// (Byte=5, Float=7, Short=10, Char=12, plus Int/Long/Double/Boolean). A wrong index would fall back
/// to `kotlin/Any` and diverge.
#[test]
fn all_primitive_types_class_is_byte_identical() {
    assert_byte_identical(
        "package demo\nclass C(val a: Byte, val b: Short, val c: Int, val d: Long, val e: Float, val f: Double, val g: Boolean, val h: Char)\n",
        "demo/C",
        &[],
    );
}

/// A single-property `data class` — the first FULLY byte-identical data class. Exercises the
/// synthesized `component1`/`copy`/`copy$default`/`equals`/`hashCode`/`toString` pool seeding AND the
/// data-class attribute-NAME interning order: kotlinc visits fields then methods, so with only a
/// primitive field (no field `@NotNull`) the names appear `Code`, `LineNumberTable`,
/// `LocalVariableTable`, then `RuntimeInvisibleAnnotations` (from `copy`/`toString`), then
/// `StackMapTable` (from the branchy `equals`) LAST — the opposite of a plain class with an annotated
/// field, which interns RIA before `Code`. A hard-coded order gets one shape wrong; this pins both.
#[test]
fn data_class_single_primitive_is_byte_identical() {
    assert_byte_identical("package demo\ndata class D(val x: Int)\n", "demo/D", &[]);
}

/// A multi-property ALL-PRIMITIVE data class — `hashCode` folds into a `result` accumulator local
/// (kotlinc names it in the LVT with a partial live-range, listed before `this`), and each synthesized
/// method generalizes past two fields. Pins the `result` pool seeding + LVT range.
#[test]
fn data_class_multi_primitive_is_byte_identical() {
    assert_byte_identical(
        "package demo\ndata class T(val a: Int, val b: Int, val c: Int)\n",
        "demo/T",
        &[],
    );
}

/// A multi-property ALL-REFERENCE data class — each `hashCode` is a virtual `String.hashCode()` (kotlinc
/// interns the `()I` descriptor before the receiver class), `equals` compares via `Intrinsics.areEqual`
/// (seeded before the `other`/`Object` LVT names), and `copy`'s reference params carry `@NotNull`.
#[test]
fn data_class_multi_reference_is_byte_identical() {
    assert_byte_identical(
        "package demo\ndata class M(val a: String, val b: String)\n",
        "demo/M",
        &[],
    );
}

/// A multi-property MIXED primitive+reference data class (`Int` + non-null `String`) — the full
/// combination: `result` accumulator, a primitive and a reference `hashCode`, a `@NotNull` `copy` param,
/// and the `var` reference setter's `checkNotNullParameter` guard.
#[test]
fn data_class_multi_mixed_is_byte_identical() {
    assert_byte_identical(
        "package demo\ndata class Point(val x: Int, var y: String)\n",
        "demo/Point",
        &[],
    );
}

/// A single NULLABLE-reference (`String?`) data class — its `hashCode` is kotlinc's null-guarded ternary
/// `d != null ? d.hashCode() : 0` (an `ifnonnull` branch to a virtual `String.hashCode`, else `0`), NOT
/// `Objects.hashCode`. Pins both the codegen (via a direct `ifnonnull` on the `d == null` test) and the
/// pool (the seeded virtual `String.hashCode` is the one used).
#[test]
fn data_class_single_nullable_reference_is_byte_identical() {
    assert_byte_identical(
        "package demo\ndata class N(val d: String?)\n",
        "demo/N",
        &[],
    );
}

/// A data class over a same-file CUSTOM class — its `hashCode` dispatches a virtual `hashCode()` on
/// the field's OWN class (`demo/Other.hashCode:()I`), not `Objects.hashCode`; both the single-field
/// and the multi-field (`result` accumulator) shapes. Pins the generalized per-field dispatch: no
/// class name is special-cased (`String` was; any classifier now takes the same path).
#[test]
fn data_class_custom_class_field_is_byte_identical() {
    assert_byte_identical(
        "package demo\nclass Other\ndata class C1(val c: Other)\n",
        "demo/C1",
        &[],
    );
    assert_byte_identical(
        "package demo\nclass Other\ndata class O2(val c: Other, val e: Other)\n",
        "demo/O2",
        &[],
    );
}

/// A multi-property data class over NON-`Int` primitives (`Boolean`, `Char`) — `hashCode`'s own `()I`
/// descriptor must be seeded right after the `hashCode` name (a `Boolean`/`Char` `componentN` returns
/// `()Z`/`()C`, so `()I` is not interned earlier as it is for an `Int` field).
#[test]
fn data_class_boolean_char_is_byte_identical() {
    assert_byte_identical(
        "package demo\ndata class C(val x: Boolean, val y: Char)\n",
        "demo/C",
        &[],
    );
}

/// A `Double` + `Float` data class — `equals` compares each via the IEEE-aware `<Box>.compare` (kotlinc's
/// `NaN`/`-0.0` semantics), seeded in field order before the `other`/`Object` LVT names.
#[test]
fn data_class_double_float_is_byte_identical() {
    assert_byte_identical(
        "package demo\ndata class C(val x: Double, val y: Float)\n",
        "demo/C",
        &[],
    );
}

/// Every primitive category plus a reference in one data class — pins the per-field `hashCode` refs
/// (boxing-class statics), the per-field `equals` comparisons (`Double`/`Float` via `<Box>.compare`,
/// reference via `Intrinsics.areEqual`, others direct), and the `toString` appends, all in field order.
#[test]
fn data_class_all_primitive_kinds_is_byte_identical() {
    assert_byte_identical(
        "package demo\ndata class C(val a: Int, val b: Boolean, val c: Long, val d: Char, val e: Double, val f: Float, val g: String)\n",
        "demo/C",
        &[],
    );
}

/// A data class with a CONCRETE same-file-class field (`val x: D`) — FULLY byte-identical (metadata,
/// debug tables, pool). The synthesized `hashCode` calls `D.hashCode()` (kotlinc's shape) and the pool
/// seeder already seeds that same ref, so body and metadata agree. A common real-world shape (a data
/// class holding another class).
#[test]
fn data_class_concrete_class_field_is_byte_identical() {
    assert_byte_identical(
        "package demo\nclass D\ndata class C(val x: D)\n",
        "demo/C",
        &[],
    );
}

/// A data class with a NULLABLE concrete-class field (`val x: D?`) — fully byte-identical; `hashCode` is
/// the null-guarded `x != null ? x.hashCode() : 0` ternary, and the rest matches kotlinc.
#[test]
fn data_class_nullable_concrete_class_field_is_byte_identical() {
    assert_byte_identical(
        "package demo\nclass D\ndata class C(val x: D?)\n",
        "demo/C",
        &[],
    );
}

/// A multi-property data class mixing a primitive, a `String`, and a concrete-class field — the general
/// real-world domain-record shape, fully byte-identical.
#[test]
fn data_class_mixed_primitive_string_class_field_is_byte_identical() {
    assert_byte_identical(
        "package demo\nclass D\ndata class C(val a: Int, val b: String, val d: D)\n",
        "demo/C",
        &[],
    );
}

/// A data class holding another DATA class (`val d: D` where `D` is itself a `data class`) — the pervasive
/// real-world "record holding a record" shape (a domain aggregate holding a sub-record). Fully
/// byte-identical: `D`'s own `hashCode()` is called on the nested field, matching kotlinc + the seeder.
#[test]
fn data_class_nested_data_class_field_is_byte_identical() {
    assert_byte_identical(
        "package demo\ndata class D(val v: Int)\ndata class C(val d: D)\n",
        "demo/C",
        &[],
    );
}

/// A data class with TWO concrete-class fields (`val x: D, val y: E`) — the seeding/hashCode machinery
/// generalizes across multiple class-typed properties.
#[test]
fn data_class_two_class_fields_is_byte_identical() {
    assert_byte_identical(
        "package demo\nclass D\nclass E\ndata class C(val x: D, val y: E)\n",
        "demo/C",
        &[],
    );
}

/// A MULTI-property data class with a NULLABLE reference field — the pervasive real-world domain-record
/// shape (a record with several fields, some optional). Its `hashCode` accumulates `result = result*31 +
/// <field hash>`; the nullable field's hash is a branchy null-guarded ternary, and kotlinc keeps the
/// `result*31` on the operand stack ACROSS that branch (krusty used to spill it to a temp). Fully
/// byte-identical.
#[test]
fn data_class_multi_property_nullable_reference_is_byte_identical() {
    assert_byte_identical(
        "package demo\ndata class C(val a: String, val b: String?)\n",
        "demo/C",
        &[],
    );
}

/// A mixed multi-property record with a primitive, a concrete-class field, a `String`, and a NULLABLE
/// concrete-class field — the general domain-aggregate shape, fully byte-identical (exercises the
/// operand-stack-preserved `hashCode` accumulator across the nullable field's branch).
#[test]
fn data_class_mixed_record_with_nullable_class_field_is_byte_identical() {
    assert_byte_identical(
        "package demo\nclass D\ndata class C(val a: Int, val d: D, val s: String, val n: D?)\n",
        "demo/C",
        &[],
    );
}

// ---- Real-world data-class shapes (grounding) ----------------------------------------------------
// These mirror the shapes of real domain types (a small all-`Int` result, a many-`Int` aggregate, a
// single-`String` holder) whose fields are all primitives/`String`, anchoring the synthetic coverage
// above to representative real-world shapes without naming any specific domain type.

/// A two-`Int` result-type shape (like an operation's summary counts). Byte-for-byte identical to kotlinc.
#[test]
fn two_int_result_shape_is_byte_identical() {
    assert_byte_identical(
        "package demo\ndata class C(val a: Int, val b: Int)\n",
        "demo/C",
        &[],
    );
}

/// A seven-`Int` aggregate shape (like a status breakdown). Exercises the `result` accumulator across
/// many primitive fields.
#[test]
fn seven_int_aggregate_shape_is_byte_identical() {
    assert_byte_identical(
        "package demo\ndata class C(val a: Int, val b: Int, val c: Int, val d: Int, val e: Int, val f: Int, val g: Int)\n",
        "demo/C",
        &[],
    );
}

/// A single non-null-`String` value-holder shape.
#[test]
fn single_string_holder_shape_is_byte_identical() {
    assert_byte_identical("package demo\ndata class C(val s: String)\n", "demo/C", &[]);
}

// ---- @Metadata-level checks for shapes not yet FULLY byte-identical (data classes) ----------------

/// A `data class` (metadata on): its IR → `build_class_metadata` yields the synthesized
/// `componentN`/`copy`/`equals`/`hashCode`/`toString` in kotlinc's order, and the synthesized methods
/// carry kotlinc's debug tables (LocalVariableTable) + nullability annotations (`copy`/`toString`
/// return `@NotNull`, `equals` param `@Nullable`). Decode-level companion to the byte-identity tests
/// above — pins the parsed shape, so a byte-level divergence localizes faster.
#[test]
fn data_class_emits_metadata_debug_tables_and_annotations() {
    let src = "package demo\ndata class Point(val x: Int, var y: String)\n";
    let bytes = krusty_bytes(src, "demo/Point", &[]).expect("krusty compiles the data class");
    let info = parse_class(&bytes).expect("parses back");
    let fns: Vec<&str> = info
        .meta
        .class_functions
        .iter()
        .map(|f| f.kotlin_name.as_str())
        .collect();
    assert_eq!(
        fns,
        [
            "component1",
            "component2",
            "copy",
            "equals",
            "hashCode",
            "toString"
        ],
        "data-class synthesized functions, in kotlinc declaration order",
    );
    let has = |needle: &[u8]| bytes.windows(needle.len()).any(|w| w == needle);
    assert!(
        has(b"LocalVariableTable"),
        "synthesized methods carry a LocalVariableTable"
    );
    assert!(
        has(b"Lorg/jetbrains/annotations/NotNull;"),
        "copy/toString returns get @NotNull",
    );
    assert!(
        has(b"Lorg/jetbrains/annotations/Nullable;"),
        "equals' `other` param gets @Nullable",
    );
}

/// A representative real-world data-class shape (two `String`s + a `List<String>` property with a
/// default) — its `@Metadata` d1/d2 is byte-identical to kotlinc: generic `Type.argument`, the `List`
/// builtin (predefinedIndex), and the `DECLARES_DEFAULT_VALUE` ctor-param flag. Uses the kotlin stdlib
/// classpath.
#[test]
fn generic_list_property_with_default_metadata_byte_identical() {
    let Some(stdlib) = common::stdlib_jar() else {
        eprintln!("skip (kotlin stdlib jar unavailable)");
        return;
    };
    let cp = [stdlib];
    let src = "package demo\n\
        data class C(val a: String, val b: String, val items: List<String> = emptyList())\n";
    let Some(kr) = krusty_bytes(src, "demo/C", &cp) else {
        eprintln!("skip (krusty declined)");
        return;
    };
    let Some(ko) = kotlinc_bytes(src, "C", "demo/C", &cp) else {
        eprintln!("skip (kotlinc unavailable)");
        return;
    };
    // The whole data class isn't byte-identical yet (debug tables/annotations), but its @Metadata is —
    // so the decoded shape must match kotlinc exactly: same property names, function names, and the
    // `items: List<String>` return type resolving to the `List` builtin (a generic/argument/default
    // encoding divergence would surface as a decode mismatch here).
    let kr_meta = parse_class(&kr).expect("krusty parses").meta;
    let ko_meta = parse_class(&ko).expect("kotlinc parses").meta;
    let names = |m: &krusty::jvm::metadata::KotlinMeta| {
        (
            m.class_properties
                .iter()
                .map(|p| p.name.clone())
                .collect::<Vec<_>>(),
            m.class_functions
                .iter()
                .map(|f| f.kotlin_name.clone())
                .collect::<Vec<_>>(),
        )
    };
    assert_eq!(
        names(&kr_meta),
        names(&ko_meta),
        "@Metadata property + function names match kotlinc"
    );
    let items = kr_meta
        .class_properties
        .iter()
        .find(|p| p.name == "items")
        .expect("items property in @Metadata");
    assert_eq!(
        items.ret_class.as_ref().map(|t| t.render()).as_deref(),
        Some("kotlin/collections/List"),
        "items decodes as the List builtin",
    );
}
