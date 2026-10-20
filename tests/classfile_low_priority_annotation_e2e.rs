//! A library compiled without `AnnotationsInMetadata` (language level below 2.4) states a
//! function's annotations only on its class-file method, not in `@Metadata`. kotlinc still reads
//! `@LowPriorityInOverloadResolution` there, so a call prefers the other declaration with the same
//! signature: kotlinx-coroutines' `runBlocking` resolves to the `runBlockingK` realization, not the
//! low-priority one. The two declarations come from two separately built libraries here.

use std::path::PathBuf;

use super::common;

const LOW: (&str, &str) = (
    "Low.kt",
    "@file:Suppress(\"INVISIBLE_REFERENCE\", \"INVISIBLE_MEMBER\")\n\
     package lib\n\
     @kotlin.internal.LowPriorityInOverloadResolution\n\
     fun pick(value: String): String = \"low priority\"\n",
);

const NORMAL: (&str, &str) = (
    "Normal.kt",
    "package lib\n\
     fun pick(value: String): String = value\n",
);

const CALLER: &str = "import lib.pick\nfun box(): String = pick(\"OK\")\n";

/// `source` compiled by krusty at language level 2.2, written to a classpath directory.
fn library_at_language_2_2(source: (&str, &str)) -> PathBuf {
    let settings = krusty::language_settings::LanguageSettings::new(
        krusty::language_version::LanguageVersion::V2_2,
        None,
        &[],
    )
    .expect("2.2 language settings");
    let classes = common::compile_in_process_files_language_settings(
        &[source],
        &[common::stdlib_jar()],
        Some(&common::jdk_modules()),
        &settings,
        Some([2, 2, 0]),
    )
    .expect("krusty compiles the library");
    let out = common::scratch_dir()
        .expect("allocate a library directory")
        .join("lib");
    for (name, bytes) in classes {
        // Classes come back by internal name; the module file keeps its own extension.
        let file = if name.ends_with(".kotlin_module") {
            name
        } else {
            format!("{name}.class")
        };
        let path = out.join(file);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
    }
    out
}

#[test]
fn a_class_file_low_priority_annotation_leaves_the_other_overload_selected() {
    let classpath = [
        library_at_language_2_2(LOW),
        library_at_language_2_2(NORMAL),
        common::stdlib_jar(),
    ];
    let reference = common::kotlinc_box_files_result_with_classpath(
        &[("Caller.kt", CALLER)],
        "CallerKt",
        &classpath,
    );
    assert_eq!(reference, "OK", "kotlinc");
    let output = common::expect_box_run(CALLER, "Caller", &classpath, Some(&common::jdk_modules()));
    assert_eq!(output, "OK");
}
