use super::common;

#[test]
fn same_package_class_precedes_default_import() {
    const DECLARATION: &str = "package sample\n\
class UInt(val value: Int)\n";
    const USE: &str = "package sample\n\
fun read(value: UInt): Int = value.value\n\
fun box(): String = if (read(UInt(42)) == 42) \"OK\" else \"fail\"\n";

    assert_eq!(
        common::compile_and_run_files_with_stdlib(&[
            ("Declaration.kt", DECLARATION),
            ("Use.kt", USE),
        ])
        .expect("same-package classifier"),
        "OK"
    );
}

#[test]
fn value_class_precedes_default_import_in_signatures() {
    const SOURCE: &str = "@JvmInline\n\
value class ULong(val value: Long)\n\
class Holder(val value: ULong)\n\
fun make(value: Long): ULong = ULong(value)\n\
fun box(): String {\n\
    val result = Holder(make(42L)).value.value\n\
    return if (result == 42L) \"OK\" else \"fail\"\n\
}\n";

    assert_eq!(
        common::compile_and_run_with_stdlib(SOURCE, "Main").expect("value-class classifier"),
        "OK"
    );
}

#[test]
fn type_parameter_precedes_default_import() {
    const SOURCE: &str = "fun <String> identity(value: String): String = value\n\
fun box(): String = identity<String>(\"OK\")\n";

    assert_eq!(
        common::compile_and_run_with_stdlib(SOURCE, "Main").expect("type parameter"),
        "OK"
    );
}

#[test]
fn classifier_outside_scope_does_not_precede_default_import() {
    const DECLARATION: &str = "package support\n\
class String(val value: Int)\n";
    const USE: &str = "package sample\n\
fun identity(value: String): String = value\n\
fun box(): String = identity(\"OK\")\n";

    assert_eq!(
        common::compile_and_run_files_with_stdlib(&[
            ("Declaration.kt", DECLARATION),
            ("Use.kt", USE),
        ])
        .expect("default import"),
        "OK"
    );
}
