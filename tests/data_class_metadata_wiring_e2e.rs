//! Wiring guard: a `data class`'s IR → `build_class_metadata` produces a `@kotlin.Metadata` whose
//! decoded shape (IS_DATA class flag + the synthesized `componentN`/`copy`/`equals`/`hashCode`/
//! `toString` functions) matches kotlinc. The raw d1 bytes for this exact shape are pinned by
//! `metadata::class_builder::data_class_metadata_byte_matches_kotlinc`; this pins that the IR path
//! actually feeds `build_class` the inputs that produce them.
use super::common;
use krusty::jvm::classreader::parse_class;

#[test]
fn data_class_metadata_wired_from_ir() {
    let src = "package demo\ndata class Point(val x: Int, var y: String)\n";
    let classes = common::compile_in_process_with_class_metadata(src, "Point")
        .expect("krusty compiles the data class");
    let (_, bytes) = classes
        .iter()
        .find(|(n, _)| n == "demo/Point")
        .expect("emitted demo/Point");
    let info = parse_class(bytes).expect("parses back");
    let fns: Vec<&str> = info
        .meta
        .class_functions
        .iter()
        .map(|f| f.kotlin_name.as_str())
        .collect();
    assert_eq!(
        fns,
        vec![
            "component1",
            "component2",
            "copy",
            "equals",
            "hashCode",
            "toString"
        ],
        "data-class synthesized functions, in kotlinc declaration order",
    );
}

/// A plain property class (metadata on) emits kotlinc-style debug tables for its synthesized members:
/// a `LineNumberTable` mapping to the class declaration line (line 2 here) and a `LocalVariableTable`
/// naming `this`. For `class C(val x: Int)` the emitted class is byte-identical to kotlinc in size and
/// pool count — only constant-pool interning order still differs (see project memory).
#[test]
fn plain_class_emits_synth_debug_tables() {
    let src = "package demo\nclass C(val x: Int)\n";
    let classes =
        common::compile_in_process_with_class_metadata(src, "C").expect("krusty compiles class C");
    let (_, bytes) = classes
        .iter()
        .find(|(n, _)| n == "demo/C")
        .expect("emitted demo/C");
    let contains = |needle: &[u8]| bytes.windows(needle.len()).any(|w| w == needle);
    assert!(contains(b"LineNumberTable"), "emits a LineNumberTable");
    assert!(
        contains(b"LocalVariableTable"),
        "emits a LocalVariableTable"
    );
    assert!(contains(b"this"), "LocalVariableTable names `this`");
    // The decl-line plumbing must have produced a real (non-zero) line for the synthesized members.
    let info = parse_class(bytes).expect("parses back");
    assert!(info.meta.class_functions.is_empty(), "no member functions");
}

/// A plain class with a `var` property and a wide (2-slot) type exercises the setter accessor path and
/// the `Long`/`Double` slot-width branch of the debug-table + pool-seeding code — shapes the `val`/`Int`
/// and (gated-out) data-class cases don't reach. Verifies it compiles and emits a setter's tables.
#[test]
fn var_and_wide_slot_class_emits_debug_tables() {
    let src = "package demo\nclass C(var a: Long, val b: Int)\n";
    let classes =
        common::compile_in_process_with_class_metadata(src, "C").expect("krusty compiles class C");
    let (_, bytes) = classes
        .iter()
        .find(|(n, _)| n == "demo/C")
        .expect("emitted demo/C");
    let info = parse_class(bytes).expect("parses back");
    // A `var` property means a setter method is present alongside the getters.
    assert!(
        info.methods.iter().any(|m| m.name == "setA"),
        "var property emits a setter (setA)",
    );
    let contains = |needle: &[u8]| bytes.windows(needle.len()).any(|w| w == needle);
    assert!(
        contains(b"LocalVariableTable"),
        "emits a LocalVariableTable"
    );
    assert!(contains(b"LineNumberTable"), "emits a LineNumberTable");
}

