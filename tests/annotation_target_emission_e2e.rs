//! The meta-annotations kotlinc stamps on an `annotation class` that declares `@Target(...)`.
//!
//! kotlinc writes them in this order, all into `RuntimeVisibleAnnotations`:
//!   1. every annotation written in the source, in SOURCE order — including `kotlin.annotation.Target`
//!      itself, which survives into the classfile as `allowedTargets = [...]`;
//!   2. `java.lang.annotation.Retention(...)` — the JVM retention mirror;
//!   3. `java.lang.annotation.Target(value = [...])` — the JVM target mirror.
//!
//! The Java mirror is a PROJECTION, not a copy: each `AnnotationTarget` maps to at most one
//! `ElementType` (`CLASS → TYPE`, `VALUE_PARAMETER → PARAMETER`, `FUNCTION`/`PROPERTY_GETTER`/
//! `PROPERTY_SETTER` → `METHOD`, …), and the Kotlin-only targets (`PROPERTY`, `FILE`, `TYPEALIAS`,
//! `EXPRESSION`) map to nothing. The projected set behaves like an `EnumSet<ElementType>`: duplicates
//! collapse and the entries come out in `ElementType` DECLARATION order rather than the order they
//! were written in Kotlin. The projection may be empty: a set of only Kotlin-only targets still gets a
//! `java.lang.annotation.Target(value = [])`. Only the ABSENCE of `@Target` in the source omits the
//! two target meta-annotations. Every expectation here was measured against kotlinc 2.4.10.
//!
//! These cases compare the whole `RuntimeVisibleAnnotations` attribute against kotlinc's, minus the
//! `kotlin.Metadata` entry: krusty does not describe an `annotation class` in `@Metadata` at all
//! (`class_metadata_common_shape_admitted` in `src/jvm/ir_emit.rs` bails on `is_annotation`), so
//! whole-class byte identity is blocked on that separate gap, with or without a declared `@Target`.
use super::common;
use std::path::PathBuf;

/// krusty's emitted bytes for `class_internal`, compiled in-process with class metadata on.
///
/// A `None` from `compile_in_process_metadata_cp` conflates "toolchain unavailable" with "krusty
/// REJECTED the source", so it is not a skip signal: the caller gates on the toolchain first and
/// this panics with the front-end diagnostics instead of reporting a declined source as a pass.
fn krusty_bytes(src: &str, class_internal: &str, cp: &[PathBuf]) -> Vec<u8> {
    let stem = class_internal.rsplit('/').next().unwrap();
    let classes = common::compile_in_process_metadata_cp(src, stem, cp).unwrap_or_else(|| {
        let diagnostics = common::front_end_diagnostics(src, cp, None);
        panic!("{class_internal}: krusty declined the source; diagnostics: {diagnostics:?}")
    });
    classes
        .into_iter()
        .find(|(n, _)| n == class_internal)
        .map(|(_, b)| b)
        .unwrap_or_else(|| panic!("{class_internal} was not emitted"))
}

/// kotlinc's reference bytes for `class_internal` (server-backed). `None` ⇒ toolchain unavailable.
/// kotlinc puts the stdlib on its own classpath, so nothing is passed here.
fn kotlinc_bytes(src: &str, class_internal: &str) -> Option<Vec<u8>> {
    let stem = class_internal.rsplit('/').next().unwrap();
    common::java_home();
    let dir = common::scratch_dir()?;
    let out = dir.join("out");
    std::fs::create_dir_all(&out).ok()?;
    let kt = dir.join(format!("{stem}.kt"));
    std::fs::write(&kt, src).ok()?;
    let args = vec![
        kt.to_string_lossy().into_owned(),
        "-d".to_string(),
        out.to_string_lossy().into_owned(),
    ];
    let (code, stderr) = common::kotlinc_compile(&args)?;
    assert_eq!(code, 0, "kotlinc failed: {stderr}");
    let bytes = std::fs::read(out.join(format!("{class_internal}.class"))).ok();
    let _ = std::fs::remove_dir_all(&dir);
    bytes
}

