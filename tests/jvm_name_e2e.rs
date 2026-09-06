use super::common;

#[test]
fn cross_file_jvm_name_is_realized_after_stable_frontend_selection() {
    let sources = [
        (
            "Declaration.kt",
            r#"import kotlin.jvm.JvmName

               @JvmName("physicalSource")
               fun source(value: String): String = value"#,
        ),
        (
            "Use.kt",
            r#"fun box(): String {
                   val reference = ::source
                   return source("O") + reference("K")
               }"#,
        ),
    ];

    assert_eq!(
        common::compile_and_run_files_with_stdlib(&sources).as_deref(),
        Some("OK")
    );
}

#[test]
fn cross_module_member_jvm_name_keeps_its_kotlin_declaration_name() {
    let jdk = common::jdk_modules();
    let stdlib = common::stdlib_jar();
    let Some(library) = common::compile_lib(
        "member_jvm_name",
        r#"package lib
           open class A {
               @JvmName("physicalF")
               fun <T> f(value: T, fallback: Int = 1): T = value
           }"#,
    ) else {
        return;
    };
    let output = common::compile_and_run_box(
        r#"import lib.A
           fun box(): String = A().f("OK")"#,
        "Main",
        &[library, stdlib, jdk.clone()],
        Some(jdk.as_path()),
    );
    assert_eq!(output.as_deref(), Some("OK"));
}