/// Compile `src` (a single `demo.C` class) with both kotlinc and the krusty CLI (class metadata on) and
/// assert `demo/C.class` is byte-for-byte identical. Skips when the provisioned toolchain is absent.
fn assert_class_c_byte_identical(src_text: &str) {
    use std::process::Command;
    if common::java_home().is_none() {
        eprintln!("skip (JAVA_HOME unavailable)");
        return;
    }
    let dir = std::env::temp_dir().join(format!(
        "krusty_byteid_{}_{}",
        std::process::id(),
        src_text.len()
    ));
    let src_dir = dir.join("src");
    let kref = dir.join("kref");
    let krout = dir.join("krout");
    for d in [&src_dir, &kref, &krout] {
        std::fs::create_dir_all(d).unwrap();
    }
    let src = src_dir.join("C.kt");
    std::fs::write(&src, src_text).unwrap();

    let args = vec![
        src.to_string_lossy().into_owned(),
        "-d".to_string(),
        kref.to_string_lossy().into_owned(),
    ];
    let Some((code, stderr)) = common::kotlinc_compile(&args) else {
        eprintln!("skip (provisioned kotlinc unavailable)");
        let _ = std::fs::remove_dir_all(&dir);
        return;
    };
    assert_eq!(code, 0, "kotlinc failed: {stderr}");

    let out = Command::new(env!("CARGO_BIN_EXE_krusty"))
        .env("KRUSTY_EMIT_CLASS_METADATA", "1")
        .args(["-d", krout.to_str().unwrap()])
        .arg(&src)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "krusty failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let ko = std::fs::read(kref.join("demo/C.class")).unwrap();
    let kr = std::fs::read(krout.join("demo/C.class")).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(
        kr,
        ko,
        "{src_text:?} must be byte-for-byte identical to kotlinc (len kr={} ko={})",
        kr.len(),
        ko.len()
    );
}

/// A plain class with a non-null reference property (metadata on) emits kotlinc's `@NotNull` on the
/// backing field, the getter return, and the setter parameter — exercising the nullability-annotation
/// path (`set_field_nullability`/`set_method_nullability`) in-process (the byte-identity test drives the
/// CLI subprocess, which coverage instrumentation doesn't see).
#[test]
fn reference_property_class_emits_notnull_in_process() {
    let src = "package demo\nclass C(val x: Int, var y: String)\n";
    let classes =
        common::compile_in_process_with_class_metadata(src, "C").expect("krusty compiles class C");
    let (_, bytes) = classes
        .iter()
        .find(|(n, _)| n == "demo/C")
        .expect("emitted demo/C");
    let contains = |needle: &[u8]| bytes.windows(needle.len()).any(|w| w == needle);
    assert!(
        contains(b"Lorg/jetbrains/annotations/NotNull;"),
        "emits @NotNull for the non-null reference property",
    );
    assert!(
        contains(b"RuntimeInvisibleParameterAnnotations"),
        "emits parameter annotations for the reference ctor/setter params",
    );
    assert!(contains(b"<set-?>"), "setter guards its value parameter");
}

/// The end-to-end goal, pinned: a plain single-`val` property class compiled by krusty (metadata on) is
/// BYTE-FOR-BYTE identical to kotlinc — @Metadata, debug tables, and constant-pool interning order.
#[test]
fn plain_class_is_byte_identical_to_kotlinc() {
    assert_class_c_byte_identical("package demo\nclass C(val x: Int)\n");
}

/// A `val` + `var` class with a reference-typed property is also byte-identical — exercises the
/// `@NotNull` field/getter/setter/parameter annotations, the setter's `checkNotNullParameter("<set-?>")`
/// guard, the post-prologue LineNumberTable offsets, and the larger constant-pool interning order.
#[test]
fn var_reference_class_is_byte_identical_to_kotlinc() {
    assert_class_c_byte_identical("package demo\nclass C(val x: Int, var y: String)\n");
}