/// The class-level `RuntimeVisibleAnnotations` attribute of `bytes`, rendered by `javap -v` and
/// normalized: constant-pool index lines are dropped (the two compilers number their pools
/// differently) and the trailing `kotlin.Metadata` entry is cut off (krusty emits none for an
/// annotation class — a separate gap this suite deliberately does not measure).
fn runtime_visible_annotations(bytes: &[u8], stem: &str, tag: &str) -> String {
    let dir = common::scratch_dir().expect("scratch dir");
    let class_file = dir.join(format!("{stem}-{tag}.class"));
    std::fs::write(&class_file, bytes).expect("write class file");
    let text = common::javap(&["-v", "-p", &class_file.to_string_lossy()])
        .expect("pooled JavaRunner unavailable");
    let (_, section) = text
        .rsplit_once("RuntimeVisibleAnnotations:")
        .unwrap_or_else(|| panic!("{tag}: no RuntimeVisibleAnnotations attribute in:\n{text}"));
    let mut out = String::new();
    for line in section.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("kotlin.Metadata(") {
            break;
        }
        // `  0: #7(#8=[e#9.#10])` — the raw constant-pool form of the entry javap renders next.
        if trimmed.is_empty()
            || trimmed
                .split_once(": #")
                .is_some_and(|(n, _)| n.parse::<u32>().is_ok())
        {
            continue;
        }
        out.push_str(trimmed);
        out.push('\n');
    }
    out
}

/// Assert krusty's `RuntimeVisibleAnnotations` for `class_internal` matches kotlinc's, entry for
/// entry and in order. Skips only when the reference toolchain is unavailable; a source krusty
/// declines FAILS.
fn assert_same_meta_annotations(src: &str, class_internal: &str) {
    let stem = class_internal.rsplit('/').next().unwrap();
    let Some(ko) = kotlinc_bytes(src, class_internal) else {
        eprintln!("skip ({class_internal}: provisioned kotlinc unavailable)");
        return;
    };
    // `AnnotationTarget` is a stdlib classifier; kotlinc resolves it from its own default classpath.
    let kr = krusty_bytes(src, class_internal, &[common::stdlib_jar()]);
    assert_eq!(
        runtime_visible_annotations(&kr, stem, "krusty"),
        runtime_visible_annotations(&ko, stem, "kotlinc"),
        "{class_internal}: meta-annotations must match kotlinc for source:\n{src}",
    );
}

/// The baseline: a `@Target`-less annotation class carries only `@java.lang.annotation.Retention`.
/// Pins that adding the target meta-annotations does not disturb the shape that already matched.
#[test]
fn annotation_class_without_target_keeps_only_retention() {
    assert_same_meta_annotations("package p\n\nannotation class Mark\n", "p/Mark");
}

/// One declared target with a Java counterpart: all three meta-annotations, in kotlinc's order.
#[test]
fn one_declared_target_emits_kotlin_and_java_meta_annotations() {
    assert_same_meta_annotations(
        "package p\n\n@Target(AnnotationTarget.FIELD)\nannotation class Mark\n",
        "p/Mark",
    );
}

/// `CLASS` maps to `ElementType.TYPE` — the mapping is a rename, not an identity function.
#[test]
fn class_target_maps_to_element_type_type() {
    assert_same_meta_annotations(
        "package p\n\n@Target(AnnotationTarget.CLASS)\nannotation class Mark\n",
        "p/Mark",
    );
}

/// `PROPERTY` is Kotlin-only: the Kotlin meta-annotation records it, and the Java mirror is still
/// emitted — with an EMPTY array, since the target projects to no `ElementType`. Omitting the mirror
/// (the shape a first reading of `javap` suggests) is a different classfile.
#[test]
fn kotlin_only_target_still_emits_an_empty_java_mirror() {
    assert_same_meta_annotations(
        "package p\n\n@Target(AnnotationTarget.PROPERTY)\nannotation class Mark\n",
        "p/Mark",
    );
}

/// `FILE` and `TYPEALIAS` are the other Kotlin-only targets; together they still project to nothing,
/// and the mirror is still present and empty.
#[test]
fn only_kotlin_only_targets_still_emit_an_empty_java_mirror() {
    assert_same_meta_annotations(
        "package p\n\n@Target(AnnotationTarget.FILE, AnnotationTarget.TYPEALIAS)\nannotation class Mark\n",
        "p/Mark",
    );
}

