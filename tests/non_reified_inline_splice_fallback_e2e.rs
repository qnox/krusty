//! Runtime differential for a declined splice of a public, non-reified inline declaration.

use super::common;

const LIBRARY: &str = "@file:JvmName(\"Api\")\n\
    @file:JvmMultifileClass\n\
    package dependency\n\
    inline fun <T> retain(value: T): T =\n\
    \x20 if (System.nanoTime() == Long.MIN_VALUE) throw IllegalStateException() else value\n";

const MAIN: &str = "package consumer\n\
    import dependency.retain\n\
    fun box(): String {\n\
    \x20 val value = \"OK!\".substring(0, retain<Int>(2))\n\
    \x20 return if (value == \"OK\") \"OK\" else \"F:$value\"\n\
    }\n";

#[test]
fn a_declined_non_reified_splice_matches_kotlinc_runtime_behavior() {
    let jdk = common::jdk_modules();
    let stdlib = common::stdlib_jar();
    let scratch = common::scratch_dir().expect("scratch directory");
    let library_source = scratch.join("Library.kt");
    let main_source = scratch.join("Main.kt");
    let library_output = scratch.join("library");
    let reference_output = scratch.join("reference");
    std::fs::write(&library_source, LIBRARY).expect("write dependency source");
    std::fs::write(&main_source, MAIN).expect("write consumer source");
    std::fs::create_dir_all(&library_output).expect("create dependency output directory");
    std::fs::create_dir_all(&reference_output).expect("create reference output directory");

    let (code, stderr) = common::kotlinc_compile(&[
        "-d".to_string(),
        library_output.to_string_lossy().into_owned(),
        library_source.to_string_lossy().into_owned(),
    ])
    .expect("reference kotlinc available");
    assert_eq!(
        code, 0,
        "kotlinc must build the inline dependency: {stderr}"
    );
    let classpath = std::env::join_paths([library_output.as_path(), stdlib.as_path()])
        .expect("reference classpath")
        .to_string_lossy()
        .into_owned();
    let (code, stderr) = common::kotlinc_compile(&[
        "-d".to_string(),
        reference_output.to_string_lossy().into_owned(),
        "-cp".to_string(),
        classpath,
        main_source.to_string_lossy().into_owned(),
    ])
    .expect("reference kotlinc available");
    assert_eq!(code, 0, "kotlinc must accept the consumer: {stderr}");

    let reference_result = common::run_box(
        &[],
        "consumer.MainKt",
        &[
            reference_output.clone(),
            library_output.clone(),
            stdlib.clone(),
            jdk.clone(),
        ],
    )
    .expect("run kotlinc-built consumer");
    let classes = common::expect_compile_in_process(
        MAIN,
        "Main",
        &[library_output.clone(), stdlib.clone(), jdk.clone()],
        Some(jdk.as_path()),
    );
    let bytes = classes
        .iter()
        .find_map(|(name, bytes)| (name == "consumer/MainKt").then_some(bytes))
        .expect("exact consumer facade");
    let body = krusty::jvm::classreader::read_method_code(bytes, "box", "()Ljava/lang/String;")
        .expect("consumer box body");
    let calls = krusty::jvm::inline::disassemble(&body.code)
        .expect("consumer instructions")
        .iter()
        .filter_map(|instruction| krusty::jvm::inline::invoked_method(instruction, &body.source_cp))
        .filter(|(owner, _, _, _)| *owner == "dependency/Api")
        .map(|(owner, name, descriptor, _)| {
            (owner.to_owned(), name.to_owned(), descriptor.to_owned())
        })
        .collect::<Vec<_>>();
    assert_eq!(
        calls,
        vec![(
            "dependency/Api".to_string(),
            "retain".to_string(),
            "(Ljava/lang/Object;)Ljava/lang/Object;".to_string(),
        )],
        "the unsupported public non-reified body must use its legal direct call",
    );
    let krusty_result =
        common::run_box(&classes, "consumer.MainKt", &[library_output, stdlib, jdk])
            .expect("run krusty-built consumer");
    let _ = std::fs::remove_dir_all(scratch);
    assert_eq!(reference_result, "OK");
    assert_eq!(krusty_result, reference_result);
}
