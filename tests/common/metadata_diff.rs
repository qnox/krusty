//! `@kotlin.Metadata` differentials against kotlinc class files.
//!
//! The kotlinc side is a recorded class dump. A miss, a fingerprint change, or `KRUSTY_RECORD=1`
//! compiles with kotlinc; a release or RC compiler then stores the dump. A snapshot, dev, or beta
//! compiler always compiles and never reads or writes one.

use std::collections::BTreeMap;
use std::path::PathBuf;

use super::{
    byte_dump, compile_in_process_metadata_cp, compile_in_process_metadata_cp_module,
    kotlin_metadata, kotlinc_compile, kotlinc_lib_out, raw_kotlin_metadata, scratch_dir,
    stdlib_jar,
};

/// [`metadata_diff_against_kotlinc_cp`] with a DEPENDENCY compiled by the reference kotlinc first.
///
/// The dependency is what makes a classpath `typealias` reachable: a same-file alias is rewritten
/// away at the parse seam, while one declared in a dependency is never rewritten and only name
/// resolution can identify it — two different routes into the same metadata.
#[allow(dead_code)]
pub fn metadata_diff_against_kotlinc_lib(
    name: &str,
    lib: &[(&str, &str)],
    src: &str,
    class: &str,
) -> Option<Result<(), String>> {
    let libout = kotlinc_lib_out(lib)?;
    let inputs = byte_dump::class_dump_inputs(src, "default", &[], std::slice::from_ref(&libout));
    let reference = byte_dump::kotlinc_class_dumps(
        name,
        "default",
        &inputs.variant,
        inputs.fingerprint,
        &[class],
        || {
            let dir = scratch_dir()?;
            let kref = dir.join("ref");
            std::fs::create_dir_all(&kref).ok()?;
            let src_path = dir.join(format!("{name}.kt"));
            std::fs::write(&src_path, src).ok()?;
            let args = vec![
                "-d".to_string(),
                kref.to_string_lossy().into_owned(),
                "-cp".to_string(),
                libout.to_string_lossy().into_owned(),
                src_path.to_string_lossy().into_owned(),
            ];
            let (code, stderr) = kotlinc_compile(&args)?;
            assert_eq!(code, 0, "{name}: kotlinc failed: {stderr}");
            let bytes = std::fs::read(kref.join(format!("{class}.class"))).ok()?;
            let _ = std::fs::remove_dir_all(&dir);
            let mut produced = BTreeMap::new();
            produced.insert(class.to_string(), bytes);
            Some(produced)
        },
    )?
    .pop()?;
    let classpath = [libout, stdlib_jar()];
    let classes = compile_in_process_metadata_cp(src, name, &classpath)
        .unwrap_or_else(|| panic!("{name}: krusty failed to compile"));
    let (_, actual) = classes
        .iter()
        .find(|(emitted, _)| emitted == class)
        .unwrap_or_else(|| panic!("{name}: krusty did not emit {class}"));
    Some(compare_kotlin_metadata(name, class, actual, &reference))
}

/// Compare only the `@kotlin.Metadata` PAYLOAD — `d1` bytes and the `d2` string table — of
/// `class` against kotlinc's, rather than the whole class file.
///
/// This is the right instrument for metadata work. Whole-classfile identity also covers the
/// constant pool, code, and attributes, which diverge for reasons that have nothing to do with the
/// metadata (a generic function's class file differs by hundreds of bytes today while its `d1` is
/// already identical), so a whole-file comparison cannot say whether a metadata change landed.
///
/// `Ok(())` when both sides agree. `None` when the reference toolchain is unavailable; krusty
/// REJECTING the source panics, since a fixture krusty cannot compile is not a skip.
#[allow(dead_code)]
pub fn metadata_diff_against_kotlinc_cp(
    name: &str,
    src: &str,
    class: &str,
    cp_jars: &[PathBuf],
) -> Option<Result<(), String>> {
    let (actual, reference) = compile_class_with_kotlinc_and_krusty(name, src, class, cp_jars)?;
    Some(compare_kotlin_metadata(name, class, &actual, &reference))
}

