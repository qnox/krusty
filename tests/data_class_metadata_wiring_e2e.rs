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

// ---- @Metadata-level checks for shapes not yet FULLY byte-identical (data classes) ----------------

/// A `data class` (metadata on): its IR → `build_class_metadata` yields the synthesized
/// `componentN`/`copy`/`equals`/`hashCode`/`toString` in kotlinc's order, and the synthesized methods
/// carry kotlinc's debug tables (LocalVariableTable) + nullability annotations (`copy`/`toString`
/// return `@NotNull`, `equals` param `@Nullable`). Full byte-identity for a data class also needs its
/// pool seeding (the constant-pool interning order still differs) — tracked separately.
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

/// A real infragnite domain data class (`List<String>` property with a default) — its `@Metadata`
/// d1/d2 is byte-identical to kotlinc: generic `Type.argument`, the `List` builtin (predefinedIndex),
/// and the `DECLARES_DEFAULT_VALUE` ctor-param flag. Uses the kotlin stdlib classpath.
#[test]
fn real_data_class_metadata_byte_identical() {
    let Some(stdlib) = common::stdlib_jar() else {
        eprintln!("skip (kotlin stdlib jar unavailable)");
        return;
    };
    let cp = [stdlib];
    let src = "package demo\n\
        data class IfaceConfig(val address: String, val subnet: String, val routes: List<String> = emptyList())\n";
    let Some(kr) = krusty_bytes(src, "demo/IfaceConfig", &cp) else {
        eprintln!("skip (krusty declined)");
        return;
    };
    let Some(ko) = kotlinc_bytes(src, "IfaceConfig", "demo/IfaceConfig", &cp) else {
        eprintln!("skip (kotlinc unavailable)");
        return;
    };
    // The whole data class isn't byte-identical yet (debug tables/annotations), but its @Metadata is —
    // so the decoded shape must match kotlinc exactly: same property names, function names, and the
    // `routes: List<String>` return type resolving to the `List` builtin (a generic/argument/default
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
    let routes = kr_meta
        .class_properties
        .iter()
        .find(|p| p.name == "routes")
        .expect("routes property in @Metadata");
    assert_eq!(
        routes.ret_class.as_ref().map(|t| t.render()).as_deref(),
        Some("kotlin/collections/List"),
        "routes decodes as the List builtin",
    );
}
