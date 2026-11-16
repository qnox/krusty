//! A library compiled without `AnnotationsInMetadata` (language level below 2.4) states
//! `@DeprecatedSinceKotlin(hiddenSince = ...)` only on the function's class-file method. kotlinc
//! reads it there and removes the function from resolution once the API version reaches it, so a
//! call selects the remaining overload. Gradle's bundled stdlib 1.8.20 hides `String.toBoolean()`
//! this way in favour of `String?.toBoolean()`.

use super::common;

const HIDDEN: (&str, &str) = (
    "Hidden.kt",
    "package kotlin.hiddenfixture\n\
     @Deprecated(\"Use the nullable receiver.\")\n\
     @DeprecatedSinceKotlin(hiddenSince = \"1.4\")\n\
     fun String.pick(): String = \"hidden\"\n",
);

const VISIBLE: (&str, &str) = (
    "Visible.kt",
    "package kotlin.hiddenfixture\n\
     fun String?.pick(): String = this ?: \"null\"\n",
);

const CALLER: &str = "import kotlin.hiddenfixture.pick\n\
    fun box(): String = \"OK\".pick()\n";

#[test]
fn a_class_file_hidden_since_removes_the_function_from_resolution() {
    let library = common::kotlinc_lib::kotlinc_lib_out_with(
        &[HIDDEN, VISIBLE],
        &[
            "-Xallow-kotlin-package".into(),
            "-language-version".into(),
            "2.2".into(),
        ],
    )
    .expect("reference compiles the library");
    let classpath = vec![library, common::stdlib_jar()];
    let reference = common::kotlinc_box_files_result_with_classpath(
        &[("Caller.kt", CALLER)],
        "CallerKt",
        &classpath,
    );
    assert_eq!(reference, "OK", "kotlinc");
    let output = common::expect_box_run(CALLER, "Caller", &classpath, Some(&common::jdk_modules()));
    assert_eq!(output, "OK");
}