/// [`metadata_diff_against_kotlinc_cp`] for a class whose `@Metadata` may carry no `d1`, such as a
/// `k=3` synthetic class: every element (`k`, `mv`, `xi`, `d1`, `d2`) must equal kotlinc's.
#[allow(dead_code)]
pub fn metadata_header_diff_against_kotlinc_cp(
    name: &str,
    src: &str,
    class: &str,
    cp_jars: &[PathBuf],
) -> Option<Result<(), String>> {
    metadata_headers_diff_against_kotlinc_cp(name, src, &[class], cp_jars)
}

/// [`metadata_header_diff_against_kotlinc_cp`] for each of `classes`, from one compilation by each
/// compiler: every class whose header differs is reported.
#[allow(dead_code)]
pub fn metadata_headers_diff_against_kotlinc_cp(
    name: &str,
    src: &str,
    classes: &[&str],
    cp_jars: &[PathBuf],
) -> Option<Result<(), String>> {
    let compiled = compile_classes_with_kotlinc_and_krusty(name, src, classes, cp_jars)?;
    let header = |bytes: &[u8]| {
        (
            kotlin_metadata::kotlin_metadata_ints(bytes),
            raw_kotlin_metadata(bytes),
        )
    };
    let differences: Vec<String> = classes
        .iter()
        .zip(&compiled)
        .filter_map(|(class, (actual, reference))| {
            let (expected, emitted) = (header(reference), header(actual));
            assert!(
                expected.0.is_some(),
                "{name}: kotlinc {class} carries no @Metadata"
            );
            (expected != emitted).then(|| {
                format!(
                    "{name}/{class}: @Metadata differs from kotlinc\n  kotlinc: {expected:?}\n  krusty : {emitted:?}"
                )
            })
        })
        .collect();
    Some(match differences.is_empty() {
        true => Ok(()),
        false => Err(differences.join("\n")),
    })
}

/// `class` as kotlinc and krusty compile `src`: `(krusty, kotlinc)` class-file bytes, or `None`
/// when no reference compiler is provisioned.
fn compile_class_with_kotlinc_and_krusty(
    name: &str,
    src: &str,
    class: &str,
    cp_jars: &[PathBuf],
) -> Option<(Vec<u8>, Vec<u8>)> {
    compile_classes_with_kotlinc_and_krusty(name, src, &[class], cp_jars)?.pop()
}

/// Each of `classes` as one kotlinc and one krusty compilation of `src` write it:
/// `(krusty, kotlinc)` class-file bytes, or `None` when no reference compiler is provisioned.
fn compile_classes_with_kotlinc_and_krusty(
    name: &str,
    src: &str,
    classes: &[&str],
    cp_jars: &[PathBuf],
) -> Option<Vec<(Vec<u8>, Vec<u8>)>> {
    let inputs = byte_dump::class_dump_inputs(src, "default", &[], &[]);
    let references = byte_dump::kotlinc_class_dumps(
        name,
        "default",
        &inputs.variant,
        inputs.fingerprint,
        classes,
        || {
            let dir = scratch_dir()?;
            let kref = dir.join("ref");
            std::fs::create_dir_all(&kref).ok()?;
            let src_path = dir.join(format!("{name}.kt"));
            std::fs::write(&src_path, src).ok()?;
            let args = vec![
                "-d".to_string(),
                kref.to_string_lossy().into_owned(),
                src_path.to_string_lossy().into_owned(),
            ];
            let (code, stderr) = kotlinc_compile(&args)?;
            assert_eq!(code, 0, "{name}: kotlinc failed: {stderr}");
            let mut produced = BTreeMap::new();
            for class in classes {
                let bytes =
                    std::fs::read(kref.join(format!("{class}.class"))).unwrap_or_else(|error| {
                        panic!("{name}: kotlinc did not write {class}: {error}")
                    });
                produced.insert((*class).to_string(), bytes);
            }
            let _ = std::fs::remove_dir_all(&dir);
            Some(produced)
        },
    )?;
    let emitted = compile_in_process_metadata_cp(src, name, cp_jars)
        .unwrap_or_else(|| panic!("{name}: krusty failed to compile"));
    Some(
        classes
            .iter()
            .zip(references)
            .map(|(class, reference)| {
                let (_, actual) = emitted
                    .iter()
                    .find(|(emitted, _)| emitted == class)
                    .unwrap_or_else(|| panic!("{name}: krusty did not emit {class}"));
                (actual.clone(), reference)
            })
            .collect(),
    )
}

