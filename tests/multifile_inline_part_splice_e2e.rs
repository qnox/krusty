//! A public, non-reified inline function of a multifile class is spliced from its facade part, as
//! kotlinc does: the facade only forwards to the part, so the part holds the body to inline. The
//! forwarder names that part, which decides the body when another part declares a file-private
//! function with the same JVM name and descriptor.

use super::common;

const LIBRARY: &str = "@file:JvmName(\"Api\")\n\
    @file:JvmMultifileClass\n\
    package dependency\n\
    inline fun <T> retain(value: T, fail: Boolean): T =\n\
    \x20 if (fail) throw IllegalStateException() else value\n";

const MAIN: &str = "package consumer\n\
    import dependency.retain\n\
    fun box(): String {\n\
    \x20 val value = \"OK!\".substring(0, retain<Int>(2, false))\n\
    \x20 return if (value == \"OK\") \"OK\" else \"F:$value\"\n\
    }\n";

#[test]
fn a_multifile_inline_body_is_spliced_from_its_part_like_kotlinc() {
    assert_consumer_matches_kotlinc(&[("Library.kt", LIBRARY)], MAIN);
}

/// The earlier part's private function is `pick(String)` on the JVM too; Kotlin rejects two
/// `pick` declarations, so it takes the JVM name through `@JvmName`.
const PRIVATE_PART: &str = "@file:JvmName(\"Api\")\n\
    @file:JvmMultifileClass\n\
    package dependency\n\
    @JvmName(\"pick\")\n\
    private fun hidden(value: String): String = \"private:$value\"\n\
    fun first(): String = hidden(\"a\")\n";

const PUBLIC_PART: &str = "@file:JvmName(\"Api\")\n\
    @file:JvmMultifileClass\n\
    package dependency\n\
    inline fun pick(value: String): String = \"public:$value\"\n";

const PICK_MAIN: &str = "package consumer\n\
    import dependency.pick\n\
    fun box(): String {\n\
    \x20 val value = pick(\"x\")\n\
    \x20 return if (value == \"public:x\") \"OK\" else \"F:$value\"\n\
    }\n";

#[test]
fn a_facade_function_is_the_part_its_forwarder_names_like_kotlinc() {
    assert_consumer_matches_kotlinc(&[("A.kt", PRIVATE_PART), ("B.kt", PUBLIC_PART)], PICK_MAIN);
}

/// Builds `library` with kotlinc (krusty does not write multifile classes yet), compiles `main`
/// against it with both compilers, and requires kotlinc's classes byte for byte and its result.
fn assert_consumer_matches_kotlinc(library: &[(&str, &str)], main: &str) {
    let jdk = common::jdk_modules();
    let stdlib = common::stdlib_jar();
    let scratch = common::scratch_dir().expect("scratch directory");
    let main_source = scratch.join("Main.kt");
    let library_output = scratch.join("library");
    let reference_output = scratch.join("reference");
    std::fs::write(&main_source, main).expect("write consumer source");
    std::fs::create_dir_all(&library_output).expect("create dependency output directory");
    std::fs::create_dir_all(&reference_output).expect("create reference output directory");

    let mut library_arguments = vec![
        "-d".to_string(),
        library_output.to_string_lossy().into_owned(),
    ];
    for (file, source) in library {
        let path = scratch.join(file);
        std::fs::write(&path, source).expect("write dependency source");
        library_arguments.push(path.to_string_lossy().into_owned());
    }
    let (code, stderr) =
        common::kotlinc_compile(&library_arguments).expect("reference kotlinc available");
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
        main,
        "Main",
        &[library_output.clone(), stdlib.clone(), jdk.clone()],
        Some(jdk.as_path()),
    );
    let reference_dir = reference_output.join("consumer");
    let mut reference_classes = std::fs::read_dir(&reference_dir)
        .expect("kotlinc consumer classes")
        .map(|entry| {
            let path = entry.expect("kotlinc class entry").path();
            let stem = path.file_stem().expect("class file name").to_string_lossy();
            let bytes = std::fs::read(&path).expect("kotlinc class bytes");
            (format!("consumer/{stem}"), bytes)
        })
        .collect::<Vec<_>>();
    reference_classes.sort();
    let mut krusty_classes = classes.clone();
    krusty_classes.sort();
    assert_eq!(
        krusty_classes
            .iter()
            .map(|(name, _)| name.as_str())
            .collect::<Vec<_>>(),
        reference_classes
            .iter()
            .map(|(name, _)| name.as_str())
            .collect::<Vec<_>>(),
        "kotlinc's classes"
    );
    for ((name, bytes), (_, reference_bytes)) in krusty_classes.iter().zip(&reference_classes) {
        assert!(
            bytes == reference_bytes,
            "{}",
            common::exact_class_difference(name, reference_bytes, bytes)
        );
    }
    let krusty_result =
        common::run_box(&classes, "consumer.MainKt", &[library_output, stdlib, jdk])
            .expect("run krusty-built consumer");
    let _ = std::fs::remove_dir_all(scratch);
    assert_eq!(reference_result, "OK");
    assert_eq!(krusty_result, reference_result);
}