/// A mixed set drops the Kotlin-only entry from the Java mirror while keeping it in the Kotlin one.
#[test]
fn mixed_target_set_projects_only_mappable_entries() {
    assert_same_meta_annotations(
        "package p\n\n@Target(AnnotationTarget.PROPERTY, AnnotationTarget.FIELD)\nannotation class Mark\n",
        "p/Mark",
    );
}

/// `FUNCTION`, `PROPERTY_GETTER` and `PROPERTY_SETTER` all project to `ElementType.METHOD`; the
/// mirror carries it ONCE.
#[test]
fn targets_sharing_an_element_type_collapse_to_one_entry() {
    assert_same_meta_annotations(
        "package p\n\n@Target(AnnotationTarget.FUNCTION, AnnotationTarget.PROPERTY_GETTER, AnnotationTarget.PROPERTY_SETTER)\nannotation class Mark\n",
        "p/Mark",
    );
}

/// The Java mirror is ordered by `ElementType` declaration order, not by the order the Kotlin targets
/// were written: `TYPE, CLASS, VALUE_PARAMETER, ANNOTATION_CLASS` comes back out as
/// `TYPE, PARAMETER, ANNOTATION_TYPE, TYPE_USE`.
#[test]
fn java_mirror_is_ordered_by_element_type_not_source_order() {
    assert_same_meta_annotations(
        "package p\n\n@Target(AnnotationTarget.TYPE, AnnotationTarget.CLASS, AnnotationTarget.VALUE_PARAMETER, AnnotationTarget.ANNOTATION_CLASS)\nannotation class Mark\n",
        "p/Mark",
    );
}

/// Every remaining `AnnotationTarget` with a Java counterpart, one class per target, so a wrong row in
/// the mapping table cannot hide behind the targets already covered above. (`EXPRESSION` is excluded:
/// kotlinc rejects it at any retention other than `SOURCE`.)
#[test]
fn every_mappable_target_matches_kotlinc() {
    for target in [
        "ANNOTATION_CLASS",
        "TYPE_PARAMETER",
        "LOCAL_VARIABLE",
        "VALUE_PARAMETER",
        "CONSTRUCTOR",
        "FUNCTION",
        "PROPERTY_GETTER",
        "PROPERTY_SETTER",
        "TYPE",
    ] {
        assert_same_meta_annotations(
            &format!("package p\n\n@Target(AnnotationTarget.{target})\nannotation class Mark\n"),
            "p/Mark",
        );
    }
}

/// An explicitly EMPTY target set is not the same as an absent `@Target`: kotlinc emits BOTH
/// meta-annotations with empty arrays.
#[test]
fn empty_target_set_emits_empty_arrays() {
    assert_same_meta_annotations("package p\n\n@Target()\nannotation class Mark\n", "p/Mark");
}

/// `@Target` keeps its SOURCE position among the class's other annotations — it is not hoisted to the
/// front or pushed behind them; only the two Java mirrors are appended after the written ones.
#[test]
fn target_keeps_its_source_position_among_other_annotations() {
    assert_same_meta_annotations(
        "package p\n\n@Target(AnnotationTarget.CLASS)\nannotation class Marker\n\n@Marker\n@Target(AnnotationTarget.FIELD)\nannotation class Mark\n",
        "p/Mark",
    );
}

/// An EXPLICIT `@Retention` written before `@Target` keeps that order: kotlinc emits
/// `kotlin.annotation.Retention` then `kotlin.annotation.Target`, then the two Java mirrors. This is
/// the case a naive "user annotations first, then the retention stamps" ordering gets backwards, since
/// `kotlin.annotation.Retention` is SYNTHESIZED from the class's retention rather than carried through
/// as a written annotation — so its source position has to be honoured deliberately.
#[test]
fn explicit_retention_before_target_keeps_source_order() {
    assert_same_meta_annotations(
        "package p\n\n@Retention(AnnotationRetention.BINARY)\n@Target(AnnotationTarget.FIELD)\nannotation class Mark\n",
        "p/Mark",
    );
}

/// The same two written the other way round. Together with the case above this pins that the order is
/// the SOURCE's, not a fixed one that happens to match in the common spelling.
#[test]
fn explicit_target_before_retention_keeps_source_order() {
    assert_same_meta_annotations(
        "package p\n\n@Target(AnnotationTarget.FIELD)\n@Retention(AnnotationRetention.BINARY)\nannotation class Mark\n",
        "p/Mark",
    );
}