/// [`metadata_diff_against_kotlinc_cp`] with BOTH sides compiled under an explicit module name
/// (kotlinc `-module-name <name>`). The module-name string is part of the `@Metadata` payload —
/// `classModuleName`/`packageModuleName` (f101) plus its d2 intern position — so fixtures probing
/// it cannot ride the default-module helper, which elides the string entirely.
#[allow(dead_code)]
pub fn metadata_diff_against_kotlinc_module(
    name: &str,
    src: &str,
    class: &str,
    cp_jars: &[PathBuf],
    module_name: &str,
) -> Option<Result<(), String>> {
    let extra = vec!["-module-name".to_string(), module_name.to_string()];
    let inputs = byte_dump::class_dump_inputs(src, "default", &extra, &[]);
    let reference = byte_dump::kotlinc_class_dumps(
        name,
        "default",
        &inputs.variant,
        inputs.fingerprint,
        &[class],
        || {
            let dir = scratch_dir()?;
            let kref = dir.join("ref");
            std::fs::create_dir_all(&kref).ok()?;
            let src_path = dir.join(format!("{name}.kt"));
            std::fs::write(&src_path, src).ok()?;
            let args = vec![
                "-d".to_string(),
                kref.to_string_lossy().into_owned(),
                "-module-name".to_string(),
                module_name.to_string(),
                src_path.to_string_lossy().into_owned(),
            ];
            let (code, stderr) = kotlinc_compile(&args)?;
            assert_eq!(code, 0, "{name}: kotlinc failed: {stderr}");
            let bytes = std::fs::read(kref.join(format!("{class}.class"))).ok()?;
            let _ = std::fs::remove_dir_all(&dir);
            let mut produced = BTreeMap::new();
            produced.insert(class.to_string(), bytes);
            Some(produced)
        },
    )?
    .pop()?;

    let classes = compile_in_process_metadata_cp_module(src, name, cp_jars, module_name)
        .unwrap_or_else(|| panic!("{name}: krusty failed to compile"));
    let (_, actual) = classes
        .iter()
        .find(|(emitted, _)| emitted == class)
        .unwrap_or_else(|| panic!("{name}: krusty did not emit {class}"));
    Some(compare_kotlin_metadata(name, class, actual, &reference))
}

/// Compare two class files' `@Metadata` payloads, reporting the `d2` tables and the `d1` bytes on a
/// mismatch — the two things a metadata change actually moves.
fn compare_kotlin_metadata(
    name: &str,
    class: &str,
    actual: &[u8],
    reference: &[u8],
) -> Result<(), String> {
    let metadata = |bytes: &[u8], side: &str| -> (Vec<u8>, Vec<String>) {
        raw_kotlin_metadata(bytes)
            .unwrap_or_else(|| panic!("{name}: {side} {class} carries no readable @Metadata"))
    };
    let (reference_d1, reference_d2) = metadata(reference, "kotlinc");
    let (actual_d1, actual_d2) = metadata(actual, "krusty");
    if actual_d1 == reference_d1 && actual_d2 == reference_d2 {
        return Ok(());
    }
    let mut report = format!("{name}/{class}: @Metadata differs from kotlinc\n");
    if actual_d2 != reference_d2 {
        report.push_str(&format!(
            "  d2 kotlinc: {reference_d2:?}\n  d2 krusty : {actual_d2:?}\n"
        ));
    }
    if actual_d1 != reference_d1 {
        let at = actual_d1
            .iter()
            .zip(&reference_d1)
            .position(|(left, right)| left != right)
            .unwrap_or_else(|| actual_d1.len().min(reference_d1.len()));
        report.push_str(&format!(
            "  d1 differs at byte {at} (krusty {} B, kotlinc {} B)\n    kotlinc: {}\n    krusty : {}\n",
            actual_d1.len(),
            reference_d1.len(),
            hex(&reference_d1),
            hex(&actual_d1),
        ));
    }
    Err(report)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}
