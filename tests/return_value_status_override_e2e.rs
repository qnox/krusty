//! An override retains the exact return-value-use contract selected from its base declaration.
//!
//! Kotlin metadata encodes a three-state enum in function-flag bits 16..=17. In particular,
//! `ExplicitlyIgnorable` is not merely the absence of `MustUse`, so a boolean copy of bit 16 loses
//! a real declaration state. The fixture uses repository-owned classifier/member names and compares
//! the complete decoded status list with kotlinc.

use krusty::types::ReturnValueStatus;

use super::{common, common_core};

const BASE_SOURCE: &str = "@kotlin.MustUseReturnValues\n\
                          open class ResultContractBase {\n\
                          \x20   open fun requiredValue(): Int = 1\n\
                          \x20   @kotlin.IgnorableReturnValue\n\
                          \x20   open fun discardableValue(): Int = 2\n\
                          }\n";
const LEAF_SOURCE: &str = "class ResultContractLeaf : ResultContractBase() {\n\
                          \x20   override fun requiredValue(): Int = 3\n\
                          \x20   override fun discardableValue(): Int = 4\n\
                          }\n";

fn reference_classes(classpath: &[std::path::PathBuf]) -> (std::path::PathBuf, Vec<u8>) {
    let work = common::scratch_dir()
        .expect("allocate return-value-status scratch directory")
        .join("return-value-status-reference");
    std::fs::create_dir_all(&work).expect("create reference fixture directory");
    let base_source = work.join("ResultContractBase.kt");
    std::fs::write(&base_source, BASE_SOURCE).expect("write reference base fixture");
    let base_output = work.join("base-classes");
    let joined = std::env::join_paths(classpath).expect("join reference classpath");
    let base_arguments = [
        "-Xreturn-value-checker=full".to_string(),
        "-jvm-target".to_string(),
        "25".to_string(),
        "-cp".to_string(),
        joined.to_string_lossy().into_owned(),
        "-d".to_string(),
        base_output.display().to_string(),
        base_source.display().to_string(),
    ];
    let (code, diagnostics) = common::kotlinc_compile(&base_arguments)
        .expect("invoke reference compiler for return-value-status base");
    assert_eq!(code, 0, "kotlinc rejected the base fixture: {diagnostics}");

    let leaf_source = work.join("ResultContractLeaf.kt");
    std::fs::write(&leaf_source, LEAF_SOURCE).expect("write reference leaf fixture");
    let leaf_output = work.join("leaf-classes");
    let mut leaf_classpath = classpath.to_vec();
    leaf_classpath.push(base_output.clone());
    let joined = std::env::join_paths(&leaf_classpath).expect("join leaf reference classpath");
    let leaf_arguments = [
        "-Xreturn-value-checker=full".to_string(),
        "-jvm-target".to_string(),
        "25".to_string(),
        "-cp".to_string(),
        joined.to_string_lossy().into_owned(),
        "-d".to_string(),
        leaf_output.display().to_string(),
        leaf_source.display().to_string(),
    ];
    let (code, diagnostics) = common::kotlinc_compile(&leaf_arguments)
        .expect("invoke reference compiler for return-value-status leaf");
    assert_eq!(code, 0, "kotlinc rejected the leaf fixture: {diagnostics}");
    let leaf = std::fs::read(leaf_output.join("ResultContractLeaf.class"))
        .expect("kotlinc emitted the overriding classifier");
    (base_output, leaf)
}

fn statuses(bytes: &[u8], owner: &str) -> Vec<(String, ReturnValueStatus)> {
    let (d1, d2) = common_core::raw_kotlin_metadata(bytes).expect("read Kotlin metadata");
    let d1 = vec![d1.into_iter().map(char::from).collect::<String>()];
    krusty::jvm::metadata::decode_metadata(&d1, &d2, Some(1), owner, None, &[])
        .expect("classifier metadata decodes")
        .class_functions
        .iter()
        .map(|function| (function.kotlin_name.clone(), function.return_value_status))
        .collect()
}

fn krusty_class(
    stem: &str,
    source: &str,
    classpath: &[std::path::PathBuf],
    class: &str,
) -> Vec<u8> {
    let classes = common::compile_in_process_files(
        &[(stem, source)],
        classpath,
        Some(common::jdk_modules().as_path()),
    )
    .unwrap_or_else(|| panic!("krusty compiles {stem}"));
    classes
        .iter()
        .find(|(name, _)| name == class)
        .map(|(_, bytes)| bytes.clone())
        .unwrap_or_else(|| {
            let emitted = classes
                .iter()
                .map(|(name, _)| name.as_str())
                .collect::<Vec<_>>();
            panic!("krusty emitted no {class}; emitted: {emitted:?}")
        })
}

#[test]
fn overrides_keep_must_use_and_explicitly_ignorable_as_distinct_statuses() {
    let classpath = vec![common::stdlib_jar()];
    let (base_classes, reference) = reference_classes(&classpath);
    let mut leaf_classpath = classpath;
    leaf_classpath.push(base_classes);
    let actual = krusty_class(
        "ResultContractLeaf",
        LEAF_SOURCE,
        &leaf_classpath,
        "ResultContractLeaf",
    );

    let expected = vec![
        ("requiredValue".to_string(), ReturnValueStatus::MustUse),
        (
            "discardableValue".to_string(),
            ReturnValueStatus::ExplicitlyIgnorable,
        ),
    ];
    assert_eq!(
        statuses(&reference, "ResultContractLeaf"),
        expected,
        "the pinned reference compiler's contract changed"
    );
    assert_eq!(
        statuses(&actual, "ResultContractLeaf"),
        expected,
        "the emitted override statuses must come from the exact selected declarations"
    );

    let base = krusty_class(
        "ResultContractBase",
        BASE_SOURCE,
        &[common::stdlib_jar()],
        "ResultContractBase",
    );
    let round_trip_dir = common::scratch_dir()
        .expect("allocate return-value-status round-trip directory")
        .join("return-value-status-krusty-base");
    std::fs::create_dir_all(&round_trip_dir).expect("create round-trip classpath directory");
    std::fs::write(round_trip_dir.join("ResultContractBase.class"), base)
        .expect("write krusty base class");
    let round_trip = krusty_class(
        "ResultContractLeafRoundTrip",
        LEAF_SOURCE,
        &[common::stdlib_jar(), round_trip_dir],
        "ResultContractLeaf",
    );
    assert_eq!(
        statuses(&round_trip, "ResultContractLeaf"),
        expected,
        "krusty-emitted base metadata must preserve both statuses for the next compilation"
    );
}
