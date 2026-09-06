use super::common;

#[test]
fn java_lang_package_still_resolves_string_to_its_kotlin_source_identity() {
    const SOURCE: &str = r#"
        package java.lang

        object Math {
            const val OK: String = "OK"
        }

        fun box(): String = Math.OK
    "#;

    common::expect_front_end_ok_files_with_stdlib(
        &[SOURCE],
        "platform classifiers must be canonical in both frontend passes",
    );
}
